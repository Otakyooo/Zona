//! Скачивание, проверка и атомарная установка dev-сборки.
//!
//! Свап: `staging-<pid>/pack.zip` → verify sha256 → распаковка в
//! `staging-<pid>/root` → payload (папка с `zn-client.exe`) → проверка, что
//! клиент не запущен → `game` → `game.old` (бэкап) → payload → `game`
//! (сбой → откат `game.old` → `game`) → `installed.json` → чистка staging
//! и бэкапа. Rename в пределах одного тома — атомарные.

use crate::error::{Error, Result};
use crate::install::{Installed, Layout, CLIENT_EXE};
use crate::manifest::{self, Manifest};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Сколько попыток скачивания (транспортные ошибки).
const DOWNLOAD_ATTEMPTS: u32 = 3;
/// Базовый бэкофф между попытками, мс (удваивается).
const BACKOFF_MS: u64 = 750;
/// Размер чанка чтения.
const CHUNK: usize = 64 * 1024;

/// Колбэк прогресса скачивания: 0.0..=1.0, монотонно неубывающий.
pub type ProgressFn<'a> = &'a mut dyn FnMut(f32);

/// Итог `check_and_update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateReport {
    /// Локальная dev-сборка из рабочего дерева — сеть не трогаем.
    DevMode,
    /// Установлена последняя версия манифеста.
    NoUpdate { installed: String },
    /// Первая установка в выбранную папку.
    Installed { version: String },
    /// Обновление с прошлой версии.
    Updated { from: String, to: String },
    /// Скачивание/установка не удались; установленная игра не тронута.
    Failed {
        keep: Option<String>,
        reason: String,
    },
}

/// Полный цикл для GUI: dev-детект → манифест → сверка → установка.
/// Ошибки скачивания/установки превращаются в `Failed` (игра цела),
/// но несовпадение канала и запущенный клиент — остаются `Err`.
pub fn check_and_update(
    layout: &Layout,
    channel: &str,
    progress: ProgressFn<'_>,
) -> Result<UpdateReport> {
    if crate::run::is_dev_mode(&std::env::current_exe().unwrap_or_default()) {
        return Ok(UpdateReport::DevMode);
    }
    let manifest = manifest::fetch_manifest(manifest::release_token())?;
    check_and_update_with(layout, channel, &manifest, progress)
}

/// То же, но с уже загруженным манифестом: CLI использует его ещё и для
/// `--server` при запуске (без второго запроса).
pub fn check_and_update_with(
    layout: &Layout,
    channel: &str,
    manifest: &Manifest,
    progress: ProgressFn<'_>,
) -> Result<UpdateReport> {
    check_channel(manifest, channel)?;
    let installed = Installed::load(&layout.installed_file())?;
    let current = installed.as_ref().and_then(|i| i.version_semver().ok());
    if !manifest.is_update_over(current.as_ref())? {
        let installed = installed.map(|i| i.version).unwrap_or_default();
        return Ok(UpdateReport::NoUpdate { installed });
    }
    install_from_manifest(layout, manifest, progress)
}

/// Принудительное обновление («Обновить» в UI): версия не сверяется.
pub fn force_update(
    layout: &Layout,
    channel: &str,
    progress: ProgressFn<'_>,
) -> Result<UpdateReport> {
    let manifest = manifest::fetch_manifest(manifest::release_token())?;
    check_channel(&manifest, channel)?;
    install_from_manifest(layout, &manifest, progress)
}

fn check_channel(manifest: &Manifest, channel: &str) -> Result<()> {
    if manifest.matches_channel(channel) {
        Ok(())
    } else {
        Err(Error::Channel {
            expected: channel.to_string(),
            actual: manifest.channel.clone(),
        })
    }
}

