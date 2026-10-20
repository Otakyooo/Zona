//! ZONA launcher — библиотека: манифест канала, конфиг и раскладка установки.
//!
//! CLI (`src/main.rs`, M6-3) и GUI (`src/gui.rs`, M6-4) — тонкие обёртки над
//! этим крейтом: вся логика здесь и покрыта headless-тестами.

pub mod error;
pub mod gui;
pub mod install;
pub mod launcher_update;
pub mod manifest;
pub mod run;
pub mod selfupdate;
pub mod update;

pub use error::{Error, Result};

/// Версия этого лаунчера: штамп CI (`ZONA_LAUNCHER_VERSION`)
/// иначе версия крейта.
pub const LAUNCHER_VERSION: &str = match option_env!("ZONA_LAUNCHER_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
