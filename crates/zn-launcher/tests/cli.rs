//! Интеграционные тесты CLI лаунчера: только то, что не ходит по сети
//! и не трогает конфиг пользователя.

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
