//! Папка установки и пользовательский конфиг лаунчера.
//!
//! Игрок сам выбирает, куда ставить игру: флаг `--install-dir`, GUI-диалог
//! (M6-4) или интерактивный вопрос (M6-3). Выбор и канал запоминаются в
//! `launcher-config.json` под `%LOCALAPPDATA%\ZONA`; версия установленной
//! сборки — в `installed.json` в корне папки канала.

use crate::error::{Error, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Имя папки лаунчера в профиле пользователя.
pub const APP_DIR_NAME: &str = "ZONA";
/// Файл конфига: канал + выбранная игроком папка установки.
pub const CONFIG_FILE: &str = "launcher-config.json";
/// Файл версии установленной сборки в корне папки канала.
pub const INSTALLED_FILE: &str = "installed.json";
/// Подпапка каналов в дефолтной раскладке.
const CHANNELS_DIR: &str = "channels";
/// Финальная папка игры внутри канала.
pub const GAME_DIR: &str = "game";
/// Подпапка контента внутри игры.
pub const CONTENT_DIR: &str = "content";
/// Имя exe клиента по платформе.
pub const CLIENT_EXE: &str = if cfg!(windows) {
    "zn-client.exe"
} else {
    "zn-client"
};
/// Имя exe лаунчера по платформе (его же несёт клиентский zip).
pub const LAUNCHER_EXE: &str = if cfg!(windows) {
    "zn-launcher.exe"
} else {
    "zn-launcher"
};

/// Пользовательский конфиг лаунчера.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_channel")]
    pub channel: String,
    /// Выбранная игроком папка установки; `None` до первого выбора.
    #[serde(default)]
    pub install_dir: Option<PathBuf>,
    /// «Свой сервер» (`host:port`) из GUI — приоритетнее манифеста.
    #[serde(default)]
    pub server_override: Option<String>,
    /// Логин аккаунта (M5-1) из GUI; `None` — гость (склад не сохраняется).
    #[serde(default)]
    pub account: Option<String>,
    /// Пароль аккаунта (M5-1). Лежит в локальном `launcher-config.json`
    /// открыто — это файл игрока на его машине; сервер хранит только
    /// argon2-хэш sha2-дайджеста, сам пароль по сети не идёт.
    #[serde(default)]
    pub password: Option<String>,
}

fn default_channel() -> String {
    crate::manifest::DEFAULT_CHANNEL.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            channel: default_channel(),
            install_dir: None,
            server_override: None,
            account: None,
            password: None,
        }
    }
}

impl Config {
    /// Читает конфиг; отсутствующий файл — дефолтный конфиг, а не ошибка.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&text)?;
        if config.channel.trim().is_empty() {
            return Err(Error::Config("пустой channel в конфиге".into()));
        }
        Ok(config)
    }

    /// Пишет конфиг атомарно (tmp + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        save_json_atomic(path, self)
    }

    /// Путь конфига в `config_home`.
    pub fn path_in(config_home: &Path) -> PathBuf {
        config_home.join(CONFIG_FILE)
    }
}

/// Версия установленной в канале сборки (`installed.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    #[serde(default)]
    pub build: Option<u32>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub installed_at: Option<u64>,
}

impl Installed {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)?;
        let installed: Installed = serde_json::from_str(&text)?;
        Ok(Some(installed))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_json_atomic(path, self)
    }

    pub fn version_semver(&self) -> Result<Version> {
        Version::parse(&self.version)
            .map_err(|e| Error::Version(format!("installed version '{}': {e}", self.version)))
    }

    /// Строит запись из манифеста (M6-3 дополняет временем установки).
    pub fn from_manifest(manifest: &crate::manifest::Manifest) -> Self {
        Self {
            version: manifest.version.clone(),
            build: manifest.build,
            commit: manifest.commit.clone(),
            installed_at: epoch_secs(),
        }
    }
}

