use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use glam::{Mat4, Quat, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, WindowId};

use zn_core::FixedStep;
use zn_input::InputState;
use zn_net::{PlayerNetState, CLIENT_TIMEOUT, INPUT_REDUNDANCY};
use zn_physics::{PLAYER_EYE_HEIGHT, PLAYER_HALF_HEIGHT, PLAYER_RADIUS};
use zn_render::{
    cull::visible_instances, Camera, DrawList, InstanceRaw, MaterialId, MeshId, Renderer,
};
use zn_scene::{Scene, SceneObject};
use zn_sim::{ClientInput, Simulation, FLAG_SPRINTING};

use crate::assets::{AssetEvent, AssetLoader};
use crate::interp::{player_color, InterpolatedPlayer, INTERP_DELAY_TICKS};
use crate::level::{build_draw_list, load_level, DEFAULT_CONTENT_ROOT, DEFAULT_LEVEL};
use crate::net::{NetClient, NetEvent, NetStatus};
use crate::prediction::{reconcile, ErrorSmoother, InputHistory};

/// Чувствительность мыши (рад на пиксель). Скорости движения, гравитация и
/// прыжок живут в `zn_sim`, чтобы клиент и сервер считали кинематику одинаково.
const LOOK_SENSITIVITY: f32 = 0.0022;

/// Имя игрока в handshake с сервером.
const CLIENT_NAME: &str = "Сталкер";

/// Масштаб unit-капсулы (радиус 0.5, полная высота 2.0) под капсулу игрока
/// (`PLAYER_RADIUS` 0.3, полувысота `PLAYER_HALF_HEIGHT` 0.6).
const PLAYER_CAPSULE_SCALE: Vec3 = Vec3::new(
    PLAYER_RADIUS / 0.5,
    (PLAYER_HALF_HEIGHT + PLAYER_RADIUS) / 1.0,
    PLAYER_RADIUS / 0.5,
);

/// Подъём центра капсулы от ступней (полкапсулы).
const PLAYER_CAPSULE_LIFT: Vec3 = Vec3::new(0.0, PLAYER_HALF_HEIGHT + PLAYER_RADIUS, 0.0);

/// Интервал диагностического лога reconciliation, с.
const RECONCILE_LOG_INTERVAL_SECS: f32 = 5.0;

/// Состояние загрузки ассета по ключу (путь или `builtin://…`).
pub enum AssetState {
    Pending,
    Loaded {
        meshes: Vec<MeshId>,
        /// Индекс материала glTF для каждого меша (primitive.material).
        mesh_materials: Vec<Option<usize>>,
        materials: Vec<(Option<String>, MaterialId)>,
    },
    Failed,
}

pub struct ClientApp {
    renderer: Option<Renderer>,
    input: InputState,
    simulation: Simulation,
    scene: Scene,
    camera: Camera,
    player_id: u64,
    /// id игрока на сервере (снапшоты); `None` — offline/ещё не принят.
    net_player_id: Option<u64>,
    net: Option<NetClient>,
    /// Связь оборвалась (таймаут снапшотов) — заголовок показывает «нет связи».
    connection_lost: bool,
    /// Инпуты, отправленные «серверу» (реальные или виртуальные) — для реплея.
    history: InputHistory,
    /// Визуальное гашение поправок сервера.
    smoother: ErrorSmoother,
    /// Буферы интерполяции удалённых игроков.
    remote: HashMap<u64, InterpolatedPlayer>,
    /// Тик времени рендера удалённых игроков (новейший снапшот − задержка).
    render_tick: u64,
    window_title: String,
    reconcile_count: u64,
    reconcile_error_m: f32,
    last_reconcile_log: Instant,
    asset_loader: AssetLoader,
    assets: HashMap<String, AssetState>,
    /// Материалы с `AlphaMode::Blend` — для back-to-front сортировки кадра.
    blend_materials: HashSet<MaterialId>,
    builtin_mesh_ids: HashMap<String, MeshId>,
    cursor_grabbed: bool,
    fixed: FixedStep,
    last_frame: Option<Instant>,
    client_tick: u64,
}

impl ClientApp {
    /// Новая offline-сессия клиента: уровень с диска (или demo_zone-fallback),
    /// симуляция с коллайдерами сцены, фоновый загрузчик ассетов.
    pub fn with_content(content_root: impl Into<PathBuf>, level: &str) -> Self {
        Self::new_session(content_root, level, None)
    }

