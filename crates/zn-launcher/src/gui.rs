//! GUI лаунчера (eframe/egui 0.36): «Играть», «Обновить», «Папка…»,
//! «Свой сервер», прогресс и статус. Вся логика — в lib; GUI — тонкий
//! слой. Сеть идёт в воркер-потоке, UI не блокируется.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use eframe::egui;

use crate::install::{self, Config, Layout};
use crate::launcher_update::{self, SelfUpdateStep};
use crate::manifest::Manifest;
use crate::run::{self, ServerChoice};
use crate::update::{self, UpdateReport};
use crate::LAUNCHER_VERSION;

/// Что UI просит у воркера.
pub enum WorkCmd {
    /// Проверить и установить (`force` — «Обновить» без сверки версии).
    Install {
        layout: Layout,
        channel: String,
        force: bool,
    },
}

/// Что воркер отвечает UI.
pub enum WorkMsg {
    Progress(f32),
    /// Грузный ответ — в боксе (large_enum_variant).
    Done(Box<WorkDone>),
}

/// Итог работы воркера.
pub struct WorkDone {
    pub report: Result<UpdateReport, String>,
    pub manifest: Option<Manifest>,
    /// Итог шагов обновления самого лаунчера (M6-7 бандл + M6-8 манифест).
    pub launcher: LauncherOutcome,
}

/// Что UI должен сделать с лаунчером после шагов самообновления.
#[derive(Debug, Default)]
pub struct LauncherOutcome {
    /// Лаунчер подменён на месте — нужен relaunch.
    pub relaunch: bool,
    /// Предупреждение для пользователя (занят exe / не скачалось).
    pub warning: Option<String>,
}

fn launcher_outcome(step: SelfUpdateStep) -> LauncherOutcome {
    match step {
        SelfUpdateStep::UpToDate => LauncherOutcome::default(),
        SelfUpdateStep::Updated { .. } => LauncherOutcome {
            relaunch: true,
            warning: None,
        },
        SelfUpdateStep::Deferred => LauncherOutcome {
            relaunch: false,
            warning: Some(
                "лаунчер не заменен (занят) — замените zn-launcher.exe вручную (файл .new рядом)"
                    .to_string(),
            ),
        },
        SelfUpdateStep::Failed { reason } => LauncherOutcome {
            relaunch: false,
            warning: Some(format!("обновление лаунчера: {reason}")),
        },
    }
}

enum Phase {
    /// Первый запуск: диалог выбора папки, без ответа не продолжается.
    FirstRun {
        input: String,
        error: Option<String>,
    },
    /// Воркер качает/ставит.
    Working,
    /// Есть чем играть (или старая версия жива после `Failed{keep}`).
    Ready,
}

pub struct LauncherApp {
    config_home: PathBuf,
    config_path: PathBuf,
    config: Config,
    layout: Option<Layout>,
    phase: Phase,
    progress: f32,
    status: String,
    error: Option<String>,
    server_field: String,
    /// Логин/пароль аккаунта (M5-1): уходят в клиент через ZN_PROFILE/ZN_PASSWORD.
    account_field: String,
    password_field: String,
    manifest: Option<Manifest>,
    /// Можно жать «Играть» (есть установленная сборка).
    game_ready: bool,
    /// Авто-проверка обновлений на старте уже уходила воркеру (фикс серой
    /// «Играть»: без неё `game_ready` не поднимался до ручного «Обновить»).
    checked_once: bool,
    play_after_install: bool,
    /// «Играть гостем» после установки (M5-3): запуск без аккаунта.
    guest_after_install: bool,
    /// Куда переносим игру — ждём подтверждения удаления старой папки.
    move_prompt: Option<PathBuf>,
    work_tx: Sender<WorkCmd>,
    work_rx: Receiver<WorkMsg>,
    dev_mode: bool,
}

