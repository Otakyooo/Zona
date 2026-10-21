//! Обновление самого лаунчера (M6-8): отдельный лаунчер-ассет в манифесте.
//!
//! Раньше (M6-7) лаунчер обновлялся только бандлом из клиентского zip:
//! новая версия лаунчера доезжала до игроков вместе с игрой. Теперь
//! `manifest.json` несёт блок `launcher { version, url, sha256, size }` —
//! хотфикс лаунчера можно выкатить без пересборки и перекачки игры
//! (`scripts/publish-launcher-update.ps1`), а при следующем старте
//! лаунчер сам скачает и подменит себя.
//!
//! Откат назад исключён версионным гейтом: и бандл (M6-7), и манифест
//! (M6-8) применяются только если их версия строго новее запущенного
//! лаунчера. Версия запущенного — файл состояния
//! [`LAUNCHER_VERSION_FILE`] в профиле пользователя (пишется после каждой
//! удачной подмены), иначе штамп сборки [`crate::LAUNCHER_VERSION`]
//! (CI-переменная `ZONA_LAUNCHER_VERSION`, фолбэк — версия крейта).
//! Раньше версия жила в sidecar `<exe>.lversion` рядом с exe — на рабочем
//! столе это посторонний файл, поэтому состояние переехало в
//! `%LOCALAPPDATA%\ZONA` (рядом с конфигом); старый sidecar дочитывается
//! один раз при миграции и удаляется.

use crate::error::{Error, Result};
use crate::install::{Layout, LAUNCHER_EXE};
use crate::manifest::Manifest;
use crate::selfupdate::{self, ReplaceOutcome};
use crate::LAUNCHER_VERSION;
use semver::Version;
use std::fs;
use std::path::{Path, PathBuf};

/// Переменная окружения-маркер relaunch: второй проход после подмены
/// лаунчера не пытается обновиться снова (страховка от петли, если
/// sidecar не удалось записать).
pub const RELAUNCHED_ENV: &str = "ZN_LAUNCHER_RELAUNCHED";

/// Итог шага обновления самого лаунчера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfUpdateStep {
    /// Запущенный лаунчер актуален.
    UpToDate,
    /// Лаунчер подменён на месте — нужен relaunch.
    Updated { from: String, to: String },
    /// Заменить запущенный exe не удалось (образ занят) — `.new` рядом.
    Deferred,
    /// Скачать/применить не удалось — предупреждение, играть можно.
    Failed { reason: String },
}

/// Файл версии запущенного лаунчера в каталоге состояния
/// (`%LOCALAPPDATA%\ZONA\launcher-version.txt`) — не рядом с exe, чтобы
/// не мусорить (ранее был sidecar `<exe>.lversion`, в т.ч. на рабочем
/// столе).
pub const LAUNCHER_VERSION_FILE: &str = "launcher-version.txt";

/// Путь к файлу версии в каталоге состояния.
pub fn version_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(LAUNCHER_VERSION_FILE)
}

/// Legacy sidecar `<exe>.lversion`: только миграция, потом удаление.
fn legacy_sidecar_path(current_exe: &Path) -> PathBuf {
    let mut name = current_exe.file_name().unwrap_or_default().to_os_string();
    name.push(".lversion");
    current_exe.with_file_name(name)
}

/// Читает версию из файла состояния; мусор/отсутствие — `None`.
fn read_version_file(path: &Path) -> Option<Version> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| Version::parse(text.trim()).ok())
}

/// Версия запущенного лаунчера: файл состояния в профиле, иначе (разовая
/// миграция) legacy sidecar `<exe>.lversion`, иначе штамп сборки, иначе
/// версия крейта (её semver всегда валиден).
pub fn running_version_in(state_dir: Option<&Path>, current_exe: &Path) -> Version {
    if let Some(dir) = state_dir {
        if let Some(version) = read_version_file(&version_state_path(dir)) {
            return version;
        }
    }
    // Миграция со старого sidecar: забираем значение и убираем файл.
    let legacy = legacy_sidecar_path(current_exe);
    if let Some(version) = read_version_file(&legacy) {
        if record_launcher_version(state_dir, &version.to_string()).is_ok() {
            let _ = fs::remove_file(&legacy);
        }
        return version;
    }
    Version::parse(LAUNCHER_VERSION).unwrap_or_else(|_| Version::new(0, 1, 0))
}

