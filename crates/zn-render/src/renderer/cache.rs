use std::collections::BTreeMap;

/// Идентификаторы GPU-ресурсов: живут только внутри zn-render.
pub type MeshId = u64;
pub type MaterialId = u64;

pub const MESH_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
pub const TEXTURE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Запись кэша: ресурс + счётчик последнего использования + размер.
pub struct CacheEntry<T> {
    pub resource: T,
    last_used: u64,
    pub size_bytes: u64,
}

/// LRU-кэш GPU-ресурсов с бюджетом в байтах.
///
/// `clock` монотонно растёт; `get`/`touch` обновляют `last_used`. Выселение
/// происходит при вставке (`insert`), чтобы уложиться в бюджет; запись,
/// вставленная одной операцией, не выселяется сама собой.
pub struct Cache<T> {
    entries: BTreeMap<u64, CacheEntry<T>>,
    budget: u64,
    used: u64,
    clock: u64,
    next_id: u64,
    evicted: u64,
}

impl<T> Cache<T> {
    pub fn with_budget(budget: u64) -> Self {
        Self {
            entries: BTreeMap::new(),
            budget,
            used: 0,
            clock: 0,
            next_id: 1,
            evicted: 0,
        }
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn evicted_total(&self) -> u64 {
        self.evicted
    }

    /// Вставляет ресурс и возвращает его ID; выселяет LRU-записи при
    /// превышении бюджета. Ресурс больше бюджета вставляется с предупреждением.
    pub fn insert(&mut self, resource: T, size_bytes: u64) -> u64 {
        if size_bytes > self.budget {
            log::warn!(
                "ресурс {size_bytes} Б превышает бюджет кэша {} Б — выселение невозможно",
                self.budget
            );
        }
        self.clock += 1;
        let id = self.next_id;
        self.next_id += 1;
        self.used += size_bytes;
        self.entries.insert(
            id,
            CacheEntry {
                resource,
                last_used: self.clock,
                size_bytes,
            },
        );
        self.evict_to_budget();
        id
    }

    /// Помечает запись использованной (кадр отрисовки).
    pub fn touch(&mut self, id: u64) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.last_used = clock;
        }
    }

    pub fn contains(&self, id: u64) -> bool {
        self.entries.contains_key(&id)
    }

    /// Неблокирующий доступ без обновления свежести (для отрисовки).
    pub fn peek(&self, id: u64) -> Option<&T> {
        self.entries.get(&id).map(|entry| &entry.resource)
    }

    pub fn get(&mut self, id: u64) -> Option<&T> {
        self.touch(id);
        self.entries.get(&id).map(|entry| &entry.resource)
    }

    pub fn get_entry(&mut self, id: u64) -> Option<&CacheEntry<T>> {
        self.touch(id);
        self.entries.get(&id)
    }

    /// Прямой доступ к записям (для простановки id после вставки).
    pub fn entries_mut(&mut self) -> &mut BTreeMap<u64, CacheEntry<T>> {
        &mut self.entries
    }

    pub fn remove(&mut self, id: u64) -> Option<T> {
        if let Some(entry) = self.entries.remove(&id) {
            self.used = self.used.saturating_sub(entry.size_bytes);
            Some(entry.resource)
        } else {
            None
        }
    }

    /// Выселяет наименее недавно использованные записи, пока `used > budget`.
    /// Только что вставленная запись (last_used == clock) не выселяется.
    pub fn evict_to_budget(&mut self) {
        let current = self.clock;
        while self.used > self.budget {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.last_used < current)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.used = self.used.saturating_sub(entry.size_bytes);
                self.evicted += 1;
                log::debug!(
                    "выселение LRU-записи {oldest}: освобождено {} Б, занято {}/{}",
                    entry.size_bytes,
                    self.used,
                    self.budget
                );
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_assigns_incremental_ids() {
        let mut cache = Cache::with_budget(1024);
        let first = cache.insert(vec![0u8; 10], 10);
        let second = cache.insert(vec![0u8; 20], 20);
        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(cache.used(), 30);
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(first));
    }

    #[test]
    fn eviction_respects_budget_lru_order() {
        let mut cache = Cache::with_budget(100);
        let a = cache.insert(vec![0u8; 40], 40);
        let b = cache.insert(vec![0u8; 40], 40);
        cache.touch(a); // a использована позже b → b выселится
        let c = cache.insert(vec![0u8; 40], 40); // 120 > 100 → evict b
        assert!(!cache.contains(b), "LRU-запись b должна быть выселена");
        assert!(cache.contains(a));
        assert!(cache.contains(c));
        assert_eq!(cache.used(), 80);
        assert_eq!(cache.evicted_total(), 1);
    }

    #[test]
    fn oversize_entry_is_kept_but_warned() {
        let mut cache = Cache::with_budget(10);
        let big = cache.insert(vec![0u8; 100], 100);
        assert!(cache.contains(big)); // не выселяем то, что только что вставили
        assert_eq!(cache.used(), 100);
    }

    #[test]
    fn get_updates_recency() {
        let mut cache = Cache::with_budget(100);
        let a = cache.insert("a", 30);
        let b = cache.insert("b", 30);
        assert_eq!(cache.get(a), Some(&"a")); // get обновляет свежесть a
        let c = cache.insert("c", 50); // 110 > 100 → выселяется b (самый старый)
        assert!(!cache.contains(b));
        assert!(cache.contains(a));
        assert!(cache.contains(c));
        let _ = b;
    }

    #[test]
    fn remove_frees_bytes() {
        let mut cache = Cache::with_budget(1000);
        let a = cache.insert(vec![0u8; 50], 50);
        let removed = cache.remove(a);
        assert!(removed.is_some());
        assert!(!cache.contains(a));
        assert_eq!(cache.used(), 0);
        assert_eq!(cache.len(), 0);
    }
}