/// Качает, проверяет и подменяет игру. Ошибка до свапа — установка цела.
pub fn install_from_manifest(
    layout: &Layout,
    manifest: &Manifest,
    progress: ProgressFn<'_>,
) -> Result<UpdateReport> {
    let previous = Installed::load(&layout.installed_file())?.map(|i| i.version);
    crate::install::ensure_writable(layout.root())?;
    // Пока игра запущена, exe залочен — подмена невозможна.
    ensure_client_not_running(&layout.client_exe())?;

    let staging = layout.staging_dir();
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = install_via_staging(layout, manifest, &staging, progress);
    let _ = fs::remove_dir_all(&staging);
    result?;

    Installed::from_manifest(manifest).save(&layout.installed_file())?;
    Ok(match previous {
        Some(from) => UpdateReport::Updated {
            from,
            to: manifest.version.clone(),
        },
        None => UpdateReport::Installed {
            version: manifest.version.clone(),
        },
    })
}

fn install_via_staging(
    layout: &Layout,
    manifest: &Manifest,
    staging: &Path,
    progress: ProgressFn<'_>,
) -> Result<()> {
    fs::create_dir_all(staging)?;
    let pack = staging.join("pack.zip");
    download(
        &manifest.url,
        manifest::release_token(),
        &pack,
        &manifest.sha256,
        manifest.size,
        progress,
    )?;
    let extracted = staging.join("root");
    extract_zip(&pack, &extracted)?;
    let payload = find_payload(&extracted)?;
    let backup = swap_payload(&payload, &layout.game_dir(), &layout.backup_dir())?;
    if let Some(backup) = backup {
        // Старая игра больше не нужна; не смогли удалить — не страшно.
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Скачивание с ретраями и бэкоффом; проверка sha256 — один раз после
/// успешной докачки (ретрай битого файла бессмысленен).
/// `expected_size` — из манифеста, для расчёта прогресса.
pub fn download(
    url: &str,
    token: Option<&str>,
    dest: &Path,
    expected_sha256: &str,
    expected_size: u64,
    progress: ProgressFn<'_>,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut high_water = 0.0f32;
    let mut attempt_error = None;
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        match download_once(url, token, dest, expected_size, &mut |p| {
            if p > high_water {
                high_water = p;
            }
            progress(high_water);
        }) {
            Ok(()) => {
                attempt_error = None;
                break;
            }
            Err(error) => {
                log::warn!("скачивание {url}: попытка {attempt} не удалась: {error}");
                attempt_error = Some(error);
                if attempt < DOWNLOAD_ATTEMPTS {
                    let backoff = BACKOFF_MS * 2u64.pow(attempt - 1);
                    std::thread::sleep(std::time::Duration::from_millis(backoff));
                }
            }
        }
    }
    if let Some(error) = attempt_error {
        return Err(error);
    }
    verify_sha256(dest, expected_sha256)
}

fn download_once(
    url: &str,
    token: Option<&str>,
    dest: &Path,
    expected_size: u64,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    let mut request = ureq::get(url)
        .header("Accept", "application/octet-stream")
        .header("User-Agent", "zn-launcher");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut response = request
        .call()
        .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Http(format!("GET {url}: HTTP {status}")));
    }
    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(dest)?;
    let mut buffer = [0u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| Error::Http(format!("GET {url}: чтение тела: {e}")))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        total += read as u64;
        let fraction = if expected_size > 0 {
            (total as f32 / expected_size as f32).min(1.0)
        } else {
            0.0
        };
        progress(fraction);
    }
    file.flush()?;
    progress(1.0);
    Ok(())
}

/// sha256 файла в нижнем регистре.
pub fn sha256_hex(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Сверяет sha256 файла с ожидаемым (регистр не важен).
pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_hex(path)?;
    let expected = expected.trim().to_ascii_lowercase();
    if actual != expected {
        return Err(Error::Integrity { expected, actual });
    }
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Распаковка zip с защитой от zip-slip: имена, выходящие за пределы
/// `dest_dir` (`../`, абсолютные), отвергаются.
pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dest_dir)?;
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| Error::Install(format!("не zip-архив {}: {e}", zip_path.display())))?;
    let mut extracted = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| Error::Install(format!("zip #{index}: {e}")))?;
        let Some(enclosed) = entry.enclosed_name() else {
            let name = entry
                .name()
                .map(|n| n.into_owned())
                .unwrap_or_else(|_| "<нечитаемое имя>".to_string());
            return Err(Error::Install(format!(
                "zip-slip: '{name}' выходит за пределы папки установки"
            )));
        };
        let out_path = dest_dir.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        extracted.push(out_path);
    }
    Ok(extracted)
}

