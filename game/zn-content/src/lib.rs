use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const CONTENT_VERSION: u32 = 1;
pub const CONTENT_FORMAT: &str = "ron";

/// Константы FNV-1a (64 бита) для [`ItemCatalog::content_hash`].
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub const SUPPORTED_SCHEMAS: &[&str] =
    &["scene", "anomaly", "artifact", "item", "quest", "creature"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anomaly {
    pub id: String,
    pub display_name: String,
    pub radius: f32,
    pub damage_per_second: f32,
    #[serde(default)]
    pub push_force: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub value: u32,
    #[serde(default)]
    pub effects: Vec<String>,
}

/// Предмет каталога лута (M4b): то, что можно носить, выбрасывать и подбирать.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Стабильный ключ (латиница), например `"medkit"`.
    pub key: String,
    pub display_name: String,
    pub weight_kg: f32,
    pub stack_max: u32,
    /// Эффект использования; `None` — предмет в M4b только переносится.
    #[serde(default)]
    pub use_effect: Option<UseEffect>,
    /// Цвет для рендера предметов на земле и ячеек инвентаря.
    #[serde(default = "default_item_color")]
    pub color: [f32; 3],
}

/// Эффект использования предмета (например, лечение аптечкой).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UseEffect {
    /// Сколько HP восстанавливает.
    pub heal: u16,
    /// Длительность применения в секундах.
    pub duration_secs: f32,
}

fn default_item_color() -> [f32; 3] {
    [0.85, 0.85, 0.85]
}

/// Каталог предметов — результат загрузки `items.ron`.
///
/// После [`ItemCatalog::from_ron`]/[`ItemCatalog::load_ron`] предметы
/// отсортированы по `key`; индекс в этом порядке — номер предмета в сетевом
/// протоколе, поэтому порядок записей в файле на протокол не влияет.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemCatalog {
    #[serde(default)]
    pub items: Vec<Item>,
}

impl ItemCatalog {
    pub fn from_ron(text: &str) -> Result<Self, ContentError> {
        let mut catalog: Self = ron::from_str(text).map_err(ContentError::Deserialize)?;
        catalog.validate()?;
        catalog.items.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(catalog)
    }

    pub fn load_ron(path: impl AsRef<Path>) -> Result<Self, ContentError> {
        let text = std::fs::read_to_string(path).map_err(ContentError::Io)?;
        Self::from_ron(&text)
    }

    pub fn to_ron(&self) -> Result<String, ContentError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(ContentError::Serialize)
    }

    /// Детерминированный хэш каталога (FNV-1a по каноническому RON);
    /// сверяется при handshake рядом с хэшем сцены.
    pub fn content_hash(&self) -> u64 {
        let text = self
            .to_ron()
            .unwrap_or_else(|_| format!("{:?}", self.items.len()));
        let mut hash = FNV_OFFSET_BASIS;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    /// Проверяет каталог перед использованием: ключи, веса, стаки, цвета.
    pub fn validate(&self) -> Result<(), ContentError> {
        let mut seen = std::collections::HashSet::with_capacity(self.items.len());
        for item in &self.items {
            if item.key.is_empty() {
                return Err(ContentError::Validation("у предмета пустой key".into()));
            }
            if !seen.insert(item.key.as_str()) {
                return Err(ContentError::Validation(format!(
                    "повторяющийся key предмета '{}'",
                    item.key
                )));
            }
            if item.display_name.is_empty() {
                return Err(ContentError::Validation(format!(
                    "у предмета '{}' пустое display_name",
                    item.key
                )));
            }
            if !item.weight_kg.is_finite() || item.weight_kg <= 0.0 {
                return Err(ContentError::Validation(format!(
                    "у предмета '{}' некорректный вес {}",
                    item.key, item.weight_kg
                )));
            }
            if item.stack_max == 0 {
                return Err(ContentError::Validation(format!(
                    "у предмета '{}' stack_max = 0",
                    item.key
                )));
            }
            if item.stack_max > u32::from(u16::MAX) {
                return Err(ContentError::Validation(format!(
                    "у предмета '{}' stack_max {} не помещается в сетевое поле u16",
                    item.key, item.stack_max
                )));
            }
            for channel in item.color {
                if !(0.0..=1.0).contains(&channel) {
                    return Err(ContentError::Validation(format!(
                        "у предмета '{}' цвет {channel} вне диапазона [0,1]",
                        item.key
                    )));
                }
            }
            if let Some(use_effect) = &item.use_effect {
                if use_effect.heal == 0 {
                    return Err(ContentError::Validation(format!(
                        "у предмета '{}' use_effect.heal = 0",
                        item.key
                    )));
                }
                if !use_effect.duration_secs.is_finite() || use_effect.duration_secs <= 0.0 {
                    return Err(ContentError::Validation(format!(
                        "у предмета '{}' некорректная длительность применения {}",
                        item.key, use_effect.duration_secs
                    )));
                }
            }
        }
        Ok(())
    }

    /// Номер предмета в протоколе — индекс в отсортированном по ключу каталоге.
    pub fn index_of(&self, key: &str) -> Option<u16> {
        self.items
            .binary_search_by(|item| item.key.as_str().cmp(key))
            .ok()
            .and_then(|index| u16::try_from(index).ok())
    }

    pub fn get(&self, index: u16) -> Option<&Item> {
        self.items.get(usize::from(index))
    }

    pub fn get_by_key(&self, key: &str) -> Option<&Item> {
        self.index_of(key).and_then(|index| self.get(index))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Debug)]