    /// Сессия клиента с подключением к серверу (`--server host:port`);
    /// движение до/без сервера — локальное предсказание.
    pub fn with_server(
        server: impl Into<String>,
        content_root: impl Into<PathBuf>,
        level: &str,
    ) -> Self {
        Self::new_session(content_root, level, Some(server.into()))
    }

    fn new_session(content_root: impl Into<PathBuf>, level: &str, server: Option<String>) -> Self {
        let content_root = content_root.into();
        let (scene, _) = load_level(&content_root, level);
        log::info!("уровень '{}': {} объектов", scene.name, scene.len());

        let mut simulation = Simulation::with_scene(&scene);
        let player_id = simulation.spawn_player(scene.spawn);
        let camera = Camera::new(
            scene.spawn + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0),
            scene.spawn_yaw,
            16.0 / 9.0,
        );

        let net = server.and_then(|addr| {
            match NetClient::connect(&addr, scene.content_hash(), CLIENT_NAME) {
                Ok(net) => {
                    log::info!("подключаемся к серверу {addr}");
                    Some(net)
                }
                Err(error) => {
                    log::warn!("не удалось подключиться к {addr}: {error} — offline-режим");
                    None
                }
            }
        });

        let mut assets = HashMap::new();
        let loader = AssetLoader::new(&content_root);
        for object in &scene.objects {
            let key = object.asset_key().to_string();
            if key.starts_with("builtin://") {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = assets.entry(key.clone()) {
                entry.insert(AssetState::Pending);
                loader.request(&key);
            }
        }
        if assets.is_empty() {
            log::info!("внешних ассетов в уровне нет — только встроенные примитивы");
        }

        Self {
            renderer: None,
            input: InputState::new(),
            simulation,
            scene,
            camera,
            player_id,
            net_player_id: None,
            net,
            connection_lost: false,
            history: InputHistory::new(),
            smoother: ErrorSmoother::default(),
            remote: HashMap::new(),
            render_tick: 0,
            window_title: String::new(),
            reconcile_count: 0,
            reconcile_error_m: 0.0,
            last_reconcile_log: Instant::now(),
            asset_loader: loader,
            assets,
            blend_materials: HashSet::new(),
            builtin_mesh_ids: HashMap::new(),
            cursor_grabbed: false,
            fixed: FixedStep::from_hz(60),
            last_frame: None,
            client_tick: 0,
        }
    }

    pub fn new() -> Self {
        Self::with_content(DEFAULT_CONTENT_ROOT, DEFAULT_LEVEL)
    }

