//! Персист аккаунтов (M5): SQLite вместо файла стэшей (M4d-2).
//!
//! Один процесс сервера — один писатель БД; WAL + `busy_timeout` переживают
//! сбои питания. Ошибка открытия по явному пути — hard fail (молчаливая
//! потеря персиста хуже); ошибки запросов в рантайме возвращаются наверх и
//! логируются вызывающим кодом (`log::warn!` + retry на следующем flush).
//!
//! Схема создаётся сразу полная (wallets/facilities/stats задействует M5-2) —
//! миграция внутри M5 не нужна. Рост схемы — `DB_SCHEMA_VERSION`, как
//! `STASH_FILE_VERSION` у легаси-файла.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use zn_sim::Stack;

use rusqlite::{Connection, OptionalExtension};

/// Версия схемы БД: файл от новой версии при чтении старой — предупреждение
/// и отказ работать с персистом (fail-soft, как у легаси-стэшей).
pub const DB_SCHEMA_VERSION: u32 = 1;

/// Имя файла БД по умолчанию (в рабочей директории сервера).
pub const DB_FILE_DEFAULT: &str = "zona.db";

/// Полная схема БД (M5): аккаунты и стэшы — сейчас, кошельки/апгрейды/статистика
/// (M5-2) — сразу, чтобы не мигрировать позже.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    password_hash TEXT,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS stashes (
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    slot INTEGER NOT NULL,
    item INTEGER NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (account_id, slot)
);
CREATE TABLE IF NOT EXISTS wallets (
    account_id INTEGER PRIMARY KEY REFERENCES accounts(id),
    balance INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS facilities (
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    facility INTEGER NOT NULL,
    level INTEGER NOT NULL,
    PRIMARY KEY (account_id, facility)
);
CREATE TABLE IF NOT EXISTS stats (
    account_id INTEGER PRIMARY KEY REFERENCES accounts(id),
    raids INTEGER NOT NULL DEFAULT 0,
    extractions INTEGER NOT NULL DEFAULT 0,
    deaths INTEGER NOT NULL DEFAULT 0,
    kills INTEGER NOT NULL DEFAULT 0
);
";

/// Аккаунт из БД. `password_hash = NULL` — «незаклеймленный» аккаунт из
/// миграции `stashes.ron`: первый успешный вход задаёт пароль (claim).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub name: String,
    /// PHC-строка argon2id от дайджеста пароля; `None` — claim.
    pub password_hash: Option<String>,
}

/// Сводка по аккаунту для консоли `accounts` (M5-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSummary {
    pub id: i64,
    pub name: String,
    /// Пароль задан; `false` — миграционный аккаунт без claim.
    pub claimed: bool,
    /// Баланс кошелька, RU.
    pub wallet: u32,
    /// Уровни станций [верстак, медпункт, сейф].
    pub facilities: [u8; 3],
    /// Предметов в стэше.
    pub stash_items: usize,
    /// Unix-время последнего входа.
    pub last_seen_at: i64,
}

