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
//! лаунчера. Версия запущенного — sidecar `<exe>.lversion` (пишется после
//! каждой удачной подмены), иначе штамп сборки [`crate::LAUNCHER_VERSION`]
//! (CI-переменная `ZONA_LAUNCHER_VERSION`, фолбэк — версия крейта).

use crate::error::{Error, Result};
use crate::install::{Layout, LAUNCHER_EXE};
use crate::manifest::{self, Manifest};
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

/// Sidecar-файл версии запущенного лаунчера: `<exe>.lversion`.
fn sidecar_path(current_exe: &Path) -> PathBuf {
    let mut name = current_exe.file_name().unwrap_or_default().to_os_string();
    name.push(".lversion");
    current_exe.with_file_name(name)
}

/// Версия запущенного лаунчера: sidecar после последней подмены, иначе
/// штамп сборки, иначе версия крейта (её semver всегда валиден).
pub fn running_version(current_exe: &Path) -> Version {
    if let Ok(text) = fs::read_to_string(sidecar_path(current_exe)) {
        if let Ok(version) = Version::parse(text.trim()) {
            return version;
        }
    }
    Version::parse(LAUNCHER_VERSION).unwrap_or_else(|_| Version::new(0, 1, 0))
}

/// Запоминает версию подменённого лаунчера (best-effort: read-only
/// каталог не должен ломать запуск).
fn record_launcher_version(current_exe: &Path, version: &str) {
    let path = sidecar_path(current_exe);
    if let Err(error) = fs::write(&path, version) {
        log::debug!("не удалось записать {}: {error}", path.display());
    }
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
pub fn bundle_newer_than_running(manifest: &Manifest, current_exe: &Path) -> bool {
    manifest
        .version_semver()
        .is_ok_and(|bundle| bundle > running_version(current_exe))
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
    current_exe: &Path,
    progress: &mut dyn FnMut(f32),
) -> SelfUpdateStep {
    let before = running_version(current_exe);
    // 1. Бандл из zip (M6-7). Раньше был hash-based; теперь под версионным
    //    гейтом, иначе хотфикс из манифеста откатывался бы бандлом.
    if bundle_newer_than_running(manifest, current_exe) {
        match selfupdate::check_and_apply(current_exe, &layout.bundled_launcher()) {
            Ok(selfupdate::SelfUpdate::Updated) => {
                record_launcher_version(current_exe, &manifest.version);
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
    let pending = match manifest.pending_launcher_version(&running_version(current_exe)) {
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
        return match install_launcher(manifest, current_exe, progress, &target) {
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
        crate::update::download(
            &asset.url,
            manifest::release_token(),
            &pack,
            &asset.sha256,
            asset.size,
            progress,
        )?;
        let extracted = staging.join("root");
        crate::update::extract_zip(&pack, &extracted)?;
        let payload = crate::update::find_file_payload(&extracted, LAUNCHER_EXE)?;
        let fresh = payload.join(LAUNCHER_EXE);
        match selfupdate::replace_executable(current_exe, &fresh)? {
            ReplaceOutcome::Replaced => {
                record_launcher_version(current_exe, &asset.version);
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
    fn running_version_prefers_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        // без sidecar — версия крейта/штампа (в тестах это версия пакета)
        let fallback = running_version(&exe);
        fixture(dir.path(), "zn-launcher.exe.lversion", b"0.1.99");
        let stamped = running_version(&exe);
        assert_eq!(stamped, Version::parse("0.1.99").unwrap());
        assert_ne!(stamped, fallback);
        // мусор в sidecar — снова фолбэк
        fixture(dir.path(), "zn-launcher.exe.lversion", b"not-semver");
        assert_eq!(running_version(&exe), fallback);
    }

    #[test]
    fn bundle_gate_blocks_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        let manifest = test_manifest("0.1.5");
        // без sidecar: фолбэк (версия крейта) < 0.1.5 — бандл применяется
        assert!(bundle_newer_than_running(&manifest, &exe));
        // hotfix 0.1.99 уже применён — бандл 0.1.5 не трогаем
        fixture(dir.path(), "zn-launcher.exe.lversion", b"0.1.99");
        assert!(!bundle_newer_than_running(&manifest, &exe));
        // равные версии — не применяем
        fixture(dir.path(), "zn-launcher.exe.lversion", b"0.1.5");
        assert!(!bundle_newer_than_running(&manifest, &exe));
    }

    #[test]
    fn update_self_noop_when_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"build");
        let layout = Layout::new(dir.path());
        let manifest = test_manifest("0.1.0"); // версия крейта в тестах
                                               // ни бандл, ни лаунчер-блок не новее — тишина
        assert_eq!(
            update_self(&manifest, &layout, &exe, &mut |_| {}),
            SelfUpdateStep::UpToDate
        );
    }

    #[test]
    fn update_self_applies_bundle_then_records_version() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"running-old");
        let layout = Layout::new(dir.path());
        // бандл из «игры» — свежий лаунчер
        fixture(dir.path(), "game/zn-launcher.exe", b"bundled-fresh");
        let manifest = test_manifest("9.9.9");
        let step = update_self(&manifest, &layout, &exe, &mut |_| {});
        assert_eq!(
            step,
            SelfUpdateStep::Updated {
                from: env!("CARGO_PKG_VERSION").to_string(),
                to: "9.9.9".to_string()
            }
        );
        // exe подменён, sidecar записан
        assert_eq!(fs::read(&exe).unwrap(), b"bundled-fresh");
        assert_eq!(
            fs::read_to_string(sidecar_path(&exe)).unwrap().trim(),
            "9.9.9"
        );
    }

    #[test]
    fn update_self_skips_stale_launcher_asset() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(dir.path(), "zn-launcher.exe", b"running");
        let layout = Layout::new(dir.path());
        // хотфис 0.1.99 уже применён (sidecar); релиз и ассет старее —
        // ничего не качаем и не откатываем
        fixture(dir.path(), "zn-launcher.exe.lversion", b"0.1.99");
        let mut manifest = test_manifest("0.1.5");
        manifest.launcher = Some(LauncherAsset {
            version: "0.1.6".into(),
            url: "https://example.invalid/zona-launcher.zip".into(),
            sha256: sha(),
            size: 1,
        });
        assert_eq!(
            update_self(&manifest, &layout, &exe, &mut |_| {}),
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
        let step = update_self(&manifest, &layout, &exe, &mut |_| {});
        assert_eq!(
            step,
            SelfUpdateStep::Updated {
                from: env!("CARGO_PKG_VERSION").to_string(),
                to: "9.9.9".to_string()
            }
        );
        // exe подменён скачанным, sidecar записан, staging убран
        assert_eq!(fs::read(&exe).unwrap(), b"fake-launcher fresh");
        assert_eq!(
            fs::read_to_string(sidecar_path(&exe)).unwrap().trim(),
            "9.9.9"
        );
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
        let step = update_self(&manifest, &layout, &exe, &mut |_| {});
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