/// Запоминает версию подменённого лаунчера (best-effort: read-only
/// каталог не должен ломать запуск).
fn record_launcher_version(
    state_dir: Option<&Path>,
    version: &str,
) -> std::result::Result<(), Error> {
    let Some(dir) = state_dir else {
        return Err(Error::Config("нет каталога состояния лаунчера".into()));
    };
    let path = version_state_path(dir);
    fs::create_dir_all(dir)?;
    fs::write(&path, version)?;
    Ok(())
}

/// Процесс уже перезапущен после подмены лаунчера?
pub fn is_relaunched() -> bool {
    relaunch_marker_from(
        std::env::var_os(RELAUNCHED_ENV)
            .as_deref()
            .and_then(|value| value.to_str()),
    )
}

/// Чистая логика маркера: включается только значением `1`.
pub fn relaunch_marker_from(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

/// Бандл лаунчера из клиентского zip (M6-7) новее запущенного?
/// Гейт против откатов: если ранее применён хотфикс из манифеста
/// (версия выше текущего релиза), старый бандл не должен его заменять.
pub fn bundle_newer_than_running(
    manifest: &Manifest,
    state_dir: Option<&Path>,
    current_exe: &Path,
) -> bool {
    manifest
        .version_semver()
        .is_ok_and(|bundle| bundle > running_version_in(state_dir, current_exe))
}

/// Полный цикл обновления самого лаунчера после обновления игры:
/// 1. бандл из клиентского zip (M6-7; версия = версия релиза) — только
///    если он новее запущенного;
/// 2. лаунчер-ассет манифеста (M6-8) — только если он новее запущенного.
///
/// Первый успешный шаг возвращает [`SelfUpdateStep::Updated`] — вызывающий
/// делает relaunch; второй шаг в этом же процессе уже не нужен.
pub fn update_self(
    manifest: &Manifest,
    layout: &Layout,
    state_dir: Option<&Path>,
    current_exe: &Path,
    progress: &mut dyn FnMut(f32),
) -> SelfUpdateStep {
    let before = running_version_in(state_dir, current_exe);
    // 1. Бандл из zip (M6-7). Раньше был hash-based; теперь под версионным
    //    гейтом, иначе хотфикс из манифеста откатывался бы бандлом.
    if bundle_newer_than_running(manifest, state_dir, current_exe) {
        match selfupdate::check_and_apply(current_exe, &layout.bundled_launcher()) {
            Ok(selfupdate::SelfUpdate::Updated) => {
                let _ = record_launcher_version(state_dir, &manifest.version);
                return SelfUpdateStep::Updated {
                    from: before.to_string(),
                    to: manifest.version.clone(),
                };
            }
            Ok(selfupdate::SelfUpdate::Deferred) => return SelfUpdateStep::Deferred,
            Ok(selfupdate::SelfUpdate::UpToDate) => {}
            Err(error) => {
                log::warn!("самообновление лаунчера (бандл из zip): {error}");
            }
        }
    }
    // 2. Лаунчер-ассет манифеста (M6-8). На relaunch-втором проходе —
    //    только предупреждение, чтобы не уйти в петлю.
    let pending =
        match manifest.pending_launcher_version(&running_version_in(state_dir, current_exe)) {
            Ok(pending) => pending,
            Err(error) => {
                return SelfUpdateStep::Failed {
                    reason: error.to_string(),
                }
            }
        };
    if let Some(target) = pending {
        if is_relaunched() {
            return SelfUpdateStep::Failed {
                reason: format!(
                    "доступна версия лаунчера {target}, но подмена уже выполнялась \
                     в этом сеансе — перезапустите лаунчер"
                ),
            };
        }
        return match install_launcher(manifest, current_exe, state_dir, progress, &target) {
            Ok(true) => SelfUpdateStep::Updated {
                from: before.to_string(),
                to: target.to_string(),
            },
            Ok(false) => SelfUpdateStep::Deferred,
            Err(error) => SelfUpdateStep::Failed {
                reason: error.to_string(),
            },
        };
    }
    SelfUpdateStep::UpToDate
}

/// M6-8: скачать лаунчер-ассет манифеста, проверить sha256, распаковать
/// и подменить себя. `true` — подменён, `false` — отложено (`.new` рядом).
fn install_launcher(
    manifest: &Manifest,
    current_exe: &Path,
    state_dir: Option<&Path>,
    progress: &mut dyn FnMut(f32),
    _target: &Version,
) -> Result<bool> {
    let Some(asset) = &manifest.launcher else {
        return Ok(false);
    };
    let Some(dir) = current_exe.parent() else {
        return Err(Error::Install(
            "у лаунчера нет родительского каталога".into(),
        ));
    };
    // staging рядом с exe — rename/копирование в пределах одного тома.
    let staging = dir.join(format!("launcher-staging-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| -> Result<bool> {
        fs::create_dir_all(&staging)?;
        let pack = staging.join("pack.zip");
        crate::update::download(&asset.url, None, &pack, &asset.sha256, asset.size, progress)?;
        let extracted = staging.join("root");
        crate::update::extract_zip(&pack, &extracted)?;
        let payload = crate::update::find_file_payload(&extracted, LAUNCHER_EXE)?;
        let fresh = payload.join(LAUNCHER_EXE);
        match selfupdate::replace_executable(current_exe, &fresh)? {
            ReplaceOutcome::Replaced => {
                let _ = record_launcher_version(state_dir, &asset.version);
                Ok(true)
            }
            ReplaceOutcome::Busy => {
                let staged = staged_new_path(current_exe);
                fs::copy(&fresh, &staged)?;
                log::warn!(
                    "лаунчер не подменён (занят): свежий лежит как {} — замените вручную",
                    staged.display()
                );
                Ok(false)
            }
        }
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Путь отложенной замены: `<exe>.new` рядом (как в `selfupdate`).
fn staged_new_path(current_exe: &Path) -> PathBuf {
    let mut name = current_exe.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    current_exe.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::LauncherAsset;

    fn sha() -> String {
        "a".repeat(64)
    }

    fn fixture(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn relaunch_marker_only_on_one() {
        assert!(relaunch_marker_from(Some("1")));
        assert!(!relaunch_marker_from(Some("0")));
        assert!(!relaunch_marker_from(None));
    }

    #[test]
    fn running_version_prefers_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        // без файла состояния — версия крейта/штампа (в тестах это версия пакета)
        let fallback = running_version_in(Some(state.path()), &exe);
        fixture(state.path(), LAUNCHER_VERSION_FILE, b"0.1.99");
        let stamped = running_version_in(Some(state.path()), &exe);
        assert_eq!(stamped, Version::parse("0.1.99").unwrap());
        assert_ne!(stamped, fallback);
        // мусор в файле — снова фолбэк
        fixture(state.path(), LAUNCHER_VERSION_FILE, b"not-semver");
        assert_eq!(running_version_in(Some(state.path()), &exe), fallback);
        // без каталога состояния (нет %LOCALAPPDATA%) — тоже фолбэк
        assert_eq!(running_version_in(None, &exe), fallback);
    }

    #[test]
    fn running_version_migrates_legacy_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        // старый sidecar рядом с exe (на рабочих столах таких полно)
        fixture(dir.path(), "zn-launcher.exe.lversion", b"0.1.77");
        // значение подхвачено…
        assert_eq!(
            running_version_in(Some(state.path()), &exe),
            Version::parse("0.1.77").unwrap()
        );
        // …переписано в профиль…
        assert_eq!(
            fs::read_to_string(version_state_path(state.path()))
                .unwrap()
                .trim(),
            "0.1.77"
        );
        // …и sidecar удалён
        assert!(!legacy_sidecar_path(&exe).exists());
        // второй раз читаем уже из состояния
        assert_eq!(
            running_version_in(Some(state.path()), &exe),
            Version::parse("0.1.77").unwrap()
        );
    }

    #[test]
    fn bundle_gate_blocks_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        let manifest = test_manifest("0.1.5");
        let state = state.path();
        // без файла состояния: фолбэк (версия крейта) < 0.1.5 — бандл применяется
        assert!(bundle_newer_than_running(&manifest, Some(state), &exe));
        // hotfix 0.1.99 уже применён — бандл 0.1.5 не трогаем
        fixture(state, LAUNCHER_VERSION_FILE, b"0.1.99");
        assert!(!bundle_newer_than_running(&manifest, Some(state), &exe));
        // равные версии — не применяем
        fixture(state, LAUNCHER_VERSION_FILE, b"0.1.5");
        assert!(!bundle_newer_than_running(&manifest, Some(state), &exe));
    }

    #[test]
    fn update_self_noop_when_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        let layout = Layout::new(dir.path());
        let manifest = test_manifest("0.1.0"); // версия крейта в тестах
                                               // ни бандл, ни лаунчер-блок не новее — тишина
        assert_eq!(
            update_self(&manifest, &layout, Some(state.path()), &exe, &mut |_| {}),
            SelfUpdateStep::UpToDate
        );
    }

    #[test]
    fn update_self_applies_bundle_then_records_version() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"running-old");
        let layout = Layout::new(dir.path());
        // бандл из «игры» — свежий лаунчер
        fixture(dir.path(), "game/zn-launcher.exe", b"bundled-fresh");
        let manifest = test_manifest("9.9.9");
        let step = update_self(&manifest, &layout, Some(state.path()), &exe, &mut |_| {});
        assert_eq!(
            step,
            SelfUpdateStep::Updated {
                from: env!("CARGO_PKG_VERSION").to_string(),
                to: "9.9.9".to_string()
            }
        );
        // exe подменён, версия записана в профиль (не рядом с exe!)
        assert_eq!(fs::read(&exe).unwrap(), b"bundled-fresh");
        assert_eq!(
            fs::read_to_string(version_state_path(state.path()))
                .unwrap()
                .trim(),
            "9.9.9"
        );
        assert!(!legacy_sidecar_path(&exe).exists());
    }

    #[test]
    fn update_self_skips_stale_launcher_asset() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"running");
        let layout = Layout::new(dir.path());
        // хотфис 0.1.99 уже применён; релиз и ассет старее —
        // ничего не качаем и не откатываем
        fixture(state.path(), LAUNCHER_VERSION_FILE, b"0.1.99");
        let mut manifest = test_manifest("0.1.5");
        manifest.launcher = Some(LauncherAsset {
            version: "0.1.6".into(),
            url: "https://example.invalid/zona-launcher.zip".into(),
            sha256: sha(),
            size: 1,
        });
        assert_eq!(
            update_self(&manifest, &layout, Some(state.path()), &exe, &mut |_| {}),
            SelfUpdateStep::UpToDate
        );
        assert_eq!(fs::read(&exe).unwrap(), b"running");
    }

    #[test]
    fn update_self_applies_launcher_asset() {
        let (bytes, hash) = fixture_launcher_zip("fresh");
        let size = bytes.len() as u64;
        let url = serve_bytes(bytes);
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"running-old");
        let layout = Layout::new(dir.path());
        // версия релиза = версия крейта (бандл не новее) — работает ассет
        let mut manifest = test_manifest(env!("CARGO_PKG_VERSION"));
        manifest.launcher = Some(LauncherAsset {
            version: "9.9.9".into(),
            url,
            sha256: hash,
            size,
        });
        let step = update_self(&manifest, &layout, Some(state.path()), &exe, &mut |_| {});
        assert_eq!(
            step,
            SelfUpdateStep::Updated {
                from: env!("CARGO_PKG_VERSION").to_string(),
                to: "9.9.9".to_string()
            }
        );
        // exe подменён скачанным, версия в профиле, staging убран
        assert_eq!(fs::read(&exe).unwrap(), b"fake-launcher fresh");
        assert_eq!(
            fs::read_to_string(version_state_path(state.path()))
                .unwrap()
                .trim(),
            "9.9.9"
        );
        assert!(!legacy_sidecar_path(&exe).exists());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("launcher-staging-"))
            .collect();
        assert!(leftovers.is_empty(), "staging не убран: {leftovers:?}");
    }

    #[test]
    fn update_self_launcher_asset_download_failure_is_soft() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"running");
        let layout = Layout::new(dir.path());
        let mut manifest = test_manifest(env!("CARGO_PKG_VERSION"));
        manifest.launcher = Some(LauncherAsset {
            version: "9.9.9".into(),
            // мёртвый адрес: скачивание не удалось
            url: "http://127.0.0.1:1/zona-launcher.zip".into(),
            sha256: sha(),
            size: 1,
        });
        let step = update_self(&manifest, &layout, Some(state.path()), &exe, &mut |_| {});
        assert!(
            matches!(step, SelfUpdateStep::Failed { .. }),
            "ожидали мягкий провал, получили {step:?}"
        );
        // запущенный лаунчер не тронут
        assert_eq!(fs::read(&exe).unwrap(), b"running");
    }

    // ---- фикстуры ----

    fn test_manifest(version: &str) -> Manifest {
        Manifest {
            channel: "dev".into(),
            version: version.into(),
            build: None,
            commit: None,
            created_at: None,
            url: "https://example.invalid/zona.zip".into(),
            sha256: sha(),
            size: 1,
            server_addr: None,
            server: None,
            launcher: None,
            notes: None,
        }
    }

    fn fixture_launcher_zip(tag: &str) -> (Vec<u8>, String) {
        use sha2::{Digest, Sha256};
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            writer
                .start_file(LAUNCHER_EXE, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, format!("fake-launcher {tag}").as_bytes())
                .unwrap();
            writer.finish().unwrap();
        }
        let mut hasher = Sha256::new();
        hasher.update(&buffer);
        let hash = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        (buffer, hash)
    }

    fn serve_bytes(body: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let head_text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head_text.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/zona-launcher.zip")
    }
}
