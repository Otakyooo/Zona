//! Самообновление лаунчера (M6-7).
//!
//! Клиентский zip несёт оба бинарника — после обновления игры в
//! `<игра>/game/` лежит свежий `zn-launcher.exe`. Если его хэш отличается
//! от запущенного — подменяем себя:
//!
//! 1. `zn-launcher.exe` → `zn-launcher.exe.old` (Windows разрешает
//!    переименование запущенного образа: загрузчик открывает его с
//!    share-delete; удаление — нет, поэтому только rename);
//! 2. свежий → `zn-launcher.exe` (копирование, файл был свободен);
//! 3. relaunch и выход;
//! 4. `zn-launcher.exe.old` удаляется на следующем старте
//!    ([`cleanup_old`]).
//!
//! Если rename невозможен (Linux держит запущенный образ; каталог только
//! для чтения) — статус [`SelfUpdate::Deferred`]: свежий кладётся рядом как
//! `.new`, пользователю предлагается заменить вручную.

use crate::error::{Error, Result};
use crate::install::Layout;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Итог самообновления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfUpdate {
    /// Запущенный лаунчер и есть свежая сборка.
    UpToDate,
    /// Лаунчер подменён — нужно перезапуститься (relaunch).
    Updated,
    /// Не удалось подменить запущенный exe; рядом лежит `.new` для ручной замены.
    Deferred,
}

/// Сравнивает запущенный exe с приехавшим в игровой папке.
pub fn needs_update(current: &Path, fresh: &Path) -> Result<bool> {
    if !fresh.is_file() {
        return Ok(false);
    }
    let current_hash = crate::update::sha256_hex(current)?;
    let fresh_hash = crate::update::sha256_hex(fresh)?;
    Ok(current_hash != fresh_hash)
}

/// Проверяет и применяет самообновление.
/// `current_exe` — путь к запущенному бинарнику, `fresh` — из игровой папки.
pub fn check_and_apply(current_exe: &Path, fresh: &Path) -> Result<SelfUpdate> {
    if !fresh.is_file() || !needs_update(current_exe, fresh)? {
        return Ok(SelfUpdate::UpToDate);
    }
    match replace_executable(current_exe, fresh)? {
        ReplaceOutcome::Replaced => Ok(SelfUpdate::Updated),
        ReplaceOutcome::Busy => {
            // Фолбэк: положим .new рядом — заменить вручную при следующем входе.
            let staged = staged_path(current_exe);
            fs::copy(fresh, &staged)?;
            log::warn!(
                "лаунчер не подменён (занят): свежий лежит как {} — замените вручную",
                staged.display()
            );
            Ok(SelfUpdate::Deferred)
        }
    }
}

/// Что вышло с подменой [`replace_executable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceOutcome {
    /// Текущий exe уехал в `.old`, на его месте свежий.
    Replaced,
    /// Переименовать не удалось (образ занят ОС).
    Busy,
}

/// Подмена: `current` → `old`, `fresh` → `current`.
/// Сбой копирования после rename — откат из `old`.
pub fn replace_executable(current: &Path, fresh: &Path) -> Result<ReplaceOutcome> {
    let old = old_path(current);
    if old.exists() {
        let _ = fs::remove_file(&old);
    }
    match fs::rename(current, &old) {
        Ok(()) => match fs::copy(fresh, current) {
            Ok(_) => Ok(ReplaceOutcome::Replaced),
            Err(error) => {
                log::warn!("копирование свежего лаунчера не удалось ({error}) — откат");
                let _ = fs::rename(&old, current);
                Err(Error::Io(error))
            }
        },
        Err(error) => {
            log::debug!("rename запущенного лаунчера не удался: {error}");
            Ok(ReplaceOutcome::Busy)
        }
    }
}

/// Уборка `.old` от прошлой подмены. Ошибки не критичны: файл удалится
/// на одном из следующих стартов.
pub fn cleanup_old(current_exe: &Path) {
    let old = old_path(current_exe);
    if old.exists() {
        if let Err(error) = fs::remove_file(&old) {
            log::debug!("не удалось убрать {}: {error}", old.display());
        }
    }
}