/// База аккаунтов поверх SQLite. Потокобезопасность не нужна: сервер
/// однопоточный (`Session::poll(dt)` в главном цикле).
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Открывает (и при первом запуске создаёт схему) БД по пути.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", DB_SCHEMA_VERSION)?;
        } else if version > DB_SCHEMA_VERSION {
            log::warn!(
                "бд: версия файла {} > {} — персист может читаться неполно",
                version,
                DB_SCHEMA_VERSION
            );
        }
        Ok(Self { conn })
    }

    /// Ищет аккаунт по имени (без пароля).
    pub fn find_account(&self, name: &str) -> Option<Account> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, password_hash FROM accounts WHERE name = ?1")
            .ok()?;
        let mut rows = statement.query([name]).ok()?;
        let row = rows.next().ok()??;
        Some(Account {
            id: row.get(0).ok()?,
            name: row.get(1).ok()?,
            password_hash: row.get(2).ok()?,
        })
    }

    /// Создаёт аккаунт. `password_hash = None` — незаклеймленный (миграция).
    /// Возвращает `id`. Дубль имени — ошибка UNIQUE наверху.
    pub fn insert_account(
        &self,
        name: &str,
        password_hash: Option<&str>,
    ) -> Result<i64, rusqlite::Error> {
        let now = unix_secs();
        self.conn.execute(
            "INSERT INTO accounts (name, password_hash, created_at, last_seen_at) \
             VALUES (?1, ?2, ?3, ?3)",
            rusqlite::params![name, password_hash, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Задаёт (или меняет) пароль аккаунта — claim мигрированного или смена.
    pub fn set_password(&self, name: &str, password_hash: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE accounts SET password_hash = ?2 WHERE name = ?1",
            rusqlite::params![name, password_hash],
        )?;
        Ok(())
    }

    /// Отмечает время последнего входа (консоль `accounts`, отладка).
    pub fn touch(&self, name: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE accounts SET last_seen_at = ?2 WHERE name = ?1",
            rusqlite::params![name, unix_secs()],
        )?;
        Ok(())
    }

    /// Сколько аккаунтов в БД (guard идемпотентности миграции).
    pub fn account_count(&self) -> Result<usize, rusqlite::Error> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?;
        Ok(count.max(0) as usize)
    }

    /// Склад аккаунта: стопки без слияния, `slot` — позиция в панели.
    pub fn load_stash(&self, account_id: i64) -> Result<Vec<Stack>, rusqlite::Error> {
        let mut statement = self
            .conn
            .prepare("SELECT item, count FROM stashes WHERE account_id = ?1 ORDER BY slot")?;
        let rows = statement.query_map([account_id], |row| {
            let item: u16 = row.get(0)?;
            let count: u32 = row.get(1)?;
            Ok(Stack::new(item, count))
        })?;
        let mut stacks = Vec::new();
        for stack in rows {
            stacks.push(stack?);
        }
        Ok(stacks)
    }

    /// Перезаписывает склад аккаунта целиком (транзакция delete+insert).
    pub fn save_stash(&self, account_id: i64, stacks: &[Stack]) -> Result<(), rusqlite::Error> {
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute("DELETE FROM stashes WHERE account_id = ?1", [account_id])?;
        for (slot, stack) in stacks.iter().enumerate() {
            transaction.execute(
                "INSERT INTO stashes (account_id, slot, item, count) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![account_id, slot as i64, stack.item, stack.count],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Импорт легаси-стэшей (M4d-2): аккаунты без пароля (claim) + их склады.
    /// Существующие имена пропускаются — повторный вызов безопасен.
    /// Возвращает число импортированных профилей.
    pub fn import_legacy_stashes(
        &self,
        stashes: &HashMap<String, Vec<Stack>>,
    ) -> Result<usize, rusqlite::Error> {
        let mut imported = 0usize;
        for (name, stacks) in stashes {
            if self.find_account(name).is_some() {
                continue;
            }
            let account_id = self.insert_account(name, None)?;
            self.save_stash(account_id, stacks)?;
            imported += 1;
        }
        Ok(imported)
    }

    /// Баланс кошелька аккаунта (RU). Нет строки — аккаунт новый:
    /// возвращает `START_WALLET` (первое сохранение задаёт явный баланс).
    pub fn load_wallet(&self, account_id: i64) -> Result<u32, rusqlite::Error> {
        let balance: Option<i64> = self
            .conn
            .query_row(
                "SELECT balance FROM wallets WHERE account_id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(balance.map_or(crate::START_WALLET, |value| value.max(0) as u32))
    }

    /// Сохраняет баланс кошелька (upsert — вызывается после любой траты).
    pub fn save_wallet(&self, account_id: i64, balance: u32) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO wallets (account_id, balance) VALUES (?1, ?2) \
             ON CONFLICT(account_id) DO UPDATE SET balance = ?2",
            rusqlite::params![account_id, balance],
        )?;
        Ok(())
    }

    /// Уровни станций базы [верстак, медпункт, сейф]; нет строк — нули.
    pub fn load_facilities(&self, account_id: i64) -> Result<[u8; 3], rusqlite::Error> {
        let mut levels = [0u8; 3];
        let mut statement = self
            .conn
            .prepare("SELECT facility, level FROM facilities WHERE account_id = ?1")?;
        let rows = statement.query_map([account_id], |row| {
            let facility: i64 = row.get(0)?;
            let level: u32 = row.get(1)?;
            Ok((facility, level))
        })?;
        for row in rows {
            let (facility, level) = row?;
            if (0..3).contains(&facility) {
                levels[facility as usize] = level.min(u8::MAX as u32) as u8;
            }
        }
        Ok(levels)
    }

    /// Сохраняет уровни станций (upsert по каждой — вызывается после апгрейда).
    pub fn save_facilities(&self, account_id: i64, levels: [u8; 3]) -> Result<(), rusqlite::Error> {
        let transaction = self.conn.unchecked_transaction()?;
        for (facility, level) in levels.iter().enumerate() {
            transaction.execute(
                "INSERT INTO facilities (account_id, facility, level) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(account_id, facility) DO UPDATE SET level = ?3",
                rusqlite::params![account_id, facility as i64, level],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Сводка по всем аккаунтам для консоли `accounts` (M5-3): id, имя,
    /// заклеймованность, кошелёк, уровни станций, размер стэша, последний вход.
    pub fn list_accounts(&self) -> Result<Vec<AccountSummary>, rusqlite::Error> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, password_hash, last_seen_at FROM accounts ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.is_some(),
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            let (id, name, claimed, last_seen_at) = row?;
            let stash_items: usize = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM stashes WHERE account_id = ?1",
                    [id],
                    |row| row.get::<_, i64>(0),
                )?
                .max(0) as usize;
            accounts.push(AccountSummary {
                id,
                name,
                claimed,
                wallet: self.load_wallet(id)?,
                facilities: self.load_facilities(id)?,
                stash_items,
                last_seen_at,
            });
        }
        Ok(accounts)
    }

    /// Копирует БД в файл `path` через SQLite Online Backup API (M5-3):
    /// консистентный снапшот даже при живых читателях. Для ночных бэкапов
    /// и ручного снятия копии перед выкаткой.
    pub fn backup_to(&self, path: &Path) -> Result<(), rusqlite::Error> {
        let mut destination = Connection::open(path)?;
        let backup = rusqlite::backup::Backup::new(&self.conn, &mut destination)?;
        backup.run_to_completion(4096, std::time::Duration::from_millis(10), None)
    }
}

/// unix-время в секундах (столбцы `created_at`/`last_seen_at`).
fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().expect("временная директория");
        let db = Database::open(&dir.path().join("zona.db")).expect("БД создаётся");
        (dir, db)
    }

    #[test]
    fn account_insert_find_and_claim() {
        let (_dir, db) = test_db();
        assert_eq!(db.account_count().unwrap(), 0);

        // Мигрированный аккаунт: без пароля.
        let id = db.insert_account("stalker", None).unwrap();
        let account = db.find_account("stalker").expect("аккаунт создан");
        assert_eq!(account.id, id);
        assert_eq!(account.password_hash, None);
        assert!(db.find_account("другой").is_none());

        // Claim: первый вход задаёт пароль.
        db.set_password("stalker", "phc-hash").unwrap();
        assert_eq!(
            db.find_account("stalker").unwrap().password_hash.as_deref(),
            Some("phc-hash")
        );
        db.touch("stalker").unwrap();
    }

    #[test]
    fn stash_roundtrips_with_slot_order() {
        let (_dir, db) = test_db();
        let id = db.insert_account("trader", Some("hash")).unwrap();
        let stacks = vec![Stack::new(3, 10), Stack::new(0, 1), Stack::new(7, 2)];
        db.save_stash(id, &stacks).unwrap();

        let loaded = db.load_stash(id).unwrap();
        assert_eq!(loaded, stacks, "склад читается в том же порядке");

        // Перезапись очищает прошлые стопки.
        db.save_stash(id, &[Stack::new(1, 5)]).unwrap();
        assert_eq!(db.load_stash(id).unwrap(), vec![Stack::new(1, 5)]);
    }

    #[test]
    fn legacy_import_is_idempotent_and_preserves_stashes() {
        let (_dir, db) = test_db();
        let mut stashes = HashMap::new();
        stashes.insert("alpha".to_string(), vec![Stack::new(2, 4)]);
        stashes.insert("beta".to_string(), vec![Stack::new(5, 60)]);

        assert_eq!(db.import_legacy_stashes(&stashes).unwrap(), 2);
        assert_eq!(db.account_count().unwrap(), 2);
        let alpha = db.find_account("alpha").unwrap();
        assert_eq!(db.load_stash(alpha.id).unwrap(), vec![Stack::new(2, 4)]);
        assert_eq!(alpha.password_hash, None, "миграция — без пароля (claim)");

        // Повторный импорт ничего не добавляет и не затирает claim-пароли.
        db.set_password("alpha", "phc").unwrap();
        assert_eq!(db.import_legacy_stashes(&stashes).unwrap(), 0);
        assert_eq!(db.account_count().unwrap(), 2);
        assert_eq!(
            db.find_account("alpha").unwrap().password_hash.as_deref(),
            Some("phc"),
            "claim переживает повторный импорт"
        );
    }

    #[test]
    fn empty_stash_roundtrips() {
        let (_dir, db) = test_db();
        let id = db.insert_account("ghost", Some("hash")).unwrap();
        db.save_stash(id, &[Stack::new(4, 3)]).unwrap();
        db.save_stash(id, &[]).unwrap();
        assert!(db.load_stash(id).unwrap().is_empty());
    }

    #[test]
    fn wallet_defaults_to_start_balance_and_upserts() {
        let (_dir, db) = test_db();
        let id = db.insert_account("rich", Some("hash")).unwrap();
        assert_eq!(db.load_wallet(id).unwrap(), crate::START_WALLET);

        db.save_wallet(id, 3_500).unwrap();
        assert_eq!(db.load_wallet(id).unwrap(), 3_500);

        // upsert: повторная запись перезаписывает, а не дублирует строку.
        db.save_wallet(id, 100).unwrap();
        assert_eq!(db.load_wallet(id).unwrap(), 100);

        // Гость без аккаунта строки не создаёт: другой id — стартовый баланс.
        let other = db.insert_account("other", Some("hash")).unwrap();
        assert_eq!(db.load_wallet(other).unwrap(), crate::START_WALLET);
    }

    #[test]
    fn facilities_roundtrip_with_levels() {
        let (_dir, db) = test_db();
        let id = db.insert_account("builder", Some("hash")).unwrap();
        assert_eq!(db.load_facilities(id).unwrap(), [0, 0, 0]);

        db.save_facilities(id, [2, 0, 1]).unwrap();
        assert_eq!(db.load_facilities(id).unwrap(), [2, 0, 1]);

        // Частичный апгрейд одной станции не затирает остальные.
        db.save_facilities(id, [2, 3, 1]).unwrap();
        assert_eq!(db.load_facilities(id).unwrap(), [2, 3, 1]);

        // Аккаунт без строк — нули.
        let other = db.insert_account("fresh", Some("hash")).unwrap();
        assert_eq!(db.load_facilities(other).unwrap(), [0, 0, 0]);
    }

    #[test]
    fn list_accounts_summarizes_wallet_facilities_and_stash() {
        let (_dir, db) = test_db();
        assert!(
            db.list_accounts().unwrap().is_empty(),
            "пустая бд — пустой список"
        );

        let rich = db.insert_account("rich", Some("hash")).unwrap();
        db.save_wallet(rich, 3_500).unwrap();
        db.save_facilities(rich, [2, 0, 1]).unwrap();
        db.save_stash(rich, &[Stack::new(3, 10), Stack::new(7, 2)])
            .unwrap();

        let migrated = db.insert_account("migrated", None).unwrap();
        db.save_stash(migrated, &[Stack::new(0, 1)]).unwrap();

        let accounts = db.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2, "сводка по всем аккаунтам, по порядку id");

        let rich = &accounts[0];
        assert_eq!(rich.name, "rich");
        assert!(rich.claimed, "задан пароль");
        assert_eq!(rich.wallet, 3_500);
        assert_eq!(rich.facilities, [2, 0, 1]);
        assert_eq!(rich.stash_items, 2);
        assert!(rich.last_seen_at > 0, "unix-время создания");

        let migrated = &accounts[1];
        assert_eq!(migrated.name, "migrated");
        assert!(!migrated.claimed, "миграционный аккаунт без claim");
        assert_eq!(migrated.wallet, crate::START_WALLET);
        assert_eq!(migrated.facilities, [0, 0, 0]);
        assert_eq!(migrated.stash_items, 1);
    }

    #[test]
    fn backup_to_copies_all_accounts() {
        let (dir, db) = test_db();
        let id = db.insert_account("stalker", Some("hash")).unwrap();
        db.save_wallet(id, 777).unwrap();
        db.save_stash(id, &[Stack::new(3, 10)]).unwrap();

        let out = dir.path().join("backup.db");
        db.backup_to(&out).expect("копия создаётся");

        // Копия открывается независимо и содержит те же данные.
        let copy = Database::open(&out).expect("копия открывается");
        let accounts = copy.list_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "stalker");
        assert_eq!(accounts[0].wallet, 777);
        assert_eq!(accounts[0].stash_items, 1);
        assert_eq!(copy.load_stash(id).unwrap(), vec![Stack::new(3, 10)]);
    }
}