impl LauncherApp {
    pub fn new() -> Self {
        let (config_home, config, config_path, load_error) = match install::config_home() {
            Ok(home) => {
                let path = Config::path_in(&home);
                let config = Config::load(&path).unwrap_or_else(|error| {
                    log::warn!("конфиг не прочитан ({error}) — дефолтный");
                    Config::default()
                });
                (home, config, path, None)
            }
            Err(error) => (
                PathBuf::from("."),
                Config::default(),
                PathBuf::from("."),
                Some(error.to_string()),
            ),
        };
        let server_field = config.server_override.clone().unwrap_or_default();
        let account_field = config.account.clone().unwrap_or_default();
        let password_field = config.password.clone().unwrap_or_default();
        let layout = config
            .install_dir
            .as_ref()
            .map(|dir| Layout::new(dir.clone()));
        let phase = if layout.is_none() {
            Phase::FirstRun {
                input: String::new(),
                error: None,
            }
        } else {
            Phase::Ready
        };
        let (work_tx, commands) = channel();
        let (messages, work_rx) = channel();
        let state_dir = config_home.clone();
        thread::spawn(move || worker(commands, messages, state_dir));
        let dev_mode = run::is_dev_mode(&std::env::current_exe().unwrap_or_default());
        // Самообновление (M6-7): уборка .old от прошлой подмены.
        if let Ok(exe) = std::env::current_exe() {
            crate::selfupdate::cleanup_old(&exe);
        }
        Self {
            config_home,
            config_path,
            config,
            layout,
            phase,
            progress: 0.0,
            status: load_error.unwrap_or_else(|| "Готов к проверке обновлений".to_string()),
            error: None,
            server_field,
            account_field,
            password_field,
            manifest: None,
            game_ready: false,
            checked_once: false,
            play_after_install: false,
            guest_after_install: false,
            move_prompt: None,
            work_tx,
            work_rx,
            dev_mode,
        }
    }

    fn save_config(&mut self) {
        self.config.server_override =
            Some(self.server_field.trim().to_string()).filter(|value| !value.is_empty());
        self.config.account =
            Some(self.account_field.trim().to_string()).filter(|value| !value.is_empty());
        self.config.password = Some(self.password_field.clone()).filter(|value| !value.is_empty());
        if let Err(error) = self.config.save(&self.config_path) {
            self.error = Some(format!("не удалось сохранить конфиг: {error}"));
        }
    }

