use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const CONTENT_VERSION: u32 = 1;
pub const CONTENT_FORMAT: &str = "ron";

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

#[derive(Debug)]
pub enum ContentError {
    Io(std::io::Error),
    Serialize(ron::Error),
    Deserialize(ron::error::SpannedError),
    UnsupportedSchema(&'static str),
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Serialize(e) => write!(f, "serialize error: {e}"),
            Self::Deserialize(e) => write!(f, "deserialize error: {e}"),
            Self::UnsupportedSchema(schema) => write!(f, "unsupported content schema: {schema}"),
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
        assert!(!is_supported_schema("shader"));
    }

    #[test]
    fn artifact_effects_default_to_empty() {
        let artifact: Artifact =
            ron::from_str("(id: \"flash\", display_name: \"Вспышка\", value: 100)").unwrap();
        assert_eq!(artifact.effects, Vec::<String>::new());
    }
}