    fn grab_cursor(&mut self, grab: bool) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let window = renderer.window();
        let mode = if grab {
            CursorGrabMode::Locked
        } else {
            CursorGrabMode::None
        };
        let result = window.set_cursor_grab(mode);
        if result.is_err() && grab {
            let _ = window.set_cursor_grab(CursorGrabMode::Confined);
        }
        window.set_cursor_visible(!grab);
        self.cursor_grabbed = grab;
    }

    /// Регистрирует загруженные ассеты в GPU-кэшах и добирает builtin-меши
    /// (sphere/cylinder/quad), встречающиеся в сцене.
    fn register_loaded_assets(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        for name in zn_assets::BUILTIN_MESH_NAMES {
            if *name == "cube" || self.builtin_mesh_ids.contains_key(*name) {
                continue;
            }
            if !self
                .scene
                .objects
                .iter()
                .any(|object| object.asset_key() == format!("builtin://{name}"))
            {
                continue;
            }
            if let Some(mesh) = zn_assets::builtin_mesh(name) {
                let id = renderer.register_mesh(&mesh);
                self.builtin_mesh_ids.insert(name.to_string(), id);
            }
        }

        // Капсула нужна даже без объектов-капсул в сцене: ею рисуются удалённые
        // игроки в сетевой сессии.
        if !self.builtin_mesh_ids.contains_key("capsule") {
            if let Some(mesh) = zn_assets::builtin_mesh("capsule") {
                let id = renderer.register_mesh(&mesh);
                self.builtin_mesh_ids.insert("capsule".to_string(), id);
            }
        }

        for event in self.asset_loader.drain_events() {
            match event {
                AssetEvent::Loaded { key, asset } => {
                    let meshes: Vec<MeshId> = asset
                        .meshes
                        .iter()
                        .map(|mesh| renderer.register_mesh(mesh))
                        .collect();
                    let mesh_materials: Vec<Option<usize>> =
                        asset.meshes.iter().map(|mesh| mesh.material).collect();
                    let mut materials: Vec<(Option<String>, MaterialId)> = Vec::new();
                    for material in &asset.materials {
                        let images = material_images(&asset, material);
                        let id = renderer.register_material(material, &images);
                        if material.alpha_mode == zn_assets::AlphaMode::Blend {
                            self.blend_materials.insert(id);
                        }
                        materials.push((material.name.clone(), id));
                    }
                    log::info!(
                        "ассет '{key}' зарегистрирован: мешей {}, материалов {}",
                        meshes.len(),
                        materials.len()
                    );
                    self.assets.insert(
                        key,
                        AssetState::Loaded {
                            meshes,
                            mesh_materials,
                            materials,
                        },
                    );
                }
                AssetEvent::Failed { key, error } => {
                    log::warn!("ассет '{key}' не загружен: {error} — fallback на builtin");
                    self.assets.insert(key, AssetState::Failed);
                }
            }
        }
    }

    /// Пары (меш, материал) для draw list; незагруженное → builtin fallback.
    /// Приоритет материала: имя объекта → индекс материала примитива (glTF)
    /// → первый материал ассета → дефолтный.
    fn resolve_asset(&self, object: &SceneObject) -> Vec<(MeshId, MaterialId)> {
        let Some(renderer) = self.renderer.as_ref() else {
            return vec![(0, 0)];
        };
        let cube = renderer.builtin_cube_mesh();
        let default = renderer.default_material();
        let key = object.asset_key();

        if let Some(name) = zn_assets::parse_builtin_key(key) {
            return vec![if name == "cube" {
                (cube, default)
            } else {
                self.builtin_mesh_ids
                    .get(name)
                    .map_or((cube, default), |id| (*id, default))
            }];
        }

        match self.assets.get(key) {
            Some(AssetState::Loaded {
                meshes,
                mesh_materials,
                materials,
            }) => {
                if meshes.is_empty() {
                    return vec![(cube, default)];
                }
                // Имя материала объекта — для всех примитивов сразу.
                if let Some(name) = object.material.as_deref() {
                    if let Some((_, id)) = materials
                        .iter()
                        .find(|(material_name, _)| material_name.as_deref() == Some(name))
                    {
                        return meshes.iter().map(|mesh| (*mesh, *id)).collect();
                    }
                    log::debug!("материал '{name}' не найден в '{key}' — по примитивам");
                }
                meshes
                    .iter()
                    .enumerate()
                    .map(|(index, mesh)| {
                        let material = mesh_materials
                            .get(index)
                            .copied()
                            .flatten()
                            .and_then(|material_index| {
                                materials.get(material_index).map(|(_, id)| *id)
                            })
                            .or_else(|| materials.first().map(|(_, id)| *id))
                            .unwrap_or(default);
                        (*mesh, material)
                    })
                    .collect()
            }
            _ => vec![(cube, default)],
        }
    }

    /// Кадровый draw list: статика сцены + капсулы удалённых игроков;
    /// полупрозрачные батчи упорядочены back-to-front.
    fn build_frame_draw_list(&mut self) -> DrawList {
        let mut list = self.collect_frame_draw_list();
        // Blend рисуется после opaque; внутри батча — дальние раньше, чтобы
        // результат смешивания не зависел от порядка объектов в сцене.
        let camera_position = self.camera.position;
        list.sort_blended_back_to_front(camera_position, |material| {
            self.blend_materials.contains(&material)
        });
        list
    }

    /// Сырой кадровый draw list: статика сцены + капсулы удалённых игроков
    /// на их интерполированных позициях. Локальный игрок не рисуется — его
    /// точка зрения и есть камера.
    fn collect_frame_draw_list(&mut self) -> DrawList {
        let mut list = build_draw_list(&self.scene, |object| self.resolve_asset(object));

        if self.remote.is_empty() {
            return list;
        }
        let (capsule, material) = {
            let Some(renderer) = self.renderer.as_ref() else {
                return list;
            };
            // До регистрации капсулы (или без GPU) fallback — куб.
            let capsule = self
                .builtin_mesh_ids
                .get("capsule")
                .copied()
                .unwrap_or_else(|| renderer.builtin_cube_mesh());
            (capsule, renderer.default_material())
        };

        // Порядок обхода HashMap случаен — рисуем в порядке id (детерминизм).
        let mut ids: Vec<u64> = self.remote.keys().copied().collect();
        ids.sort_unstable();
        let render_tick = self.render_tick;
        for id in ids {
            let Some(player) = self.remote.get_mut(&id) else {
                continue;
            };
            let Some(sample) = player.sample(render_tick) else {
                continue;
            };
            let model = Mat4::from_scale_rotation_translation(
                PLAYER_CAPSULE_SCALE,
                Quat::from_rotation_y(sample.yaw),
                sample.position + PLAYER_CAPSULE_LIFT,
            );
            list.push(
                capsule,
                material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: player_color(id).into(),
                },
            );
        }
        list
    }

    /// Один кадр симуляции (offline и online): для каждого фиксированного шага
    /// собирается `ClientInput` (та же структура, что пойдёт по сети), пишется в
    /// историю (reconciliation), кладётся в очередь общей симуляции и
    /// выполняется шаг. После шагов камера синхронизируется из состояния
    /// игрока, а последние инпуты уходят серверу с redundancy.
    fn simulate(&mut self, frame_delta: std::time::Duration) {
        let steps = self.fixed.accumulate(frame_delta);

        for _ in 0..steps {
            let (dx, dy) = self.input.take_mouse_delta();
            let (yaw_delta, pitch_delta) = look_delta(dx, dy);

            let [right_axis, forward_axis] =
                self.input
                    .move_axis(KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD);
            let sprinting = self.input.is_key_down(KeyCode::ShiftLeft);

            let command = ClientInput {
                tick: self.client_tick,
                move_forward: forward_axis,
                move_right: right_axis,
                yaw_delta,
                pitch_delta,
                jump: self.input.just_pressed(KeyCode::Space),
                buttons: if sprinting { FLAG_SPRINTING } else { 0 },
            };
            self.history.push(command);
            self.simulation.queue_input(self.player_id, command);
            self.simulation.step_once();
            self.client_tick += 1;
        }

        if steps > 0 {
            self.sync_camera_from_simulation();
            self.input.end_frame();
            if let Some(net) = self.net.as_mut() {
                // Последние INPUT_REDUNDANCY инпутов: потеря датаграммы не
                // должна превращаться в потерю движения.
                net.send_inputs(&self.history.last_n(INPUT_REDUNDANCY));
            }
        }
    }

    /// Камера из состояния игрока: ступни + высота глаз + незагасшая
    /// визуальная поправка сервера; углы — из симуляции.
    fn sync_camera_from_simulation(&mut self) {
        let Some(state) = self.simulation.player_state(self.player_id) else {
            return;
        };
        self.camera.position =
            state.position + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0) + self.smoother.offset();
        self.camera.yaw = state.yaw;
        self.camera.pitch = state.pitch;
    }

    /// Сеть в начале кадра: события `NetClient`, таймаут связи и диагностика
    /// reconciliation. Без `net` — no-op (offline).
    fn process_net(&mut self, now: Instant) {
        let Some(net) = self.net.as_mut() else {
            return;
        };
        let events = net.poll(now);

        for event in events {
            match event {
                NetEvent::Welcome {
                    player_id,
                    server_tick,
                    spawn,
                } => {
                    log::info!(
                        "сервер принял игрока: net id {player_id}, tick {server_tick}, \
                         спавн {spawn:?}"
                    );
                    self.net_player_id = Some(player_id);
                }
                NetEvent::Reject(reason) => {
                    log::warn!("сервер отклонил подключение: {reason:?} — offline-режим");
                    self.drop_connection();
                }
                NetEvent::Snapshot {
                    server_tick,
                    acked_input_tick,
                    players,
                    // Визуал событий боя — следующий коммит M4a.
                    events: _,
                } => self.apply_snapshot(server_tick, acked_input_tick, &players),
            }
        }

        if self.net.as_ref().is_some_and(|net| net.timed_out(now)) {
            log::warn!(
                "сервер молчит дольше {} с — переход в offline",
                CLIENT_TIMEOUT.as_secs()
            );
            self.connection_lost = true;
            self.drop_connection();
        }

        // Диагностика reconciliation — раз в RECONCILE_LOG_INTERVAL_SECS.
        if now.duration_since(self.last_reconcile_log)
            >= std::time::Duration::from_secs_f32(RECONCILE_LOG_INTERVAL_SECS)
        {
            if self.reconcile_count > 0 {
                log::debug!(
                    "reconciliation: {} коррекций за {RECONCILE_LOG_INTERVAL_SECS:.0} с, \
                     последняя ошибка {:.3} м",
                    self.reconcile_count,
                    self.reconcile_error_m
                );
            }
            self.reconcile_count = 0;
            self.last_reconcile_log = now;
        }
    }

    /// Сбрасывает сетевую сессию (отказ сервера/таймаут): дальше — offline.
    fn drop_connection(&mut self) {
        self.net = None;
        self.net_player_id = None;
        self.remote.clear();
    }

    /// Снапшот сервера: буферы удалённых игроков, время рендера интерполяции и
    /// reconciliation локального предсказания.
    fn apply_snapshot(
        &mut self,
        server_tick: u64,
        acked_input_tick: u64,
        players: &[PlayerNetState],
    ) {
        // Удалённые игроки рисуются с задержкой 100 мс (нужна пара снапшотов
        // вокруг рендер-тика); время рендера назад не идёт.
        self.render_tick = self
            .render_tick
            .max(server_tick.saturating_sub(INTERP_DELAY_TICKS));

        let mut present = HashSet::with_capacity(players.len());
        for state in players {
            present.insert(state.player_id);
            if Some(state.player_id) == self.net_player_id {
                continue; // себя рисуем камерой, не капсулой
            }
            self.remote
                .entry(state.player_id)
                .or_insert_with(|| InterpolatedPlayer::new(state.player_id))
                .push(server_tick, *state);
        }
        // Игроки, пропавшие из снапшота, вышли из сессии.
        self.remote
            .retain(|player_id, _| present.contains(player_id));

        let Some(net_player_id) = self.net_player_id else {
            return;
        };
        let Some(me) = players
            .iter()
            .find(|state| state.player_id == net_player_id)
        else {
            return;
        };
        let Some(outcome) = reconcile(
            &mut self.simulation,
            self.player_id,
            me,
            acked_input_tick,
            &self.history,
        ) else {
            return;
        };
        let error = outcome.error();
        self.reconcile_count += 1;
        self.reconcile_error_m = error.length();
        self.smoother.on_correction(error);
        log::trace!(
            "reconciliation: реплей {} инпутов (ack {acked_input_tick}), ошибка {:.3} м",
            outcome.replayed,
            self.reconcile_error_m
        );
    }

    /// Заголовок окна: режим связи и RTT (`ZONA — ping 42 ms`). Обновляется
    /// только при изменении, чтобы не дёргать ОС каждый кадр.
    fn update_window_title(&mut self) {
        let title = match self.net.as_ref() {
            Some(net) => match net.status() {
                NetStatus::Connected => match net.rtt_ms() {
                    Some(rtt) => format!("ZONA — ping {rtt:.0} ms"),
                    None => "ZONA — в сети".to_string(),
                },
                NetStatus::Connecting => "ZONA — подключение…".to_string(),
                NetStatus::Rejected(_) => "ZONA — offline".to_string(),
            },
            None if self.connection_lost => "ZONA — нет связи".to_string(),
            None => "ZONA — offline".to_string(),
        };
        if title == self.window_title {
            return;
        }
        self.window_title = title.clone();
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.window().set_title(&title);
        }
    }
}

