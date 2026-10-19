//! ZONA launcher — библиотека: манифест канала, конфиг и раскладка установки.
//!
//! CLI (`src/main.rs`, M6-3) и GUI (`src/gui.rs`, M6-4) — тонкие обёртки над
//! этим крейтом: вся логика здесь и покрыта headless-тестами.

pub mod error;
pub mod gui;
pub mod install;
pub mod manifest;
pub mod run;
pub mod update;

pub use error::{Error, Result};