/// Запуск нового экземпляра (после подмены по исходному пути лежит свежий).
pub fn relaunch(current_exe: &Path) -> Result<()> {
    Command::new(current_exe).spawn().map_err(|e| {
        Error::Install(format!(
            "не удалось перезапустить лаунчер ({}): {e}",
            current_exe.display()
        ))
    })?;
    Ok(())
}

fn old_path(current_exe: &Path) -> PathBuf {
    let mut name = current_exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    current_exe.with_file_name(name)
}

fn staged_path(current_exe: &Path) -> PathBuf {
    let mut name = current_exe.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    current_exe.with_file_name(name)
}

/// Тип-алias для вызывающих: самообновление относительно раскладки.
pub fn update_from_layout(layout: &Layout, current_exe: &Path) -> Result<SelfUpdate> {
    check_and_apply(current_exe, &layout.bundled_launcher())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn needs_update_detects_hash_difference() {
        let dir = tempfile::tempdir().unwrap();
        let current = fixture(dir.path(), "launcher.exe", b"old-build");
        let fresh = fixture(dir.path(), "fresh.exe", b"new-build");
        assert!(needs_update(&current, &fresh).unwrap());
        let same = fixture(dir.path(), "same.exe", b"old-build");
        assert!(!needs_update(&current, &same).unwrap());
    }

    #[test]
    fn needs_update_false_when_no_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let current = fixture(dir.path(), "launcher.exe", b"build");
        assert!(!needs_update(&current, &dir.path().join("missing.exe")).unwrap());
    }

    #[test]
    fn replace_executable_swaps_and_keeps_old() {
        let dir = tempfile::tempdir().unwrap();
        let current = fixture(dir.path(), "zn-launcher.exe", b"old-build");
        let fresh = fixture(dir.path(), "game/zn-launcher.exe", b"new-build");

        assert_eq!(
            replace_executable(&current, &fresh).unwrap(),
            ReplaceOutcome::Replaced
        );
        // на исходном пути свежий, старый уехал в .old
        assert_eq!(fs::read(&current).unwrap(), b"new-build");
        assert_eq!(fs::read(old_path(&current)).unwrap(), b"old-build");
        // повторный заход: .old перезаписывается, не мешает
        let fresh2 = fixture(dir.path(), "game/zn-launcher.exe", b"newer-build");
        assert!(needs_update(&current, &fresh2).unwrap());
        assert_eq!(
            replace_executable(&current, &fresh2).unwrap(),
            ReplaceOutcome::Replaced
        );
        assert_eq!(fs::read(&current).unwrap(), b"newer-build");
        // cleanup на следующем старте убирает .old
        cleanup_old(&current);
        assert!(!old_path(&current).exists());
    }

    #[test]
    fn replace_executable_restores_on_copy_failure() {
        let dir = tempfile::tempdir().unwrap();
        let current = fixture(dir.path(), "zn-launcher.exe", b"old-build");
        // «свежий» — несуществующий файл: rename пройдёт, copy упадёт → откат
        let missing = dir.path().join("game/zn-launcher.exe");
        assert!(replace_executable(&current, &missing).is_err());
        assert_eq!(fs::read(&current).unwrap(), b"old-build");
    }

    #[test]
    fn cleanup_old_noop_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let current = fixture(dir.path(), "zn-launcher.exe", b"build");
        cleanup_old(&current); // не паникует
        assert!(!old_path(&current).exists());
    }

    #[test]
    fn check_and_apply_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let current = fixture(dir.path(), "zn-launcher.exe", b"old-build");
        let fresh = fixture(dir.path(), "game/zn-launcher.exe", b"new-build");
        assert_eq!(
            check_and_apply(&current, &fresh).unwrap(),
            SelfUpdate::Updated
        );
        // второй раз — уже актуально
        assert_eq!(
            check_and_apply(&current, &fresh).unwrap(),
            SelfUpdate::UpToDate
        );
    }

    #[test]
    fn old_and_new_paths_named_next_to_exe() {
        let exe = Path::new(r"C:\Users\t\launcher\zn-launcher.exe");
        assert_eq!(
            old_path(exe),
            Path::new(r"C:\Users\t\launcher\zn-launcher.exe.old")
        );
        assert_eq!(
            staged_path(exe),
            Path::new(r"C:\Users\t\launcher\zn-launcher.exe.new")
        );
    }
}
