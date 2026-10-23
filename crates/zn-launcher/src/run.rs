//! Запуск клиента из лаунчера: состав аргументов, профиль игрока и
//! dev-режим (рабочее дерево вместо скачанной сборки).

use crate::install::CLIENT_EXE;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Откуда берём адрес сервера: ручной override лаунчера важнее манифеста.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerChoice<'a> {
    pub manual: Option<&'a str>,
    pub manifest: Option<&'a str>,
}

impl ServerChoice<'_> {
    /// Итоговый адрес или `None` (offline). Невалидные значения
    /// отфильтровываются — клиент получит только `host:port`.
    pub fn resolve(&self) -> Option<&str> {
        self.manual
            .filter(|addr| is_host_port(addr))
            .or_else(|| self.manifest.filter(|addr| is_host_port(addr)))
    }
}

/// Предупреждение, если введённый адрес невалиден (в лаунчер ввели мусор).
pub fn server_warning(choice: &ServerChoice<'_>) -> Option<String> {
    let broken = choice
        .manual
        .or(choice.manifest)
        .filter(|addr| !is_host_port(addr))?;
    Some(format!(
        "адрес '{broken}' не похож на host:port — запуск без сети"
    ))
}

/// `host:port` — грубая проверка: непустой хост (можно IPv6 в скобках),
/// порт — число 1..=65535.
pub fn is_host_port(addr: &str) -> bool {
    let Some((host, port)) = addr.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return false;
    }
    matches!(port.parse::<u16>(), Ok(port) if port > 0)
}

/// Команда запуска клиента: абсолютный `--content-root`, `--server` при
/// валидном адресе (иначе offline), вход в аккаунт через `ZN_PROFILE` /
/// `ZN_PASSWORD` (M5-1).
pub fn client_command(
    exe: &Path,
    content_dir: &Path,
    server: &ServerChoice<'_>,
    credentials: &Credentials<'_>,
) -> Command {
    let mut command = Command::new(exe);
    command.arg("--content-root").arg(content_dir);
    if let Some(addr) = server.resolve() {
        command.arg("--server").arg(addr);
    } else {
        // Клиент без флагов идёт на локальный сервер — offline задаём явно.
        command.arg("--offline");
    }
    let account = credentials
        .account
        .map(str::trim)
        .filter(|value| !value.is_empty());
    command.env(
        "ZN_PROFILE",
        account.map(str::to_string).unwrap_or_else(profile_name),
    );
    match credentials
        .password
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(password) => {
            command.env("ZN_PASSWORD", password);
        }
        // Лаунчер — источник правды: без пароля в полях клиент подключается
        // гостем, даже если в окружении лаунчера остался ZN_PASSWORD.
        None => {
            command.env_remove("ZN_PASSWORD");
        }
    }
    command
}

/// Логин/пароль аккаунта (M5-1): лаунчер прокидывает их в клиент
/// переменными окружения. Без пароля клиент — гость: играет, но склад
/// между рейдами не сохраняется.
#[derive(Debug, Clone, Copy, Default)]
pub struct Credentials<'a> {
    pub account: Option<&'a str>,
    pub password: Option<&'a str>,
}

/// Профиль игрока: `ZN_PROFILE`, иначе имя пользователя ОС — стэши не теряются.
pub fn profile_name() -> String {
    let zona = std::env::var("ZN_PROFILE").ok();
    let user = std::env::var("USERNAME")
        .ok()
        .or_else(|| std::env::var("USER").ok());
    profile_from(zona.as_deref(), user.as_deref())
}

/// Чистая логика [`profile_name`]: пробелы/пустая строка не в счёт.
pub fn profile_from(zona_profile: Option<&str>, username: Option<&str>) -> String {
    zona_profile
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| username.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or("player")
        .to_string()
}

/// Dev-режим: `ZN_LAUNCHER_DEV=1` или `Cargo.toml` рабочего дерева рядом с
/// exe (лаунчер запущен из `target/debug/`).
pub fn is_dev_mode(launcher_exe: &Path) -> bool {
    if dev_flag_from(
        std::env::var_os("ZN_LAUNCHER_DEV")
            .as_deref()
            .and_then(|value| value.to_str()),
    ) {
        return true;
    }
    workspace_markers(launcher_exe).iter().any(|marker| *marker)
}