fn epoch_secs() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Раскладка папки канала: `<root>` — папка, выбранная игроком (или дефолт).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Готовая игра: `<root>/game` (сюда кладётся содержимое zip).
    pub fn game_dir(&self) -> PathBuf {
        self.root.join(GAME_DIR)
    }

    /// Бэкап предыдущей игры при свапе: `<root>/game.old` (M6-3).
    pub fn backup_dir(&self) -> PathBuf {
        self.root.join("game.old")
    }

    /// Временная папка текущего процесса: `<root>/staging-<pid>`.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(format!("staging-{}", std::process::id()))
    }

    /// Файл версии: `<root>/installed.json`.
    pub fn installed_file(&self) -> PathBuf {
        self.root.join(INSTALLED_FILE)
    }

    /// Exe клиента: `<root>/game/zn-client.exe`.
    pub fn client_exe(&self) -> PathBuf {
        self.game_dir().join(CLIENT_EXE)
    }

    /// Лаунчер, приехавший вместе с клиентским zip: `<root>/game/zn-launcher.exe`.
    /// Источник для самообновления (M6-7).
    pub fn bundled_launcher(&self) -> PathBuf {
        self.game_dir().join(LAUNCHER_EXE)
    }

    /// Контент: `<root>/game/content`.
    pub fn content_dir(&self) -> PathBuf {
        self.game_dir().join(CONTENT_DIR)
    }
}

/// `%LOCALAPPDATA%\ZONA` (Windows) или `~/.local/share/ZONA` (фолбэк).
pub fn config_home() -> Result<PathBuf> {
    let home = env_path("HOME").or_else(|| env_path("USERPROFILE"));
    config_home_from(env_path("LOCALAPPDATA").as_deref(), home.as_deref())
}

/// Чистая версия [`config_home`] для тестов и нестандартных профилей.
pub fn config_home_from(local_appdata: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    if let Some(base) = local_appdata {
        return Ok(base.join(APP_DIR_NAME));
    }
    if let Some(base) = home {
        return Ok(base.join(".local").join("share").join(APP_DIR_NAME));
    }
    Err(Error::Config(
        "не найдены ни %LOCALAPPDATA%, ни %HOME% — негде хранить конфиг лаунчера".into(),
    ))
}

/// Дефолтная папка установки: `<config_home>/channels/<channel>`.
pub fn default_install_dir(config_home: &Path, channel: &str) -> PathBuf {
    config_home.join(CHANNELS_DIR).join(channel)
}

/// Приоритет выбора папки: флаг → конфиг → дефолт.
pub fn resolve_install_dir(
    cli_flag: Option<&Path>,
    config: &Config,
    config_home: &Path,
) -> Result<PathBuf> {
    if let Some(flag) = cli_flag {
        return validate_install_dir(flag);
    }
    if let Some(dir) = &config.install_dir {
        return validate_install_dir(dir);
    }
    Ok(default_install_dir(config_home, &config.channel))
}

