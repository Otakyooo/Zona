//! Единый тип ошибки лаунчера: обёртки над io/serde_json плюс доменные
//! варианты (манифест, канал, установка, конфиг).

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Файловая система: нет прав, диск, отсутствующие файлы.
    Io(std::io::Error),
    /// HTTP: запрос не удался или вернул не-2xx.
    Http(String),
    /// Ответ не разобрался как JSON.
    Json(serde_json::Error),
    /// Строка версии — не валидный semver.
    Version(String),
    /// Манифест структурно невалиден.
    Manifest(String),
    /// sha256 скачанного файла не совпал с манифестом.
    Integrity { expected: String, actual: String },
    /// Канал манифеста не совпал с ожидаемым.
    Channel { expected: String, actual: String },
    /// Папка установки: не абсолютный путь или нет прав на запись.
    Install(String),
    /// Клиент запущен: нельзя подменить exe — нужно закрыть игру.
    GameRunning,
    /// Конфиг лаунчера повреждён.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "ошибка ФС: {e}"),
            Error::Http(m) => write!(f, "сеть: {m}"),
            Error::Json(e) => write!(f, "JSON: {e}"),
            Error::Version(m) => write!(f, "версия: {m}"),
            Error::Manifest(m) => write!(f, "манифест: {m}"),
            Error::Integrity { expected, actual } => write!(
                f,
                "контрольная сумма не совпала: ожидалась {expected}, получена {actual}"
            ),
            Error::Channel { expected, actual } => {
                write!(f, "канал '{actual}' не совпадает с ожидаемым '{expected}'")
            }
            Error::Install(m) => write!(f, "установка: {m}"),
            Error::GameRunning => {
                write!(f, "игра запущена — закройте ZONA и повторите обновление")
            }
            Error::Config(m) => write!(f, "конфиг: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io(source)
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Error::Json(source)
    }
}
