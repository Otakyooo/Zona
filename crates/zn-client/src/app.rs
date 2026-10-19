use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_wgpu::ScreenDescriptor;
use glam::{Mat4, Quat, Vec3, Vec4};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, WindowId};

use zn_audio::{AudioEngine, SoundId};
use zn_content::ItemCatalog;
use zn_core::{FixedStep, SIMULATION_HZ};
use zn_input::InputState;
use zn_net::{
    dequantize_pos, dequantize_yaw, BoltNet, CorpseNet, GameEvent, GroundItemNet, InventoryAction,
    ItemStackNet, PlayerNetState, StashAction, CLIENT_TIMEOUT, INPUT_REDUNDANCY,
};
use zn_physics::{PLAYER_EYE_HEIGHT, PLAYER_HALF_HEIGHT, PLAYER_RADIUS};
use zn_render::{
    cull::visible_instances, Camera, DrawList, InstanceRaw, MaterialId, MaterialSpec, MeshId,
    PostSettings, Renderer,
};
use zn_scene::{AnomalyKind, LocationId, Scene, SceneObject, LOCATION_BASE};
use zn_sim::{
    ClientInput, PlayerAction, Simulation, FIRE_INTERVAL_TICKS, FLAG_FIRE, FLAG_SPRINTING,
    INTERACT_MIN_DOT, INTERACT_RANGE_M, INVENTORY_COLS, INVENTORY_ROWS, INVENTORY_SLOTS,
    MAX_CARRY_WEIGHT_KG, MAX_HEALTH, RESPAWN_DELAY_TICKS, RIFLE_RANGE,
};

use crate::assets::{AssetEvent, AssetLoader};
use crate::interp::{player_color, InterpolatedPlayer, INTERP_DELAY_TICKS};
use crate::level::{
    build_draw_list, load_catalog, load_level, load_world, DEFAULT_CONTENT_ROOT, DEFAULT_LEVEL,
};
use crate::net::{NetClient, NetEvent, NetStatus};
use crate::prediction::{reconcile, ErrorSmoother, InputHistory};

/// Чувствительность мыши (рад на пиксель). Скорости движения, гравитация и
/// прыжок живут в `zn_sim`, чтобы клиент и сервер считали кинематику одинаково.
const LOOK_SENSITIVITY: f32 = 0.0022;

/// Имя игрока в handshake с сервером.
const CLIENT_NAME: &str = "Сталкер";

/// Время жизни эффектов выстрела.
const TRACER_TTL: Duration = Duration::from_millis(80);
const FLASH_TTL: Duration = Duration::from_millis(60);
/// Размеры эффектов (инстансы unit-куба): толщина трейсера и ребро вспышки.
const TRACER_THICKNESS: f32 = 0.025;
const FLASH_SIZE: f32 = 0.12;
/// Смещение дула от глаз вдоль прицела.
const MUZZLE_OFFSET: f32 = 0.45;
const TRACER_COLOR: Vec4 = Vec4::new(1.0, 0.92, 0.6, 1.0);
const FLASH_COLOR: Vec4 = Vec4::new(1.0, 0.8, 0.4, 1.0);

/// Kill-фид: сколько строк показывать и сколько живёт строка.
const KILL_FEED_MAX: usize = 3;
const KILL_FEED_TTL: Duration = Duration::from_secs(4);

/// Ячейка сетки инвентаря в HUD и зазор между ячейками (логические пиксели).
const HUD_CELL: f32 = 44.0;
const HUD_CELL_GAP: f32 = 4.0;

/// Масштаб unit-капсулы (радиус 0.5, полная высота 2.0) под капсулу игрока
/// (`PLAYER_RADIUS` 0.3, полувысота `PLAYER_HALF_HEIGHT` 0.6).
const PLAYER_CAPSULE_SCALE: Vec3 = Vec3::new(
    PLAYER_RADIUS / 0.5,
    (PLAYER_HALF_HEIGHT + PLAYER_RADIUS) / 1.0,
    PLAYER_RADIUS / 0.5,
);

/// Подъём центра капсулы от ступней (полкапсулы).
const PLAYER_CAPSULE_LIFT: Vec3 = Vec3::new(0.0, PLAYER_HALF_HEIGHT + PLAYER_RADIUS, 0.0);

/// Предмет на земле — куб у ступней (сервер кладёт лут на уровень ступней).
const GROUND_ITEM_SIZE: f32 = 0.22;
/// Труп — лежачий бокс жертвы: длина × высота × ширина.
const CORPSE_SIZE: Vec3 = Vec3::new(1.9, 0.35, 0.7);
/// Цвет предмета, которого нет в каталоге (контент разошёлся с сервером).
const DEFAULT_ITEM_COLOR: Vec4 = Vec4::new(0.75, 0.75, 0.75, 1.0);

/// Аномалии (M4c): еле заметные мерцающие сферы; болт, сработавший в аномалии,
/// проявляет её ярко на `ANOMALY_REVEAL_TTL` c вспышкой в точке срабатывания.
const ANOMALY_BASE_ALPHA: f32 = 0.12;
const ANOMALY_FLICKER_ALPHA: f32 = 0.05;
const ANOMALY_FLICKER_SPEED: f32 = 2.4;
const ANOMALY_REVEAL_ALPHA: f32 = 0.5;
const ANOMALY_REVEAL_TTL: Duration = Duration::from_secs(5);
const ANOMALY_FLASH_TTL: Duration = Duration::from_millis(350);
const ANOMALY_FLASH_SIZE: f32 = 1.1;
const ANOMALY_HEAT_COLOR: [f32; 3] = [1.0, 0.45, 0.2];
const ANOMALY_GRAVITY_COLOR: [f32; 3] = [0.5, 0.35, 1.0];
const ANOMALY_FLASH_COLOR: Vec4 = Vec4::new(1.0, 0.95, 0.75, 0.8);
/// HDR-усиление ярких эффектов (M4g-G3): цвет выше единицы — bloom
/// подхватывает проявленные аномалии и вспышки, базовая мгла остаётся
/// еле заметной (её вклад мал из-за низкой альфы).
const ANOMALY_HDR_GAIN: f32 = 3.2;

/// Болт в полёте: тонкая палочка вдоль направления движения.
const BOLT_THICKNESS: f32 = 0.05;
const BOLT_LENGTH: f32 = 0.34;
const BOLT_COLOR: Vec4 = Vec4::new(0.85, 0.86, 0.8, 1.0);

/// Мир локаций (M4d): вспышка при смене локации и вид активных пузырей.
const TP_FLASH_TTL: Duration = Duration::from_millis(260);
/// Радиус пузыря, если якорь не совпал ни с одним кандидатом сцены.
const BUBBLE_DEFAULT_RADIUS: f32 = 1.2;
const BUBBLE_COLOR: [f32; 3] = [0.45, 0.75, 1.0];
const BUBBLE_ALPHA: f32 = 0.22;

/// Артефакты светятся (M4g-G3): цвет из каталога усиливается до HDR, bloom
/// делает лежащие в аномалиях артефакты заметными с первого взгляда.
const ARTIFACT_HDR_GAIN: f32 = 2.4;
/// Префикс ключей артефактов в каталоге предметов.
const ARTIFACT_KEY_PREFIX: &str = "artifact_";

/// Настройки постобработки (M4g-G3) — клавиши F6 (виньетка), F7 (bloom),
/// F8 (порог bloom); подсказка с текущими значениями живёт `POST_HINT_TTL`.
const VIGNETTE_ON: f32 = 0.35;
const BLOOM_INTENSITY_STEPS: [f32; 4] = [0.0, 0.25, 0.45, 0.8];
const BLOOM_THRESHOLD_STEPS: [f32; 3] = [0.6, 0.9, 1.2];
const POST_HINT_TTL: Duration = Duration::from_secs(2);

/// Следующее значение цикла настроек: ближайшее в `steps`, иначе первый шаг.
fn cycle_value(current: f32, steps: &[f32]) -> f32 {
    let index = steps
        .iter()
        .position(|step| (*step - current).abs() < 1e-4)
        .map_or(0, |index| (index + 1) % steps.len());
    steps[index]
}

/// Цвет предмета на земле (M4g-G3): артефакты усиливаются до HDR, чтобы
/// bloom делал их заметными издалека; обычный лут остаётся в LDR.
fn ground_item_color(definition: &zn_content::Item) -> Vec4 {
    let gain = if definition.key.starts_with(ARTIFACT_KEY_PREFIX) {
        ARTIFACT_HDR_GAIN
    } else {
        1.0
    };
    Vec4::new(
        definition.color[0] * gain,
        definition.color[1] * gain,
        definition.color[2] * gain,
        1.0,
    )
}

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

/// Трейсер выстрела: отрезок от дула до попадания, живущий доли секунды.
#[derive(Debug)]
struct Tracer {
    from: Vec3,
    to: Vec3,
    until: Instant,
}

/// Вспышка выстрела у дула.
#[derive(Debug)]
struct MuzzleFlash {
    at: Vec3,
    until: Instant,
}

/// Болт в полёте: позиции двух последних снапшотов для интерполяции рендера.
#[derive(Clone, Copy, Debug)]
struct BoltView {
    id: u32,
    prev: Vec3,
    pos: Vec3,
    prev_tick: u64,
    tick: u64,
}

impl BoltView {
    /// Позиция на момент рендера: линейная интерполяция между снапшотами.
    fn sample(&self, render_tick: u64) -> Vec3 {
        if self.tick <= self.prev_tick {
            return self.pos;
        }
        let span = (self.tick - self.prev_tick) as f32;
        let fraction = (render_tick.saturating_sub(self.prev_tick) as f32 / span).clamp(0.0, 1.0);
        self.prev.lerp(self.pos, fraction)
    }

    /// Направление полёта (prev → pos) — для ориентации палочки.
    fn direction(&self) -> Vec3 {
        (self.pos - self.prev).try_normalize().unwrap_or(Vec3::Y)
    }
}

/// Вспышка в точке срабатывания болта (проявление аномалии).
#[derive(Debug)]
struct AnomalyFlash {
    at: Vec3,
    until: Instant,
}

/// Строка kill-фида: текст и время жизни.
#[derive(Debug)]
struct KillFeedEntry {
    text: String,
    until: Instant,
}

/// Подсказка настроек (F6–F10): текст и время жизни.
#[derive(Debug)]
struct PostHint {
    text: String,
    until: Instant,
}

/// Вид цели наведения — подсказка взаимодействия.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HintKind {
    /// Ящик или труп — «[E] Обыскать».
    Container,
    /// Предмет на земле — «[E] Подобрать».
    Item,
}

/// Открытый контейнер обыска: id и содержимое из персонального апдейта.
struct OpenContainer {
    container: u64,
    items: Vec<ItemStackNet>,
}

/// Фокус панелей, когда открыто больше одной: ↑/↓ и Enter работают с ним.
/// Порядок фокуса слева направо: контейнер, склад, инвентарь.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelFocus {
    Container,
    Stash,
    Inventory,
}

/// Локальный прогресс лечения (UseStarted): полоска живёт до `until`,
/// длительность — для доли заполнения в HUD.
struct HealingView {
    until: Instant,
    total_secs: f32,
}

/// Стек предмета для панелей HUD: имя/количество/цвет из каталога.
struct HudItemStack {
    name: String,
    count: u16,
    color: [f32; 3],
}

/// Данные панели инвентаря (сетка 8×5, выделение, вес).
struct InventoryHud {
    /// Ячейки: `None` — пусто.
    slots: Vec<Option<HudItemStack>>,
    select: usize,
    weight_kg: f32,
    max_weight_kg: f32,
    /// Выделенный стек — строка названия; `None` — ячейка пуста.
    selected: Option<HudItemStack>,
    /// Фокус ввода на панели (когда открыты обе: ←/→).
    focused: bool,
}

/// Данные панели обыска: заголовок («Ящик» / «Тело: имя») и строки.
struct ContainerHud {
    title: String,
    items: Vec<HudItemStack>,
    select: usize,
    /// Фокус ввода на панели (когда открыто больше одной: ←/→).
    focused: bool,
    /// Подсказка клавиш внизу панели (склад); обыск — без подсказки.
    hint: Option<String>,
}

