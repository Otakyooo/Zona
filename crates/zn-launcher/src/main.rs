//! CLI лаунчера: первый запуск (выбор папки) → проверка/установка
//! обновления → запуск клиента. GUI (M6-4) — тонкая обёртка над той же lib.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use zn_launcher::install::{self, Config, Layout};
use zn_launcher::manifest;
use zn_launcher::run::{self, ServerChoice};
use zn_launcher::update::{self, UpdateReport};
use zn_launcher::{Error, Result};

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Cli {
    channel: String,
    install_dir: Option<PathBuf>,
    check_only: bool,
    keep_console: bool,
    /// Принудительный консольный режим (по умолчанию — GUI).
    cli_mode: bool,
}

fn usage() -> String {
    format!(
        "ZONA launcher {VERSION}\n\n\
         Использование:\n  \
           zn-launcher [--cli] [--channel <канал>] [--install-dir <путь>] [--check] [--console]\n\n\
         Опции:\n  \
           --cli                 консольный режим вместо GUI;\n  \
           --channel <канал>    канал обновлений (по умолчанию dev);\n  \
           --install-dir <путь> куда ставить игру (абсолютный путь); без флага —\n  \
                                папка из конфига или выбор при первом запуске;\n  \
           --check              только проверить обновление, не запускать;\n  \
           --console            не закрывать консоль после выхода из игры;\n  \
           -h, --help           эта справка;\n  \
           --version            версия лаунчера\n"
    )
}

fn parse_args() -> std::result::Result<Cli, String> {
    let mut cli = Cli {
        channel: manifest::DEFAULT_CHANNEL.to_string(),
        install_dir: None,
        check_only: false,
        keep_console: false,
        cli_mode: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--cli" => cli.cli_mode = true,
            "--channel" => {
                cli.channel = args
                    .next()
                    .ok_or_else(|| format!("--channel требует значение\n\n{}", usage()))?;
            }
            "--install-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--install-dir требует путь\n\n{}", usage()))?;
                cli.install_dir = Some(PathBuf::from(value));
            }
            "--check" => cli.check_only = true,
            "--console" => cli.keep_console = true,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--version" => {
                println!("zn-launcher {VERSION}");
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный аргумент: {other}\n\n{}", usage())),
        }
    }
    Ok(cli)
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let cli = match parse_args() {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if !cli.cli_mode {
        // В GUI-режиме флаги не молчим: --install-dir/--channel засеивают
        // конфиг, GUI стартует сразу с нужной папкой (без диалога).
        if let Some(dir) = &cli.install_dir {
            install::validate_install_dir(dir)?;
        }
        if cli.install_dir.is_some() || cli.channel != manifest::DEFAULT_CHANNEL {
            let config_home = install::config_home()?;
            let path = Config::path_in(&config_home);
            let mut config = Config::load(&path)?;
            if let Some(dir) = &cli.install_dir {
                config.install_dir = Some(dir.clone());
            }
            if cli.channel != manifest::DEFAULT_CHANNEL {
                config.channel = cli.channel.clone();
            }
            config.save(&path)?;
        }
        // GUI-режим по умолчанию: тестер дважды кликает по exe.
        let options = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([520.0, 400.0])
                .with_title("ZONA launcher"),
            ..Default::default()
        };
        eframe::run_native(
            "ZONA launcher",
            options,
            Box::new(|_cc| Ok(Box::new(zn_launcher::gui::LauncherApp::new()))),
        )?;
        return Ok(());
    }
    match run(cli) {
        Ok(()) => Ok(()),
        Err(error) => {
            eprintln!("ошибка: {error}");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    // Флаг проверяем сразу: понятная ошибка до любого ФС/сети.
    if let Some(dir) = &cli.install_dir {
        install::validate_install_dir(dir)?;
    }
    let config_home = install::config_home()?;
    let config_path = Config::path_in(&config_home);
    let mut config = Config::load(&config_path)?;

    // Первый запуск без папки в конфиге — спрашиваем.
    let install_dir =
        match install::resolve_install_dir(cli.install_dir.as_deref(), &config, &config_home) {
            Ok(dir) => dir,
            Err(_) if config.install_dir.is_none() && cli.install_dir.is_none() => {
                let default = install::default_install_dir(&config_home, &config.channel);
                let chosen = prompt_install_dir(&default)?;
                config.install_dir = Some(chosen.clone());
                config.save(&config_path)?;
                chosen
            }
            Err(error) => return Err(error),
        };
    let layout = Layout::new(install_dir);
    // Самообновление: уборка .old от прошлой подмены (лучше усилия).
    if let Ok(exe) = std::env::current_exe() {
        zn_launcher::selfupdate::cleanup_old(&exe);
    }
    println!(
        "ZONA launcher {VERSION} (канал: {}, папка: {})",
        config.channel,
        layout.root().display()
    );

    // Dev-режим: клиент из рабочего дерева, сеть не трогаем.
    let launcher_exe = std::env::current_exe().unwrap_or_default();
    if run::is_dev_mode(&launcher_exe) {
        println!("dev-режим (ZN_LAUNCHER_DEV или Cargo.toml рядом) — клиент из рабочего дерева");
        return launch_dev(&launcher_exe, cli.keep_console);
    }

    let manifest = manifest::fetch_manifest(manifest::release_token())?;
    manifest.validate()?;
    let mut report =
        update::check_and_update_with(&layout, &config.channel, &manifest, &mut print_progress)?;
    println!();
    print_report(&report);

    // Самообновление лаунчера (M6-7): zip несёт и свежий zn-launcher.exe.
    match zn_launcher::selfupdate::update_from_layout(&layout, &std::env::current_exe()?) {
        Ok(zn_launcher::selfupdate::SelfUpdate::Updated) => {
            println!("Лаунчер обновлён — перезапуск");
            zn_launcher::selfupdate::relaunch(&std::env::current_exe()?)?;
            return Ok(());
        }
        Ok(zn_launcher::selfupdate::SelfUpdate::Deferred) => eprintln!(
            "предупреждение: не удалось заменить запущенный лаунчер — \
             замените zn-launcher.exe вручную (файл .new рядом)"
        ),
        Ok(_) => {}
        Err(error) => eprintln!("предупреждение: самообновление лаунчера: {error}"),
    }

    if cli.check_only {
        return Ok(());
    }
    if matches!(report, UpdateReport::Failed { keep: None, .. }) {
        eprintln!("игра не установлена — запускать нечего");
        return Ok(());
    }

    loop {
        println!(
            "ZONA [{}] — [1] Играть  [2] Обновить  [q] Выход",
            status_line(&report)
        );
        let choice = read_line();
        match choice.trim() {
            "" | "1" => break,
            "2" => match update::force_update(&layout, &config.channel, &mut print_progress) {
                Ok(updated) => {
                    println!();
                    print_report(&updated);
                    report = updated;
                }
                Err(error) => eprintln!("обновление не удалось: {error}"),
            },
            "q" | "й" => return Ok(()),
            _ => continue,
        }
    }

    let server = ServerChoice {
        manual: None,
        manifest: manifest.server_addr.as_deref(),
    };
    if let Some(warning) = run::server_warning(&server) {
        eprintln!("предупреждение: {warning}");
    }
    let mut command = run::client_command(&layout.client_exe(), &layout.content_dir(), &server);
    let status = command
        .status()
        .map_err(|e| Error::Install(format!("не удалось запустить клиента: {e}")))?;
    if !status.success() {
        eprintln!("клиент завершился с кодом {:?}", status.code());
    }
    if cli.keep_console {
        println!("нажмите Enter для выхода");
        let _ = read_line();
    }
    Ok(())
}

fn launch_dev(launcher_exe: &Path, keep_console: bool) -> Result<()> {
    let client = run::resolve_client_exe(launcher_exe, true);
    let content = run::dev_content_root(launcher_exe).ok_or_else(|| {
        Error::Install("dev-режим: не найден game/content рабочего дерева".into())
    })?;
    if !client.is_file() {
        return Err(Error::Install(format!(
            "dev-режим: клиент не собран ({}) — выполните cargo build -p zn-client",
            client.display()
        )));
    }
    let status = run::client_command(&client, &content, &ServerChoice::default())
        .status()
        .map_err(|e| Error::Install(format!("не удалось запустить dev-клиента: {e}")))?;
    if !status.success() {
        eprintln!("dev-клиент завершился с кодом {:?}", status.code());
    }
    if keep_console {
        println!("нажмите Enter для выхода");
        let _ = read_line();
    }
    Ok(())
}

fn prompt_install_dir(default: &Path) -> Result<PathBuf> {
    println!("Первый запуск: куда установить ZONA?");
    println!(
        "Enter — {}, или введите свой абсолютный путь",
        default.display()
    );
    let line = read_line();
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(default.to_path_buf());
    }
    let path = PathBuf::from(trimmed);
    install::validate_install_dir(&path)?;
    Ok(path)
}