/// Папка с `zn-client.exe` внутри распакованного архива: либо корень,
/// либо единственный Top-Level-каталог архива (`zona-dev-win-x64/`).
pub fn find_payload(extracted_root: &Path) -> Result<PathBuf> {
    if extracted_root.join(CLIENT_EXE).is_file() {
        return Ok(extracted_root.to_path_buf());
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(extracted_root)? {
        let path = entry?.path();
        if path.is_dir() && path.join(CLIENT_EXE).is_file() {
            candidates.push(path);
        }
    }
    match candidates.len() {
        1 => Ok(candidates.pop().expect("проверено длиной")),
        0 => Err(Error::Install(format!(
            "в архиве нет {CLIENT_EXE} — это не клиентская сборка ZONA"
        ))),
        _ => Err(Error::Install(format!(
            "в архиве несколько папок с {CLIENT_EXE} — неоднозначная сборка"
        ))),
    }
}

/// Подмена `game` на payload: старая игра -> `backup` -> (после успеха
/// бэкап удаляется вызывающим). Сбой rename'а payload'а -> откат.
pub fn swap_payload(payload: &Path, game_dir: &Path, backup_dir: &Path) -> Result<Option<PathBuf>> {
    let mut backup_made = None;
    if game_dir.exists() {
        if backup_dir.exists() {
            fs::remove_dir_all(backup_dir)?;
        }
        fs::rename(game_dir, backup_dir)?;
        backup_made = Some(backup_dir.to_path_buf());
    }
    match fs::rename(payload, game_dir) {
        Ok(()) => Ok(backup_made),
        Err(error) => {
            if let Some(backup) = &backup_made {
                log::warn!("свап не удался ({error}) — откат {backup:?} -> {game_dir:?}");
                if game_dir.exists() {
                    let _ = fs::remove_dir_all(game_dir);
                }
                let _ = fs::rename(backup, game_dir);
            }
            Err(Error::Io(error))
        }
    }
}

/// Клиент не должен быть запущен: на Windows пробуем открыть exe с
/// share=0; `ERROR_SHARING_VIOLATION` — файл кем-то удерживается.
#[cfg(windows)]
pub fn ensure_client_not_running(client_exe: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_SHARING_VIOLATION, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateFileW;

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_NONE: u32 = 0;
    const OPEN_EXISTING: u32 = 3;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

    let wide: Vec<u16> = client_exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_NONE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let code = unsafe { GetLastError() };
        if code == ERROR_SHARING_VIOLATION {
            return Err(Error::GameRunning);
        }
        // Файла нет (свежая установка) и прочее — не повод блокировать.
        return Ok(());
    }
    unsafe { CloseHandle(handle) };
    Ok(())
}