/// Абсолютность пути — минимальное требование к папке игрока.
pub fn validate_install_dir(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(Error::Install("путь установки пуст".into()));
    }
    if !path.is_absolute() {
        return Err(Error::Install(format!(
            "путь установки должен быть абсолютным: {}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

/// Проверяет и создаёт папку: писать в неё должны уметь ДО скачивания.
pub fn ensure_writable(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|e| Error::Install(format!("не удалось создать {}: {e}", dir.display())))?;
    let probe = dir.join(format!(".zona-write-{}", std::process::id()));
    fs::write(&probe, b"zona").map_err(|e| {
        Error::Install(format!(
            "папка {} недоступна для записи: {e}",
            dir.display()
        ))
    })?;
    fs::remove_file(&probe).map_err(|e| {
        Error::Install(format!(
            "не удалось убрать пробник записи {}: {e}",
            probe.display()
        ))
    })?;
    Ok(())
}

/// Атомарная запись JSON: `<path>.tmp` + rename (перезапись разрешена).
fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let text = serde_json::to_string_pretty(value)?;
    let tmp = tmp_sibling(path);
    fs::write(&tmp, text.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_and_default_on_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = Config::path_in(dir.path());
        // нет файла — дефолт
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.channel, "dev");
        assert!(cfg.install_dir.is_none());
        assert!(cfg.server_override.is_none());
        assert!(cfg.account.is_none());
        assert!(cfg.password.is_none());
        // запись и чтение обратно
        let cfg = Config {
            channel: "dev".into(),
            install_dir: Some(dir.path().join("D:/Games/ZONA")),
            server_override: Some("10.0.0.5:7700".into()),
            account: Some("stalker".into()),
            password: Some("s3cret".into()),
        };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
    }

    #[test]
    fn installed_roundtrip_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INSTALLED_FILE);
        assert!(Installed::load(&path).unwrap().is_none());
        let installed = Installed {
            version: "0.1.7".into(),
            build: Some(7),
            commit: Some("deadbee".into()),
            installed_at: Some(1_760_000_000),
        };
        installed.save(&path).unwrap();
        assert_eq!(Installed::load(&path).unwrap(), Some(installed.clone()));
        assert_eq!(
            installed.version_semver().unwrap(),
            Version::parse("0.1.7").unwrap()
        );
    }

    #[test]
    fn resolve_install_dir_priority_flag_over_config_over_default() {
        let home = tempfile::tempdir().unwrap();
        let config = Config {
            channel: "dev".into(),
            install_dir: Some(home.path().join("from-config")),
            server_override: None,
            account: None,
            password: None,
        };
        let flag = home.path().join("from-flag");
        // флаг бьёт конфиг
        assert_eq!(
            resolve_install_dir(Some(&flag), &config, home.path()).unwrap(),
            flag
        );
        // конфиг бьёт дефолт
        assert_eq!(
            resolve_install_dir(None, &config, home.path()).unwrap(),
            home.path().join("from-config")
        );
        // дефолт: <config_home>/channels/dev
        let empty = Config::default();
        assert_eq!(
            resolve_install_dir(None, &empty, home.path()).unwrap(),
            home.path().join("channels").join("dev")
        );
    }

    #[test]
    fn resolve_install_dir_rejects_relative() {
        let config = Config::default();
        let err = resolve_install_dir(Some(Path::new("games/zona")), &config, Path::new("/tmp"))
            .unwrap_err();
        assert!(matches!(err, Error::Install(_)), "получили {err:?}");
    }

    #[test]
    fn ensure_writable_creates_dir_and_cleans_probe() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("install");
        ensure_writable(&target).unwrap();
        assert!(target.is_dir());
        // пробников не осталось
        let leftovers: Vec<_> = fs::read_dir(&target)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(leftovers.is_empty(), "остались файлы: {leftovers:?}");
    }

    #[test]
    fn ensure_writable_fails_when_file_blocks_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blocked");
        fs::write(&file, b"x").unwrap();
        // файл на месте будущей папки — create_dir_all обязан упасть
        assert!(ensure_writable(&file.join("sub")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn ensure_writable_fails_on_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(ensure_writable(&ro).is_err());
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn config_home_prefers_localappdata_then_home() {
        let la = Path::new("/c/Users/tester/AppData/Local");
        assert_eq!(
            config_home_from(Some(la), None).unwrap(),
            la.join(APP_DIR_NAME)
        );
        let home = Path::new("/home/tester");
        assert_eq!(
            config_home_from(None, Some(home)).unwrap(),
            home.join(".local").join("share").join(APP_DIR_NAME)
        );
        assert!(config_home_from(None, None).is_err());
    }

    #[test]
    fn layout_paths() {
        let layout = Layout::new("/data/zona-dev");
        assert_eq!(layout.game_dir(), Path::new("/data/zona-dev/game"));
        assert_eq!(layout.backup_dir(), Path::new("/data/zona-dev/game.old"));
        assert_eq!(
            layout.installed_file(),
            Path::new("/data/zona-dev/installed.json")
        );
        assert_eq!(
            layout.client_exe(),
            Path::new("/data/zona-dev/game").join(CLIENT_EXE)
        );
        assert_eq!(
            layout.content_dir(),
            Path::new("/data/zona-dev/game/content")
        );
        let staging = layout.staging_dir();
        assert!(
            staging.starts_with(Path::new("/data/zona-dev")),
            "staging: {staging:?}"
        );
        assert!(
            staging
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("staging-"),
            "staging: {staging:?}"
        );
    }
}