/// Срез состояния для отрисовки HUD (собирается до заимствования renderer).
struct HudData {
    health: u16,
    max_health: u16,
    /// Имя убийцы — только когда локальный игрок мёртв.
    killer: Option<String>,
    /// Секунд до возрождения — только когда локальный игрок мёртв.
    respawn_secs: Option<f32>,
    /// Строки kill-фида: текст и альфа (1 — свежая, 0 — угасла).
    feed: Vec<(String, f32)>,
    /// Подсказка настроек (F6–F10), пока свежа.
    post_hint: Option<String>,
    /// Секунды с запуска клиента — время для egui.
    time: f64,
    /// Подсказка взаимодействия под прицелом («[E] Обыскать»).
    hint: Option<String>,
    /// Панель инвентаря (открыта клавишей I).
    inventory: Option<InventoryHud>,
    /// Панель обыска (сервер держит контейнер открытым).
    container: Option<ContainerHud>,
    /// Панель склада базы (открыта клавишей H на базе).
    stash: Option<ContainerHud>,
    /// Полоска лечения: доля оставшегося времени (1.0 — только началась).
    healing: Option<f32>,
    /// Вспышка смены локации: доля времени жизни (1.0 — только что перешёл).
    flash: Option<f32>,
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
    /// Персональные материалы объектов сцены (M4g): спека → GPU-материал.
    scene_materials: HashMap<MaterialSpec, MaterialId>,
    /// Трейсеры выстрелов: свои — локальный отклик, чужие — события Shot.
    tracers: Vec<Tracer>,
    /// Вспышки у дула.
    flashes: Vec<MuzzleFlash>,
    /// Раньше этого клиентского тика локальный отклик огня не повторяется.
    next_local_fire_tick: u64,
    /// Контекст egui и рендерер HUD (создаётся в `resumed`, как у редактора).
    egui_ctx: egui::Context,
    ui_renderer: Option<egui_wgpu::Renderer>,
    /// Начало работы клиента — время для egui.
    started: Instant,
    /// Последний серверный тик (из снапшотов) — отсчёт респавна.
    last_server_tick: u64,
    /// Серверный тик ожидаемого респавна локального игрока (экран смерти).
    my_respawn_tick: Option<u64>,
    /// Имя убийцы локального игрока — для экрана смерти.
    my_killer: Option<String>,
    /// Последние смерти — строки kill-фида.
    kill_feed: Vec<KillFeedEntry>,
    /// Подсказка настроек (клавиши F6–F10).
    post_hint: Option<PostHint>,
    /// Настройки постобработки (M4g-G3): источник правды клиента, кладётся
    /// в рендер при его создании и при каждой правке клавишами.
    post_settings: PostSettings,
    /// Аудио-слой (A1): процедурные звуки, громкость (F9) и мьют (F10).
    audio: AudioEngine,
    builtin_mesh_ids: HashMap<String, MeshId>,
    cursor_grabbed: bool,
    fixed: FixedStep,
    last_frame: Option<Instant>,
    client_tick: u64,
    /// Каталог предметов (цвета/эффекты) — тот же, что у сервера.
    catalog: ItemCatalog,
    /// Предметы на земле: снапшоты сервера (offline — локальная симуляция).
    world_loot: Vec<GroundItemNet>,
    /// Трупы: снапшоты сервера (offline — локальная симуляция).
    world_corpses: Vec<CorpseNet>,
    /// Болты в полёте: снапшоты сервера (offline — локальная симуляция);
    /// позиции интерполируются между кадрами.
    world_bolts: Vec<BoltView>,
    /// Проявленные болтом аномалии: индекс → до какого `Instant` яркие.
    revealed: HashMap<u32, Instant>,
    /// Вспышки в точках срабатывания болтов.
    anomaly_flashes: Vec<AnomalyFlash>,
    /// Полупрозрачный материал аномалий (blend); появляется с renderer.
    anomaly_material: Option<MaterialId>,
    /// Инвентарь локального игрока из персональных апдейтов.
    inventory: Vec<Option<ItemStackNet>>,
    /// Открытый контейнер обыска; `None` — панель закрыта.
    open_container: Option<OpenContainer>,
    /// Панель инвентаря открыта (клавиша I).
    inventory_open: bool,
    /// Фокус панелей при нескольких открытых (←/→).
    panel_focus: PanelFocus,
    /// Выбранная ячейка инвентаря (сетка 8×5).
    inventory_select: usize,
    /// Выбранная строка открытого контейнера.
    container_select: usize,
    /// Полоска лечения локального игрока (старт — событие UseStarted).
    healing: Option<HealingView>,
    /// Цель в прицеле для подсказки взаимодействия (косметика).
    interact_hint: Option<HintKind>,
    /// Монотонный счётчик действий — дедупликация повторов на сервере.
    action_seq: u64,
    /// Мир локаций (M4d): сцены по id; пусто в одиночном режиме.
    scenes: HashMap<LocationId, Scene>,
    /// Имена локаций мира (`locations.ron`); пусто в одиночном режиме.
    location_names: HashMap<LocationId, String>,
    /// Текущая локация игрока (0 в одиночном режиме).
    location: LocationId,
    /// Активные пузыри текущей локации (якоря из снапшота).
    world_bubbles: Vec<Vec3>,
    /// Вспышка смены локации: белый экран гаснет до этого мгновения.
    tp_flash: Option<Instant>,
    /// Содержимое склада базы (последний `StashUpdate`).
    stash_items: Vec<ItemStackNet>,
    /// Панель склада открыта (клавиша H, только на базе).
    stash_open: bool,
    /// Выбранная строка склада.
    stash_select: usize,
}

impl ClientApp {
    /// Новая offline-сессия клиента: уровень с диска (или demo_zone-fallback),
    /// симуляция с коллайдерами сцены, фоновый загрузчик ассетов.
    pub fn with_content(content_root: impl Into<PathBuf>, level: &str) -> Self {
        Self::new_session(content_root, Some(level), None)
    }

    /// Сессия клиента с подключением к серверу (`--server host:port`); движение
    /// до/без сервера — локальное предсказание. `level` — одиночная сцена
    /// (`--level`); без него загружается мир локаций и его хэш.
    pub fn with_server(
        server: impl Into<String>,
        content_root: impl Into<PathBuf>,
        level: Option<&str>,
    ) -> Self {
        Self::new_session(content_root, level, Some(server.into()))
    }

    fn new_session(
        content_root: impl Into<PathBuf>,
        level: Option<&str>,
        server: Option<String>,
    ) -> Self {
        let content_root = content_root.into();
        let catalog = load_catalog(&content_root);
        let items_hash = catalog.content_hash();

        // Одиночная сцена (`--level`) или мир локаций: мир — когда уровня нет
        // (обязан сходиться по `world_content_hash` с сервером).
        let mut scenes: HashMap<LocationId, Scene> = HashMap::new();
        let mut location_names: HashMap<LocationId, String> = HashMap::new();
        let (scene, content_hash) = match level {
            Some(level) => {
                let (scene, _) = load_level(&content_root, level);
                log::info!("уровень '{}': {} объектов", scene.name, scene.len());
                let hash = scene.content_hash();
                (scene, hash)
            }
            None => {
                let (registry, world) = load_world(&content_root);
                for (id, scene) in &world {
                    scenes.insert(*id, scene.clone());
                }
                for info in &registry.locations {
                    location_names.insert(info.id, info.name.clone());
                }
                let base_id = registry.base_id().unwrap_or(LOCATION_BASE);
                let scene = world
                    .get(&base_id)
                    .cloned()
                    .unwrap_or_else(Scene::demo_zone);
                let mut ids: Vec<LocationId> = world.keys().copied().collect();
                ids.sort_unstable();
                let refs: Vec<(LocationId, &Scene)> =
                    ids.iter().map(|id| (*id, &world[id])).collect();
                let hash = zn_scene::world_content_hash(&registry, &refs);
                log::info!(
                    "мир локаций: {} локаций, старт на '{}' ({} объектов)",
                    world.len(),
                    scene.name,
                    scene.len()
                );
                (scene, hash)
            }
        };

        let mut simulation = Simulation::with_scene_and_catalog(&scene, catalog.clone());
        let player_id = simulation.spawn_player(scene.spawn);
        let camera = Camera::new(
            scene.spawn + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0),
            scene.spawn_yaw,
            16.0 / 9.0,
        );