pub enum ContentError {
    Io(std::io::Error),
    Serialize(ron::Error),
    Deserialize(ron::error::SpannedError),
    UnsupportedSchema(&'static str),
    Validation(String),
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Serialize(e) => write!(f, "serialize error: {e}"),
            Self::Deserialize(e) => write!(f, "deserialize error: {e}"),
            Self::UnsupportedSchema(schema) => write!(f, "unsupported content schema: {schema}"),
            Self::Validation(message) => write!(f, "invalid content: {message}"),
        }
    }
}

impl std::error::Error for ContentError {}

pub fn load_ron<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, ContentError> {
    let text = std::fs::read_to_string(path).map_err(ContentError::Io)?;
    ron::from_str(&text).map_err(ContentError::Deserialize)
}

pub fn save_ron<T: Serialize>(value: &T, path: impl AsRef<Path>) -> Result<(), ContentError> {
    let text = ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
        .map_err(ContentError::Serialize)?;
    std::fs::write(path, text).map_err(ContentError::Io)
}

pub fn is_supported_schema(schema: &str) -> bool {
    SUPPORTED_SCHEMAS.contains(&schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG_RON: &str = r#"(
        items: [
            (
                key: "bolt",
                display_name: "Болт",
                weight_kg: 0.05,
                stack_max: 20,
                color: (0.6, 0.6, 0.65),
            ),
            (
                key: "medkit",
                display_name: "Аптечка",
                weight_kg: 0.5,
                stack_max: 3,
                use_effect: Some((heal: 50, duration_secs: 2.0)),
                color: (0.85, 0.22, 0.22),
            ),
        ],
    )"#;

    #[test]
    fn anomaly_roundtrips_through_ron() {
        let anomaly = Anomaly {
            id: "funnel".into(),
            display_name: "Воронка".into(),
            radius: 3.5,
            damage_per_second: 12.0,
            push_force: 4.0,
        };
        let text = ron::ser::to_string(&anomaly).unwrap();
        let loaded: Anomaly = ron::from_str(&text).unwrap();
        assert_eq!(loaded, anomaly);
    }

    #[test]
    fn schemas_are_registered() {
        assert!(is_supported_schema("anomaly"));
        assert!(is_supported_schema("artifact"));
        assert!(is_supported_schema("item"));
        assert!(!is_supported_schema("shader"));
    }

    #[test]
    fn artifact_effects_default_to_empty() {
        let artifact: Artifact =
            ron::from_str("(id: \"flash\", display_name: \"Вспышка\", value: 100)").unwrap();
        assert_eq!(artifact.effects, Vec::<String>::new());
    }

    #[test]
    fn catalog_sorts_items_and_maps_protocol_index() {
        let catalog = ItemCatalog::from_ron(CATALOG_RON).unwrap();
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.items[0].key, "bolt");
        assert_eq!(catalog.index_of("bolt"), Some(0));
        assert_eq!(catalog.index_of("medkit"), Some(1));
        assert_eq!(catalog.get(1).unwrap().display_name, "Аптечка");
        assert_eq!(
            catalog.get_by_key("medkit").unwrap().use_effect,
            Some(UseEffect {
                heal: 50,
                duration_secs: 2.0,
            })
        );
        assert_eq!(catalog.index_of("nope"), None);
        assert_eq!(catalog.get(99), None);
    }

    #[test]
    fn catalog_roundtrips_through_ron() {
        let catalog = ItemCatalog::from_ron(CATALOG_RON).unwrap();
        let text = catalog.to_ron().unwrap();
        let loaded = ItemCatalog::from_ron(&text).unwrap();
        assert_eq!(loaded, catalog);
    }

    #[test]
    fn catalog_hash_ignores_file_order() {
        let reordered = r#"(
            items: [
                (
                    key: "medkit",
                    display_name: "Аптечка",
                    weight_kg: 0.5,
                    stack_max: 3,
                    use_effect: Some((heal: 50, duration_secs: 2.0)),
                    color: (0.85, 0.22, 0.22),
                ),
                (
                    key: "bolt",
                    display_name: "Болт",
                    weight_kg: 0.05,
                    stack_max: 20,
                    color: (0.6, 0.6, 0.65),
                ),
            ],
        )"#;
        let a = ItemCatalog::from_ron(CATALOG_RON).unwrap();
        let b = ItemCatalog::from_ron(reordered).unwrap();
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn item_defaults_apply() {
        let item: Item =
            ron::from_str(r#"(key: "x", display_name: "X", weight_kg: 1.0, stack_max: 1)"#)
                .unwrap();
        assert_eq!(item.use_effect, None);
        assert_eq!(item.color, default_item_color());
    }

    #[test]
    fn catalog_rejects_duplicate_keys() {
        let duplicated = r#"(
            items: [
                (key: "bolt", display_name: "Болт", weight_kg: 0.05, stack_max: 20),
                (key: "bolt", display_name: "Болт 2", weight_kg: 0.06, stack_max: 10),
            ],
        )"#;
        expect_validation_error(duplicated);
    }

    #[test]
    fn catalog_rejects_invalid_fields() {
        let cases = [
            r#"(items: [(key: "", display_name: "X", weight_kg: 1.0, stack_max: 1)])"#,
            r#"(items: [(key: "x", display_name: "", weight_kg: 1.0, stack_max: 1)])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: 0.0, stack_max: 1)])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: -1.0, stack_max: 1)])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: 1.0, stack_max: 0)])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: 1.0, stack_max: 100000)])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: 1.0, stack_max: 1, color: (1.5, 0.0, 0.0))])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: 1.0, stack_max: 1, use_effect: Some((heal: 0, duration_secs: 1.0)))])"#,
            r#"(items: [(key: "x", display_name: "X", weight_kg: 1.0, stack_max: 1, use_effect: Some((heal: 10, duration_secs: 0.0)))])"#,
        ];
        for case in cases {
            expect_validation_error(case);
        }
    }

    #[test]
    fn shipped_catalog_loads_and_validates() {
        let catalog = ItemCatalog::load_ron("../content/items/items.ron")
            .expect("game/content/items/items.ron должен загружаться");
        assert_eq!(catalog.len(), 6);
        assert!(catalog.index_of("medkit").is_some());
        assert!(catalog.index_of("bandage").is_some());
        assert!(catalog.get_by_key("medkit").unwrap().use_effect.is_some());
    }

    fn expect_validation_error(text: &str) {
        match ItemCatalog::from_ron(text) {
            Err(ContentError::Validation(_)) => {}
            other => panic!("ожидали ошибку валидации, получили {other:?}"),
        }
    }
}