/// Чистая логика флага: включается только значением `1`.
pub fn dev_flag_from(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

/// Ищет `Cargo.toml` рабочего дерева рядом с exe, на уровень и два выше
/// (`target/debug` → `target` → корень дерева).
fn workspace_markers(launcher_exe: &Path) -> Vec<bool> {
    let Some(dir) = launcher_exe.parent() else {
        return Vec::new();
    };
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(parent) = dir.parent() {
        dirs.push(parent.to_path_buf());
        if let Some(grand) = parent.parent() {
            dirs.push(grand.to_path_buf());
        }
    }
    dirs.into_iter()
        .map(|dir| is_workspace_cargo(&dir))
        .collect()
}

fn is_workspace_cargo(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    text.contains("[workspace]")
        || text
            .lines()
            .any(|line| line.trim_start().starts_with("members"))
}

/// Путь к клиенту: в dev-режиме — соседний `zn-client.exe` в том же
/// `target/debug/`, иначе установленный.
pub fn resolve_client_exe(installed_exe: &Path, dev: bool) -> PathBuf {
    if dev {
        installed_exe.with_file_name(CLIENT_EXE)
    } else {
        installed_exe.to_path_buf()
    }
}

/// Корень контента для dev-клиента: `<workspace>/game/content`.
/// exe в `<workspace>/target/debug/` — три уровня вверх до дерева.
pub fn dev_content_root(launcher_exe: &Path) -> Option<PathBuf> {
    let workspace = launcher_exe.parent()?.parent()?.parent()?;
    Some(workspace.join("game").join("content"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_host_port_accepts_valid_and_rejects_junk() {
        assert!(is_host_port("127.0.0.1:7700"));
        assert!(is_host_port("zona.playit.gg:7700"));
        assert!(is_host_port("[::1]:7700"));
        assert!(is_host_port("host:1"));
        assert!(!is_host_port("host"));
        assert!(!is_host_port("host:"));
        assert!(!is_host_port(":7700"));
        assert!(!is_host_port("host:0"));
        assert!(!is_host_port("host:99999"));
        assert!(!is_host_port("host:77 00"));
        assert!(!is_host_port(""));
    }

    #[test]
    fn server_choice_prefers_manual_then_manifest() {
        let choice = ServerChoice {
            manual: Some("10.0.0.5:7700"),
            manifest: Some("zona.playit.gg:7700"),
        };
        assert_eq!(choice.resolve(), Some("10.0.0.5:7700"));
        let choice = ServerChoice {
            manual: Some("мусор"),
            manifest: Some("zona.playit.gg:7700"),
        };
        assert_eq!(choice.resolve(), Some("zona.playit.gg:7700"));
        assert!(server_warning(&choice).is_some());
        let choice = ServerChoice {
            manual: None,
            manifest: None,
        };
        assert_eq!(choice.resolve(), None);
        assert!(server_warning(&choice).is_none());
        let choice = ServerChoice {
            manual: Some("10.0.0.5:7700"),
            manifest: None,
        };
        assert!(server_warning(&choice).is_none());
    }

    #[test]
    fn profile_from_prefers_zona_then_username() {
        assert_eq!(profile_from(Some("zona"), Some("tester")), "zona");
        assert_eq!(profile_from(Some("  "), Some("tester")), "tester");
        assert_eq!(profile_from(None, Some("tester")), "tester");
        assert_eq!(profile_from(None, Some("")), "player");
        assert_eq!(profile_from(None, None), "player");
    }

    #[test]
    fn client_command_arguments_and_profile() {
        let exe = Path::new(r"C:\ZONA\game\zn-client.exe");
        let content = Path::new(r"C:\ZONA\game\content");
        let command = client_command(
            exe,
            content,
            &ServerChoice {
                manual: Some("10.0.0.5:7700"),
                manifest: Some("zona.playit.gg:7700"),
            },
            &Credentials::default(),
        );
        assert_eq!(command.get_program(), exe.as_os_str());
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--content-root".to_string(),
                r"C:\ZONA\game\content".to_string(),
                "--server".to_string(),
                "10.0.0.5:7700".to_string(),
            ]
        );
        let profile = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("ZN_PROFILE"))
            .and_then(|(_, value)| value.map(|v| v.to_string_lossy().into_owned()));
        assert!(profile.is_some(), "ZN_PROFILE не проставлен");
    }

    #[test]
    fn client_command_offline_without_server() {
        let exe = Path::new("/opt/zona/game/zn-client");
        let content = Path::new("/opt/zona/game/content");
        let command = client_command(
            exe,
            content,
            &ServerChoice::default(),
            &Credentials::default(),
        );
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--content-root".to_string(),
                "/opt/zona/game/content".to_string(),
                "--offline".to_string()
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--server"));
    }

    #[test]
    fn client_command_passes_credentials_and_removes_stale_password() {
        let exe = Path::new("/opt/zona/game/zn-client");
        let content = Path::new("/opt/zona/game/content");
        let env_of = |command: &Command| -> Vec<(String, Option<String>)> {
            command
                .get_envs()
                .filter_map(|(key, value)| {
                    let key = key.to_string_lossy().into_owned();
                    if key != "ZN_PROFILE" && key != "ZN_PASSWORD" {
                        return None;
                    }
                    Some((key, value.map(|v| v.to_string_lossy().into_owned())))
                })
                .collect()
        };

        // Пара логин/пароль уходит в окружение клиента.
        let command = client_command(
            exe,
            content,
            &ServerChoice::default(),
            &Credentials {
                account: Some("  stalker  "),
                password: Some("s3cret"),
            },
        );
        let env = env_of(&command);
        assert!(env.contains(&("ZN_PROFILE".to_string(), Some("stalker".to_string()))));
        assert!(env.contains(&("ZN_PASSWORD".to_string(), Some("s3cret".to_string()))));

        // Без пароля — ZN_PASSWORD явно убран (клиент гость).
        let command = client_command(
            exe,
            content,
            &ServerChoice::default(),
            &Credentials {
                account: Some("stalker"),
                password: Some("   "),
            },
        );
        let env = env_of(&command);
        assert!(env.contains(&("ZN_PROFILE".to_string(), Some("stalker".to_string()))));
        assert!(env.contains(&("ZN_PASSWORD".to_string(), None)), "{env:?}");
    }

    #[test]
    fn dev_mode_detection() {
        assert!(dev_flag_from(Some("1")));
        assert!(!dev_flag_from(Some("0")));
        assert!(!dev_flag_from(None));
        // exe в <workspace>/target/<profile>/ — дерево опознаётся
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        assert!(
            is_dev_mode(
                &workspace
                    .join("target")
                    .join("debug")
                    .join("zn-launcher.exe")
            ),
            "дерево должно опознаваться как dev"
        );
        assert!(
            is_dev_mode(
                &workspace
                    .join("target")
                    .join("release")
                    .join("zn-launcher.exe")
            ),
            "release в дереве — тоже dev"
        );
        // вне дерева dev-режима нет
        assert!(!is_dev_mode(
            &std::env::temp_dir().join("nowhere").join("zn-launcher.exe")
        ));
    }

    #[test]
    fn resolve_client_exe_and_content_root() {
        let installed = Path::new(r"C:\Users\t\ZONA\game\zn-client.exe");
        assert_eq!(resolve_client_exe(installed, false), installed);
        assert_eq!(
            resolve_client_exe(installed, true),
            Path::new(r"C:\Users\t\ZONA\game\zn-client.exe")
        );
        // dev: target/debug/zn-launcher.exe → дерево/game/content
        let launcher = Path::new(r"E:\ai_games\target\debug\zn-launcher.exe");
        assert_eq!(
            resolve_client_exe(launcher, true),
            Path::new(r"E:\ai_games\target\debug\zn-client.exe")
        );
        assert_eq!(
            dev_content_root(launcher).unwrap(),
            Path::new(r"E:\ai_games\game\content")
        );
    }
}