        let net = server.and_then(|addr| {
            match NetClient::connect(&addr, content_hash, items_hash, CLIENT_NAME) {
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

        // Внешние ассеты — из активной сцены и всех локаций мира.
        let mut assets = HashMap::new();
        let loader = AssetLoader::new(&content_root);
        for object in scenes
            .values()
            .flat_map(|scene| scene.objects.iter())
            .chain(scene.objects.iter())
        {
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

        let location = location_names
            .keys()
            .copied()
            .min()
            .unwrap_or(LOCATION_BASE);

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
            scene_materials: HashMap::new(),
            tracers: Vec::new(),
            flashes: Vec::new(),
            next_local_fire_tick: 0,
            egui_ctx: egui::Context::default(),
            ui_renderer: None,
            started: Instant::now(),
            last_server_tick: 0,
            my_respawn_tick: None,
            my_killer: None,
            kill_feed: Vec::new(),
            post_hint: None,
            post_settings: PostSettings::default(),
            audio: AudioEngine::new(),
            builtin_mesh_ids: HashMap::new(),
            cursor_grabbed: false,
            fixed: FixedStep::from_hz(60),
            last_frame: None,
            client_tick: 0,
            catalog,
            world_loot: Vec::new(),
            world_corpses: Vec::new(),
            world_bolts: Vec::new(),
            revealed: HashMap::new(),
            anomaly_flashes: Vec::new(),
            anomaly_material: None,
            inventory: Vec::new(),
            open_container: None,
            inventory_open: false,
            panel_focus: PanelFocus::Container,
            inventory_select: 0,
            container_select: 0,
            healing: None,
            interact_hint: None,
            action_seq: 0,
            scenes,
            location_names,
            location,
            world_bubbles: Vec::new(),
            tp_flash: None,
            stash_items: Vec::new(),
            stash_open: false,
            stash_select: 0,
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

    /// Имя локации для логов и титула; пусто — локации нет в реестре (одиночный
    /// режим без имён).
    fn location_name(&self, location: LocationId) -> &str {
        self.location_names
            .get(&location)
            .map_or("", String::as_str)
    }

    /// Склад доступен только в сети и только на базе.
    fn stash_available(&self) -> bool {
        self.net.is_some() && self.location == LOCATION_BASE
    }

    /// Открытые панели в порядке фокуса слева направо.
    fn open_panels(&self) -> Vec<PanelFocus> {
        let mut panels = Vec::with_capacity(3);
        if self.open_container.is_some() {
            panels.push(PanelFocus::Container);
        }
        if self.stash_open && self.stash_available() {
            panels.push(PanelFocus::Stash);
        }
        if self.inventory_open {
            panels.push(PanelFocus::Inventory);
        }
        panels
    }

    /// Переход в локацию мира (Welcome или `LocationChanged`): сцена и коллайдеры
    /// меняются, локальный игрок переносится в точку прибытия, экран вспыхивает.
    /// `None` в `yaw` — поворот спавна сцены.
    fn enter_location(&mut self, location: LocationId, position: [f32; 3], yaw: Option<f32>) {
        let Some(scene) = self.scenes.get(&location).cloned() else {
            log::warn!("локация {location} отсутствует в загруженном мире — остаёмся");
            return;
        };
        let position = Vec3::from_array(position);
        self.scene = scene;
        self.simulation = Simulation::with_scene_and_catalog(&self.scene, self.catalog.clone());
        self.player_id = self.simulation.spawn_player(position);
        // Поворот прибытия: спавн смотрит по +X сцены — ставим целевой yaw.
        if let Some(mut state) = self
            .simulation
            .player_state(self.player_id)
            .map(|state| state.net_state())
        {
            state.yaw = yaw.unwrap_or(self.scene.spawn_yaw);
            let _ = self.simulation.restore_player(self.player_id, &state);
        }
        self.history = InputHistory::new();
        self.smoother = ErrorSmoother::default();
        self.remote.clear();
        self.world_loot.clear();
        self.world_corpses.clear();
        self.world_bolts.clear();
        self.world_bubbles.clear();
        self.revealed.clear();
        self.anomaly_flashes.clear();
        self.open_container = None;
        self.location = location;
        self.tp_flash = Some(Instant::now() + TP_FLASH_TTL);
        if !self.stash_available() {
            self.stash_open = false;
        }
        self.sync_camera_from_simulation();
        log::info!(
            "локация '{}' (id {location}): {} объектов",
            self.scene.name,
            self.scene.len()
        );
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
            // Меш нужен, если он встречается в текущей сцене или любой локации
            // мира — при смене локации builtin-кэш должен быть готов.
            let wanted = format!("builtin://{name}");
            let present = self
                .scene
                .objects
                .iter()
                .chain(self.scenes.values().flat_map(|scene| scene.objects.iter()))
                .any(|object| object.asset_key() == wanted);
            if !present {
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

        // Сфера нужна даже без сферических объектов сцены: ею рисуются
        // аномалии (M4c).
        if !self.builtin_mesh_ids.contains_key("sphere") {
            if let Some(mesh) = zn_assets::builtin_mesh("sphere") {
                let id = renderer.register_mesh(&mesh);
                self.builtin_mesh_ids.insert("sphere".to_string(), id);
            }
        }

        // Материал аномалий: альфа-блендинг, прозрачность — из instance-цвета.
        if self.anomaly_material.is_none() {
            let material = zn_assets::CpuMaterial {
                name: Some("anomaly".to_string()),
                alpha_mode: zn_assets::AlphaMode::Blend,
                double_sided: true,
                ..zn_assets::CpuMaterial::default()
            };
            let id = renderer.register_material(&material, &[]);
            self.blend_materials.insert(id);
            self.anomaly_material = Some(id);
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

        // Персональные материалы объектов сцены (M4g) — после ассетов.
        self.ensure_scene_materials();
    }

    /// Регистрирует персональные материалы объектов сцены (M4g): текстура,
    /// тайлинг, факторы. Материалы кэшируются по спеке — одинаковые объекты
    /// рисуются одним материалом и батчатся.
    fn ensure_scene_materials(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        for object in &self.scene.objects {
            let Some(spec) = MaterialSpec::from_object(object) else {
                continue;
            };
            if self.scene_materials.contains_key(&spec) {
                continue;
            }
            let (material, images) = spec.to_material();
            let refs: Vec<Option<&zn_assets::CpuImage>> =
                images.iter().map(|image| image.as_ref()).collect();
            let id = renderer.register_material(&material, &refs);
            self.scene_materials.insert(spec, id);
        }
    }

    /// Пары (меш, материал) для draw list; незагруженное → builtin fallback.
    /// Приоритет материала: персональный (M4g) → имя объекта → индекс материала
    /// примитива (glTF) → первый материал ассета → дефолтный.
    fn resolve_asset(&self, object: &SceneObject) -> Vec<(MeshId, MaterialId)> {
        let Some(renderer) = self.renderer.as_ref() else {
            return vec![(0, 0)];
        };
        let cube = renderer.builtin_cube_mesh();
        let default = renderer.default_material();
        // Персональный материал объекта (M4g) перекрывает материалы ассета.
        let override_material = MaterialSpec::from_object(object)
            .and_then(|spec| self.scene_materials.get(&spec).copied());
        let key = object.asset_key();

        if let Some(name) = zn_assets::parse_builtin_key(key) {
            let material = override_material.unwrap_or(default);
            return vec![if name == "cube" {
                (cube, material)
            } else {
                self.builtin_mesh_ids
                    .get(name)
                    .map_or((cube, material), |id| (*id, material))
            }];
        }

        match self.assets.get(key) {
            Some(AssetState::Loaded {
                meshes,
                mesh_materials,
                materials,
            }) => {
                if meshes.is_empty() {
                    return vec![(cube, override_material.unwrap_or(default))];
                }
                // Имя материала объекта — для всех примитивов сразу.
                if let Some(name) = object.material.as_deref() {
                    if let Some((_, id)) = materials
                        .iter()
                        .find(|(material_name, _)| material_name.as_deref() == Some(name))
                    {
                        let material = override_material.unwrap_or(*id);
                        return meshes.iter().map(|mesh| (*mesh, material)).collect();
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
                        (*mesh, override_material.unwrap_or(material))
                    })
                    .collect()
            }
            _ => vec![(cube, override_material.unwrap_or(default))],
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

        // Без GPU (например, в тестах) — только статика сцены.
        let Some(renderer) = self.renderer.as_ref() else {
            return list;
        };
        // До регистрации капсулы fallback — куб.
        let capsule = self
            .builtin_mesh_ids
            .get("capsule")
            .copied()
            .unwrap_or_else(|| renderer.builtin_cube_mesh());
        let cube = renderer.builtin_cube_mesh();
        let material = renderer.default_material();

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
            if sample.health == 0 {
                // Мёртвые не рисуются до респавна.
                continue;
            }
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

        // Лут и трупы мира: снапшот сервера (offline — локальная симуляция).
        for item in &self.world_loot {
            let [x, y, z] = dequantize_pos(item.pos);
            let color = self
                .catalog
                .get(item.item)
                .map_or(DEFAULT_ITEM_COLOR, ground_item_color);
            let model = Mat4::from_scale_rotation_translation(
                Vec3::splat(GROUND_ITEM_SIZE),
                Quat::IDENTITY,
                Vec3::new(x, y + GROUND_ITEM_SIZE * 0.5, z),
            );
            list.push(
                cube,
                material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: color.to_array(),
                },
            );
        }
        for corpse in &self.world_corpses {
            let [x, y, z] = dequantize_pos(corpse.pos);
            let model = Mat4::from_scale_rotation_translation(
                CORPSE_SIZE,
                Quat::from_rotation_y(dequantize_yaw(corpse.yaw)),
                Vec3::new(x, y + CORPSE_SIZE.y * 0.5, z),
            );
            list.push(
                cube,
                material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: player_color(corpse.owner).into(),
                },
            );
        }

        // Эффекты: трейсеры, вспышки выстрелов, аномалии и болты — живут доли
        // секунд (кроме подсветки аномалий); просроченные отбрасываются.
        let now = Instant::now();
        self.tracers.retain(|tracer| tracer.until > now);
        self.flashes.retain(|flash| flash.until > now);
        self.anomaly_flashes.retain(|flash| flash.until > now);
        self.revealed.retain(|_, until| *until > now);

        // Аномалии (M4c): еле заметные мерцающие сферы; проявленные болтом —
        // яркие на `ANOMALY_REVEAL_TTL`.
        let sphere = self.builtin_mesh_ids.get("sphere").copied().unwrap_or(cube);
        let anomaly_material = self.anomaly_material.unwrap_or(material);
        let time = self.started.elapsed().as_secs_f32();
        for (index, anomaly) in self.scene.anomalies.iter().enumerate() {
            let rgb = match anomaly.kind {
                AnomalyKind::Heat => ANOMALY_HEAT_COLOR,
                AnomalyKind::Gravity => ANOMALY_GRAVITY_COLOR,
            };
            let alpha = if self
                .revealed
                .get(&(index as u32))
                .is_some_and(|until| *until > now)
            {
                ANOMALY_REVEAL_ALPHA
            } else {
                let phase = index as f32 * 1.7;
                let flicker = 0.5 + 0.5 * (time * ANOMALY_FLICKER_SPEED + phase).sin();
                ANOMALY_BASE_ALPHA + ANOMALY_FLICKER_ALPHA * flicker
            };
            let model = Mat4::from_scale_rotation_translation(
                Vec3::splat(anomaly.radius * 2.0),
                Quat::IDENTITY,
                anomaly.center(),
            );
            list.push(
                sphere,
                anomaly_material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    // HDR (M4g-G3): проявленная аномалия ярче единицы — bloom
                    // подсвечивает её, базовая мгла (низкая альфа) — нет.
                    color: [
                        rgb[0] * ANOMALY_HDR_GAIN,
                        rgb[1] * ANOMALY_HDR_GAIN,
                        rgb[2] * ANOMALY_HDR_GAIN,
                        alpha,
                    ],
                },
            );
        }

        // Вспышки проявления аномалий: яркий шар в точке срабатывания болта.
        for flash in &self.anomaly_flashes {
            let model = Mat4::from_scale_rotation_translation(
                Vec3::splat(ANOMALY_FLASH_SIZE),
                Quat::IDENTITY,
                flash.at,
            );
            list.push(
                sphere,
                anomaly_material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    // Вспышка — тоже HDR (M4g-G3): короткий яркий импульс.
                    color: [
                        ANOMALY_FLASH_COLOR.x * ANOMALY_HDR_GAIN,
                        ANOMALY_FLASH_COLOR.y * ANOMALY_HDR_GAIN,
                        ANOMALY_FLASH_COLOR.z * ANOMALY_HDR_GAIN,
                        ANOMALY_FLASH_COLOR.w,
                    ],
                },
            );
        }

        // Активные пузыри локации (M4d): полупрозрачные сферы у пола; набор
        // приходит снапшотом и меняется при использовании/респавне. Радиус —
        // у кандидата сцены, если якорь с ним совпал.
        for anchor in &self.world_bubbles {
            let radius = self
                .scene
                .bubbles
                .iter()
                .find(|bubble| (bubble.position - *anchor).length_squared() < 1e-4)
                .map_or(BUBBLE_DEFAULT_RADIUS, |bubble| bubble.radius);
            let model = Mat4::from_scale_rotation_translation(
                Vec3::splat(radius * 2.0),
                Quat::IDENTITY,
                *anchor + Vec3::Y * radius,
            );
            list.push(
                sphere,
                anomaly_material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: [
                        BUBBLE_COLOR[0],
                        BUBBLE_COLOR[1],
                        BUBBLE_COLOR[2],
                        BUBBLE_ALPHA,
                    ],
                },
            );
        }

        // Болты в полёте (M4c): палочка по направлению движения, позиция —
        // интерполяция между снапшотами (offline — из локальной симуляции).
        for view in &self.world_bolts {
            let model = Mat4::from_scale_rotation_translation(
                Vec3::new(BOLT_THICKNESS, BOLT_LENGTH, BOLT_THICKNESS),
                Quat::from_rotation_arc(Vec3::Y, view.direction()),
                view.sample(render_tick),
            );
            list.push(
                cube,
                material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: BOLT_COLOR.to_array(),
                },
            );
        }

        // Трейсеры (растянутый по направлению куб) и вспышки у дула.
        for tracer in &self.tracers {
            let delta = tracer.to - tracer.from;
            let length = delta.length();
            if length < 1e-4 {
                continue;
            }
            let model = Mat4::from_scale_rotation_translation(
                Vec3::new(TRACER_THICKNESS, length, TRACER_THICKNESS),
                Quat::from_rotation_arc(Vec3::Y, delta / length),
                (tracer.from + tracer.to) * 0.5,
            );
            list.push(
                cube,
                material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: TRACER_COLOR.to_array(),
                },
            );
        }
        for flash in &self.flashes {
            let model = Mat4::from_scale_rotation_translation(
                Vec3::splat(FLASH_SIZE),
                Quat::IDENTITY,
                flash.at,
            );
            list.push(
                cube,
                material,
                InstanceRaw {
                    model: model.to_cols_array(),
                    color: FLASH_COLOR.to_array(),
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
        let now = Instant::now();

        for _ in 0..steps {
            let (dx, dy) = self.input.take_mouse_delta();
            let (yaw_delta, pitch_delta) = look_delta(dx, dy);

            let [right_axis, forward_axis] =
                self.input
                    .move_axis(KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD);
            let sprinting = self.input.is_key_down(KeyCode::ShiftLeft);
            // Огонь — только с захваченным курсором и живым игроком.
            let firing = self.cursor_grabbed
                && self.input.is_button_down(MouseButton::Left)
                && self
                    .simulation
                    .player_state(self.player_id)
                    .is_none_or(|state| state.health > 0);
            let mut buttons = 0;
            if sprinting {
                buttons |= FLAG_SPRINTING;
            }
            if firing {
                buttons |= FLAG_FIRE;
            }

            let command = ClientInput {
                tick: self.client_tick,
                move_forward: forward_axis,
                move_right: right_axis,
                yaw_delta,
                pitch_delta,
                jump: self.input.just_pressed(KeyCode::Space),
                buttons,
            };
            self.history.push(command);
            self.simulation.queue_input(self.player_id, command);
            self.simulation.step_once();

            // Локальный отклик своего выстрела (чужой приходит событием Shot).
            if firing && self.client_tick >= self.next_local_fire_tick {
                self.next_local_fire_tick = self.client_tick + FIRE_INTERVAL_TICKS;
                self.spawn_local_shot(now);
            }
            self.client_tick += 1;
        }

        // Локальное предсказание не репликует события боя: урон и события —
        // авторитет сервера; очередь очищаем, чтобы она не росла. Исключение —
        // offline: старт лечения приходит из локальной симуляции.
        let local_events = self.simulation.take_events();

        if steps > 0 {
            self.sync_camera_from_simulation();
            self.handle_action_keys();
            self.input.end_frame();
            if let Some(net) = self.net.as_mut() {
                // Последние INPUT_REDUNDANCY инпутов: потеря датаграммы не
                // должна превращаться в потерю движения.
                net.send_inputs(&self.history.last_n(INPUT_REDUNDANCY));
            }
        }

        if self.net.is_none() {
            // Offline: истина — локальная симуляция (лут, трупы, болты, апдейты).
            for event in &local_events {
                match event {
                    GameEvent::UseStarted { player, ticks } if *player == self.player_id => {
                        self.start_healing_view(*ticks, now);
                        self.audio.play(SoundId::Heal);
                    }
                    GameEvent::AnomalyRevealed { anomaly, position } => {
                        self.reveal_anomaly(*anomaly, *position, now);
                        self.audio.play(SoundId::BoltAnomaly);
                    }
                    _ => {}
                }
            }
            self.world_loot = self.simulation.snapshot_loot();
            self.world_corpses = self.simulation.snapshot_corpses();
            self.world_bolts = self
                .simulation
                .snapshot_bolts()
                .into_iter()
                .map(|bolt| {
                    // Offline-болты обновляются каждый тик — интерполяция не нужна.
                    let position = Vec3::from_array(dequantize_pos(bolt.pos));
                    BoltView {
                        id: bolt.id,
                        prev: position,
                        pos: position,
                        prev_tick: self.client_tick,
                        tick: self.client_tick,
                    }
                })
                .collect();
            self.apply_net_updates();
        } else {
            // Online: локальные апдейты предсказания не нужны — истина в сети.
            let _ = self.simulation.take_net_updates();
        }
        self.update_interact_hint();
    }

    /// Локальный отклик своего выстрела: трейсер до первой стены и вспышка.
    fn spawn_local_shot(&mut self, now: Instant) {
        let dir = self.camera.forward();
        let start = self.camera.position + dir * MUZZLE_OFFSET;
        let end = self
            .simulation
            .cast_ray(start, dir, RIFLE_RANGE)
            .unwrap_or(start + dir * RIFLE_RANGE);
        self.flashes.push(MuzzleFlash {
            at: start,
            until: now + FLASH_TTL,
        });
        self.tracers.push(Tracer {
            from: start,
            to: end,
            until: now + TRACER_TTL,
        });
        self.audio.play(SoundId::Shot);
    }

    /// События боя из снапшота: чужие выстрелы (трейсер и вспышка) и смерти
    /// (kill-фид и экран смерти). Свои выстрелы уже показаны локальным
    /// откликом — событие своего shot пропускается.
    fn handle_combat_events(&mut self, events: &[GameEvent], now: Instant) {
        for event in events {
            match event {
                GameEvent::Shot {
                    shooter,
                    origin,
                    dir,
                } => {
                    if Some(*shooter) == self.net_player_id {
                        continue;
                    }
                    let origin = Vec3::from_array(*origin);
                    let Some(dir) = Vec3::from_array(*dir).try_normalize() else {
                        continue;
                    };
                    let end = self
                        .simulation
                        .cast_ray(origin, dir, RIFLE_RANGE)
                        .unwrap_or(origin + dir * RIFLE_RANGE);
                    self.flashes.push(MuzzleFlash {
                        at: origin + dir * MUZZLE_OFFSET,
                        until: now + FLASH_TTL,
                    });
                    self.tracers.push(Tracer {
                        from: origin,
                        to: end,
                        until: now + TRACER_TTL,
                    });
                    self.audio.play(SoundId::Shot);
                }
                GameEvent::Death {
                    victim,
                    victim_name,
                    killer,
                    killer_name,
                } => {
                    let victim_text = side_name(*victim, victim_name, self.net_player_id);
                    let killer_text = side_name(*killer, killer_name, self.net_player_id);
                    self.kill_feed.push(KillFeedEntry {
                        text: format!("{killer_text} → {victim_text}"),
                        until: now + KILL_FEED_TTL,
                    });
                    while self.kill_feed.len() > KILL_FEED_MAX {
                        self.kill_feed.remove(0);
                    }
                    if Some(*victim) == self.net_player_id {
                        self.my_killer = Some(killer_text);
                    }
                    self.audio.play(SoundId::Death);
                }
                GameEvent::UseStarted { player, ticks } => {
                    if Some(*player) == self.net_player_id {
                        self.start_healing_view(*ticks, now);
                        self.audio.play(SoundId::Heal);
                    }
                }
                // Болт сработал в аномалии (M4c): яркая подсветка и вспышка.
                GameEvent::AnomalyRevealed { anomaly, position } => {
                    self.reveal_anomaly(*anomaly, *position, now);
                    self.audio.play(SoundId::BoltAnomaly);
                }
            }
        }
    }

    /// Проявление аномалии болтом: индекс аномалии и точка вспышки (место
    /// срабатывания); видят все — событие приходит всем клиентам.
    fn reveal_anomaly(&mut self, anomaly: u32, position: [f32; 3], now: Instant) {
        self.revealed.insert(anomaly, now + ANOMALY_REVEAL_TTL);
        self.anomaly_flashes.push(AnomalyFlash {
            at: Vec3::from_array(position),
            until: now + ANOMALY_FLASH_TTL,
        });
    }

    /// Действия вне инпутов: E (обыск/подбор/закрыть), G (бросок болта),
    /// I (инвентарь), H (склад на базе), T (взять всё), Q (выбросить),
    /// Enter (взять/использовать/сдать), стрелки (навигация), F6–F8
    /// (постобработка), F9/F10 (звук). Панели passive — курсор остаётся в игре.
    fn handle_action_keys(&mut self) {
        if self.input.just_pressed(KeyCode::KeyE) {
            self.send_action(PlayerAction::Interact);
            self.audio.play(SoundId::Panel);
        }
        if self.input.just_pressed(KeyCode::KeyG) {
            self.send_action(PlayerAction::ThrowBolt);
            self.audio.play(SoundId::Throw);
        }
        if self.input.just_pressed(KeyCode::KeyI) {
            self.inventory_open = !self.inventory_open;
            self.audio.play(SoundId::Panel);
        }
        if self.input.just_pressed(KeyCode::KeyH) && self.stash_available() {
            self.stash_open = !self.stash_open;
            if self.stash_open {
                self.panel_focus = PanelFocus::Stash;
            }
            self.audio.play(SoundId::Panel);
        }
        if self.input.just_pressed(KeyCode::KeyT) {
            if let Some(container) = self.open_container.as_ref().map(|open| open.container) {
                self.send_action(PlayerAction::Inventory(InventoryAction::TakeAll {
                    container,
                }));
                self.audio.play(SoundId::Pickup);
            }
        }
        if self.input.just_pressed(KeyCode::KeyQ) {
            if let Some(slot) = self.selected_inventory_slot() {
                self.send_action(PlayerAction::Inventory(InventoryAction::Drop { slot }));
                self.audio.play(SoundId::Pickup);
            }
        }
        if self.input.just_pressed(KeyCode::Enter) {
            self.press_enter();
        }
        self.handle_post_keys();
        self.handle_audio_keys();
        self.handle_panel_navigation();
    }

    /// Настройки постобработки (M4g-G3): F6 — виньетка вкл/выкл, F7 — цикл
    /// интенсивности bloom, F8 — цикл порога. Клиент держит значения у себя
    /// (работает и без рендера), рендер получает их при создании и правке.
    fn handle_post_keys(&mut self) {
        let mut changed = false;
        if self.input.just_pressed(KeyCode::F6) {
            self.post_settings.vignette = if self.post_settings.vignette > 0.0 {
                0.0
            } else {
                VIGNETTE_ON
            };
            changed = true;
        }
        if self.input.just_pressed(KeyCode::F7) {
            self.post_settings.bloom_intensity =
                cycle_value(self.post_settings.bloom_intensity, &BLOOM_INTENSITY_STEPS);
            changed = true;
        }
        if self.input.just_pressed(KeyCode::F8) {
            self.post_settings.bloom_threshold =
                cycle_value(self.post_settings.bloom_threshold, &BLOOM_THRESHOLD_STEPS);
            changed = true;
        }
        if !changed {
            return;
        }
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_post_settings(self.post_settings);
        }
        let vignette = if self.post_settings.vignette > 0.0 {
            "вкл"
        } else {
            "выкл"
        };
        self.post_hint = Some(PostHint {
            text: format!(
                "виньетка {vignette} (F6) · bloom {:.2} (F7) · порог {:.2} (F8)",
                self.post_settings.bloom_intensity, self.post_settings.bloom_threshold
            ),
            until: Instant::now() + POST_HINT_TTL,
        });
    }

    /// Настройки аудио (A1): F9 — цикл громкости, F10 — мьют. Клиент держит
    /// значения у себя (движок звука общий для сетевой и offline-сессии).
    fn handle_audio_keys(&mut self) {
        let mut changed = false;
        if self.input.just_pressed(KeyCode::F9) {
            self.audio.cycle_volume();
            changed = true;
        }
        if self.input.just_pressed(KeyCode::F10) {
            self.audio.set_muted(!self.audio.muted());
            changed = true;
        }
        if !changed {
            return;
        }
        let sound = if self.audio.muted() {
            "выкл".to_string()
        } else {
            format!("{:.0}%", self.audio.volume() * 100.0)
        };
        self.post_hint = Some(PostHint {
            text: format!("звук {sound} (F9/F10)"),
            until: Instant::now() + POST_HINT_TTL,
        });
    }

    /// Отправляет действие: серверу (с повтором `INPUT_REDUNDANCY` раз) либо в
    /// очередь локальной симуляции (offline). `tick` растёт монотонно — сервер
    /// дедуплицирует повторы.
    fn send_action(&mut self, action: PlayerAction) {
        self.action_seq += 1;
        let tick = self.action_seq;
        match self.net.as_mut() {
            Some(net) => match action {
                PlayerAction::Interact => net.send_interact(tick),
                PlayerAction::Inventory(inventory) => net.send_inventory_action(tick, inventory),
                PlayerAction::ThrowBolt => net.send_throw_bolt(tick),
            },
            None => self.simulation.queue_action(self.player_id, action),
        }
    }

    /// Отправляет действие склада: серверу (с повтором `INPUT_REDUNDANCY` раз).
    /// Offline склад недоступен (`stash_available` гейтит панель).
    fn send_stash_action(&mut self, action: StashAction) {
        self.action_seq += 1;
        let tick = self.action_seq;
        if let Some(net) = self.net.as_mut() {
            net.send_stash_action(tick, action);
        }
    }

    /// Enter: взять выбранную строку контейнера (фокус на контейнере), взять
    /// стопку со склада или сдать туда ячейку инвентаря (склад открыт),
    /// иначе — использовать предмет инвентаря.
    fn press_enter(&mut self) {
        match self.effective_focus() {
            Some(PanelFocus::Container) => {
                let Some(open) = self.open_container.as_ref() else {
                    return;
                };
                if self.container_select >= open.items.len() {
                    return;
                }
                let container = open.container;
                let index = self.container_select as u16;
                self.send_action(PlayerAction::Inventory(InventoryAction::TakeFrom {
                    container,
                    index,
                }));
            }
            Some(PanelFocus::Stash) => {
                if self.stash_select >= self.stash_items.len() {
                    return;
                }
                self.send_stash_action(StashAction::Take {
                    index: self.stash_select as u16,
                });
            }
            Some(PanelFocus::Inventory) => {
                let slot = self.inventory_select.min(INVENTORY_SLOTS - 1);
                // Открытый склад превращает Enter в «сдать ячейку».
                if self.stash_open && self.stash_available() {
                    let occupied = self
                        .inventory
                        .get(slot)
                        .is_some_and(|stack| stack.is_some());
                    if occupied {
                        self.send_stash_action(StashAction::Put { slot: slot as u8 });
                    }
                    return;
                }
                let usable = self
                    .inventory
                    .get(slot)
                    .and_then(|stack| *stack)
                    .is_some_and(|stack| {
                        self.catalog
                            .get(stack.item)
                            .is_some_and(|item| item.use_effect.is_some())
                    });
                if usable {
                    self.send_action(PlayerAction::Inventory(InventoryAction::Use {
                        slot: slot as u8,
                    }));
                }
            }
            None => {}
        }
    }

    /// Слот инвентаря под выделением с предметом (для Q).
    fn selected_inventory_slot(&self) -> Option<u8> {
        if !self.inventory_open {
            return None;
        }
        let slot = self.inventory_select.min(INVENTORY_SLOTS - 1);
        let occupied = self
            .inventory
            .get(slot)
            .is_some_and(|stack| stack.is_some());
        occupied.then_some(slot as u8)
    }

    /// Эффективный фокус панелей: одна открыта — она; несколько — выбранный
    /// ←/→ (если фокус указывает на закрытую — первая открытая).
    fn effective_focus(&self) -> Option<PanelFocus> {
        let panels = self.open_panels();
        match panels.len() {
            0 => None,
            1 => Some(panels[0]),
            _ => Some(if panels.contains(&self.panel_focus) {
                self.panel_focus
            } else {
                panels[0]
            }),
        }
    }

    /// Стрелки: ↑/↓ — выделение внутри панели фокуса; ←/→ при нескольких
    /// открытых панелях — переключение фокуса (слева направо: контейнер,
    /// склад, инвентарь), иначе ←/→ двигают по сетке инвентаря.
    fn handle_panel_navigation(&mut self) {
        let up = self.input.just_pressed(KeyCode::ArrowUp);
        let down = self.input.just_pressed(KeyCode::ArrowDown);
        let left = self.input.just_pressed(KeyCode::ArrowLeft);
        let right = self.input.just_pressed(KeyCode::ArrowRight);
        if !(up || down || left || right) {
            return;
        }

        let panels = self.open_panels();
        let multiple = panels.len() > 1;
        if multiple && (left || right) {
            let current = panels
                .iter()
                .position(|panel| *panel == self.effective_focus().unwrap_or(panels[0]))
                .unwrap_or(0);
            let next = if left {
                current.saturating_sub(1)
            } else {
                (current + 1).min(panels.len() - 1)
            };
            self.panel_focus = panels[next];
            return;
        }

        match self.effective_focus() {
            Some(PanelFocus::Container) => {
                let rows = self
                    .open_container
                    .as_ref()
                    .map_or(0, |open| open.items.len());
                if up {
                    self.container_select = self.container_select.saturating_sub(1);
                }
                if down && rows > 0 && self.container_select + 1 < rows {
                    self.container_select += 1;
                }
            }
            Some(PanelFocus::Stash) => {
                let rows = self.stash_items.len();
                if up {
                    self.stash_select = self.stash_select.saturating_sub(1);
                }
                if down && rows > 0 && self.stash_select + 1 < rows {
                    self.stash_select += 1;
                }
            }
            Some(PanelFocus::Inventory) => {
                let mut col = self.inventory_select % INVENTORY_COLS;
                let mut row = self.inventory_select / INVENTORY_COLS;
                if up {
                    row = row.saturating_sub(1);
                } else if down {
                    row = (row + 1).min(INVENTORY_ROWS - 1);
                } else if left && !multiple {
                    col = col.saturating_sub(1);
                } else if right && !multiple {
                    col = (col + 1).min(INVENTORY_COLS - 1);
                }
                self.inventory_select = row * INVENTORY_COLS + col;
            }
            None => {}
        }
    }

    /// Панель обыска: `container = 0` — сервер закрыл контейнер.
    fn apply_container_update(&mut self, container: u64, items: Vec<ItemStackNet>) {
        if container == 0 {
            self.open_container = None;
            return;
        }
        let switched = self
            .open_container
            .as_ref()
            .is_none_or(|open| open.container != container);
        if switched {
            self.container_select = 0;
            self.panel_focus = PanelFocus::Container;
        } else {
            // После изъятия строк выделение может выйти за список.
            self.container_select = self.container_select.min(items.len().saturating_sub(1));
        }
        self.open_container = Some(OpenContainer { container, items });
    }

    /// Персональные апдейты локальной симуляции (offline): инвентарь и
    /// контейнер — как если бы пришли от сервера.
    fn apply_net_updates(&mut self) {
        let updates = self.simulation.take_net_updates();
        for (id, slots) in updates.inventory {
            if id == self.player_id {
                self.inventory = slots;
            }
        }
        for (id, update) in updates.containers {
            if id == self.player_id {
                self.apply_container_update(update.container, update.items);
            }
        }
    }

    /// Старт полоски лечения: длительность приходит в тиках.
    fn start_healing_view(&mut self, ticks: u16, now: Instant) {
        let total_secs = f32::from(ticks) / SIMULATION_HZ as f32;
        self.healing = Some(HealingView {
            until: now + Duration::from_secs_f32(total_secs),
            total_secs,
        });
    }

    /// Подсказка взаимодействия по камере: ящики сцены, трупы и предметы на
    /// земле. Косметика — истина серверная.
    fn update_interact_hint(&mut self) {
        let mut containers: Vec<Vec3> = self
            .scene
            .objects
            .iter()
            .filter(|object| object.loot.is_some())
            .map(|object| object.position)
            .collect();
        containers.extend(self.world_corpses.iter().map(|corpse| {
            let [x, y, z] = dequantize_pos(corpse.pos);
            Vec3::new(x, y, z)
        }));
        let items: Vec<Vec3> = self
            .world_loot
            .iter()
            .map(|item| {
                let [x, y, z] = dequantize_pos(item.pos);
                Vec3::new(x, y, z)
            })
            .collect();
        self.interact_hint = pick_interact_hint(
            self.camera.position,
            self.camera.forward(),
            &containers,
            &items,
        );
    }

    /// Стек для панелей HUD: имя/цвет из каталога; неизвестный предмет
    /// (каталог разошёлся с сервером) в панели не показывается.
    fn hud_stack(&self, stack: ItemStackNet) -> Option<HudItemStack> {
        let definition = self.catalog.get(stack.item)?;
        Some(HudItemStack {
            name: definition.display_name.clone(),
            count: stack.count,
            color: definition.color,
        })
    }

    /// Стек для панелей со списком сервера (контейнер, склад): неизвестный
    /// каталогу предмет остаётся строкой-заглушкой, чтобы индексы строк
    /// совпадали с серверными.
    fn hud_stack_or_placeholder(&self, stack: ItemStackNet) -> HudItemStack {
        self.hud_stack(stack).unwrap_or(HudItemStack {
            name: format!("предмет #{}", stack.item),
            count: stack.count,
            color: [0.5, 0.5, 0.5],
        })
    }

    /// Суммарный вес инвентаря, кг (неизвестные предметы не считаются).
    fn inventory_weight_kg(&self) -> f32 {
        self.inventory
            .iter()
            .flatten()
            .filter_map(|stack| {
                self.catalog
                    .get(stack.item)
                    .map(|item| item.weight_kg * f32::from(stack.count))
            })
            .sum()
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
                    location,
                } => {
                    log::info!(
                        "сервер принял игрока: net id {player_id}, tick {server_tick}, \
                         локация {location}, спавн {spawn:?}"
                    );
                    self.net_player_id = Some(player_id);
                    if !self.scenes.is_empty() && location != self.location {
                        // Мир: Welcome уточняет стартовую локацию (обычно база).
                        self.enter_location(location, spawn, None);
                    }
                }
                NetEvent::Reject(reason) => {
                    log::warn!("сервер отклонил подключение: {reason:?} — offline-режим");
                    self.drop_connection();
                }
                NetEvent::Snapshot {
                    server_tick,
                    acked_input_tick,
                    players,
                    loot,
                    corpses,
                    bolts,
                    bubbles,
                    events,
                } => {
                    // Мировые списки лута/трупов — прямое замещение снапшотом,
                    // болты — с позициями прошлого кадра (для интерполяции).
                    self.world_loot = loot;
                    self.world_corpses = corpses;
                    self.apply_snapshot_bolts(&bolts, server_tick);
                    self.apply_snapshot_bubbles(&bubbles);
                    self.apply_snapshot(server_tick, acked_input_tick, &players, &events, now);
                }
                NetEvent::InventoryUpdate { slots } => self.inventory = slots,
                NetEvent::ContainerUpdate { container, items } => {
                    self.apply_container_update(container, items);
                }
                NetEvent::LocationChanged {
                    location,
                    position,
                    yaw,
                } => {
                    self.enter_location(location, position, Some(yaw));
                    self.audio.play(SoundId::Transition);
                }
                NetEvent::StashUpdate { items } => {
                    self.stash_items = items;
                    self.stash_select = self
                        .stash_select
                        .min(self.stash_items.len().saturating_sub(1));
                }
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
        // Сетевая истина лута теряется — offline продолжится с локальной.
        self.world_loot.clear();
        self.world_corpses.clear();
        self.world_bolts.clear();
        self.world_bubbles.clear();
        self.revealed.clear();
        self.anomaly_flashes.clear();
        self.inventory.clear();
        self.open_container = None;
        // Склад существует только на сервере.
        self.stash_items.clear();
        self.stash_open = false;
    }

    /// Болты из снапшота: список замещается, но позиции обновляются с
    /// сохранением прошлого кадра — рендер интерполируется между снапшотами.
    fn apply_snapshot_bolts(&mut self, bolts: &[BoltNet], server_tick: u64) {
        self.world_bolts
            .retain(|view| bolts.iter().any(|bolt| bolt.id == view.id));
        for bolt in bolts {
            let position = Vec3::from_array(dequantize_pos(bolt.pos));
            match self.world_bolts.iter_mut().find(|view| view.id == bolt.id) {
                Some(view) => {
                    if view.pos != position {
                        view.prev = view.pos;
                        view.prev_tick = view.tick;
                        view.pos = position;
                        view.tick = server_tick;
                    }
                }
                None => self.world_bolts.push(BoltView {
                    id: bolt.id,
                    prev: position,
                    pos: position,
                    prev_tick: server_tick,
                    tick: server_tick,
                }),
            }
        }
    }

    /// Активные пузыри локации из снапшота: якоря на полу (квантованные).
    fn apply_snapshot_bubbles(&mut self, bubbles: &[[i16; 3]]) {
        self.world_bubbles = bubbles
            .iter()
            .map(|anchor| Vec3::from_array(dequantize_pos(*anchor)))
            .collect();
    }

    /// Снапшот сервера: события боя, буферы удалённых игроков, время рендера
    /// интерполяции и reconciliation локального предсказания.
    fn apply_snapshot(
        &mut self,
        server_tick: u64,
        acked_input_tick: u64,
        players: &[PlayerNetState],
        events: &[GameEvent],
        now: Instant,
    ) {
        self.handle_combat_events(events, now);
        self.last_server_tick = server_tick;
        // Переход жив↔мёртв локального игрока: серверный дедлайн респавна
        // для экрана смерти; при возрождении сбрасывается.
        match players
            .iter()
            .find(|state| Some(state.player_id) == self.net_player_id)
            .map(|state| state.health)
        {
            Some(0) => {
                self.my_respawn_tick
                    .get_or_insert(server_tick + RESPAWN_DELAY_TICKS);
                // Смерть отменяет лечение (расход не начисляется).
                self.healing = None;
            }
            Some(_) => self.my_respawn_tick = None,
            None => {}
        }

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

    /// Срез состояния HUD: HP, экран смерти, kill-фид. Собирается до
    /// заимствования рендерера; здесь же чистятся угасшие строки фида.
    fn collect_hud_data(&mut self) -> HudData {
        let health = self
            .simulation
            .player_state(self.player_id)
            .map(|state| state.health)
            .unwrap_or(MAX_HEALTH);
        if health > 0 {
            // Живой: экран смерти и его отсчёт сбрасываются.
            self.my_killer = None;
            self.my_respawn_tick = None;
        }
        let respawn_secs = self
            .my_respawn_tick
            .map(|tick| tick.saturating_sub(self.last_server_tick) as f32 / SIMULATION_HZ as f32);

        let now = Instant::now();
        if self.healing.as_ref().is_some_and(|heal| heal.until <= now) {
            self.healing = None;
        }
        self.kill_feed.retain(|entry| entry.until > now);
        if self
            .post_hint
            .as_ref()
            .is_some_and(|hint| hint.until <= now)
        {
            self.post_hint = None;
        }
        let feed = self
            .kill_feed
            .iter()
            .map(|entry| {
                let left = entry.until.saturating_duration_since(now).as_secs_f32();
                (
                    entry.text.clone(),
                    (left / KILL_FEED_TTL.as_secs_f32()).clamp(0.0, 1.0),
                )
            })
            .collect();

        let healing = self.healing.as_ref().map(|heal| {
            let left = heal.until.saturating_duration_since(now).as_secs_f32();
            (left / heal.total_secs).clamp(0.0, 1.0)
        });

        let hint = self.interact_hint.map(|kind| match kind {
            HintKind::Container => "[E] Обыскать".to_string(),
            HintKind::Item => "[E] Подобрать".to_string(),
        });

        let inventory = self.inventory_open.then(|| {
            let select = self.inventory_select.min(INVENTORY_SLOTS - 1);
            let slots = (0..INVENTORY_SLOTS)
                .map(|slot| {
                    self.inventory
                        .get(slot)
                        .copied()
                        .flatten()
                        .and_then(|stack| self.hud_stack(stack))
                })
                .collect();
            let selected = self
                .inventory
                .get(select)
                .copied()
                .flatten()
                .and_then(|stack| self.hud_stack(stack));
            InventoryHud {
                slots,
                select,
                weight_kg: self.inventory_weight_kg(),
                max_weight_kg: MAX_CARRY_WEIGHT_KG,
                selected,
                focused: self.effective_focus() == Some(PanelFocus::Inventory),
            }
        });

        let container = self.open_container.as_ref().map(|open| {
            let title = self
                .world_corpses
                .iter()
                .find(|corpse| corpse.container == open.container)
                .map_or_else(
                    || "Ящик".to_string(),
                    |corpse| {
                        if corpse.name.is_empty() {
                            "Тело".to_string()
                        } else {
                            format!("Тело: {}", corpse.name)
                        }
                    },
                );
            ContainerHud {
                title,
                // Индексы строк — индексы стека на сервере: неизвестный
                // предмет остаётся строкой-заглушкой (взять его всё равно
                // можно, название подтянет каталог).
                items: open
                    .items
                    .iter()
                    .map(|stack| self.hud_stack_or_placeholder(*stack))
                    .collect(),
                select: self
                    .container_select
                    .min(open.items.len().saturating_sub(1)),
                focused: self.effective_focus() == Some(PanelFocus::Container),
                hint: None,
            }
        });

        let stash = (self.stash_open && self.stash_available()).then(|| ContainerHud {
            title: "Склад базы".to_string(),
            items: self
                .stash_items
                .iter()
                .map(|stack| self.hud_stack_or_placeholder(*stack))
                .collect(),
            select: self
                .stash_select
                .min(self.stash_items.len().saturating_sub(1)),
            focused: self.effective_focus() == Some(PanelFocus::Stash),
            hint: Some("↑/↓ — выбор · Enter — взять (инвентарь: сдать) · H — закрыть".to_string()),
        });

        // Вспышка смены локации гаснет линейно.
        if self.tp_flash.is_some_and(|until| until <= now) {
            self.tp_flash = None;
        }
        let flash = self.tp_flash.map(|until| {
            (until.saturating_duration_since(now).as_secs_f32() / TP_FLASH_TTL.as_secs_f32())
                .clamp(0.0, 1.0)
        });

        HudData {
            health,
            max_health: MAX_HEALTH,
            killer: if health == 0 {
                Some(
                    self.my_killer
                        .clone()
                        .unwrap_or_else(|| "неизвестно".to_string()),
                )
            } else {
                None
            },
            respawn_secs: if health == 0 { respawn_secs } else { None },
            feed,
            post_hint: self.post_hint.as_ref().map(|hint| hint.text.clone()),
            time: self.started.elapsed().as_secs_f64(),
            hint,
            inventory,
            container,
            stash,
            healing,
            flash,
        }
    }

    /// Заголовок окна: локация мира (если есть) и режим связи
    /// (`ZONA — База · ping 42 ms`). Обновляется только при изменении, чтобы
    /// не дёргать ОС каждый кадр.
    fn update_window_title(&mut self) {
        let location = self.location_name(self.location);
        let mut base = String::from("ZONA");
        let separator = if location.is_empty() {
            " — "
        } else {
            base.push_str(" — ");
            base.push_str(location);
            " · "
        };
        let status = match self.net.as_ref() {
            Some(net) => match net.status() {
                NetStatus::Connected => match net.rtt_ms() {
                    Some(rtt) => format!("ping {rtt:.0} ms"),
                    None => "в сети".to_string(),
                },
                NetStatus::Connecting => "подключение…".to_string(),
                NetStatus::Rejected(_) => "offline".to_string(),
            },
            None if self.connection_lost => "нет связи".to_string(),
            None => "offline".to_string(),
        };
        let title = format!("{base}{separator}{status}");
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
        // HUD: egui-wgpu поверх сцены (msaa 1 — кадр без мультисэмплинга,
        // depth не нужен: проход оверлея идёт без depth attach).
        if let Some(renderer) = self.renderer.as_mut() {
            // Настройки постобработки (M4g-G3), накопленные клиентом.
            renderer.set_post_settings(self.post_settings);
            self.egui_ctx
                .set_pixels_per_point(renderer.window().scale_factor() as f32);
            self.ui_renderer = Some(egui_wgpu::Renderer::new(
                renderer.device(),
                renderer.surface_format(),
                egui_wgpu::RendererOptions {
                    msaa_samples: 1,
                    ..Default::default()
                },
            ));
        }
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
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: ElementState::Pressed,
                ..
            } if !self.cursor_grabbed => {
                // Клик вне захвата — снова захватить курсор; этот клик огня не
                // даёт (в инпут не попадает).
                self.grab_cursor(true);
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
                let hud = self.collect_hud_data();
                if let Some(renderer) = self.renderer.as_mut() {
                    // Атмосфера локации (M4g): свет, туман и небо каждый кадр —
                    // смена локации подхватывается сразу.
                    renderer.set_atmosphere(&self.scene.environment);
                    // Охват теневой карты — по AABB текущей локации.
                    let (bounds_min, bounds_max) = self.scene.bounds();
                    renderer.set_shadow_bounds(bounds_min, bounds_max);
                    // Ресурсы всех батчей помечаем использованными до кулинга:
                    // оффскрин-объекты не должны вытеснять видимые из LRU.
                    renderer.touch_scene(&list);
                    let visible = visible_instances(&list.batches, &self.camera, |mesh| {
                        renderer
                            .mesh_bounds(mesh)
                            .unwrap_or(zn_assets::Bounds::EMPTY)
                    });
                    renderer.update_draw_list(&DrawList { batches: visible });
                    if let Some(ui_renderer) = self.ui_renderer.as_mut() {
                        // HUD отдельным проходом поверх сцены (Load, без depth).
                        render_hud(renderer, ui_renderer, &self.egui_ctx, &self.camera, &hud);
                    } else {
                        renderer.render(&self.camera);
                    }
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

/// HUD поверх кадра: пассивный egui — RawInput без событий, весь ввод
/// остаётся игре; `render_with` открывает проход оверлея (Load, без depth).
fn render_hud(
    renderer: &mut Renderer,
    ui_renderer: &mut egui_wgpu::Renderer,
    ctx: &egui::Context,
    camera: &Camera,
    hud: &HudData,
) {
    let size = renderer.surface_size();
    let ppp = ctx.pixels_per_point();
    let logical = egui::vec2(size.width as f32, size.height as f32) / ppp;
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, logical)),
        time: Some(hud.time),
        ..Default::default()
    };

    ctx.begin_pass(raw_input);
    draw_hud(ctx, hud);
    let mut output = ctx.end_pass();
    // Текстурные дельты egui (атлас шрифта) обязан потребить вызывающий.
    let mut textures = std::mem::take(&mut output.textures_delta);
    let clipped = ctx.tessellate(output.shapes, ppp);
    let screen_descriptor = ScreenDescriptor {
        size_in_pixels: [size.width, size.height],
        pixels_per_point: ppp,
    };

    for (id, deltas) in textures.set.drain() {
        for delta in deltas {
            ui_renderer.update_texture(renderer.device(), renderer.queue(), id, &delta);
        }
    }
    for id in textures.free.drain() {
        ui_renderer.free_texture(&id);
    }

    let device = renderer.device().clone();
    let queue = renderer.queue().clone();
    // Аплоад буферов egui отдельным энкодером до кадра: рендер использует
    // буферы уже этого кадра.
    let mut upload = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("zn-client-ui-upload"),
    });
    let user_buffers =
        ui_renderer.update_buffers(&device, &queue, &mut upload, &clipped, &screen_descriptor);
    queue.submit(user_buffers.into_iter().chain([upload.finish()]));
    renderer.render_with(camera, |pass| {
        let mut pass = pass.forget_lifetime();
        ui_renderer.render(&mut pass, &clipped, &screen_descriptor);
    });
}