    /// Логин/пароль из полей GUI (M5-1).
    fn credentials(&self) -> run::Credentials<'_> {
        run::Credentials {
            account: Some(self.account_field.as_str()),
            password: Some(self.password_field.as_str()),
        }
    }

    /// Отправляет воркеру проверку/установку.
    fn request_install(&mut self, force: bool) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let _ = self.work_tx.send(WorkCmd::Install {
            layout,
            channel: self.config.channel.clone(),
            force,
        });
        self.phase = Phase::Working;
        self.progress = 0.0;
        self.error = None;
        self.status = if force {
            "Обновляем…".to_string()
        } else {
            "Проверяем обновления…".to_string()
        };
    }

    fn play(&mut self) {
        self.launch(false);
    }

    /// «Играть гостем» (M5-3): запуск без аккаунта — поля и сохранённый
    /// конфиг не трогаем, креды пустые (ZN_PASSWORD снимается с процесса).
    fn play_guest(&mut self) {
        self.launch(true);
    }

    /// Запуск клиента. `guest` — без аккаунта: конфиг не перезаписывается.
    fn launch(&mut self, guest: bool) {
        if self.dev_mode {
            self.launch_dev(guest);
            return;
        }
        let Some(layout) = self.layout.clone() else {
            return;
        };
        if !guest {
            self.save_config();
        }
        let server = server_choice(
            &self.server_field,
            self.manifest
                .as_ref()
                .and_then(|m| m.server_addr.as_deref()),
        );
        if let Some(warning) = run::server_warning(&server) {
            self.error = Some(warning);
        } else {
            self.error = None;
        }
        let credentials = if guest {
            run::Credentials::default()
        } else {
            self.credentials()
        };
        let mut command = run::client_command(
            &layout.client_exe(),
            &layout.content_dir(),
            &server,
            &credentials,
        );
        match command.spawn() {
            Ok(_) => {
                self.status = if guest {
                    "Игра запущена (гость)".to_string()
                } else {
                    "Игра запущена".to_string()
                }
            }
            Err(error) => self.error = Some(format!("не удалось запустить игру: {error}")),
        }
    }

    fn launch_dev(&mut self, guest: bool) {
        let exe = std::env::current_exe().unwrap_or_default();
        let Some(content) = run::dev_content_root(&exe) else {
            self.error = Some("dev-режим: не найден game/content".to_string());
            return;
        };
        let client = run::resolve_client_exe(&exe, true);
        let credentials = if guest {
            run::Credentials::default()
        } else {
            self.credentials()
        };
        match run::client_command(&client, &content, &ServerChoice::default(), &credentials).spawn()
        {
            Ok(_) => {
                self.status = if guest {
                    "dev-клиент запущен (гость)".to_string()
                } else {
                    "dev-клиент запущен".to_string()
                }
            }
            Err(error) => {
                self.error = Some(format!("dev-режим: не удалось запустить клиента: {error}"))
            }
        }
    }

    /// Применяет выбор папки из диалога первого запуска.
    fn apply_first_run(&mut self, dir: PathBuf) {
        self.config.install_dir = Some(dir.clone());
        if let Err(error) = self.config.save(&self.config_path) {
            self.phase = Phase::FirstRun {
                input: String::new(),
                error: Some(format!("не удалось сохранить конфиг: {error}")),
            };
            return;
        }
        self.layout = Some(Layout::new(dir));
        self.phase = Phase::Ready;
        self.status = "Папка выбрана — можно обновляться".to_string();
    }

    /// Проверяет абсолютность и доступность записи до применения.
    fn apply_first_run_validated(&mut self, dir: PathBuf, display: String) {
        if !dir.is_absolute() {
            self.phase = Phase::FirstRun {
                input: display,
                error: Some("путь должен быть абсолютным".to_string()),
            };
            return;
        }
        if let Err(error) = install::ensure_writable(&dir) {
            self.phase = Phase::FirstRun {
                input: display,
                error: Some(error.to_string()),
            };
            return;
        }
        self.apply_first_run(dir);
    }

    fn pick_folder(&self, current: &str) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new();
        if current.trim().is_empty() {
            let default = install::default_install_dir(&self.config_home, &self.config.channel);
            if let Some(parent) = default.parent() {
                dialog = dialog.set_directory(parent);
            }
        } else {
            dialog = dialog.set_directory(PathBuf::from(current));
        }
        dialog.pick_folder()
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        let mut got = false;
        while let Ok(msg) = self.work_rx.try_recv() {
            got = true;
            match msg {
                WorkMsg::Progress(fraction) => {
                    self.progress = fraction.clamp(0.0, 1.0);
                }
                WorkMsg::Done(done) => {
                    let WorkDone {
                        report,
                        manifest,
                        launcher,
                    } = *done;
                    // свежий манифест не затираем неудачей проверки
                    if manifest.is_some() {
                        self.manifest = manifest;
                    }
                    self.phase = Phase::Ready;
                    let mut playable = false;
                    match &report {
                        Ok(report) => {
                            self.status = status_line(report);
                            self.error = match report {
                                UpdateReport::Failed { reason, .. } => {
                                    Some(format!("Обновление не удалось: {reason}"))
                                }
                                _ => None,
                            };
                            playable = matches!(
                                report,
                                UpdateReport::NoUpdate { .. }
                                    | UpdateReport::Installed { .. }
                                    | UpdateReport::Updated { .. }
                                    | UpdateReport::Failed { keep: Some(_), .. }
                            );
                        }
                        Err(error) => {
                            self.status = "Ошибка".to_string();
                            self.error = Some(error.clone());
                        }
                    }
                    self.game_ready = playable;
                    // Обновление самого лаунчера (M6-7 бандл + M6-8 манифест):
                    // подмена прошла в воркере — перезапускаемся.
                    if launcher.relaunch {
                        self.status = "Лаунчер обновлён — перезапуск…".to_string();
                        if let Ok(current) = std::env::current_exe() {
                            let _ = crate::selfupdate::relaunch_marked(&current);
                        }
                        std::process::exit(0);
                    }
                    if let Some(warning) = launcher.warning {
                        self.error = Some(warning);
                    }
                    if self.play_after_install && playable {
                        self.play_after_install = false;
                        let guest = self.guest_after_install;
                        self.guest_after_install = false;
                        if guest {
                            self.play_guest();
                        } else {
                            self.play();
                        }
                    }
                }
            }
        }
        if got {
            ctx.request_repaint();
        }
    }

    fn draw_first_run(&mut self, ui: &mut egui::Ui) {
        let (mut input, error) = match &self.phase {
            Phase::FirstRun { input, error } => (input.clone(), error.clone()),
            _ => return,
        };
        let mut chosen: Option<PathBuf> = None;
        let mut browse = false;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(12.0);
            ui.heading("ZONA — первый запуск");
            ui.label("Куда установить игру?");
            let default = install::default_install_dir(&self.config_home, &self.config.channel);
            ui.label(format!("По умолчанию: {}", default.display()));
            let response = ui.add(
                egui::TextEdit::singleline(&mut input)
                    .hint_text(default.display().to_string())
                    .desired_width(f32::INFINITY),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let text = input.trim();
                chosen = Some(if text.is_empty() {
                    default.clone()
                } else {
                    PathBuf::from(text)
                });
            }
            if let Some(error) = &error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Обзор…").clicked() {
                    browse = true;
                }
                if ui.button("Продолжить").clicked() {
                    let text = input.trim();
                    chosen = Some(if text.is_empty() {
                        default.clone()
                    } else {
                        PathBuf::from(text)
                    });
                }
            });
        });
        if let Some(dir) = chosen {
            self.apply_first_run_validated(dir, input);
            return;
        }
        if browse {
            if let Some(dir) = self.pick_folder(&input) {
                input = dir.display().to_string();
            }
        }
        if let Phase::FirstRun { input: slot, .. } = &mut self.phase {
            *slot = input;
        }
    }

    fn draw_main(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(format!("ZONA launcher {LAUNCHER_VERSION}"));
            ui.horizontal(|ui| {
                ui.label("Канал:");
                ui.label(&self.config.channel);
                if let Some(layout) = &self.layout {
                    ui.separator();
                    ui.label(format!("Папка: {}", layout.root().display()));
                }
            });
            if self.dev_mode {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "dev-режим: клиент из рабочего дерева, обновления отключены",
                );
            }
            if let Phase::Working = self.phase {
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .show_percentage()
                        .desired_height(20.0),
                );
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.label(&self.status);

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let working = matches!(self.phase, Phase::Working);
                if ui
                    .add_enabled(
                        play_enabled(working, self.game_ready, self.dev_mode),
                        egui::Button::new("Играть"),
                    )
                    .clicked()
                {
                    // ensure update → spawn
                    self.play_after_install = true;
                    self.guest_after_install = false;
                    self.request_install(false);
                }
                if ui
                    .add_enabled(
                        play_enabled(working, self.game_ready, self.dev_mode),
                        egui::Button::new("Играть гостем"),
                    )
                    .on_hover_text("Вход без аккаунта: склад и кошелёк не сохраняются")
                    .clicked()
                {
                    // Гость (M5-3): ensure update → spawn без аккаунта.
                    self.play_after_install = true;
                    self.guest_after_install = true;
                    self.request_install(false);
                }
                if ui
                    .add_enabled(
                        !working && self.layout.is_some() && !self.dev_mode,
                        egui::Button::new("Обновить"),
                    )
                    .clicked()
                {
                    self.request_install(true);
                }
                if ui
                    .add_enabled(!working, egui::Button::new("Папка…"))
                    .clicked()
                {
                    let current = self
                        .layout
                        .as_ref()
                        .map(|l| l.root().display().to_string())
                        .unwrap_or_default();
                    if let Some(dir) = self.pick_folder(&current) {
                        self.move_to(dir);
                    }
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Свой сервер:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.server_field)
                        .hint_text("host:port (пусто — из манифеста)")
                        .desired_width(200.0),
                );
                if response.lost_focus() {
                    self.save_config();
                }
            });
            if let Some(addr) = self
                .manifest
                .as_ref()
                .and_then(|m| m.server_addr.as_deref())
            {
                ui.small(format!("Сервер из манифеста: {addr}"));
            }

            // Аккаунт (M5-1): логин + пароль. Без пароля клиент — гость:
            // играет онлайн, но склад между рейдами не сохраняется.
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Аккаунт:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.account_field)
                        .hint_text("логин (пусто — гость)")
                        .desired_width(200.0),
                );
                if response.lost_focus() {
                    self.save_config();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Пароль:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.password_field)
                        .password(true)
                        .hint_text("пароль (пусто — гость)")
                        .desired_width(200.0),
                );
                if response.lost_focus() {
                    self.save_config();
                }
            });
            if self.account_field.trim().is_empty() || self.password_field.is_empty() {
                ui.small("Без аккаунта склад базы не сохраняется между рейдами");
                ui.small("Можно играть без аккаунта — кнопка «Играть гостем»");
            }
        });
        self.draw_move_prompt(ui);
    }

    /// Меняет папку установки: старая игра остаётся до подтверждения.
    fn move_to(&mut self, dir: PathBuf) {
        let old = self.layout.as_ref().map(|l| l.root().to_path_buf());
        self.config.install_dir = Some(dir.clone());
        if let Err(error) = self.config.save(&self.config_path) {
            self.error = Some(format!("не удалось сохранить конфиг: {error}"));
            return;
        }
        let new_root = Layout::new(dir).root().to_path_buf();
        self.layout = Some(Layout::new(new_root.clone()));
        self.game_ready = false;
        // Авто-проверка взводится заново: новая папка ещё без сборки.
        self.checked_once = false;
        self.status = "Папка изменена — устанавливаем игру".to_string();
        if let Some(old) = old {
            if old != new_root && old.exists() {
                self.move_prompt = Some(old);
            }
        }
    }

    fn draw_move_prompt(&mut self, ui: &mut egui::Ui) {
        let Some(old) = self.move_prompt.clone() else {
            return;
        };
        let mut close = false;
        let mut delete = false;
        egui::Window::new("Удалить старую папку?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui, |ui| {
                ui.label(format!(
                    "Игра установится в новую папку. Удалить {}?",
                    old.display()
                ));
                ui.horizontal(|ui| {
                    if ui.button("Удалить").clicked() {
                        delete = true;
                    }
                    if ui.button("Оставить").clicked() {
                        close = true;
                    }
                });
            });
        if delete {
            if let Err(error) = std::fs::remove_dir_all(&old) {
                self.error = Some(format!("не удалось удалить {}: {error}", old.display()));
            }
            close = true;
        }
        if close {
            self.move_prompt = None;
        }
    }
}