fn print_progress(fraction: f32) {
    let percent = (fraction * 100.0).round() as u32;
    print!("\rЗагрузка: {percent:>3}%");
    let _ = std::io::stdout().flush();
}

fn print_report(report: &UpdateReport) {
    match report {
        UpdateReport::DevMode => println!("dev-режим: клиент из рабочего дерева"),
        UpdateReport::NoUpdate { installed } => {
            println!("Установлена актуальная версия {installed}")
        }
        UpdateReport::Installed { version } => println!("Установлена версия {version}"),
        UpdateReport::Updated { from, to } => println!("Обновлено: {from} → {to}"),
        UpdateReport::Failed { keep, reason } => match keep {
            Some(version) => {
                eprintln!("Обновление не удалось: {reason} (версия {version} не тронута)")
            }
            None => eprintln!("Обновление не удалось: {reason}"),
        },
    }
}

fn status_line(report: &UpdateReport) -> String {
    match report {
        UpdateReport::Failed {
            keep: Some(version),
            ..
        } => format!("установлена {version}"),
        UpdateReport::Failed { keep: None, .. } => "не установлена".to_string(),
        UpdateReport::NoUpdate { installed } => format!("установлена {installed}"),
        UpdateReport::Installed { version } | UpdateReport::Updated { to: version, .. } => {
            format!("установлена {version}")
        }
        UpdateReport::DevMode => "dev-режим".to_string(),
    }
}

fn read_line() -> String {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line
}