/// Разметка HUD: прицел, HP, kill-фид, экран смерти (пассивные примитивы).
fn draw_hud(ctx: &egui::Context, hud: &HudData) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("hud"),
    ));
    let screen = ctx.viewport_rect();

    // Прицел — только живому.
    if hud.health > 0 {
        let center = screen.center();
        let stroke = (1.5, egui::Color32::from_white_alpha(220));
        let (gap, len) = (4.0, 10.0);
        for (from, to) in [
            (
                center - egui::vec2(gap + len, 0.0),
                center - egui::vec2(gap, 0.0),
            ),
            (
                center + egui::vec2(gap, 0.0),
                center + egui::vec2(gap + len, 0.0),
            ),
            (
                center - egui::vec2(0.0, gap + len),
                center - egui::vec2(0.0, gap),
            ),
            (
                center + egui::vec2(0.0, gap),
                center + egui::vec2(0.0, gap + len),
            ),
        ] {
            painter.line_segment([from, to], stroke);
        }

        // Подсказка взаимодействия — под прицелом.
        if let Some(hint) = &hud.hint {
            painter.text(
                center + egui::vec2(0.0, 34.0),
                egui::Align2::CENTER_CENTER,
                hint,
                egui::FontId::proportional(16.0),
                egui::Color32::from_white_alpha(220),
            );
        }
    }

    // Kill-фид — правый верхний угол, свежие строки ярче.
    let mut y = screen.top() + 12.0;
    for (text, alpha) in &hud.feed {
        let color = egui::Color32::from_rgba_unmultiplied(255, 235, 210, (alpha * 230.0) as u8);
        painter.text(
            egui::pos2(screen.right() - 14.0, y),
            egui::Align2::RIGHT_TOP,
            text,
            egui::FontId::proportional(15.0),
            color,
        );
        y += 20.0;
    }

    // Подсказка настроек постобработки (M4g-G3) — верхний левый угол.
    if let Some(hint) = &hud.post_hint {
        painter.text(
            egui::pos2(screen.left() + 18.0, screen.top() + 14.0),
            egui::Align2::LEFT_TOP,
            hint,
            egui::FontId::proportional(14.0),
            egui::Color32::from_white_alpha(210),
        );
    }

    // HP — левый нижний угол: полоска и число.
    let bar_pos = egui::pos2(screen.left() + 18.0, screen.bottom() - 38.0);
    let bar_size = egui::vec2(220.0, 12.0);
    let fraction = hud.health as f32 / hud.max_health.max(1) as f32;
    painter.rect_filled(
        egui::Rect::from_min_size(bar_pos, bar_size),
        2.0,
        egui::Color32::from_black_alpha(150),
    );
    let hp_color = if fraction > 0.5 {
        egui::Color32::from_rgb(120, 200, 90)
    } else if fraction > 0.25 {
        egui::Color32::from_rgb(220, 180, 70)
    } else {
        egui::Color32::from_rgb(210, 80, 70)
    };
    painter.rect_filled(
        egui::Rect::from_min_size(bar_pos, egui::vec2(bar_size.x * fraction, bar_size.y)),
        2.0,
        hp_color,
    );
    painter.text(
        egui::pos2(bar_pos.x, bar_pos.y - 4.0),
        egui::Align2::LEFT_BOTTOM,
        format!("HP {}/{}", hud.health, hud.max_health),
        egui::FontId::proportional(16.0),
        egui::Color32::from_white_alpha(230),
    );

    // Полоска лечения — над HP, пока идёт эффект (доля оставшегося времени).
    if let Some(fraction) = hud.healing {
        let heal_pos = egui::pos2(bar_pos.x, bar_pos.y - 34.0);
        let heal_size = egui::vec2(bar_size.x, 8.0);
        painter.rect_filled(
            egui::Rect::from_min_size(heal_pos, heal_size),
            2.0,
            egui::Color32::from_black_alpha(140),
        );
        painter.rect_filled(
            egui::Rect::from_min_size(heal_pos, egui::vec2(heal_size.x * fraction, heal_size.y)),
            2.0,
            egui::Color32::from_rgb(92, 192, 138),
        );
    }

    // Панели (M4b/M4d): инвентарь справа снизу; левее — обыск и склад
    // (слева направо: склад, обыск, инвентарь).
    let inventory_rect = hud.inventory.as_ref().map(|inventory| {
        let size = inventory_panel_size();
        let min = egui::pos2(
            screen.right() - 14.0 - size.x,
            screen.bottom() - 14.0 - size.y,
        );
        let rect = egui::Rect::from_min_size(min, size);
        draw_inventory_panel(&painter, inventory, rect);
        rect
    });
    let container_rect = hud.container.as_ref().map(|container| {
        let size = list_panel_size(container.items.len(), container.hint.is_some());
        let right = inventory_rect.map_or(screen.right() - 14.0, |rect| rect.left() - 12.0);
        let min = egui::pos2(right - size.x, screen.bottom() - 14.0 - size.y);
        let rect = egui::Rect::from_min_size(min, size);
        draw_container_panel(&painter, container, rect);
        rect
    });
    if let Some(stash) = &hud.stash {
        let size = list_panel_size(stash.items.len(), stash.hint.is_some());
        let right = container_rect
            .or(inventory_rect)
            .map_or(screen.right() - 14.0, |rect| rect.left() - 12.0);
        let min = egui::pos2(right - size.x, screen.bottom() - 14.0 - size.y);
        draw_container_panel(&painter, stash, egui::Rect::from_min_size(min, size));
    }

    // Вспышка смены локации — поверх панелей, под экраном смерти.
    if let Some(flash) = hud.flash {
        painter.rect_filled(
            screen,
            0.0,
            egui::Color32::from_white_alpha((flash.clamp(0.0, 1.0) * 230.0) as u8),
        );
    }

    // Экран смерти.
    if hud.health == 0 {
        let center = screen.center();
        painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(165));
        painter.text(
            center - egui::vec2(0.0, 36.0),
            egui::Align2::CENTER_CENTER,
            "ВЫ УБИТЫ",
            egui::FontId::proportional(44.0),
            egui::Color32::from_rgb(214, 78, 66),
        );
        painter.text(
            center + egui::vec2(0.0, 4.0),
            egui::Align2::CENTER_CENTER,
            format!("убийца: {}", hud.killer.as_deref().unwrap_or("неизвестно")),
            egui::FontId::proportional(20.0),
            egui::Color32::from_white_alpha(235),
        );
        let respawn_text = match hud.respawn_secs {
            Some(secs) => format!("возрождение через {:.1} с", secs.max(0.0)),
            None => "возрождение…".to_string(),
        };
        painter.text(
            center + egui::vec2(0.0, 36.0),
            egui::Align2::CENTER_CENTER,
            respawn_text,
            egui::FontId::proportional(18.0),
            egui::Color32::from_white_alpha(200),
        );
        painter.text(
            center + egui::vec2(0.0, 72.0),
            egui::Align2::CENTER_CENTER,
            "мышь — осмотреться",
            egui::FontId::proportional(14.0),
            egui::Color32::from_white_alpha(140),
        );
    }
}

