//! Интеграционные тесты CLI лаунчера: только то, что не ходит по сети
//! и не трогает конфиг пользователя.

use std::path::{Path, PathBuf};
use std::process::Command;

fn launcher() -> Command {
    Command::new(env!("CARGO_BIN_EXE_zn-launcher"))
}

#[test]
fn version_flag_prints_version() {
    let output = launcher().arg("--version").output().unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zn-launcher "), "получили: {stdout}");
}

#[test]
fn help_flag_prints_usage() {
    let output = launcher().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--install-dir"), "получили: {stdout}");
}

#[test]
fn unknown_flag_exits_with_code_2() {
    let output = launcher().arg("--nope").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn relative_install_dir_rejected_before_any_io() {
    let output = launcher()
        .args(["--cli", "--install-dir", "relative/path"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("абсолютн"),
        "ожидали отказ с объяснением, получили: {stderr}"
    );
}

#[test]
fn install_dir_flag_without_value_fails() {
    let output = launcher().arg("--install-dir").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// Копия лаунчера в изолированном каталоге: иначе workspace-детект
/// (`Cargo.toml` рядом с exe) включает dev-режим и тест уходит в попытку
/// запустить zn-client из target/debug.
fn launcher_isolated(dir: &Path) -> PathBuf {
    let dest = dir.join("zn-launcher.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_zn-launcher"), &dest).unwrap();
    dest
}

/// Запуск с подставленным профилем (LOCALAPPDATA) и stdin-строкой.
/// Сеть отрублена: мёртвая API-база + NO_PROXY, чтобы не зависеть от
/// окружения (в песочнице с прокси лаунчер иначе уходил в 100-секундные
/// таймауты).
fn run_first_run(profile: &Path, stdin_line: &str) -> std::process::Output {
    use std::io::Write;
    let exe = launcher_isolated(profile);
    let mut child = std::process::Command::new(exe)
        .args(["--cli", "--check"])
        .env("LOCALAPPDATA", profile)
        .env_remove("USERPROFILE")
        .env("ZONA_API_BASE", "http://127.0.0.1:1")
        .env("NO_PROXY", "*")
        .env("no_proxy", "*")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_line.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn absolute_test_path() -> String {
    if cfg!(windows) {
        r"C:\ZONAtest".to_string()
    } else {
        "/tmp/zona-test".to_string()
    }
}

#[test]
fn first_run_prompts_and_persists_chosen_folder() {
    let profile = tempfile::tempdir().unwrap();
    let chosen = absolute_test_path();

    // первый запуск: промпт + сохранение конфига (сеть мёртвая — важны
    // состояние до сетевого шага и отсутствие зависаний)
    let output = run_first_run(profile.path(), &format!("{chosen}\n"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Первый запуск"),
        "нет промпта выбора папки: {stdout}"
    );
    let config_path = profile.path().join("ZONA").join("launcher-config.json");
    let config = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("конфиг не сохранён ({e}): {config_path:?}"));
    // в JSON бэкслеши экранированы — ищем базовое имя пути
    assert!(
        config.contains("ZONAtest"),
        "в конфиге нет выбранной папки: {config}"
    );
    // сетевой шаг обязан быть быстрым и чистым (не паника)
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ошибка:"),
        "ожидали чистую сетевую ошибку, получили: {stderr}"
    );

    // второй запуск: промпта быть не должно — папка из конфига
    let output = run_first_run(profile.path(), "");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Первый запуск"),
        "промпт повторился на втором запуске: {stdout}"
    );
}