impl Default for ClientApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplicationHandler for ClientApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        self.renderer = Some(Renderer::new(event_loop));
        // Ассеты и draw list разрешаются через уже установленный renderer.
        self.register_loaded_assets();
        let list = self.build_frame_draw_list();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.update_draw_list(&list);
        let size = renderer.surface_size();
        self.camera
            .set_aspect(size.width as f32 / size.height.max(1) as f32);
        log::info!(
            "renderer ready: {}x{}, {} инстансов",
            size.width,
            size.height,
            renderer.instance_count()
        );
        self.grab_cursor(true);
        self.update_window_title();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(owned_window_id) = self
            .renderer
            .as_ref()
            .map(|renderer| renderer.window().id())
        else {
            return;
        };
        if window_id != owned_window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if let Some(net) = self.net.as_mut() {
                    net.send_bye();
                }
                event_loop.exit()
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
                self.camera
                    .set_aspect(size.width as f32 / size.height.max(1) as f32);
            }
            WindowEvent::Focused(focused) => {
                self.input.set_focused(focused);
                if !focused && self.cursor_grabbed {
                    self.grab_cursor(false);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(KeyCode::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let grab = !self.cursor_grabbed;
                self.grab_cursor(grab);
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let frame_delta = self
                    .last_frame
                    .map(|last| now - last)
                    .unwrap_or(std::time::Duration::from_millis(16));
                self.last_frame = Some(now);
                self.register_loaded_assets();
                self.process_net(now);
                self.simulate(frame_delta);
                // Поправка сервера гаснет после того, как кадр её применил.
                self.smoother.decay(frame_delta.as_secs_f32());
                let list = self.build_frame_draw_list();
                if let Some(renderer) = self.renderer.as_mut() {
                    // Ресурсы всех батчей помечаем использованными до кулинга:
                    // оффскрин-объекты не должны вытеснять видимые из LRU.
                    renderer.touch_scene(&list);
                    let visible = visible_instances(&list.batches, &self.camera, |mesh| {
                        renderer
                            .mesh_bounds(mesh)
                            .unwrap_or(zn_assets::Bounds::EMPTY)
                    });
                    renderer.update_draw_list(&DrawList { batches: visible });
                    renderer.render(&self.camera);
                }
                self.update_window_title();
            }
            _ => {
                self.input.handle_event(&event);
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.window().request_redraw();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        self.input.handle_device_event(&event);
    }
}

/// Дельта мыши (пиксели) → дельты углов обзора (радианы).
///
/// Мышь вправо (dx > 0) — поворот вправо (yaw+); мышь вверх (dy < 0) — взгляд
/// вверх (pitch+). Знаки закреплены тестом (см. также конвенцию осей в zn-sim).
fn look_delta(dx: f32, dy: f32) -> (f32, f32) {
    (dx * LOOK_SENSITIVITY, -dy * LOOK_SENSITIVITY)
}

/// Извлекает слоты текстур материала из импортированного ассета.
fn material_images<'a>(
    asset: &'a zn_assets::ImportedAsset,
    material: &'a zn_assets::CpuMaterial,
) -> [Option<&'a zn_assets::CpuImage>; 5] {
    let get = |index: Option<usize>| index.and_then(|index| asset.images.get(index));
    [
        get(material.base_color_texture),
        get(material.metallic_roughness_texture),
        get(material.normal_texture),
        get(material.occlusion_texture),
        get(material.emissive_texture),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_images_maps_all_slots() {
        let mut asset = zn_assets::ImportedAsset::default();
        asset
            .images
            .push(zn_assets::CpuImage::new(1, 1, vec![0; 4], true));
        asset
            .images
            .push(zn_assets::CpuImage::new(1, 1, vec![0; 4], false));
        let material = zn_assets::CpuMaterial {
            base_color_texture: Some(0),
            normal_texture: Some(1),
            ..Default::default()
        };
        let images = material_images(&asset, &material);
        assert!(images[0].is_some());
        assert!(images[1].is_none());
        assert!(images[2].is_some());
        assert!(images[3].is_none());
        assert!(images[4].is_none());
    }

    #[test]
    fn app_builds_with_default_content_root() {
        // Только проверка компиляции/конструирования без окна.
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        assert_eq!(app.scene.name, "demo_zone");
        assert!(!app.scene.is_empty());
        // Коллайдеры созданы для всех solid-объектов сцены.
        assert!(app.simulation.static_collider_count() > 0);
        // Игрок заспавнен в точке сцены.
        let player = app.simulation.player_state(app.player_id).unwrap();
        assert_eq!(player.position, app.scene.spawn);
    }

    #[test]
    fn app_requires_loader_events_only_for_external_assets() {
        let app = ClientApp::with_content("game/content", "demo_zone");
        // demo_zone целиком на builtin-кубах — внешних запросов нет.
        assert!(app.assets.is_empty());
    }

    #[test]
    fn look_delta_follows_screen_conventions() {
        let (yaw_right, pitch_up) = look_delta(10.0, -10.0);
        assert!(yaw_right > 0.0, "мышь вправо → поворот вправо (yaw+)");
        assert!(pitch_up > 0.0, "мышь вверх → взгляд вверх (pitch+)");

        let (yaw_left, pitch_down) = look_delta(-10.0, 10.0);
        assert!(yaw_left < 0.0, "мышь влево → поворот влево");
        assert!(pitch_down < 0.0, "мышь вниз → взгляд вниз");
    }

    #[test]
    fn offline_client_walks_via_shared_simulation() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        let start = app.simulation.player_state(app.player_id).unwrap();

        app.input.press_key(KeyCode::KeyW);
        for _ in 0..30 {
            app.simulate(std::time::Duration::from_millis(17));
        }

        let end = app.simulation.player_state(app.player_id).unwrap();
        assert!(
            (end.position - start.position).length() > 0.5,
            "игрок должен сдвинуться по W: {start:?} → {end:?}"
        );
        assert_eq!(end.velocity.y, 0.0, "игрок должен стоять на полу");
        // Камера следует за симуляцией (ступни + высота глаз).
        assert_eq!(app.camera.position.y, end.position.y + PLAYER_EYE_HEIGHT);
    }

    #[test]
    fn snapshot_reconciles_local_player_and_buffers_remotes() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.net_player_id = Some(7);

        app.input.press_key(KeyCode::KeyW);
        for _ in 0..30 {
            app.simulate(std::time::Duration::from_millis(17));
        }
        let predicted = app.simulation.player_state(app.player_id).unwrap();
        let last_tick = app.client_tick.saturating_sub(1);

        // «Сервер»: я — на полметра правее предсказания, все инпуты учтены
        // (реплея нет); второй игрок — со своим yaw.
        let me = PlayerNetState {
            player_id: 7,
            position: (predicted.position + Vec3::new(0.5, 0.0, 0.0)).to_array(),
            velocity: [0.0; 3],
            yaw: predicted.yaw,
            pitch: 0.0,
            health: zn_sim::MAX_HEALTH,
            flags: 0,
        };
        let other = PlayerNetState {
            player_id: 8,
            position: [1.0, 0.0, 2.0],
            velocity: [0.0; 3],
            yaw: 0.5,
            pitch: 0.0,
            health: zn_sim::MAX_HEALTH,
            flags: 0,
        };
        app.apply_snapshot(100, last_tick, &[me, other]);

        // Локальный игрок встал на серверную позицию…
        let corrected = app.simulation.player_state(app.player_id).unwrap();
        assert!(
            (corrected.position - Vec3::from_array(me.position)).length() < 1e-5,
            "{:?} vs {:?}",
            corrected.position,
            me.position
        );
        // …а поправка 0.5 м ушла в сглаживатель (не снап и не игнор).
        let error = app.smoother.offset().length();
        assert!((error - 0.5).abs() < 1e-4, "offset = {error}");
        assert_eq!(app.reconcile_count, 1);

        // Себя в буферах интерполяции нет, второй игрок — есть.
        assert!(!app.remote.contains_key(&7));
        let other_sample = app
            .remote
            .get_mut(&8)
            .expect("второй игрок в буфере")
            .sample(app.render_tick)
            .expect("сэмпл игрока");
        assert!(
            (other_sample.position - Vec3::new(1.0, 0.0, 2.0)).length() < 1e-5,
            "{other_sample:?}"
        );
        // Время рендера удалённых: тик снапшота минус задержка интерполяции.
        assert_eq!(app.render_tick, 100 - INTERP_DELAY_TICKS);
    }

    #[test]
    fn window_title_tracks_connection_state() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.update_window_title();
        assert_eq!(app.window_title, "ZONA — offline");

        app.connection_lost = true;
        app.update_window_title();
        assert_eq!(app.window_title, "ZONA — нет связи");

        // UDP connect ничего не шлёт — состояние «подключение».
        let server = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        app.net =
            Some(NetClient::connect(&server.local_addr().unwrap().to_string(), 0, "Тест").unwrap());
        app.update_window_title();
        assert_eq!(app.window_title, "ZONA — подключение…");
    }
}