/// Размер сетки инвентаря HUD (8×5).
fn grid_size() -> egui::Vec2 {
    egui::vec2(
        INVENTORY_COLS as f32 * HUD_CELL + (INVENTORY_COLS - 1) as f32 * HUD_CELL_GAP,
        INVENTORY_ROWS as f32 * HUD_CELL + (INVENTORY_ROWS - 1) as f32 * HUD_CELL_GAP,
    )
}

/// Размер панели инвентаря: заголовок, сетка, вес и подсказки клавиш.
fn inventory_panel_size() -> egui::Vec2 {
    let grid = grid_size();
    egui::vec2(grid.x + 20.0, 34.0 + grid.y + 66.0)
}

/// Размер панели-списка (обыск, склад): заголовок, строки и опциональная
/// подсказка клавиш внизу.
fn list_panel_size(rows: usize, hint: bool) -> egui::Vec2 {
    egui::vec2(
        280.0,
        46.0 + rows.max(1) as f32 * 24.0 + if hint { 20.0 } else { 0.0 },
    )
}

/// Цвет предмета из каталога для egui.
fn item_color(color: [f32; 3]) -> egui::Color32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0) as u8;
    egui::Color32::from_rgb(channel(color[0]), channel(color[1]), channel(color[2]))
}

/// Рамка панели: фокусная панель ярче.
fn panel_frame(painter: &egui::Painter, rect: egui::Rect, focused: bool) {
    let color = if focused {
        egui::Color32::from_white_alpha(170)
    } else {
        egui::Color32::from_white_alpha(60)
    };
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.5, color),
        egui::StrokeKind::Inside,
    );
}