impl Default for LauncherApp {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker(ui.ctx());
        // Авто-проверка обновлений на старте: без неё «Играть» оставалась
        // серой (game_ready поднимался только ответом воркера), и зайти
        // можно было лишь после ручного «Обновить». В dev-режиме проверка
        // не нужна — клиент берётся из рабочего дерева.
        if !self.dev_mode && !self.checked_once && matches!(self.phase, Phase::Ready) {
            self.checked_once = true;
            self.request_install(false);
        }
        if matches!(self.phase, Phase::FirstRun { .. }) {
            self.draw_first_run(ui);
        } else {
            self.draw_main(ui);
        }
    }
}

/// Воркер: единственное место, где UI ходит по сети и в ФС установки.
fn worker(commands: Receiver<WorkCmd>, messages: Sender<WorkMsg>, state_dir: PathBuf) {
    while let Ok(WorkCmd::Install {
        layout,
        channel,
        force,
    }) = commands.recv()
    {
        let progress_tx = messages.clone();
        let mut progress = move |fraction: f32| {
            let _ = progress_tx.send(WorkMsg::Progress(fraction));
        };
        let current_exe = std::env::current_exe().ok();
        let (report, manifest) = if force {
            match update::force_update(&layout, &channel, &mut progress) {
                Ok((report, manifest)) => (Ok(report), Some(manifest)),
                Err(e) => (Err(e.to_string()), None),
            }
        } else {
            match crate::manifest::fetch_manifest(None) {
                Ok(manifest) => match manifest.validate() {
                    Ok(()) => {
                        let report = update::check_and_update_with(
                            &layout,
                            &channel,
                            &manifest,
                            &mut progress,
                        )
                        .map_err(|e| e.to_string());
                        (report, Some(manifest))
                    }
                    Err(e) => (Err(e.to_string()), None),
                },
                Err(e) => (Err(e.to_string()), None),
            }
        };
        // Обновление самого лаунчера: бандл из клиентского zip (M6-7) и/или
        // лаунчер-ассет манифеста (M6-8). Только когда манифест известен.
        let launcher = match (&current_exe, &manifest) {
            (Some(exe), Some(manifest)) => launcher_outcome(launcher_update::update_self(
                manifest,
                &layout,
                Some(state_dir.as_path()),
                exe,
                &mut progress,
            )),
            _ => LauncherOutcome::default(),
        };
        let _ = messages.send(WorkMsg::Done(Box::new(WorkDone {
            report,
            manifest,
            launcher,
        })));
    }
}