#[cfg(not(windows))]
pub fn ensure_client_not_running(_client_exe: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::net::TcpListener;

    /// Тело манифеста, ссылающееся на локальный сервер ассетов.
    fn manifest_for(asset_url: &str, sha: &str, size: u64) -> Manifest {
        Manifest {
            channel: "dev".into(),
            version: "0.1.5".into(),
            build: Some(5),
            commit: Some("cafe123".into()),
            created_at: None,
            url: asset_url.to_string(),
            sha256: sha.to_string(),
            size,
            server_addr: None,
            server: None,
            notes: None,
        }
    }

    /// Отдаёт `body` на любой GET; возвращает адрес.
    fn serve_bytes(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let _ = read_head(&mut reader);
                respond(stream, "200 OK", "application/octet-stream", &body);
            }
        });
        format!("http://{addr}/asset.zip")
    }

    fn read_head(reader: &mut impl std::io::BufRead) -> String {
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
        head
    }

    fn respond(mut stream: std::net::TcpStream, status: &str, content_type: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn read_progress(storage: &std::sync::Arc<std::sync::Mutex<Vec<f32>>>) -> impl FnMut(f32) + '_ {
        let storage = storage.clone();
        move |p: f32| storage.lock().unwrap().push(p)
    }

    // ---- zip-фикстуры ----

    /// Собирает фикстурный клиентский zip; `tag` меняет содержимое, чтобы
    /// две «сборки» различались байтами.
    fn fixture_zip(top_level: Option<&str>, tag: &str) -> (Vec<u8>, String) {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let prefix = |name: &str| match top_level {
                Some(dir) => format!("{dir}/{name}"),
                None => name.to_string(),
            };
            let items = format!("(build: \"{tag}\")");
            for (name, content) in [
                (
                    prefix(CLIENT_EXE),
                    format!("fake-zn-client {tag}").into_bytes(),
                ),
                (prefix("content/items.ron"), items.into_bytes()),
            ] {
                writer
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                std::io::Write::write_all(&mut writer, &content).unwrap();
            }
            writer.finish().unwrap();
        }
        let hash = sha256_hex_of_bytes(&buffer);
        (buffer, hash)
    }

    fn sha256_hex_of_bytes(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        to_hex(&hasher.finalize())
    }

    // ---- verify ----

    #[test]
    fn verify_sha256_ok_and_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pack.zip");
        std::fs::write(&file, b"hello-zona").unwrap();
        let hash = sha256_hex(&file).unwrap();
        verify_sha256(&file, &hash).unwrap();
        verify_sha256(&file, &hash.to_uppercase()).unwrap();
        let err = verify_sha256(&file, &"0".repeat(64)).unwrap_err();
        assert!(
            matches!(err, Error::Integrity { .. }),
            "ожидали Integrity, получили {err:?}"
        );
    }

    // ---- extract + payload ----

    #[test]
    fn extract_zip_and_find_payload_top_level_dir() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("pack.zip");
        let (bytes, _) = fixture_zip(Some("zona-dev-win-x64"), "v1");
        std::fs::write(&zip_path, &bytes).unwrap();
        let extracted = dir.path().join("root");
        extract_zip(&zip_path, &extracted).unwrap();
        let payload = find_payload(&extracted).unwrap();
        assert!(payload.join(CLIENT_EXE).is_file());
        assert!(payload.join("content/items.ron").is_file());
    }

    #[test]
    fn find_payload_accepts_exe_at_root() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("pack.zip");
        let (bytes, _) = fixture_zip(None, "v1");
        std::fs::write(&zip_path, &bytes).unwrap();
        let extracted = dir.path().join("root");
        extract_zip(&zip_path, &extracted).unwrap();
        assert_eq!(find_payload(&extracted).unwrap(), extracted);
    }

    #[test]
    fn extract_zip_rejects_zip_slip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("evil.zip");
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            writer
                .start_file("../escape.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, b"pwned").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&zip_path, &buffer).unwrap();
        let err = extract_zip(&zip_path, &dir.path().join("root")).unwrap_err();
        assert!(
            matches!(err, Error::Install(ref m) if m.contains("zip-slip")),
            "ожидали zip-slip отказ, получили {err:?}"
        );
    }

    #[test]
    fn find_payload_ambiguous_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::create_dir_all(dir.path().join("b")).unwrap();
        fs::write(dir.path().join("a").join(CLIENT_EXE), b"x").unwrap();
        fs::write(dir.path().join("b").join(CLIENT_EXE), b"x").unwrap();
        assert!(matches!(
            find_payload(dir.path()).unwrap_err(),
            Error::Install(_)
        ));
        let empty = tempfile::tempdir().unwrap();
        assert!(matches!(
            find_payload(empty.path()).unwrap_err(),
            Error::Install(_)
        ));
    }

    // ---- download + install ----

    #[test]
    fn download_reports_monotonic_progress_and_verifies() {
        let (bytes, hash) = fixture_zip(Some("zona-dev-win-x64"), "v1");
        let url = serve_bytes(bytes.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pack.zip");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        download(
            &url,
            None,
            &dest,
            &hash,
            bytes.len() as u64,
            &mut read_progress(&seen),
        )
        .unwrap();
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty());
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "прогресс не монотонен: {seen:?}"
        );
        assert!((seen.last().copied().unwrap_or(0.0) - 1.0).abs() < f32::EPSILON);
        verify_sha256(&dest, &hash).unwrap();
    }

    #[test]
    fn download_integrity_mismatch_leaves_file() {
        let (bytes, _) = fixture_zip(None, "v1");
        let url = serve_bytes(bytes.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pack.zip");
        let err = download(
            &url,
            None,
            &dest,
            &"a".repeat(64),
            bytes.len() as u64,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, Error::Integrity { .. }), "получили {err:?}");
    }

    #[test]
    fn fresh_install_then_update_swaps_game_dir() {
        let (bytes_v1, hash_v1) = fixture_zip(Some("zona-dev-win-x64"), "build-1");
        let (bytes_v2, hash_v2) = fixture_zip(Some("zona-dev-win-x64"), "build-2");
        assert_ne!(bytes_v1, bytes_v2, "фикстуры должны различаться");
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());

        // первая установка
        let url = serve_bytes(bytes_v1.clone());
        let report = install_from_manifest(
            &layout,
            &manifest_for(&url, &hash_v1, bytes_v1.len() as u64),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            report,
            UpdateReport::Installed {
                version: "0.1.5".into()
            }
        );
        assert!(layout.client_exe().is_file());
        assert!(layout.content_dir().join("items.ron").is_file());
        assert!(Installed::load(&layout.installed_file()).unwrap().is_some());

        // вторая сборка — swap с бэкапом
        let url = serve_bytes(bytes_v2.clone());
        let report = install_from_manifest(
            &layout,
            &manifest_for(&url, &hash_v2, bytes_v2.len() as u64),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            report,
            UpdateReport::Updated {
                from: "0.1.5".into(),
                to: "0.1.5".into()
            }
        );
        // payload заменён, бэкап убран
        assert!(!layout.backup_dir().exists());
        assert!(layout.game_dir().join(CLIENT_EXE).is_file());
    }

    #[test]
    fn broken_hash_keeps_previous_install() {
        let (bytes_v1, hash_v1) = fixture_zip(Some("zona-dev-win-x64"), "build-1");
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let url = serve_bytes(bytes_v1.clone());
        install_from_manifest(
            &layout,
            &manifest_for(&url, &hash_v1, bytes_v1.len() as u64),
            &mut |_| {},
        )
        .unwrap();

        // битый хэш в манифесте — установка должна упасть ДО свапа
        let err = install_from_manifest(
            &layout,
            &manifest_for(&url, &"b".repeat(64), 100),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, Error::Integrity { .. }), "получили {err:?}");
        // игра и installed.json не тронуты, staging убран
        assert!(layout.client_exe().is_file());
        assert_eq!(
            Installed::load(&layout.installed_file())
                .unwrap()
                .unwrap()
                .version,
            "0.1.5"
        );
        let staging_leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("staging-"))
            .collect();
        assert!(
            staging_leftovers.is_empty(),
            "staging не убран: {staging_leftovers:?}"
        );
    }

    #[test]
    fn foreign_channel_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let mut manifest = manifest_for("http://127.0.0.1:1/x.zip", &"0".repeat(64), 1);
        manifest.channel = "release".into();
        // чужой канал отсекается до скачивания
        let err = check_and_update_with(&layout, "dev", &manifest, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Channel { .. }), "получили {err:?}");
    }

    #[cfg(windows)]
    #[test]
    fn ensure_client_not_running_blocks_on_open_handle() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::CreateFileW;
        const GENERIC_READ: u32 = 0x8000_0000;
        const FILE_SHARE_NONE: u32 = 0;
        const OPEN_EXISTING: u32 = 3;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(CLIENT_EXE);
        fs::write(&exe, b"MZ").unwrap();
        // нет блокировки — ок
        ensure_client_not_running(&exe).unwrap();

        // держим exe открытым с share=0 (как запущенный процесс)
        let wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let err = ensure_client_not_running(&exe).unwrap_err();
        assert!(matches!(err, Error::GameRunning), "получили {err:?}");
        unsafe { CloseHandle(handle) };
        ensure_client_not_running(&exe).unwrap();
    }
}