/// Панель инвентаря: сетка 8×5, выделение, название/вес выбранного и
/// подсказки клавиш. Панель passive — ввод не перехватывает.
fn draw_inventory_panel(painter: &egui::Painter, panel: &InventoryHud, rect: egui::Rect) {
    painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(170));
    panel_frame(painter, rect, panel.focused);

    let header = match &panel.selected {
        Some(stack) => format!("{} ×{}", stack.name, stack.count),
        None => "— пусто —".to_string(),
    };
    painter.text(
        egui::pos2(rect.left() + 10.0, rect.top() + 8.0),
        egui::Align2::LEFT_TOP,
        header,
        egui::FontId::proportional(16.0),
        egui::Color32::from_white_alpha(235),
    );

    let grid_top = rect.top() + 34.0;
    for row in 0..INVENTORY_ROWS {
        for col in 0..INVENTORY_COLS {
            let index = row * INVENTORY_COLS + col;
            let min = egui::pos2(
                rect.left() + 10.0 + col as f32 * (HUD_CELL + HUD_CELL_GAP),
                grid_top + row as f32 * (HUD_CELL + HUD_CELL_GAP),
            );
            let cell = egui::Rect::from_min_size(min, egui::vec2(HUD_CELL, HUD_CELL));
            painter.rect_filled(cell, 3.0, egui::Color32::from_white_alpha(18));
            if let Some(Some(stack)) = panel.slots.get(index) {
                painter.rect_filled(cell.shrink(10.0), 3.0, item_color(stack.color));
                painter.text(
                    cell.right_bottom() + egui::vec2(-3.0, -2.0),
                    egui::Align2::RIGHT_BOTTOM,
                    stack.count.to_string(),
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_white_alpha(235),
                );
            }
            if index == panel.select {
                painter.rect_stroke(
                    cell,
                    3.0,
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 220, 120)),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    let weight_y = grid_top + grid_size().y + 8.0;
    painter.text(
        egui::pos2(rect.left() + 10.0, weight_y),
        egui::Align2::LEFT_TOP,
        format!(
            "Вес: {:.1} / {:.0} кг",
            panel.weight_kg, panel.max_weight_kg
        ),
        egui::FontId::proportional(15.0),
        egui::Color32::from_white_alpha(225),
    );
    painter.text(
        egui::pos2(rect.left() + 10.0, weight_y + 24.0),
        egui::Align2::LEFT_TOP,
        "I — закрыть · Q — выбросить · Enter — использовать",
        egui::FontId::proportional(12.0),
        egui::Color32::from_white_alpha(140),
    );
}

/// Панель обыска: заголовок, строки «имя ×count», выделение ↑/↓.
fn draw_container_panel(painter: &egui::Painter, panel: &ContainerHud, rect: egui::Rect) {
    painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(170));
    panel_frame(painter, rect, panel.focused);

    painter.text(
        egui::pos2(rect.left() + 10.0, rect.top() + 8.0),
        egui::Align2::LEFT_TOP,
        &panel.title,
        egui::FontId::proportional(17.0),
        egui::Color32::from_white_alpha(235),
    );

    if let Some(hint) = &panel.hint {
        painter.text(
            egui::pos2(rect.left() + 10.0, rect.bottom() - 20.0),
            egui::Align2::LEFT_TOP,
            hint,
            egui::FontId::proportional(12.0),
            egui::Color32::from_white_alpha(140),
        );
    }

    if panel.items.is_empty() {
        painter.text(
            egui::pos2(rect.left() + 10.0, rect.top() + 38.0),
            egui::Align2::LEFT_TOP,
            "— пусто —",
            egui::FontId::proportional(14.0),
            egui::Color32::from_white_alpha(150),
        );
        return;
    }

    for (index, stack) in panel.items.iter().enumerate() {
        let y = rect.top() + 36.0 + index as f32 * 24.0;
        if index == panel.select {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 6.0, y - 2.0),
                    egui::vec2(rect.width() - 12.0, 22.0),
                ),
                3.0,
                egui::Color32::from_white_alpha(36),
            );
        }
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + 10.0, y + 3.0),
                egui::vec2(12.0, 12.0),
            ),
            2.0,
            item_color(stack.color),
        );
        painter.text(
            egui::pos2(rect.left() + 30.0, y + 1.0),
            egui::Align2::LEFT_TOP,
            format!("{} ×{}", stack.name, stack.count),
            egui::FontId::proportional(15.0),
            egui::Color32::from_white_alpha(230),
        );
    }
}

/// Локальный выбор цели наведения для подсказки: те же правила, что в
/// симуляции (XZ ≤ `INTERACT_RANGE_M`, косинус к прицелу ≥ `INTERACT_MIN_DOT`),
/// выигрывает цель «прицельнее», при равенстве — ближайшая. Подсказка
/// косметическая: истину (что откроет E) решает сервер.
fn pick_interact_hint(
    eye: Vec3,
    aim: Vec3,
    containers: &[Vec3],
    items: &[Vec3],
) -> Option<HintKind> {
    let mut best: Option<(f32, f32, HintKind)> = None;
    let mut consider = |position: Vec3, kind: HintKind| {
        let delta = position - eye;
        let distance = Vec3::new(delta.x, 0.0, delta.z).length();
        if distance > INTERACT_RANGE_M {
            return;
        }
        let dot = if delta.length_squared() < 1e-8 {
            1.0
        } else {
            delta.normalize().dot(aim)
        };
        if dot < INTERACT_MIN_DOT {
            return;
        }
        let better = match best {
            None => true,
            Some((best_dot, best_dist, _)) => {
                dot > best_dot + 1e-6 || ((dot - best_dot).abs() <= 1e-6 && distance < best_dist)
            }
        };
        if better {
            best = Some((dot, distance, kind));
        }
    };
    for position in containers {
        consider(*position, HintKind::Container);
    }
    for position in items {
        consider(*position, HintKind::Item);
    }
    best.map(|(_, _, kind)| kind)
}