/// Непустое поле override → ручной адрес, иначе адрес из манифеста.
pub fn server_choice<'a>(
    override_text: &'a str,
    manifest_addr: Option<&'a str>,
) -> ServerChoice<'a> {
    let manual = override_text.trim();
    ServerChoice {
        manual: if manual.is_empty() {
            None
        } else {
            Some(manual)
        },
        manifest: manifest_addr,
    }
}

/// Можно жать «Играть»/«Играть гостем»: воркер не занят, а сборка либо
/// установлена (ответ проверки), либо dev-режим — клиент из рабочего дерева
/// (фикс M5-3: в dev кнопка была навсегда серой).
pub fn play_enabled(working: bool, game_ready: bool, dev_mode: bool) -> bool {
    !working && (game_ready || dev_mode)
}

/// Текст статуса для строки состояния (чистая логика — для тестов).
pub fn status_line(report: &UpdateReport) -> String {
    match report {
        UpdateReport::NoUpdate { installed } => {
            format!("Установлена актуальная версия {installed}")
        }
        UpdateReport::Installed { version } => format!("Установлена версия {version}"),
        UpdateReport::Updated { from, to } => format!("Обновлено: {from} → {to}"),
        UpdateReport::DevMode => "dev-режим".to_string(),
        UpdateReport::Failed { keep, reason } => match keep {
            Some(version) => format!("Версия {version} не тронута: {reason}"),
            None => format!("Не установлено: {reason}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_choice_prefers_override_field() {
        let choice = server_choice("10.0.0.5:7700", Some("zona.playit.gg:7700"));
        assert_eq!(choice.resolve(), Some("10.0.0.5:7700"));
        let choice = server_choice("  ", Some("zona.playit.gg:7700"));
        assert_eq!(choice.resolve(), Some("zona.playit.gg:7700"));
        let choice = server_choice("", None);
        assert_eq!(choice.resolve(), None);
        assert!(run::server_warning(&choice).is_none());
    }

    #[test]
    fn status_line_covers_reports() {
        assert_eq!(
            status_line(&UpdateReport::NoUpdate {
                installed: "0.1.9".into()
            }),
            "Установлена актуальная версия 0.1.9"
        );
        assert_eq!(
            status_line(&UpdateReport::Updated {
                from: "0.1.9".into(),
                to: "0.1.10".into()
            }),
            "Обновлено: 0.1.9 → 0.1.10"
        );
        assert!(status_line(&UpdateReport::Failed {
            keep: Some("0.1.9".into()),
            reason: "сеть".into()
        })
        .contains("0.1.9"));
        assert!(status_line(&UpdateReport::Failed {
            keep: None,
            reason: "сеть".into()
        })
        .contains("Не установлено"));
    }

    #[test]
    fn play_button_needs_installed_build_or_dev_mode() {
        // Обычный режим: кнопка серая, пока проверка не подтвердила сборку.
        assert!(!play_enabled(false, false, false));
        assert!(play_enabled(false, true, false));
        // Воркер занят (проверка/установка на старте) — кнопка недоступна.
        assert!(!play_enabled(true, true, false));
        // dev-режим: клиент из рабочего дерева — играть можно сразу.
        assert!(play_enabled(false, false, true));
        assert!(!play_enabled(true, false, true));
    }
}