/// Имя игрока для килл-фида: «Вы» для себя, иначе имя с сервера или `#id`.
fn side_name(id: u64, name: &str, me: Option<u64>) -> String {
    if Some(id) == me {
        "Вы".to_string()
    } else if name.is_empty() {
        format!("#{id}")
    } else {
        name.to_string()
    }
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
        app.apply_snapshot(100, last_tick, &[me, other], &[], Instant::now());

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
        app.net = Some(
            NetClient::connect(&server.local_addr().unwrap().to_string(), 0, 0, "Тест").unwrap(),
        );
        app.update_window_title();
        assert_eq!(app.window_title, "ZONA — подключение…");
    }

    #[test]
    fn offline_client_fires_with_local_echo() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.cursor_grabbed = true;
        app.input.press_button(MouseButton::Left);
        for _ in 0..30 {
            app.simulate(std::time::Duration::from_millis(17));
        }

        // ~0.5 с удержания при интервале 8 тиков: 2–5 трейсеров, а не каждый кадр.
        assert!(
            (2..=5).contains(&app.tracers.len()),
            "трейсеров: {}",
            app.tracers.len()
        );
        assert_eq!(
            app.flashes.len(),
            app.tracers.len(),
            "вспышка на каждый выстрел"
        );
        // Локальный отклик упирается в стену/пол (демо-уровень тесный).
        let tracer = &app.tracers[0];
        let length = (tracer.to - tracer.from).length();
        assert!(length > 0.0 && length <= RIFLE_RANGE + 1e-3, "{tracer:?}");
    }

    #[test]
    fn shot_events_spawn_tracers_only_for_remote_shooters() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.net_player_id = Some(7);
        let now = Instant::now();

        app.handle_combat_events(
            &[
                GameEvent::Shot {
                    shooter: 8,
                    origin: [0.0, 1.62, 14.0],
                    dir: [1.0, 0.0, 0.0],
                },
                // Свой выстрел уже показан локальным откликом — пропускается.
                GameEvent::Shot {
                    shooter: 7,
                    origin: [0.0, 1.62, 14.0],
                    dir: [1.0, 0.0, 0.0],
                },
                // Мусорное направление не роняет кадр.
                GameEvent::Shot {
                    shooter: 9,
                    origin: [0.0, 1.62, 14.0],
                    dir: [0.0, 0.0, 0.0],
                },
            ],
            now,
        );

        assert_eq!(app.tracers.len(), 1, "только чужой выстрел даёт трейсер");
        assert_eq!(app.flashes.len(), 1);

        // Смерть чужого игрока от моей руки — строка килл-фида.
        app.handle_combat_events(
            &[GameEvent::Death {
                victim: 8,
                victim_name: "Цель".to_string(),
                killer: 7,
                killer_name: "Сталкер".to_string(),
            }],
            now,
        );
        assert_eq!(app.tracers.len(), 1);
        assert_eq!(app.kill_feed.len(), 1);
        assert_eq!(app.kill_feed[0].text, "Вы → Цель");
        assert!(
            app.my_killer.is_none(),
            "чужая смерть не открывает мой экран"
        );
    }

    #[test]
    fn kill_feed_keeps_last_three_deaths() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.net_player_id = Some(7);
        let now = Instant::now();

        for victim in 1..=4u64 {
            app.handle_combat_events(
                &[GameEvent::Death {
                    victim,
                    victim_name: format!("Жертва{victim}"),
                    killer: 7,
                    killer_name: "Сталкер".to_string(),
                }],
                now,
            );
        }

        assert_eq!(app.kill_feed.len(), KILL_FEED_MAX, "в фиде только свежие");
        assert!(app.kill_feed[0].text.ends_with("Жертва2"));
        assert!(app.kill_feed[2].text.ends_with("Жертва4"));
    }

    #[test]
    fn death_screen_data_tracks_local_death() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.net_player_id = Some(7);
        let now = Instant::now();

        app.handle_combat_events(
            &[GameEvent::Death {
                victim: 7,
                victim_name: "Сталкер".to_string(),
                killer: 8,
                killer_name: "Охотник".to_string(),
            }],
            now,
        );
        assert_eq!(app.my_killer.as_deref(), Some("Охотник"));

        // Снапшот со смертью: экран смерти получает отсчёт от серверного тика.
        let mut me = app
            .simulation
            .player_state(app.player_id)
            .unwrap()
            .net_state();
        me.player_id = 7;
        me.health = 0;
        app.apply_snapshot(100, 0, &[me], &[], now);

        let hud = app.collect_hud_data();
        assert_eq!(hud.health, 0);
        assert_eq!(hud.killer.as_deref(), Some("Охотник"));
        let respawn = hud.respawn_secs.expect("отсчёт респавна");
        assert!(respawn > 0.0 && respawn <= 3.0, "respawn = {respawn}");
    }

    #[test]
    fn interact_hint_prefers_aimed_target_within_range() {
        let eye = Vec3::new(0.0, 1.4, 0.0);
        let aim = -Vec3::X;

        // Ящик в конусе на ~1.6 м — цель найдена.
        let crate_pos = Vec3::new(-1.3, 0.4, 1.0);
        assert_eq!(
            pick_interact_hint(eye, aim, &[crate_pos], &[]),
            Some(HintKind::Container)
        );

        // XZ-дистанция больше 2 м — цели нет.
        assert_eq!(
            pick_interact_hint(eye, aim, &[Vec3::new(-2.5, 0.4, 0.0)], &[]),
            None
        );
        // Цель за спиной — нет.
        assert_eq!(
            pick_interact_hint(eye, aim, &[Vec3::new(1.3, 0.4, 0.0)], &[]),
            None
        );

        // Предмет «прицельнее» ящика — подсказка «подобрать».
        assert_eq!(
            pick_interact_hint(eye, aim, &[crate_pos], &[Vec3::new(-1.5, 0.0, 0.0)]),
            Some(HintKind::Item)
        );
    }

    #[test]
    fn use_started_starts_local_healing_progress() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        app.net_player_id = Some(7);
        let now = Instant::now();

        app.handle_combat_events(
            &[GameEvent::UseStarted {
                player: 7,
                ticks: 120,
            }],
            now,
        );
        let healing = app.healing.as_ref().expect("полоска лечения запущена");
        let left = healing.until.saturating_duration_since(now).as_secs_f32();
        assert!((left - 2.0).abs() < 1e-3, "left = {left}");
        // В HUD попадает доля оставшегося времени (почти 1 сразу после старта).
        let fraction = app
            .collect_hud_data()
            .healing
            .expect("полоска лечения в HUD");
        assert!((0.9..=1.0).contains(&fraction), "fraction = {fraction}");

        // Чужое лечение мою полоску не трогает.
        app.healing = None;
        app.handle_combat_events(
            &[GameEvent::UseStarted {
                player: 8,
                ticks: 120,
            }],
            now,
        );
        assert!(app.healing.is_none());

        // Просроченная полоска убирается при сборке HUD.
        app.healing = Some(HealingView {
            until: now - Duration::from_secs(1),
            total_secs: 2.0,
        });
        let _ = app.collect_hud_data();
        assert!(app.healing.is_none(), "просроченная полоска убрана");
    }

    #[test]
    fn panel_keys_toggle_inventory_and_move_selection() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");

        // I открывает панель инвентаря.
        app.input.press_key(KeyCode::KeyI);
        app.simulate(std::time::Duration::from_millis(17));
        assert!(app.inventory_open);
        app.input.release_key(KeyCode::KeyI);

        // ↓ и → двигают выделение по сетке 8×5.
        app.input.press_key(KeyCode::ArrowDown);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::ArrowDown);
        app.input.press_key(KeyCode::ArrowRight);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::ArrowRight);
        assert_eq!(app.inventory_select, INVENTORY_COLS + 1);

        // ↑ возвращает в строку 0, ← — в колонку 0.
        app.input.press_key(KeyCode::ArrowUp);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::ArrowUp);
        assert_eq!(app.inventory_select, 1);
        app.input.press_key(KeyCode::ArrowLeft);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::ArrowLeft);
        assert_eq!(app.inventory_select, 0);

        // Повторный I закрывает панель.
        app.input.press_key(KeyCode::KeyI);
        app.simulate(std::time::Duration::from_millis(17));
        assert!(!app.inventory_open);
    }

    #[test]
    fn offline_interact_opens_crate_and_take_all_fills_inventory() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");
        let ammo = app.catalog.index_of("ammo_545").expect("каталог загружен");

        // E из спавна: взгляд спавна смотрит на ящик перед игроком.
        app.input.press_key(KeyCode::KeyE);
        for _ in 0..8 {
            app.simulate(std::time::Duration::from_millis(17));
        }
        let open = app.open_container.as_ref().expect("ящик открыт");
        assert!(!open.items.is_empty(), "в ящике есть лут");
        assert_eq!(app.interact_hint, Some(HintKind::Container));

        // T — взять всё: патроны переезжают в инвентарь, ящик пустеет.
        app.input.press_key(KeyCode::KeyT);
        for _ in 0..8 {
            app.simulate(std::time::Duration::from_millis(17));
        }
        let open = app
            .open_container
            .as_ref()
            .expect("контейнер остаётся открытым");
        assert!(open.items.is_empty(), "ящик опустел: {:?}", open.items);
        let stacks: Vec<_> = app.inventory.iter().flatten().collect();
        assert!(
            stacks
                .iter()
                .any(|stack| stack.item == ammo && stack.count == 60),
            "патроны в инвентаре: {stacks:?}"
        );
    }

    #[test]
    fn hud_inventory_panel_reports_grid_weight_and_selection() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");
        let medkit = app.catalog.index_of("medkit").expect("каталог загружен");
        let ammo = app.catalog.index_of("ammo_545").unwrap();
        let medkit_name = app.catalog.get(medkit).unwrap().display_name.clone();
        let expected_weight = app.catalog.get(medkit).unwrap().weight_kg * 2.0
            + app.catalog.get(ammo).unwrap().weight_kg * 60.0;

        app.inventory = vec![None; INVENTORY_SLOTS];
        app.inventory[0] = Some(ItemStackNet {
            item: medkit,
            count: 2,
        });
        app.inventory[5] = Some(ItemStackNet {
            item: ammo,
            count: 60,
        });
        app.inventory_open = true;
        app.inventory_select = 5;

        let hud = app.collect_hud_data();
        let panel = hud.inventory.as_ref().expect("панель открыта");
        assert_eq!(panel.slots.len(), INVENTORY_SLOTS);
        assert_eq!(panel.select, 5);
        let slot0 = panel.slots[0].as_ref().expect("аптечка в ячейке 0");
        assert_eq!(slot0.name, medkit_name);
        assert_eq!(slot0.count, 2);
        let selected = panel.selected.as_ref().expect("выделена ячейка 5");
        assert_eq!(selected.count, 60);
        assert!(
            (panel.weight_kg - expected_weight).abs() < 1e-4,
            "вес {} при ожидании {expected_weight}",
            panel.weight_kg
        );
        assert_eq!(panel.max_weight_kg, MAX_CARRY_WEIGHT_KG);
        assert!(panel.focused, "одна панель — фокус на ней");
        assert!(hud.container.is_none());
    }

    #[test]
    fn hud_container_panel_title_for_crate_and_corpse() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");
        let bolt = app.catalog.index_of("bolt").unwrap();

        app.open_container = Some(OpenContainer {
            container: 42,
            items: vec![ItemStackNet {
                item: bolt,
                count: 10,
            }],
        });
        // Ящик без трупа — заголовок «Ящик».
        let hud = app.collect_hud_data();
        let panel = hud.container.as_ref().expect("панель обыска");
        assert_eq!(panel.title, "Ящик");
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].count, 10);
        assert!(panel.focused, "одна панель — фокус на ней");

        // Труп с тем же id контейнера — «Тело: имя».
        app.world_corpses = vec![CorpseNet {
            container: 42,
            owner: 8,
            name: "Жертва".to_string(),
            pos: [0, 0, 0],
            yaw: 0,
        }];
        let hud = app.collect_hud_data();
        assert_eq!(hud.container.as_ref().unwrap().title, "Тело: Жертва");
    }

    #[test]
    fn hud_hint_strings_match_target_kind() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        assert!(app.collect_hud_data().hint.is_none());

        app.interact_hint = Some(HintKind::Container);
        assert_eq!(app.collect_hud_data().hint.as_deref(), Some("[E] Обыскать"));

        app.interact_hint = Some(HintKind::Item);
        assert_eq!(
            app.collect_hud_data().hint.as_deref(),
            Some("[E] Подобрать")
        );
    }

    // ── Аномалии и болты (M4c) ──────────────────────────────────────────

    #[test]
    fn offline_throw_bolt_key_spends_bolt_and_shows_projectile() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");
        let bolt = app.catalog.index_of("bolt").unwrap();
        assert_eq!(app.simulation.debug_give(app.player_id, bolt, 2), 2);

        app.input.press_key(KeyCode::KeyG);
        for _ in 0..6 {
            app.simulate(std::time::Duration::from_millis(17));
        }

        assert_eq!(app.world_bolts.len(), 1, "болт в полёте виден рендеру");
        let state = app.simulation.player_state(app.player_id).unwrap();
        let left: u32 = state
            .inventory
            .iter()
            .flatten()
            .filter(|stack| stack.item == bolt)
            .map(|stack| stack.count)
            .sum();
        assert_eq!(left, 1, "один болт потрачен на бросок");
    }

    #[test]
    fn snapshot_bolts_interpolate_and_expire() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");

        // Квант позиции — 0.1 м: [0,0,0] → 0, [6,0,0] → 0.6 м.
        app.apply_snapshot_bolts(
            &[BoltNet {
                id: 1,
                owner: 2,
                pos: [0, 0, 0],
            }],
            100,
        );
        app.apply_snapshot_bolts(
            &[BoltNet {
                id: 1,
                owner: 2,
                pos: [6, 0, 0],
            }],
            102,
        );
        let view = app.world_bolts[0];
        assert!(
            (view.sample(101).x - 0.3).abs() < 1e-4,
            "середина пути: {:?}",
            view.sample(101)
        );
        assert!(view.sample(100).x.abs() < 1e-4, "старт — в первом кадре");
        assert!(
            (view.sample(102).x - 0.6).abs() < 1e-4,
            "финиш — во втором кадре"
        );

        // Болт исчез из снапшота — исчезает из рендера.
        app.apply_snapshot_bolts(&[], 104);
        assert!(app.world_bolts.is_empty());
    }

    #[test]
    fn anomaly_reveal_event_marks_anomaly_and_flashes() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");
        assert!(
            !app.scene.anomalies.is_empty(),
            "в demo_zone есть аномалии для рендера"
        );

        app.handle_combat_events(
            &[GameEvent::AnomalyRevealed {
                anomaly: 0,
                position: [1.0, 2.0, 3.0],
            }],
            Instant::now(),
        );

        assert!(app.revealed.contains_key(&0), "аномалия подсвечена");
        assert_eq!(app.anomaly_flashes.len(), 1, "вспышка в точке срабатывания");
        assert!(
            (app.anomaly_flashes[0].at - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-6,
            "{:?}",
            app.anomaly_flashes[0].at
        );
    }

    #[test]
    fn offline_bolt_reveals_anomaly() {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        let mut app = ClientApp::with_content(content, "demo_zone");
        let bolt = app.catalog.index_of("bolt").unwrap();

        // Индекс жгучей аномалии у (15, 15) в сцене.
        let index = app
            .scene
            .anomalies
            .iter()
            .position(|anomaly| (anomaly.position - Vec3::new(15.0, 0.0, 15.0)).length() < 0.01)
            .expect("жгучая аномалия у (15, 15)") as u32;

        // Ставим игрока в 4 м от центра с прицелом строго на него.
        let player_id = app.player_id;
        let mut net = app.simulation.player_state(player_id).unwrap().net_state();
        net.position = [11.0, -0.2, 15.0];
        net.yaw = 0.0; // прицел на +X — в центр аномалии
        net.pitch = 0.0;
        assert!(app.simulation.restore_player(player_id, &net));
        assert_eq!(app.simulation.debug_give(player_id, bolt, 1), 1);

        app.input.press_key(KeyCode::KeyG);
        for _ in 0..20 {
            app.simulate(std::time::Duration::from_millis(17));
        }

        assert!(
            app.revealed.contains_key(&index),
            "болт в аномалии проявил её: {:?}",
            app.revealed
        );
        assert!(
            !app.anomaly_flashes.is_empty(),
            "вспышка в месте срабатывания болта"
        );
    }

    // ── Мир локаций (M4d-1D) ────────────────────────────────────────────

    /// Клиент мира: все сцены локаций, старт на базе (без сети — offline-мир).
    fn world_app() -> ClientApp {
        let content = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/content");
        ClientApp::new_session(content, None, None)
    }

    #[test]
    fn world_client_loads_all_locations_and_starts_on_base() {
        let mut app = world_app();
        assert_eq!(app.scenes.len(), 4, "все локации мира загружены");
        assert_eq!(app.location, zn_scene::LOCATION_BASE);
        assert_eq!(app.scene.name, "base");
        assert_eq!(app.location_name(app.location), "База");
        // Коллайдеры базы на месте, игрок — в точке спавна сцены.
        assert!(app.simulation.static_collider_count() > 0);
        let player = app.simulation.player_state(app.player_id).unwrap();
        assert_eq!(player.position, app.scene.spawn);
    }

    #[test]
    fn location_changed_switches_scene_player_and_flash() {
        let mut app = world_app();
        app.enter_location(
            zn_scene::LOCATION_KORDON,
            [0.0, 0.0, 22.0],
            Some(std::f32::consts::PI),
        );

        assert_eq!(app.location, zn_scene::LOCATION_KORDON);
        assert_eq!(app.scene.name, "kordon");
        let player = app.simulation.player_state(app.player_id).unwrap();
        assert!((player.position - Vec3::new(0.0, 0.0, 22.0)).length() < 1e-3);
        assert!(
            (player.yaw - std::f32::consts::PI).abs() < 1e-3,
            "yaw = {}",
            player.yaw
        );
        // Камера синхронизирована с точкой прибытия.
        assert!(
            (app.camera.position - (player.position + Vec3::Y * PLAYER_EYE_HEIGHT)).length() < 1e-3
        );

        // Вспышка перехода живёт в HUD и гаснет со временем.
        let flash = app.collect_hud_data().flash.expect("вспышка перехода");
        assert!((0.5..=1.0).contains(&flash), "flash = {flash}");
        assert!(app.tp_flash.is_some());

        // Симуляция новой локации работает: после шага игрок на полу.
        app.simulate(std::time::Duration::from_millis(17));
        let player = app.simulation.player_state(app.player_id).unwrap();
        assert!(player.position.y.abs() < 1e-2, "{:?}", player.position);
    }

    #[test]
    fn stash_panel_opens_only_on_base_with_connection() {
        let mut app = world_app();
        let server = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        app.net = Some(
            NetClient::connect(&server.local_addr().unwrap().to_string(), 0, 0, "Тест").unwrap(),
        );

        // На базе H открывает панель.
        app.input.press_key(KeyCode::KeyH);
        app.simulate(std::time::Duration::from_millis(17));
        assert!(app.stash_open, "склад открывается клавишей H");
        app.input.release_key(KeyCode::KeyH);

        app.stash_items = vec![ItemStackNet { item: 0, count: 3 }];
        let hud = app.collect_hud_data();
        let stash = hud.stash.as_ref().expect("панель склада в HUD");
        assert_eq!(stash.title, "Склад базы");
        assert_eq!(stash.items.len(), 1);
        assert_eq!(stash.select, 0);
        assert!(stash.hint.is_some(), "подсказка клавиш у склада");

        // Смена локации закрывает склад и убирает панель из HUD.
        app.enter_location(zn_scene::LOCATION_KORDON, [0.0, 0.0, 22.0], None);
        assert!(!app.stash_open);
        assert!(app.collect_hud_data().stash.is_none());

        // Offline склад недоступен.
        app.enter_location(zn_scene::LOCATION_BASE, [0.0, 0.0, 14.0], None);
        app.net = None;
        app.input.press_key(KeyCode::KeyH);
        app.simulate(std::time::Duration::from_millis(17));
        assert!(!app.stash_open, "без сети склад не открывается");
    }

    #[test]
    fn snapshot_bubbles_follow_location_switch() {
        let mut app = world_app();
        app.apply_snapshot_bubbles(&[
            zn_net::quantize_pos([12.0, 0.0, -16.0]),
            zn_net::quantize_pos([18.0, 0.0, 20.0]),
        ]);
        assert_eq!(app.world_bubbles.len(), 2);
        assert!((app.world_bubbles[0] - Vec3::new(12.0, 0.0, -16.0)).length() < 0.1);

        // Смена локации чистит пузыри прежней карты.
        app.enter_location(zn_scene::LOCATION_SVALKA, [0.0, 0.0, 0.0], None);
        assert!(app.world_bubbles.is_empty());
    }

    #[test]
    fn world_window_title_includes_location_name() {
        let mut app = world_app();
        app.update_window_title();
        assert_eq!(app.window_title, "ZONA — База · offline");

        app.enter_location(zn_scene::LOCATION_KORDON, [0.0, 0.0, 22.0], None);
        app.update_window_title();
        assert_eq!(app.window_title, "ZONA — Кордон · offline");
    }

    #[test]
    fn stash_enter_takes_stack_and_inventory_enter_puts_slot() {
        let mut app = world_app();
        let server = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        app.net = Some(
            NetClient::connect(&server.local_addr().unwrap().to_string(), 0, 0, "Тест").unwrap(),
        );

        // Доводим клиента до Connected (Hello → Welcome): без этого действия
        // не уходят на сервер.
        {
            let net = app.net.as_mut().unwrap();
            net.poll(Instant::now());
            let mut buf = [0u8; 2048];
            let mut from = None;
            for _ in 0..1000 {
                if let Ok((len, addr)) = server.recv_from(&mut buf) {
                    let message: zn_net::Message = zn_net::decode_message(&buf[..len]).unwrap();
                    assert!(matches!(message, zn_net::Message::ClientHello { .. }));
                    from = Some(addr);
                    break;
                }
                std::thread::yield_now();
            }
            let from = from.expect("ClientHello дошёл до сервера");
            let welcome = zn_net::encode_message(&zn_net::Message::ServerWelcome {
                player_id: 1,
                server_tick: 10,
                spawn: [0.0, 0.0, 14.0],
                location: 0,
            })
            .unwrap();
            server.send_to(&welcome, from).unwrap();
            let events = net.poll(Instant::now());
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, NetEvent::Welcome { .. })),
                "ожидался Welcome: {events:?}"
            );
        }

        app.stash_open = true;
        app.inventory_open = true; // обе панели открыты — фокус по ←/→
        app.stash_items = vec![
            ItemStackNet { item: 0, count: 3 },
            ItemStackNet { item: 1, count: 7 },
        ];
        app.inventory = vec![None; INVENTORY_SLOTS];

        // Вычитывает действия склада из датаграмм (прочие пакеты пропускает).
        fn drain_stash_actions(server: &std::net::UdpSocket) -> Vec<(u64, StashAction)> {
            let mut actions = Vec::new();
            let mut buf = [0u8; 2048];
            while let Ok((len, _)) = server.recv_from(&mut buf) {
                let message: zn_net::Message = zn_net::decode_message(&buf[..len]).unwrap();
                if let zn_net::Message::StashAction { tick, action } = message {
                    actions.push((tick, action));
                }
            }
            actions
        }

        // Фокус на складе: Enter берёт выделенную строку (с повтором).
        app.panel_focus = PanelFocus::Stash;
        app.stash_select = 1;
        app.press_enter();
        let actions = drain_stash_actions(&server);
        assert_eq!(actions.len(), INPUT_REDUNDANCY, "{actions:?}");
        assert!(
            actions
                .iter()
                .all(|(tick, action)| *tick == 1 && *action == StashAction::Take { index: 1 }),
            "взяли стопку под индексом 1: {actions:?}"
        );

        // Фокус на инвентаре: Enter сдаёт занятую ячейку в склад.
        app.panel_focus = PanelFocus::Inventory;
        app.inventory[4] = Some(ItemStackNet { item: 2, count: 1 });
        app.inventory_select = 4;
        app.press_enter();
        let actions = drain_stash_actions(&server);
        assert!(
            actions
                .iter()
                .all(|(tick, action)| *tick == 2 && *action == StashAction::Put { slot: 4 }),
            "сдали ячейку 4: {actions:?}"
        );
        assert_eq!(actions.len(), INPUT_REDUNDANCY, "{actions:?}");

        // Пустая ячейка — Enter молчит.
        app.inventory_select = 5;
        app.press_enter();
        assert!(drain_stash_actions(&server).is_empty());
    }

    // ── Постобработка (M4g-G3) ──────────────────────────────────────────

    #[test]
    fn cycle_value_walks_steps_and_wraps() {
        let steps = [0.0, 0.25, 0.45];
        assert_eq!(cycle_value(0.0, &steps), 0.25);
        assert_eq!(cycle_value(0.25, &steps), 0.45);
        assert_eq!(cycle_value(0.45, &steps), 0.0, "цикл замкнут");
        // Неизвестное значение — первый шаг цикла.
        assert_eq!(cycle_value(9.0, &steps), 0.0);
    }

    #[test]
    fn post_keys_cycle_settings_and_show_hint() {
        let mut app = ClientApp::with_content("game/content", "demo_zone");
        let defaults = app.post_settings;
        assert!(defaults.vignette > 0.0);
        assert!(BLOOM_INTENSITY_STEPS.contains(&defaults.bloom_intensity));

        // F6 гасит виньетку, подсказка с текущими значениями уходит в HUD.
        app.input.press_key(KeyCode::F6);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::F6);
        assert_eq!(app.post_settings.vignette, 0.0);
        let hud = app.collect_hud_data();
        let hint = hud.post_hint.expect("подсказка настроек показана");
        assert!(hint.contains("виньетка выкл"), "{hint}");

        // Повторный F6 возвращает виньетку.
        app.input.press_key(KeyCode::F6);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::F6);
        assert_eq!(app.post_settings.vignette, VIGNETTE_ON);

        // F7 идёт по кругу интенсивности bloom, F8 — по порогу.
        app.input.press_key(KeyCode::F7);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::F7);
        assert!(BLOOM_INTENSITY_STEPS.contains(&app.post_settings.bloom_intensity));
        assert_ne!(app.post_settings.bloom_intensity, defaults.bloom_intensity);

        app.input.press_key(KeyCode::F8);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::F8);
        assert!(BLOOM_THRESHOLD_STEPS.contains(&app.post_settings.bloom_threshold));
        assert_ne!(app.post_settings.bloom_threshold, defaults.bloom_threshold);
    }

    #[test]
    fn audio_keys_cycle_volume_and_toggle_mute() {
        let mut app = ClientApp::new();
        let volume = app.audio.volume();
        assert_eq!(volume, zn_audio::DEFAULT_VOLUME);
        assert!(!app.audio.muted());

        // F9 идёт по шагам громкости, подсказка уходит в HUD.
        app.input.press_key(KeyCode::F9);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::F9);
        assert_eq!(app.audio.volume(), 0.75);
        let hud = app.collect_hud_data();
        let hint = hud.post_hint.expect("подсказка настроек показана");
        assert!(hint.contains("звук"), "{hint}");

        // F10 — мьют: громкость сохраняется, звук выключен.
        app.input.press_key(KeyCode::F10);
        app.simulate(std::time::Duration::from_millis(17));
        app.input.release_key(KeyCode::F10);
        assert!(app.audio.muted());
        assert_eq!(app.audio.volume(), 0.75, "громкость не сбрасывается");
        let hud = app.collect_hud_data();
        let hint = hud.post_hint.expect("подсказка настроек показана");
        assert!(hint.contains("выкл"), "{hint}");
    }

    #[test]
    fn artifact_ground_color_is_hdr_and_regular_loot_is_not() {
        let artifact = zn_content::Item {
            key: "artifact_heat".to_string(),
            display_name: "Артефакт «Жар»".to_string(),
            weight_kg: 0.5,
            stack_max: 1,
            use_effect: None,
            color: [0.95, 0.72, 0.15],
        };
        let ammo = zn_content::Item {
            key: "ammo_545".to_string(),
            ..artifact.clone()
        };

        let color = ground_item_color(&artifact);
        assert!(
            color.x > 1.0 && color.y > 1.0,
            "артефакт светится: {color:?}"
        );
        assert_eq!(color.w, 1.0, "непрозрачность обычная");
        assert_eq!(
            ground_item_color(&ammo),
            Vec4::new(0.95, 0.72, 0.15, 1.0),
            "обычный лут остаётся в LDR"
        );
    }
}
