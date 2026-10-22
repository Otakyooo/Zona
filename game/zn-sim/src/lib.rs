use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

pub use glam::Vec3;
use zn_content::{Item, ItemCatalog};
use zn_core::{Entity, FixedStep, Schedule, World, SIMULATION_HZ, SIMULATION_STEP};
use zn_net::{
    quantize_pos, quantize_yaw, BoltNet, CorpseNet, GameEvent, GroundItemNet, InventoryAction,
    ItemStackNet,
};
pub use zn_net::{ClientInput, PlayerNetState, FLAG_FIRE, FLAG_GROUNDED, FLAG_SPRINTING};
use zn_physics::{PhysicsWorld, PLAYER_HALF_HEIGHT, PLAYER_RADIUS};
use zn_scene::{AnomalyKind, Scene};

pub const WALK_SPEED: f32 = 5.5;
pub const SPRINT_SPEED: f32 = 8.5;
pub const JUMP_SPEED: f32 = 6.5;
pub const GRAVITY: f32 = 18.0;
pub const MAX_FALL_SPEED: f32 = 45.0;
pub const MAX_PITCH: f32 = 89.0f32.to_radians();

// ── Бой (M4a) ───────────────────────────────────────────────────────────
/// Полное здоровье игрока.
pub const MAX_HEALTH: u16 = 100;
/// Высота глаз над ступнями — точка вылета выстрела.
pub const EYE_HEIGHT: f32 = 1.62;
/// Дальность выстрела винтовки.
pub const RIFLE_RANGE: f32 = 100.0;
/// Границы зон попадания по высоте от ступней (рост игрока 1.8 м): ниже
/// `ZONE_TORSO_MIN` — ноги, до `ZONE_HEAD_MIN` — торс, выше — голова.
pub const ZONE_TORSO_MIN: f32 = 0.8;
pub const ZONE_HEAD_MIN: f32 = 1.5;

/// Урон винтовки в торс.
const RIFLE_DAMAGE: f32 = 25.0;
/// Множители урона головы/ног к базовому.
const HEAD_DAMAGE_MULT: f32 = 2.0;
const LEGS_DAMAGE_MULT: f32 = 0.6;
/// Интервал между выстрелами (60 Гц → ~7.5 выстрелов/с); публичен для
/// локального отклика на клиенте.
pub const FIRE_INTERVAL_TICKS: u64 = 8;
/// Задержка респавна после смерти (3 с); публична для экрана смерти клиента.
pub const RESPAWN_DELAY_TICKS: u64 = 180;

// ── Лут (M4b) ───────────────────────────────────────────────────────────
/// Предельный вес переносимого (кг): сверх лимита предметы не берутся.
pub const MAX_CARRY_WEIGHT_KG: f32 = 50.0;
/// Тот же лимит в граммах — точные сравнения без плавающей точки.
pub const MAX_CARRY_GRAMS: u32 = (MAX_CARRY_WEIGHT_KG * 1000.0) as u32;
/// Ключ патрона винтовки (M4d-3a): выстрел расходует штуку из инвентаря.
/// Нет такого предмета в каталоге — стрельба не ограничена.
pub const RIFLE_AMMO_KEY: &str = "ammo_545";
/// Сетка инвентаря 8×5: ячейка — одна стопка.
pub const INVENTORY_COLS: usize = 8;
pub const INVENTORY_ROWS: usize = 5;
pub const INVENTORY_SLOTS: usize = INVENTORY_COLS * INVENTORY_ROWS;
/// Радиус наведения при обыске/подборе (м, по горизонтали).
pub const INTERACT_RANGE_M: f32 = 2.0;
/// Нокдаун (M4d-3b): смертельный удар с здоровья выше `KNOCKDOWN_HP` валит
/// игрока — здоровье 1, таймер `KNOCKDOWN_DURATION_SECS`; повторный урон в
/// нокдауне убивает, по истечении таймера игрок встаёт с `KNOCKDOWN_HP`.
pub const KNOCKDOWN_HP: u16 = 25;
pub const KNOCKDOWN_DURATION_SECS: f32 = 12.0;
pub const KNOCKDOWN_TICKS: u64 = (KNOCKDOWN_DURATION_SECS * 60.0) as u64;
/// Минимальный косинус угла между прицелом и целью (~53°).
pub const INTERACT_MIN_DOT: f32 = 0.6;
/// На этой дистанции открытый контейнер закрывается сам (м).
pub const CONTAINER_CLOSE_DISTANCE_M: f32 = 3.0;
/// Труп исчезает через 5 минут.
pub const CORPSE_LIFETIME_TICKS: u64 = 5 * 60 * SIMULATION_HZ as u64;
/// Лимит предметов на земле: сверх — гаснут самые старые.
pub const GROUND_ITEMS_MAX: usize = 32;
/// База id контейнеров-трупов (ящики — индекс объекта сцены + 1).
pub const CORPSE_CONTAINER_BASE: u64 = 1 << 62;
/// Максимум действий в очереди одного игрока (защита от переполнения).
pub const ACTION_QUEUE_CAP: usize = 16;

/// Максимум отложенных инпутов на игрока (защита от переполнения при джиттере).
pub const INPUT_QUEUE_CAP: usize = 64;

// ── Аномалии и болты (M4c) ──────────────────────────────────────────────
/// Высота «сердца» над ступнями — точка проверки входа игрока в аномалию.
pub const ANOMALY_HEART_HEIGHT: f32 = 0.9;
/// Период тика урона жгучей аномалии (4 раза в секунду).
pub const ANOMALY_DAMAGE_INTERVAL_TICKS: u64 = 15;
/// Урон жгучей за тик у центра и у границы сферы (линейно по дистанции).
pub const ANOMALY_DAMAGE_CENTER: u16 = 6;
pub const ANOMALY_DAMAGE_EDGE: u16 = 2;
/// Скорость притяжения гравитационной (м/с) по горизонтальной дистанции до
/// оси: у края почти не мешает, в центре — сильнее пешего шага, но спринт
/// уносит ноги.
pub const GRAVITY_PULL_CENTER: f32 = 5.0;
pub const GRAVITY_PULL_EDGE: f32 = 1.5;
/// Ключ предмета «болт» в каталоге.
pub const BOLT_ITEM_KEY: &str = "bolt";
/// Начальная скорость брошенного болта (м/с); падение — общая `GRAVITY`.
pub const BOLT_SPEED: f32 = 18.0;
/// Кулдаун между бросками болтов.
pub const BOLT_THROW_COOLDOWN_TICKS: u64 = 20;
/// Максимум болтов в полёте (бюджет MTU снапшота: 12 × ~18 Б).
pub const BOLTS_MAX: usize = 12;
/// Болт летит не дольше 10 секунд, затем приземляется.
pub const BOLT_LIFETIME_TICKS: u64 = 10 * SIMULATION_HZ as u64;

/// Полуразмер невидимого пола «пустой» сцены (`Simulation::new`); верх — y = 0.
const EMPTY_SCENE_GROUND_HALF_EXTENT: f32 = 256.0;

/// Стопка предметов в инвентаре, контейнере или на земле: `item` — индекс в
/// каталоге (`ItemCatalog`), `count` ≤ `stack_max` предмета.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stack {
    pub item: u16,
    pub count: u32,
}

impl Stack {
    pub fn new(item: u16, count: u32) -> Self {
        Self { item, count }
    }
}

/// Активное использование предмета (лечение): завершится на `until_tick`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Healing {
    /// Ячейка инвентаря с используемым предметом.
    pub slot: u8,
    /// Tick, на котором препарат подействует.
    pub until_tick: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub id: u64,
    /// Положение СТУПНЕЙ игрока (центр капсулы — `PhysicsWorld::character_origin`).
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
    pub sprinting: bool,
    /// Здоровье; 0 — мёртв до респавна.
    pub health: u16,
    /// Удержание огня текущим инпутом (`FLAG_FIRE`); мёртвые не стреляют.
    pub firing: bool,
    /// Tick респавна после смерти; 0 — игрок жив.
    pub respawn_at: u64,
    /// Tick окончания нокдауна (M4d-3b); 0 — игрок не в нокдауне. В нокдауне
    /// игрок жив (health = 1), но не двигается и не стреляет.
    pub downed_until_tick: u64,
    /// Раньше этого tick стрелять нельзя (кулдаун оружия).
    pub next_fire_tick: u64,
    /// Раньше этого tick бросать болт нельзя (кулдаун броска).
    pub next_throw_tick: u64,
    /// Ячейки инвентаря (сетка `INVENTORY_COLS`×`INVENTORY_ROWS`).
    pub inventory: [Option<Stack>; INVENTORY_SLOTS],
    /// Открытый контейнер (ящик/труп) — панель обыска.
    pub open_container: Option<u64>,
    /// Идущее использование предмета (лечение).
    pub healing: Option<Healing>,
}

impl PlayerState {
    pub fn new(id: u64, spawn: Vec3, yaw: f32) -> Self {
        Self {
            id,
            position: spawn,
            velocity: Vec3::ZERO,
            yaw,
            pitch: 0.0,
            grounded: true,
            sprinting: false,
            health: MAX_HEALTH,
            firing: false,
            respawn_at: 0,
            downed_until_tick: 0,
            next_fire_tick: 0,
            next_throw_tick: 0,
            inventory: [None; INVENTORY_SLOTS],
            open_container: None,
            healing: None,
        }
    }

    pub fn net_state(&self) -> PlayerNetState {
        let mut flags = 0;
        if self.grounded {
            flags |= FLAG_GROUNDED;
        }
        if self.sprinting {
            flags |= FLAG_SPRINTING;
        }
        PlayerNetState {
            player_id: self.id,
            position: self.position.to_array(),
            velocity: self.velocity.to_array(),
            yaw: self.yaw,
            pitch: self.pitch,
            health: self.health,
            flags,
        }
    }

    pub fn apply_net_state(&mut self, state: &PlayerNetState) {
        self.position = Vec3::from_array(state.position);
        self.velocity = Vec3::from_array(state.velocity);
        self.yaw = state.yaw;
        self.pitch = state.pitch;
        self.health = state.health;
        self.grounded = state.flags & FLAG_GROUNDED != 0;
        self.sprinting = state.flags & FLAG_SPRINTING != 0;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TickInfo {
    pub dt: f32,
    pub tick: u64,
}

/// Точка респавна уровня (ступни и yaw). Ставится из сцены в `build`,
/// обновляется при `rebuild_static`.
#[derive(Clone, Copy, Debug)]
pub struct SpawnPoint {
    pub position: Vec3,
    pub yaw: f32,
}

/// Очередь игровых событий (выстрелы, смерти, начало лечения) с прошлого
/// снапшота; забирает серверная сессия. Имена в `Death` заполняет сессия —
/// симуляция их не знает.
#[derive(Clone, Debug, Default)]
pub struct CombatEvents(pub Vec<GameEvent>);

/// Каталог предметов уровня. Пустой каталог — ящики сцены не наполняются
/// (симуляциям движения и боя каталог не нужен).
#[derive(Clone, Debug, Default)]
pub struct LootCatalog(pub ItemCatalog);

/// Ящики уровня: содержимое и центр для проверки дистанции. id в сети —
/// `scene_index + 1`; трупы живут отдельным списком (`Corpses`).
#[derive(Clone, Debug, Default)]
pub struct Containers(pub Vec<ContainerState>);

#[derive(Clone, Debug)]
pub struct ContainerState {
    /// Индекс объекта сцены (id контейнера = индекс + 1).
    pub scene_index: u32,
    /// Центр ящика — для проверки дистанции наведения.
    pub position: Vec3,
    pub items: Vec<Stack>,
}

/// Трупы игроков: контейнеры с лутом, живут `CORPSE_LIFETIME_TICKS`.
#[derive(Clone, Debug, Default)]
pub struct Corpses {
    pub entries: Vec<Corpse>,
    next_raw_id: u64,
}

impl Corpses {
    /// Добавляет труп и возвращает его id контейнера.
    fn spawn(&mut self, mut corpse: Corpse) -> u64 {
        self.next_raw_id += 1;
        let id = CORPSE_CONTAINER_BASE + self.next_raw_id;
        corpse.id = id;
        self.entries.push(corpse);
        id
    }
}

#[derive(Clone, Debug)]
pub struct Corpse {
    /// id контейнера (`CORPSE_CONTAINER_BASE` + счётчик).
    pub id: u64,
    pub owner_id: u64,
    /// Ступни на момент смерти (рендер и дистанция обыска).
    pub position: Vec3,
    pub yaw: f32,
    pub items: Vec<Stack>,
    pub despawn_tick: u64,
}

/// Предметы на земле; при переполнении гаснут самые старые.
#[derive(Clone, Debug, Default)]
pub struct GroundItems {
    pub entries: Vec<GroundItem>,
    next_seq: u64,
}

#[derive(Clone, Debug)]
pub struct GroundItem {
    pub stack: Stack,
    pub position: Vec3,
    /// Порядок появления — лимит земли вытесняет старейшие.
    pub spawn_seq: u64,
}

/// Аномалии уровня (M4c): id аномалии — индекс в списке (порядок сцены).
#[derive(Clone, Debug, Default)]
pub struct Anomalies(pub Vec<AnomalyState>);

#[derive(Clone, Copy, Debug)]
pub struct AnomalyState {
    /// Центр сферы (якорь сцены + `(0, radius, 0)`).
    pub center: Vec3,
    pub radius: f32,
    pub kind: AnomalyKind,
}

impl AnomalyState {
    /// Точка внутри сферы аномалии?
    pub fn contains(&self, point: Vec3) -> bool {
        point.distance_squared(self.center) <= self.radius * self.radius
    }
}

/// Летящие болты (M4c): позиции репликуются снапшотами; приземлившиеся
/// болты становятся обычными предметами на земле.
#[derive(Clone, Debug, Default)]
pub struct Bolts {
    pub items: Vec<BoltProjectile>,
    next_id: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct BoltProjectile {
    pub id: u32,
    pub owner: u64,
    pub position: Vec3,
    pub velocity: Vec3,
    pub spawn_tick: u64,
}

impl Bolts {
    /// Добавляет болт в полёт и возвращает его сетевой id.
    fn spawn(&mut self, owner: u64, position: Vec3, velocity: Vec3, tick: u64) -> u32 {
        self.next_id += 1;
        self.items.push(BoltProjectile {
            id: self.next_id,
            owner,
            position,
            velocity,
            spawn_tick: tick,
        });
        self.next_id
    }
}

/// Клиентское действие вне инпутов; сервер уже отфильтровал дубли по tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayerAction {
    /// Обыск (E): открыть/закрыть контейнер или подобрать предмет.
    Interact,
    /// Действие с инвентарём (взять/всё/выбросить/использовать).
    Inventory(InventoryAction),
    /// Бросок болта (G).
    ThrowBolt,
}

/// Очередь клиентских действий по игрокам — обрабатывается в тике.
#[derive(Clone, Debug, Default)]
pub struct ActionQueue {
    pending: HashMap<u64, VecDeque<PlayerAction>>,
}

impl ActionQueue {
    fn push(&mut self, id: u64, action: PlayerAction) {
        let queue = self.pending.entry(id).or_default();
        while queue.len() >= ACTION_QUEUE_CAP {
            queue.pop_front();
        }
        queue.push_back(action);
    }

    fn take_all(&mut self) -> Vec<(u64, PlayerAction)> {
        let mut actions = Vec::new();
        for (id, queue) in self.pending.drain() {
            actions.extend(queue.into_iter().map(|action| (id, action)));
        }
        actions
    }

    fn clear(&mut self, id: u64) {
        self.pending.remove(&id);
    }
}

/// Персональный апдейт контейнера для сети.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerUpdateData {
    pub container: u64,
    pub items: Vec<ItemStackNet>,
}

impl ContainerUpdateData {
    /// Апдейт «контейнер закрыт».
    pub fn closed() -> Self {
        Self {
            container: 0,
            items: Vec::new(),
        }
    }
}

/// Изменения для персональной рассылки (создаёт симуляция, отправляет сессия).
/// На клиенте не используется — истина приходит в снапшотах; очереди
/// ограничены, чтобы предсказание без отправки не росло бесконечно.
#[derive(Clone, Debug, Default)]
pub struct NetUpdates {
    /// Полное состояние ячеек инвентаря владельцу.
    pub inventory: Vec<(u64, Vec<Option<ItemStackNet>>)>,
    /// Состояние контейнера открывшему его игроку (`container = 0` — закрыт).
    pub containers: Vec<(u64, ContainerUpdateData)>,
}

/// Максимум апдейтов каждого вида до вычитывания серверной сессией.
const NET_UPDATES_CAP: usize = 128;

impl NetUpdates {
    fn push_inventory(&mut self, id: u64, slots: Vec<Option<ItemStackNet>>) {
        while self.inventory.len() >= NET_UPDATES_CAP {
            self.inventory.remove(0);
        }
        self.inventory.push((id, slots));
    }

    fn push_container(&mut self, id: u64, update: ContainerUpdateData) {
        while self.containers.len() >= NET_UPDATES_CAP {
            self.containers.remove(0);
        }
        self.containers.push((id, update));
    }
}

/// Очереди клиентских инпутов по игрокам.
///
/// По сети инпуты приходят с джиттером, дублями и пропусками (в пакетах —
/// redundancy последних инпутов), поэтому хранятся в упорядоченной по tick
/// очереди: каждый шаг симуляции потребляет самый ранний инпут; если очередь
/// пуста — повторяется последний (консервативная экстраполяция на сервере).
#[derive(Clone, Debug, Default)]
pub struct InputBuffer {
    pending: HashMap<u64, BTreeMap<u64, ClientInput>>,
    last: HashMap<u64, ClientInput>,
}

impl InputBuffer {
    /// Кладёт инпут в очередь игрока. Устаревшие (tick ≤ последнего
    /// потреблённого) отбрасываются, дубли по tick перезаписываются.
    pub fn queue(&mut self, id: u64, command: ClientInput) {
        if let Some(last) = self.last.get(&id) {
            if command.tick <= last.tick {
                return;
            }
        }
        let queue = self.pending.entry(id).or_default();
        queue.insert(command.tick, command);
        while queue.len() > INPUT_QUEUE_CAP {
            queue.pop_first();
        }
    }

    /// Инпут для следующего шага: самый ранний из очереди или повтор последнего.
    pub fn consume(&mut self, id: u64) -> Option<ClientInput> {
        let next = self
            .pending
            .get_mut(&id)
            .and_then(BTreeMap::pop_first)
            .map(|(_, command)| command);
        let command = next.or_else(|| self.last.get(&id).copied())?;
        self.last.insert(id, command);
        Some(command)
    }

    /// Последний потреблённый инпут игрока.
    pub fn last(&self, id: u64) -> Option<ClientInput> {
        self.last.get(&id).copied()
    }

    /// Tick последнего потреблённого инпута (ack для клиента).
    pub fn last_tick(&self, id: u64) -> Option<u64> {
        self.last.get(&id).map(|command| command.tick)
    }

    pub fn clear(&mut self, id: u64) {
        self.pending.remove(&id);
        self.last.remove(&id);
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.last.is_empty()
    }
}

pub struct Simulation {
    world: World,
    schedule: Schedule,
    fixed: FixedStep,
    tick: u64,
    next_player_id: u64,
    spawn: Vec3,
    spawn_yaw: f32,
}

impl Default for Simulation {
    fn default() -> Self {
        Self::new()
    }
}

impl Simulation {
    /// Пустая сцена: невидимый пол на y = 0 (поведение M1/M2). В основном для
    /// тестов и отладочной консоли сервера.
    pub fn new() -> Self {
        Self::build(Vec3::ZERO, 0.0, true)
    }

    /// Симуляция уровня: статические коллайдеры строятся из объектов сцены
    /// (`solid`), точка спавна — из сцены. Каталог предметов пуст — ящики
    /// сцены не наполняются (движению и бою каталог не нужен). Пол должен
    /// быть объектом сцены — как в `demo_zone`.
    pub fn with_scene(scene: &Scene) -> Self {
        Self::with_scene_and_catalog(scene, ItemCatalog::default())
    }

    /// Симуляция уровня вместе с каталогом предметов: ящики сцены получают
    /// стартовое содержимое, артефакты аномалий появляются на полу.
    pub fn with_scene_and_catalog(scene: &Scene, catalog: ItemCatalog) -> Self {
        let mut simulation = Self::build(scene.spawn, scene.spawn_yaw, false);
        if let Some(resource) = simulation.world.get_resource_mut::<LootCatalog>() {
            resource.0 = catalog;
        }
        simulation.rebuild_static(scene);
        simulation.spawn_artifacts(scene);
        simulation
    }

    /// Пересобирает статические коллайдеры сцены, точку спавна, ящики и
    /// аномалии: единый путь загрузки уровня (ручная пол-плита пустой сцены
    /// не трогается).
    pub fn rebuild_static(&mut self, scene: &Scene) {
        self.spawn = scene.spawn;
        self.spawn_yaw = scene.spawn_yaw;
        if let Some(spawn_point) = self.world.get_resource_mut::<SpawnPoint>() {
            spawn_point.position = scene.spawn;
            spawn_point.yaw = scene.spawn_yaw;
        }
        let catalog = self.catalog();
        if let Some(containers) = self.world.get_resource_mut::<Containers>() {
            containers.0 = build_containers(scene, &catalog);
        }
        if let Some(anomalies) = self.world.get_resource_mut::<Anomalies>() {
            anomalies.0 = build_anomalies(scene);
        }
        let physics = self
            .world
            .get_resource_mut::<PhysicsWorld>()
            .expect("physics resource initialized in build()");
        physics.rebuild_static(&scene.objects);
    }

    /// Артефакты из аномалий сцены: предмет лежит на полу в якоре с начала
    /// матча; забрал — до конца матча новых нет.
    fn spawn_artifacts(&mut self, scene: &Scene) {
        let catalog = self.catalog();
        for anomaly in &scene.anomalies {
            let Some(key) = anomaly.artifact.as_deref() else {
                continue;
            };
            let Some(item) = catalog.index_of(key) else {
                log::warn!("аномалия: артефакт '{key}' отсутствует в каталоге — пропущен");
                continue;
            };
            spawn_ground_item(&mut self.world, Stack::new(item, 1), anomaly.position);
        }
    }

    fn build(spawn: Vec3, spawn_yaw: f32, ground_plate: bool) -> Self {
        let mut world = World::new();
        world.insert_resource(TickInfo::default());
        world.insert_resource(InputBuffer::default());
        world.insert_resource(SpawnPoint {
            position: spawn,
            yaw: spawn_yaw,
        });
        world.insert_resource(CombatEvents::default());
        world.insert_resource(LootCatalog::default());
        world.insert_resource(Containers::default());
        world.insert_resource(Corpses::default());
        world.insert_resource(GroundItems::default());
        world.insert_resource(Anomalies::default());
        world.insert_resource(Bolts::default());
        world.insert_resource(ActionQueue::default());
        world.insert_resource(NetUpdates::default());

        let mut physics = PhysicsWorld::with_gravity_y(-GRAVITY, SIMULATION_STEP.as_secs_f32());
        if ground_plate {
            physics.add_static_box_glam(
                Vec3::new(
                    EMPTY_SCENE_GROUND_HALF_EXTENT,
                    0.5,
                    EMPTY_SCENE_GROUND_HALF_EXTENT,
                ),
                Vec3::new(0.0, -0.5, 0.0),
            );
        }
        world.insert_resource(physics);

        Self {
            world,
            schedule: build_schedule(),
            fixed: FixedStep::new(SIMULATION_STEP),
            tick: 0,
            next_player_id: 1,
            spawn,
            spawn_yaw,
        }
    }

    pub fn tick_hz(&self) -> u32 {
        SIMULATION_HZ
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Точка спавна сцены (для клиента и сервера).
    pub fn spawn_point(&self) -> Vec3 {
        self.spawn
    }

    pub fn spawn_yaw(&self) -> f32 {
        self.spawn_yaw
    }

    pub fn player_count(&mut self) -> usize {
        self.world.query::<&PlayerState>().iter().len()
    }

    pub fn player_ids(&mut self) -> Vec<u64> {
        self.world
            .query::<&PlayerState>()
            .iter()
            .map(|state| state.id)
            .collect()
    }

    /// Спавнит игрока в `spawn` с поворотом сцены.
    pub fn spawn_player(&mut self, spawn: Vec3) -> u64 {
        self.spawn_player_with_yaw(spawn, self.spawn_yaw)
    }

    pub fn spawn_player_with_yaw(&mut self, spawn: Vec3, yaw: f32) -> u64 {
        let id = self.next_player_id;
        self.next_player_id += 1;
        self.world.spawn((PlayerState::new(id, spawn, yaw),));
        id
    }

    /// Спавнит игрока с готовым состоянием (перенос между локациями): id
    /// сохраняется, счётчик новых id не откатывается.
    pub fn spawn_player_state(&mut self, state: PlayerState) -> u64 {
        let id = state.id;
        self.next_player_id = self.next_player_id.max(id.saturating_add(1));
        self.world.spawn((state,));
        id
    }

    /// Убирает игрока и возвращает его состояние (перенос между локациями).
    /// `None`, если игрока не было; буфер ввода и очередь действий чистятся.
    pub fn take_player_state(&mut self, id: u64) -> Option<PlayerState> {
        let entity = self
            .world
            .query::<(Entity, &PlayerState)>()
            .iter()
            .find(|(_, state)| state.id == id)
            .map(|(entity, _)| entity)?;
        let state = self.player_state(id);
        self.clear_inputs(id);
        if let Some(queue) = self.world.get_resource_mut::<ActionQueue>() {
            queue.clear(id);
        }
        self.world.despawn(entity);
        state
    }

    /// Убирает игрока и его буфер ввода. `false`, если игрока не было.
    pub fn despawn_player(&mut self, id: u64) -> bool {
        self.take_player_state(id).is_some()
    }

    pub fn queue_input(&mut self, id: u64, command: ClientInput) {
        self.world
            .get_resource_or_insert_with(InputBuffer::default)
            .queue(id, command);
    }

    /// Tick последнего обработанного инпута игрока (ack снапшота).
    pub fn last_input_tick(&self, id: u64) -> Option<u64> {
        self.world
            .get_resource::<InputBuffer>()
            .and_then(|buffer| buffer.last_tick(id))
    }

    /// Сбрасывает очереди ввода игрока (перед реплеем/prection-коррекцией).
    pub fn clear_inputs(&mut self, id: u64) {
        if let Some(buffer) = self.world.get_resource_mut::<InputBuffer>() {
            buffer.clear(id);
        }
    }

    /// Восстанавливает состояние игрока из сетевого снапшота (reconciliation).
    pub fn restore_player(&mut self, id: u64, state: &PlayerNetState) -> bool {
        let mut query = self.world.query::<&mut PlayerState>();
        for player in query.iter() {
            if player.id == id {
                player.apply_net_state(state);
                return true;
            }
        }
        false
    }

    pub fn advance(&mut self, frame_delta: Duration) -> u32 {
        let steps = self.fixed.accumulate(frame_delta);
        for _ in 0..steps {
            self.step_once();
        }
        steps
    }

    pub fn step_once(&mut self) {
        let dt = self.fixed.fixed_dt().as_secs_f32();
        if let Some(info) = self.world.get_resource_mut::<TickInfo>() {
            info.dt = dt;
            info.tick = self.tick;
        }
        self.schedule.run(&mut self.world);
        self.tick += 1;
        self.world.advance_tick();
    }

    pub fn snapshot(&mut self) -> Vec<PlayerNetState> {
        self.world
            .query::<&PlayerState>()
            .iter()
            .map(|state| state.net_state())
            .collect()
    }

    pub fn player_state(&mut self, id: u64) -> Option<PlayerState> {
        self.world
            .query::<&PlayerState>()
            .iter()
            .find(|state| state.id == id)
            .cloned()
    }

    /// Забирает накопленные события боя (сервер кладёт их в снапшоты).
    pub fn take_events(&mut self) -> Vec<GameEvent> {
        self.world
            .get_resource_mut::<CombatEvents>()
            .map(|events| std::mem::take(&mut events.0))
            .unwrap_or_default()
    }

    /// Выброс (M4d-3b): урон всем живым игрокам локации. Смерти и нокдауны
    /// идут штатными путями — трупы с лутом, события в снапшот.
    pub fn surge_damage(&mut self, amount: u16) {
        let tick = self.tick;
        let mut deaths: Vec<(u64, Vec<Stack>, Vec3, f32)> = Vec::new();
        let mut downed: Vec<u64> = Vec::new();
        {
            let mut query = self.world.query::<&mut PlayerState>();
            for state in query.iter() {
                if state.health == 0 {
                    continue;
                }
                let was_downed = is_downed(state, tick);
                if damage_player(state, tick, amount) {
                    let items = kill_player(state, tick);
                    deaths.push((state.id, items, state.position, state.yaw));
                } else if !was_downed && is_downed(state, tick) {
                    downed.push(state.id);
                }
            }
        }
        for player in downed {
            self.push_event(GameEvent::KnockedDown {
                player,
                player_name: String::new(),
                ticks: KNOCKDOWN_TICKS as u16,
            });
        }
        for (victim, items, position, yaw) in deaths {
            self.push_event(GameEvent::Death {
                victim,
                victim_name: String::new(),
                killer: 0,
                killer_name: String::new(),
            });
            spawn_corpse(&mut self.world, victim, position, yaw, items, tick);
        }
    }

    /// Внешнее событие мира (M4d-2, экстракция): уходит в следующий снапшот
    /// локации вместе с событиями симуляции.
    pub fn push_event(&mut self, event: GameEvent) {
        if let Some(events) = self.world.get_resource_mut::<CombatEvents>() {
            events.0.push(event);
        }
    }

    /// Экстракция (M4d-2): забирает весь инвентарь и делит его на
    /// сохраняемую часть (`percent` по весу) и остаток. Граничная стопка
    /// режется по единицам веса — патроны/болты делятся, артефакты либо
    /// влезают, либо остаются. `percent` ∈ [0, 1]; 1 — сохраняется всё.
    pub fn extract_inventory(&mut self, id: u64, percent: f32) -> (Vec<Stack>, Vec<Stack>) {
        let items = self.take_inventory_all(id);
        split_by_weight(items, percent, &catalog_of(&self.world))
    }

    /// Медпункт (M5-2): лечение игрока без предмета (реген на базе). Мёртвым
    /// лечение не идёт; полное HP — тоже нет. `true` — лечение произошло.
    pub fn heal(&mut self, id: u64, amount: u16) -> bool {
        with_player(&mut self.world, id, |state| {
            if state.health == 0 || state.health >= MAX_HEALTH {
                return false;
            }
            state.health = state.health.saturating_add(amount).min(MAX_HEALTH);
            true
        })
        .unwrap_or(false)
    }

    /// Сейф (M5-2): спасает долю `fraction` (по весу) лута из контейнера
    /// (трупа) — возвращённые стопки уходят в стэш, остаток остаётся в трупе.
    /// `fraction` ∈ [0, 1]; 0 — труп не тронут. Несуществующий контейнер —
    /// пустой результат (fail-soft).
    pub fn salvage_corpse(&mut self, container: u64, fraction: f32) -> Vec<Stack> {
        let fraction = fraction.clamp(0.0, 1.0);
        let Some(items) = container_items_copy(&self.world, container) else {
            return Vec::new();
        };
        if items.is_empty() || fraction <= 0.0 {
            return Vec::new();
        }
        let (salvaged, rest) = split_by_weight(items, fraction, &catalog_of(&self.world));
        if !salvaged.is_empty() {
            set_container_items(&mut self.world, container, rest);
        }
        salvaged
    }

    /// Выбрасывает стопку на землю в точке (M4d-2: остаток экстракции
    /// остаётся в круге — его может подобрать любой игрок).
    pub fn drop_stack_at(&mut self, stack: Stack, position: Vec3) {
        spawn_ground_item(&mut self.world, stack, position);
    }

    /// Очередь действия клиента (обыск/инвентарь); сервер уже отфильтровал
    /// дубли по tick.
    pub fn queue_action(&mut self, id: u64, action: PlayerAction) {
        self.world
            .get_resource_or_insert_with(ActionQueue::default)
            .push(id, action);
    }

    /// Забирает персональные апдейты инвентарей/контейнеров (серверная сессия).
    pub fn take_net_updates(&mut self) -> NetUpdates {
        self.world
            .get_resource_mut::<NetUpdates>()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Содержимое контейнера (копия) — диагностика, тесты и консоль сервера.
    pub fn container_items(&self, container: u64) -> Option<Vec<Stack>> {
        container_items_copy(&self.world, container)
    }

    /// Предметы на земле для снапшота (позиции квантованы).
    pub fn snapshot_loot(&self) -> Vec<GroundItemNet> {
        self.world
            .get_resource::<GroundItems>()
            .map(|items| {
                items
                    .entries
                    .iter()
                    .map(|entry| GroundItemNet {
                        item: entry.stack.item,
                        count: u16::try_from(entry.stack.count).unwrap_or(u16::MAX),
                        pos: quantize_pos(entry.position.to_array()),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Трупы для снапшота; имена заполняет серверная сессия.
    pub fn snapshot_corpses(&self) -> Vec<CorpseNet> {
        self.world
            .get_resource::<Corpses>()
            .map(|corpses| {
                corpses
                    .entries
                    .iter()
                    .map(|corpse| CorpseNet {
                        container: corpse.id,
                        owner: corpse.owner_id,
                        name: String::new(),
                        pos: quantize_pos(corpse.position.to_array()),
                        yaw: quantize_yaw(corpse.yaw),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Летящие болты для снапшота (позиции квантованы).
    pub fn snapshot_bolts(&self) -> Vec<BoltNet> {
        self.world
            .get_resource::<Bolts>()
            .map(|bolts| {
                bolts
                    .items
                    .iter()
                    .map(|bolt| BoltNet {
                        id: bolt.id,
                        owner: bolt.owner,
                        pos: quantize_pos(bolt.position.to_array()),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Число аномалий уровня (тесты и диагностика).
    pub fn anomaly_count(&self) -> usize {
        self.world
            .get_resource::<Anomalies>()
            .map(|anomalies| anomalies.0.len())
            .unwrap_or(0)
    }

    /// Индекс предмета в каталоге по ключу (отладочная консоль сервера).
    pub fn catalog_index_of(&self, key: &str) -> Option<u16> {
        self.world
            .get_resource::<LootCatalog>()
            .and_then(|catalog| catalog.0.index_of(key))
    }

    /// Цена продажи торговцу (M5-2) по индексу предмета в каталоге;
    /// `None` — предмет не из каталога, `0` — хлам.
    pub fn item_value_rub(&self, item: u16) -> Option<u32> {
        self.world
            .get_resource::<LootCatalog>()
            .and_then(|catalog| catalog.0.get(item).map(|item| item.value_rub))
    }

    /// Отладка (консоль сервера): выдать предмет игроку — сколько влезет.
    /// Возвращает число выданных единиц.
    pub fn debug_give(&mut self, id: u64, item: u16, count: u32) -> u32 {
        self.give_stack(id, Stack::new(item, count))
    }

    /// Выдаёт стопку игроку, сколько влезет по весу и ячейкам (взятие со
    /// склада, отладка). Возвращает число перенесённых единиц; при переносе
    /// помечает инвентарь «изменился» для сети.
    pub fn give_stack(&mut self, id: u64, stack: Stack) -> u32 {
        let catalog = self.catalog();
        let moved = with_player(&mut self.world, id, |state| {
            add_stack(&catalog, &mut state.inventory, stack)
        })
        .unwrap_or(0);
        if moved > 0 {
            push_inventory_update_of(&mut self.world, id);
        }
        moved
    }

    /// Копия стопки в ячейке инвентаря без изъятия (проверка перед
    /// продажей торговцу — M5-2).
    pub fn inventory_slot(&mut self, id: u64, slot: usize) -> Option<Stack> {
        self.world
            .query::<&PlayerState>()
            .iter()
            .find(|state| state.id == id)
            .and_then(|state| state.inventory.get(slot).and_then(Option::as_ref).copied())
    }

    /// Вынимает стопку из ячейки инвентаря (положить в склад); при успехе
    /// помечает инвентарь «изменился» для сети.
    pub fn take_inventory_slot(&mut self, id: u64, slot: usize) -> Option<Stack> {
        let stack = with_player(&mut self.world, id, |state| {
            state.inventory.get_mut(slot).and_then(Option::take)
        })
        .flatten();
        if stack.is_some() {
            push_inventory_update_of(&mut self.world, id);
        }
        stack
    }

    /// Выгребает весь инвентарь игрока (авто-сдача на базе): ячейки пустеют,
    /// при непустом результате сеть получает обновление инвентаря.
    pub fn take_inventory_all(&mut self, id: u64) -> Vec<Stack> {
        let items = with_player(&mut self.world, id, |state| {
            drain_inventory(&mut state.inventory)
        })
        .unwrap_or_default();
        if !items.is_empty() {
            push_inventory_update_of(&mut self.world, id);
        }
        items
    }

    /// Копия каталога предметов: мутации инвентаря не должны держать
    /// заимствование ресурса.
    fn catalog(&self) -> ItemCatalog {
        catalog_of(&self.world)
    }

    /// Точка попадания луча в статику уровня (конец трейсера для эффектов
    /// выстрела); `dir` — единичный вектор.
    pub fn cast_ray(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Option<Vec3> {
        self.world
            .get_resource::<PhysicsWorld>()
            .and_then(|physics| physics.cast_ray_glam(origin, dir, max_distance))
            .map(|toi| origin + dir * toi)
    }

    /// Число статических коллайдеров физики (для тестов и диагностики).
    pub fn static_collider_count(&self) -> usize {
        self.world
            .get_resource::<PhysicsWorld>()
            .map(|physics| physics.static_collider_count())
            .unwrap_or(0)
    }
}

fn build_schedule() -> Schedule {
    let mut schedule = Schedule::new();
    schedule.add("player_apply_input", apply_input);
    schedule.add("anomaly_effects", anomaly_effects);
    schedule.add("player_move", move_players);
    schedule.add("player_combat", player_combat);
    schedule.add("player_actions", player_actions);
    schedule.add("player_bolts", bolts_update);
    schedule.add("player_healing", player_healing);
    schedule.add("knockdown_recovery", knockdown_recovery);
    schedule.add("corpse_decay", corpse_decay);
    schedule.add("player_respawn", player_respawn);
    schedule
}

fn tick_dt(world: &World) -> f32 {
    world
        .get_resource::<TickInfo>()
        .map(|info| info.dt)
        .unwrap_or_else(|| SIMULATION_STEP.as_secs_f32())
}

fn tick_now(world: &World) -> u64 {
    world
        .get_resource::<TickInfo>()
        .map(|info| info.tick)
        .unwrap_or(0)
}

fn apply_input(world: &mut World) {
    let dt = tick_dt(world);
    let tick = tick_now(world);

    let ids: Vec<u64> = world
        .query::<&PlayerState>()
        .iter()
        .map(|state| state.id)
        .collect();

    let mut commands: HashMap<u64, ClientInput> = HashMap::with_capacity(ids.len());
    if let Some(buffer) = world.get_resource_mut::<InputBuffer>() {
        for id in ids {
            if let Some(command) = buffer.consume(id) {
                commands.insert(id, command);
            }
        }
    }

    let mut query = world.query::<&mut PlayerState>();
    for state in query.iter() {
        if let Some(command) = commands.get(&state.id) {
            state.yaw = (state.yaw + command.yaw_delta).rem_euclid(std::f32::consts::TAU);
            state.pitch = (state.pitch + command.pitch_delta).clamp(-MAX_PITCH, MAX_PITCH);

            // Мёртвый осматривается, но не действует; в нокдауне (M4d-3b) —
            // то же самое: игрок лежит до таймера.
            if state.health == 0 || is_downed(state, tick) {
                state.firing = false;
                state.sprinting = false;
                continue;
            }
            state.firing = command.buttons & FLAG_FIRE != 0;

            let (sin_yaw, cos_yaw) = state.yaw.sin_cos();
            let forward = Vec3::new(cos_yaw, 0.0, sin_yaw);
            // «Вправо» — правая половина экрана (при yaw = 0 это +Z,
            // закреплено тестом конвенций в zn-render).
            let right = Vec3::new(-sin_yaw, 0.0, cos_yaw);
            let mut move_dir = forward * command.move_forward + right * command.move_right;
            if move_dir.length_squared() > 1.0 {
                move_dir = move_dir.normalize();
            }

            let sprinting =
                command.buttons & FLAG_SPRINTING != 0 && move_dir.length_squared() > 1e-6;
            state.sprinting = sprinting;
            let speed = if sprinting { SPRINT_SPEED } else { WALK_SPEED };
            state.velocity.x = move_dir.x * speed;
            state.velocity.z = move_dir.z * speed;

            if command.jump && state.grounded {
                state.velocity.y = JUMP_SPEED;
                state.grounded = false;
            }
        }

        if state.health > 0 && !state.grounded {
            state.velocity.y = (state.velocity.y - GRAVITY * dt).max(-MAX_FALL_SPEED);
        }
    }
}

fn move_players(world: &mut World) {
    let dt = tick_dt(world);
    let tick = tick_now(world);

    let requests: Vec<(Entity, Vec3, Vec3)> = {
        let mut query = world.query::<(Entity, &PlayerState)>();
        query
            .iter()
            .filter(|(_, state)| state.health > 0 && !is_downed(state, tick))
            .map(|(entity, state)| (entity, state.position, state.velocity * dt))
            .collect()
    };
    if requests.is_empty() {
        return;
    }

    let results: Vec<(Entity, Vec3, bool)> = {
        let Some(physics) = world.get_resource_mut::<PhysicsWorld>() else {
            return;
        };
        // Обновляет broad-phase/narrow-phase: без этого запросы move_character
        // не видят только что добавленные коллайдеры (как в старом клиенте,
        // где step вызывался каждый кадр перед ходом персонажа).
        physics.step_dynamic_bodies();
        requests
            .into_iter()
            .map(|(entity, feet, desired)| {
                let (translation, grounded) = physics.move_character_feet(feet, desired);
                (entity, translation, grounded)
            })
            .collect()
    };

    let mut query = world.query::<(Entity, &mut PlayerState)>();
    for (entity, state) in query.iter() {
        let Some((_, translation, grounded)) = results.iter().find(|(e, _, _)| *e == entity) else {
            continue;
        };
        state.position += *translation;
        state.grounded = *grounded;
        if *grounded && state.velocity.y < 0.0 {
            state.velocity.y = 0.0;
        }
    }
}

/// Направление прицела по yaw/pitch — та же конвенция, что у камеры клиента
/// (`zn_render::Camera::forward`): yaw = 0 — +X, положительный pitch — вверх.
pub fn aim_forward(yaw: f32, pitch: f32) -> Vec3 {
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    Vec3::new(cos_yaw * cos_pitch, sin_pitch, sin_yaw * cos_pitch)
}

/// Урон выстрела по высоте попадания от ступней: голова/торс/ноги.
pub fn zone_damage(hit_height: f32) -> u16 {
    let multiplier = if hit_height >= ZONE_HEAD_MIN {
        HEAD_DAMAGE_MULT
    } else if hit_height >= ZONE_TORSO_MIN {
        1.0
    } else {
        LEGS_DAMAGE_MULT
    };
    (RIFLE_DAMAGE * multiplier).round() as u16
}

/// Пересечение луча с вертикальной капсулой игрока (радиус `PLAYER_RADIUS`,
/// ядро — отрезок на высотах r..2·half_height+r от ступней). Возвращает
/// параметр `t` ближайшего входа или `None`, если попадания в `max_t` нет.
fn ray_capsule(feet: Vec3, origin: Vec3, dir: Vec3, max_t: f32) -> Option<f32> {
    let radius = PLAYER_RADIUS;
    let core_low = feet.y + radius;
    let core_high = feet.y + 2.0 * PLAYER_HALF_HEIGHT + radius;
    let ox = origin.x - feet.x;
    let oz = origin.z - feet.z;
    let mut best = f32::INFINITY;

    // Цилиндрическая часть: квадратичное уравнение в плоскости XZ.
    let a = dir.x * dir.x + dir.z * dir.z;
    if a > 1e-8 {
        let b = ox * dir.x + oz * dir.z;
        let c = ox * ox + oz * oz - radius * radius;
        let disc = b * b - a * c;
        if disc >= 0.0 {
            let sqrt = disc.sqrt();
            for t in [(-b - sqrt) / a, (-b + sqrt) / a] {
                let y = origin.y + dir.y * t;
                if t >= 0.0 && y >= core_low && y <= core_high {
                    best = best.min(t);
                }
            }
        }
    }

    // Полусферы на концах ядра.
    for (center_y, low_end) in [(core_low, true), (core_high, false)] {
        let oy = origin.y - center_y;
        let b = ox * dir.x + oy * dir.y + oz * dir.z;
        let c = ox * ox + oy * oy + oz * oz - radius * radius;
        let disc = b * b - c;
        if disc >= 0.0 {
            let sqrt = disc.sqrt();
            for t in [-b - sqrt, -b + sqrt] {
                let y = origin.y + dir.y * t;
                let in_cap = if low_end {
                    y <= core_low
                } else {
                    y >= core_high
                };
                if t >= 0.0 && in_cap {
                    best = best.min(t);
                }
            }
        }
    }

    (best <= max_t).then_some(best)
}

/// Бой: выстрелы живых игроков с `FLAG_FIRE` не чаще `FIRE_INTERVAL_TICKS`,
/// рейкаст по стенам сцены и капсулам игроков, урон по зонам, смерть с
/// отложенным респавном. События (трейсеры, килл-фид) кладутся в
/// `CombatEvents` — их забирает серверная сессия.
fn player_combat(world: &mut World) {
    let tick = world
        .get_resource::<TickInfo>()
        .map(|info| info.tick)
        .unwrap_or(0);

    // Фаза 1: выстрелы и попадания (иммутабельные заимствования).
    // Патроны (M4d-3a): выстрел расходует патрон из инвентаря; без патронов
    // стрельба молчит (сервер авторитетен — клиент только предсказывает).
    // В нокдауне (M4d-3b) игрок тоже не стреляет.
    let ammo_item = catalog_of(world).index_of(RIFLE_AMMO_KEY);
    let shooters: Vec<(u64, Vec3, Vec3)> = world
        .query::<&PlayerState>()
        .iter()
        .filter(|state| {
            state.health > 0
                && state.firing
                && tick >= state.next_fire_tick
                && !is_downed(state, tick)
        })
        .filter(|state| match ammo_item {
            Some(item) => state
                .inventory
                .iter()
                .flatten()
                .any(|stack| stack.item == item && stack.count > 0),
            None => true,
        })
        .map(|state| {
            (
                state.id,
                state.position + Vec3::new(0.0, EYE_HEIGHT, 0.0),
                aim_forward(state.yaw, state.pitch),
            )
        })
        .collect();
    if shooters.is_empty() {
        return;
    }

    let targets: Vec<(u64, Vec3)> = world
        .query::<&PlayerState>()
        .iter()
        .filter(|state| state.health > 0)
        .map(|state| (state.id, state.position))
        .collect();

    let mut events: Vec<GameEvent> = Vec::new();
    let mut fired: Vec<u64> = Vec::new();
    let mut hits: Vec<(u64, u64, u16)> = Vec::new(); // (жертва, стрелок, урон)

    {
        let physics = world.get_resource::<PhysicsWorld>();
        for (shooter, origin, dir) in &shooters {
            fired.push(*shooter);
            events.push(GameEvent::Shot {
                shooter: *shooter,
                origin: origin.to_array(),
                dir: dir.to_array(),
            });

            let wall =
                physics.and_then(|physics| physics.cast_ray_glam(*origin, *dir, RIFLE_RANGE));
            let mut closest: Option<(f32, u64, Vec3)> = None;
            for (victim, feet) in &targets {
                if victim == shooter {
                    continue;
                }
                if let Some(t) = ray_capsule(*feet, *origin, *dir, RIFLE_RANGE) {
                    if closest.is_none_or(|(best, _, _)| t < best) {
                        closest = Some((t, *victim, *feet));
                    }
                }
            }

            // Стена ближе цели — выстрел уходит в неё.
            let blocked = match (closest, wall) {
                (Some((t, _, _)), Some(wall_toi)) => wall_toi <= t,
                _ => false,
            };
            if blocked {
                continue;
            }
            if let Some((t, victim, feet)) = closest {
                let hit_point = *origin + *dir * t;
                hits.push((victim, *shooter, zone_damage(hit_point.y - feet.y)));
            }
        }
    }

    // Расход патронов (M4d-3a): по штуке за выстрел, пустые стопки убираем.
    if let Some(item) = ammo_item {
        for shooter in &fired {
            consume_item(world, *shooter, item);
        }
    }

    // Фаза 2: кулдаун, урон, смерти и нокдауны (мутабельные заимствования).
    // Лут снимается с трупа: (жертва, убийца, предметы, ступни, yaw).
    let mut deaths: Vec<(u64, u64, Vec<Stack>, Vec3, f32)> = Vec::new();
    let mut downed: Vec<u64> = Vec::new();
    {
        let mut query = world.query::<&mut PlayerState>();
        for state in query.iter() {
            if fired.contains(&state.id) {
                state.next_fire_tick = tick + FIRE_INTERVAL_TICKS;
            }
            // id — в локальную копию: фильтр не должен держать заимствование
            // `state`, иначе `kill_player` не возьмёт `&mut PlayerState`.
            let id = state.id;
            for (_, killer, damage) in hits.iter().filter(|(victim, _, _)| *victim == id) {
                if state.health == 0 {
                    // Добивание в том же тике смертью не считается.
                    continue;
                }
                let was_downed = is_downed(state, tick);
                if damage_player(state, tick, *damage) {
                    let items = kill_player(state, tick);
                    deaths.push((state.id, *killer, items, state.position, state.yaw));
                } else if !was_downed && is_downed(state, tick) {
                    downed.push(id);
                }
            }
        }
    }

    for victim in downed {
        events.push(GameEvent::KnockedDown {
            player: victim,
            player_name: String::new(),
            ticks: KNOCKDOWN_TICKS as u16,
        });
    }

    for (victim, killer, items, position, yaw) in deaths {
        events.push(GameEvent::Death {
            victim,
            victim_name: String::new(),
            killer,
            killer_name: String::new(),
        });
        spawn_corpse(world, victim, position, yaw, items, tick);
    }

    if let Some(queue) = world.get_resource_mut::<CombatEvents>() {
        queue.0.extend(events);
    }
}

/// Смертельная развязка (вызывать при `health == 0`): игрок замирает,
/// ставится таймер респавна, лечение и обыск отменяются, инвентарь
/// вынимается как лут трупа.
fn kill_player(state: &mut PlayerState, tick: u64) -> Vec<Stack> {
    state.velocity = Vec3::ZERO;
    state.sprinting = false;
    state.firing = false;
    state.respawn_at = tick + RESPAWN_DELAY_TICKS;
    state.healing = None;
    state.open_container = None;
    state.downed_until_tick = 0;
    drain_inventory(&mut state.inventory)
}

/// Урон с нокдауном (M4d-3b): первый смертельный удар с здоровья выше
/// `KNOCKDOWN_HP` валит в нокдаун (health = 1, таймер); в нокдауне любой
/// следующий урон убивает, как и удар с «низкого» здоровья. `true` — игрок
/// мёртв (вызывающий снимает лут и шлёт Death).
fn damage_player(state: &mut PlayerState, tick: u64, amount: u16) -> bool {
    let left = state.health.saturating_sub(amount);
    if left > 0 {
        state.health = left;
        return false;
    }
    if state.downed_until_tick > tick || state.health <= KNOCKDOWN_HP {
        state.health = 0;
        return true;
    }
    state.health = 1;
    state.downed_until_tick = tick + KNOCKDOWN_TICKS;
    false
}

/// Игрок в нокдауне? Живой, но обездвижен до `downed_until_tick`.
pub fn is_downed(state: &PlayerState, tick: u64) -> bool {
    state.health > 0 && state.downed_until_tick > tick
}

/// Аномалии (M4c): жгучая наносит урон «сердцу» игрока в радиусе сферы
/// (каждые `ANOMALY_DAMAGE_INTERVAL_TICKS` тиков), гравитационная добавляет
/// к желаемой скорости притяжение к центру. Работает только первая аномалия,
/// содержащая точку.
fn anomaly_effects(world: &mut World) {
    let tick = tick_now(world);
    let anomalies = world
        .get_resource::<Anomalies>()
        .map(|anomalies| anomalies.0.clone())
        .unwrap_or_default();
    if anomalies.is_empty() {
        return;
    }
    // Артефакты в инвентаре гасят урон (M4d-3a).
    let catalog = catalog_of(world);

    let mut deaths: Vec<(u64, Vec<Stack>, Vec3, f32)> = Vec::new();
    let mut downed: Vec<u64> = Vec::new();
    {
        let mut query = world.query::<&mut PlayerState>();
        for state in query.iter() {
            if state.health == 0 {
                continue;
            }
            let heart = state.position + Vec3::Y * ANOMALY_HEART_HEIGHT;
            for anomaly in &anomalies {
                if !anomaly.contains(heart) {
                    continue;
                }
                match anomaly.kind {
                    AnomalyKind::Heat => {
                        if tick.is_multiple_of(ANOMALY_DAMAGE_INTERVAL_TICKS) {
                            let distance = heart.distance(anomaly.center);
                            let closeness = (1.0 - distance / anomaly.radius).clamp(0.0, 1.0);
                            let damage = f32::from(ANOMALY_DAMAGE_EDGE)
                                + (f32::from(ANOMALY_DAMAGE_CENTER)
                                    - f32::from(ANOMALY_DAMAGE_EDGE))
                                    * closeness;
                            let damage = (damage * heat_resist_factor(&catalog, &state.inventory))
                                .round() as u16;
                            let was_downed = is_downed(state, tick);
                            if damage_player(state, tick, damage) {
                                let items = kill_player(state, tick);
                                deaths.push((state.id, items, state.position, state.yaw));
                            } else if !was_downed && is_downed(state, tick) {
                                downed.push(state.id);
                            }
                        }
                    }
                    AnomalyKind::Gravity => {
                        let to_center = anomaly.center - heart;
                        let horizontal = Vec3::new(to_center.x, 0.0, to_center.z);
                        if let Some(direction) = horizontal.try_normalize() {
                            // Тяга растёт к оси аномалии (по горизонтальной
                            // дистанции): у края почти не мешает, в центре
                            // сильнее пешего шага, но слабее спринта.
                            let closeness =
                                (1.0 - horizontal.length() / anomaly.radius).clamp(0.0, 1.0);
                            let pull = GRAVITY_PULL_EDGE
                                + (GRAVITY_PULL_CENTER - GRAVITY_PULL_EDGE) * closeness;
                            state.velocity.x += direction.x * pull;
                            state.velocity.z += direction.z * pull;
                        }
                    }
                }
                break;
            }
        }
    }

    for victim in downed {
        if let Some(events) = world.get_resource_mut::<CombatEvents>() {
            events.0.push(GameEvent::KnockedDown {
                player: victim,
                player_name: String::new(),
                ticks: KNOCKDOWN_TICKS as u16,
            });
        }
    }

    for (victim, items, position, yaw) in deaths {
        // killer = 0 — смерть без игрока-убийцы (аномалия).
        if let Some(events) = world.get_resource_mut::<CombatEvents>() {
            events.0.push(GameEvent::Death {
                victim,
                victim_name: String::new(),
                killer: 0,
                killer_name: String::new(),
            });
        }
        spawn_corpse(world, victim, position, yaw, items, tick);
    }
}

/// Обыск, инвентарь и жизненный цикл открытых контейнеров: действия из
/// `ActionQueue` (сервер уже отфильтровал дубли) и авто-закрытие панелей.
fn player_actions(world: &mut World) {
    let tick = tick_now(world);
    let actions = world
        .get_resource_mut::<ActionQueue>()
        .map(ActionQueue::take_all)
        .unwrap_or_default();

    for (player_id, action) in actions {
        match action {
            PlayerAction::Interact => interact(world, player_id),
            PlayerAction::Inventory(action) => inventory_action(world, player_id, action, tick),
            PlayerAction::ThrowBolt => throw_bolt(world, player_id, tick),
        }
    }

    maintain_open_containers(world);
}

/// Бросок болта (G): жив, кулдаун прошёл, есть болт в инвентаре (тратится
/// одна штука). Болтов в полёте не больше `BOLTS_MAX`.
fn throw_bolt(world: &mut World, player_id: u64, tick: u64) {
    let catalog = catalog_of(world);
    let Some(bolt_item) = catalog.index_of(BOLT_ITEM_KEY) else {
        return;
    };
    let flying = world
        .get_resource::<Bolts>()
        .map(|bolts| bolts.items.len())
        .unwrap_or(0);
    if flying >= BOLTS_MAX {
        return;
    }

    let thrown = with_player(world, player_id, |state| {
        if state.health == 0 || tick < state.next_throw_tick {
            return None;
        }
        let slot = state
            .inventory
            .iter()
            .position(|cell| cell.is_some_and(|stack| stack.item == bolt_item))?;
        let emptied = {
            let stack = state.inventory[slot].as_mut()?;
            stack.count -= 1;
            stack.count == 0
        };
        if emptied {
            state.inventory[slot] = None;
        }
        state.next_throw_tick = tick + BOLT_THROW_COOLDOWN_TICKS;
        Some((
            state.position + Vec3::Y * EYE_HEIGHT,
            aim_forward(state.yaw, state.pitch),
        ))
    })
    .flatten();
    let Some((eye, direction)) = thrown else {
        return;
    };

    // Чуть вперёд от глаз, чтобы болт не стартовал внутри капсулы игрока.
    let position = eye + direction * 0.4;
    if let Some(bolts) = world.get_resource_mut::<Bolts>() {
        bolts.spawn(player_id, position, direction * BOLT_SPEED, tick);
    }
    push_inventory_update_of(world, player_id);
}

/// Полёт болтов: гравитация, столкновения со статикой (приземление —
/// обычный предмет), гибель в аномалии с событием обнаружения.
fn bolts_update(world: &mut World) {
    let tick = tick_now(world);
    let dt = tick_dt(world);
    let current = world
        .get_resource::<Bolts>()
        .map(|bolts| bolts.items.clone())
        .unwrap_or_default();
    if current.is_empty() {
        return;
    }
    let anomalies = world
        .get_resource::<Anomalies>()
        .map(|anomalies| anomalies.0.clone())
        .unwrap_or_default();
    let catalog = catalog_of(world);
    let Some(bolt_item) = catalog.index_of(BOLT_ITEM_KEY) else {
        return;
    };

    let mut survivors: Vec<BoltProjectile> = Vec::with_capacity(current.len());
    let mut landed: Vec<Vec3> = Vec::new();
    let mut revealed: Vec<GameEvent> = Vec::new();
    {
        let physics = world.get_resource::<PhysicsWorld>();
        for mut bolt in current {
            let step = bolt.velocity * dt;
            let target = bolt.position + step;
            bolt.velocity.y = (bolt.velocity.y - GRAVITY * dt).max(-MAX_FALL_SPEED);

            // Вход в аномалию: болт гибнет, аномалия проявляется.
            if let Some((index, _)) = anomalies
                .iter()
                .enumerate()
                .find(|(_, anomaly)| anomaly.contains(target))
            {
                revealed.push(GameEvent::AnomalyRevealed {
                    anomaly: u32::try_from(index).unwrap_or(u32::MAX),
                    position: target.to_array(),
                });
                continue;
            }

            // Стены и пол: сегментный рейкаст от текущей позиции к новой.
            let length = step.length();
            if length > 1e-6 {
                let dir = step / length;
                let hit =
                    physics.and_then(|physics| physics.cast_ray_glam(bolt.position, dir, length));
                if let Some(toi) = hit {
                    landed.push(bolt.position + dir * toi);
                    continue;
                }
            }

            bolt.position = target;
            if tick.saturating_sub(bolt.spawn_tick) >= BOLT_LIFETIME_TICKS {
                landed.push(bolt.position);
                continue;
            }
            survivors.push(bolt);
        }
    }

    if let Some(bolts) = world.get_resource_mut::<Bolts>() {
        bolts.items = survivors;
    }
    for position in landed {
        spawn_ground_item(world, Stack::new(bolt_item, 1), position);
    }
    if !revealed.is_empty() {
        if let Some(events) = world.get_resource_mut::<CombatEvents>() {
            events.0.extend(revealed);
        }
    }
}

/// Нажатие E: контейнер в прицеле открывается/закрывается, предмет на земле
/// подбирается; без цели открытая панель закрывается.
fn interact(world: &mut World, player_id: u64) {
    let Some((eye, aim, open)) = player_aim(world, player_id) else {
        // Мёртвый или неизвестный игрок — панель обыска не держим.
        set_open_container(world, player_id, None);
        return;
    };

    match pick_interact_target(world, eye, aim) {
        Some(InteractTarget::Container(container)) => {
            if open == Some(container) {
                set_open_container(world, player_id, None);
            } else {
                set_open_container(world, player_id, Some(container));
            }
        }
        Some(InteractTarget::Ground(spawn_seq)) => pickup_items(world, player_id, spawn_seq),
        None => set_open_container(world, player_id, None),
    }
}

/// Открывает/закрывает панель обыска и сообщает сети её содержимое.
/// Повтор того же состояния не рассылается.
fn set_open_container(world: &mut World, player_id: u64, container: Option<u64>) {
    let changed = with_player(world, player_id, |state| {
        if state.open_container == container {
            return false;
        }
        state.open_container = container;
        true
    })
    .unwrap_or(false);
    if !changed {
        return;
    }

    let update = container
        .and_then(|id| {
            container_items_net(world, id).map(|items| ContainerUpdateData {
                container: id,
                items,
            })
        })
        .unwrap_or_else(ContainerUpdateData::closed);
    if let Some(updates) = world.get_resource_mut::<NetUpdates>() {
        updates.push_container(player_id, update);
    }
}

/// Авто-закрытие панелей обыска: игрок ушёл дальше
/// `CONTAINER_CLOSE_DISTANCE_M`, контейнер исчез (труп истёк) или игрок мёртв.
fn maintain_open_containers(world: &mut World) {
    let players: Vec<(u64, bool, Vec3, Option<u64>)> = world
        .query::<&PlayerState>()
        .iter()
        .map(|state| {
            (
                state.id,
                state.health > 0,
                state.position,
                state.open_container,
            )
        })
        .collect();

    let mut close = Vec::new();
    for (id, alive, position, container) in players {
        let Some(container) = container else {
            continue;
        };
        let target = container_position(world, container);
        let far = target.is_some_and(|target| {
            horizontal_distance(position, target) > CONTAINER_CLOSE_DISTANCE_M
        });
        if !alive || target.is_none() || far {
            close.push(id);
        }
    }
    for id in close {
        set_open_container(world, id, None);
    }
}

/// Действие с инвентарём: взять/всё/выбросить/использовать.
fn inventory_action(world: &mut World, player_id: u64, action: InventoryAction, tick: u64) {
    match action {
        InventoryAction::TakeFrom { container, index } => {
            take_from_container(world, player_id, container, index);
        }
        InventoryAction::TakeAll { container } => {
            take_all_from_container(world, player_id, container);
        }
        InventoryAction::Drop { slot } => drop_slot(world, player_id, slot),
        InventoryAction::Use { slot } => use_slot(world, player_id, slot, tick),
    }
}

/// Действие допустимо: игрок жив, контейнер открыт им и в радиусе обыска.
fn container_accessible(world: &mut World, player_id: u64, container: u64) -> bool {
    let open = with_player(world, player_id, |state| {
        state.health > 0 && state.open_container == Some(container)
    })
    .unwrap_or(false);
    if !open {
        return false;
    }
    let Some(position) = container_position(world, container) else {
        return false;
    };
    let player_position = with_player(world, player_id, |state| state.position).unwrap_or_default();
    horizontal_distance(player_position, position) <= INTERACT_RANGE_M
}

/// Взять стопку из открытого контейнера: сколько влезет, остаток — назад.
fn take_from_container(world: &mut World, player_id: u64, container: u64, index: u16) {
    if !container_accessible(world, player_id, container) {
        return;
    }
    let Some(stack) = take_container_stack(world, container, usize::from(index)) else {
        return;
    };
    let catalog = catalog_of(world);
    let moved = with_player(world, player_id, |state| {
        add_stack(&catalog, &mut state.inventory, stack)
    })
    .unwrap_or(0);

    let leftover = stack.count - moved;
    if leftover > 0 {
        push_container_stack(world, container, Stack::new(stack.item, leftover));
    }
    if moved > 0 {
        push_inventory_update_of(world, player_id);
    }
    if moved > 0 || leftover > 0 {
        push_container_update(world, player_id, container);
    }
}

/// Взять из открытого контейнера всё, что влезает по весу и ячейкам.
fn take_all_from_container(world: &mut World, player_id: u64, container: u64) {
    if !container_accessible(world, player_id, container) {
        return;
    }
    let Some(items) = container_items_copy(world, container) else {
        return;
    };
    let catalog = catalog_of(world);
    let mut remaining = Vec::new();
    let mut moved_any = false;
    for stack in items {
        let moved = with_player(world, player_id, |state| {
            add_stack(&catalog, &mut state.inventory, stack)
        })
        .unwrap_or(0);
        if moved > 0 {
            moved_any = true;
        }
        if moved < stack.count {
            remaining.push(Stack::new(stack.item, stack.count - moved));
        }
    }
    if !moved_any {
        return;
    }
    set_container_items(world, container, remaining);
    push_inventory_update_of(world, player_id);
    push_container_update(world, player_id, container);
}

/// Выбросить ячейку инвентаря на землю перед собой.
fn drop_slot(world: &mut World, player_id: u64, slot: u8) {
    let index = usize::from(slot);
    if index >= INVENTORY_SLOTS {
        return;
    }
    let dropped = with_player(world, player_id, |state| {
        if state.health == 0 {
            return None;
        }
        state.inventory.get_mut(index).and_then(|cell| cell.take())
    })
    .flatten();
    let Some(stack) = dropped else {
        return;
    };

    let position = drop_position(world, player_id);
    spawn_ground_item(world, stack, position);
    push_inventory_update_of(world, player_id);
}

/// Использовать предмет из ячейки: запускает лечение (применится в
/// `player_healing`). Мёртвые, здоровые и уже лечащиеся — не начинают.
fn use_slot(world: &mut World, player_id: u64, slot: u8, tick: u64) {
    let index = usize::from(slot);
    if index >= INVENTORY_SLOTS {
        return;
    }
    let catalog = catalog_of(world);
    let started = with_player(world, player_id, |state| {
        if state.health == 0 || state.health >= MAX_HEALTH || state.healing.is_some() {
            return None;
        }
        let stack = state.inventory.get(index).copied().flatten()?;
        let effect = catalog.get(stack.item)?.use_effect.as_ref()?;
        if effect.heal == 0 {
            return None;
        }
        let ticks = (effect.duration_secs * SIMULATION_HZ as f32)
            .round()
            .max(1.0) as u64;
        state.healing = Some(Healing {
            slot,
            until_tick: tick + ticks,
        });
        Some(u16::try_from(ticks).unwrap_or(u16::MAX))
    })
    .flatten();

    if let Some(ticks) = started {
        if let Some(events) = world.get_resource_mut::<CombatEvents>() {
            events.0.push(GameEvent::UseStarted {
                player: player_id,
                ticks,
            });
        }
    }
}

/// Завершение лечения: по истечении `until_tick` добавляет HP и тратит одну
/// единицу предмета. Смерть или потеря предмета отменяют эффект.
fn player_healing(world: &mut World) {
    let tick = tick_now(world);
    let catalog = catalog_of(world);
    let mut completed: Vec<u64> = Vec::new();

    {
        let mut query = world.query::<&mut PlayerState>();
        for state in query.iter() {
            let Some(healing) = state.healing else {
                continue;
            };
            if state.health == 0 {
                state.healing = None;
                continue;
            }
            if tick < healing.until_tick {
                continue;
            }
            state.healing = None;

            let slot = usize::from(healing.slot);
            let Some(stack) = state.inventory.get(slot).copied().flatten() else {
                continue;
            };
            let Some(effect) = catalog
                .get(stack.item)
                .and_then(|item| item.use_effect.as_ref())
            else {
                continue;
            };
            state.health = state.health.saturating_add(effect.heal).min(MAX_HEALTH);
            if let Some(cell) = state.inventory.get_mut(slot) {
                let emptied = match cell {
                    Some(existing) => {
                        existing.count = existing.count.saturating_sub(1);
                        existing.count == 0
                    }
                    None => false,
                };
                if emptied {
                    *cell = None;
                }
            }
            completed.push(state.id);
        }
    }

    for id in completed {
        push_inventory_update_of(world, id);
    }
}

/// Старение мира: трупы старше `CORPSE_LIFETIME_TICKS` исчезают (панели
/// обыска у смотрящих закроются в `maintain_open_containers`).
fn corpse_decay(world: &mut World) {
    let tick = tick_now(world);
    if let Some(corpses) = world.get_resource_mut::<Corpses>() {
        corpses.entries.retain(|corpse| corpse.despawn_tick > tick);
    }
}

/// Нокдаун (M4d-3b): таймер истёк — игрок встаёт с `KNOCKDOWN_HP`; событие
/// `GotUp` уходит в снапшот (клиенты снимают оверлей).
fn knockdown_recovery(world: &mut World) {
    let tick = tick_now(world);
    let mut stood_up: Vec<u64> = Vec::new();
    {
        let mut query = world.query::<&mut PlayerState>();
        for state in query.iter() {
            if state.health > 0 && state.downed_until_tick > 0 && state.downed_until_tick <= tick {
                state.downed_until_tick = 0;
                state.health = KNOCKDOWN_HP;
                stood_up.push(state.id);
            }
        }
    }
    if stood_up.is_empty() {
        return;
    }
    if let Some(events) = world.get_resource_mut::<CombatEvents>() {
        for player in stood_up {
            events.0.push(GameEvent::GotUp {
                player,
                player_name: String::new(),
            });
        }
    }
}

/// Респавн: через `RESPAWN_DELAY_TICKS` после смерти — полное здоровье и точка
/// спавна уровня.
fn player_respawn(world: &mut World) {
    let tick = world
        .get_resource::<TickInfo>()
        .map(|info| info.tick)
        .unwrap_or(0);
    let Some(spawn) = world.get_resource::<SpawnPoint>().copied() else {
        return;
    };

    let mut query = world.query::<&mut PlayerState>();
    for state in query.iter() {
        // `respawn_at > 0` — страховка клиентского предсказания: смерть из
        // снапшота не задаёт respawn_at, локальный респавн не срабатывает
        // (игрока вернёт серверный снапшот).
        if state.health == 0 && state.respawn_at > 0 && tick >= state.respawn_at {
            state.position = spawn.position;
            state.velocity = Vec3::ZERO;
            state.yaw = spawn.yaw;
            state.pitch = 0.0;
            state.grounded = true;
            state.sprinting = false;
            state.firing = false;
            state.health = MAX_HEALTH;
            state.respawn_at = 0;
            state.next_fire_tick = 0;
            state.next_throw_tick = 0;
            state.healing = None;
            state.open_container = None;
        }
    }
}

/// Цель наведения (E): контейнер (ящик/труп) или предмет на земле.
#[derive(Clone, Copy, Debug, PartialEq)]
enum InteractTarget {
    Container(u64),
    /// Порядковый номер появления предмета на земле.
    Ground(u64),
}

/// Выбирает цель в пределах `INTERACT_RANGE_M` (по горизонтали) с максимальной
/// близостью к прицелу (косинус ≥ `INTERACT_MIN_DOT`). Линия видимости не
/// проверяется: цели маленькие и выбираются в упор.
fn pick_interact_target(world: &World, eye: Vec3, aim: Vec3) -> Option<InteractTarget> {
    let mut best: Option<(f32, f32, InteractTarget)> = None;
    let mut consider = |target: InteractTarget, position: Vec3| {
        let delta = position - eye;
        let horizontal = Vec3::new(delta.x, 0.0, delta.z);
        let distance = horizontal.length();
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
            best = Some((dot, distance, target));
        }
    };

    if let Some(containers) = world.get_resource::<Containers>() {
        for container in &containers.0 {
            consider(
                InteractTarget::Container(crate_container_id(container.scene_index)),
                container.position,
            );
        }
    }
    if let Some(corpses) = world.get_resource::<Corpses>() {
        for corpse in &corpses.entries {
            consider(InteractTarget::Container(corpse.id), corpse.position);
        }
    }
    if let Some(items) = world.get_resource::<GroundItems>() {
        for item in &items.entries {
            consider(InteractTarget::Ground(item.spawn_seq), item.position);
        }
    }

    best.map(|(_, _, target)| target)
}

/// Подбор предмета с земли: сколько влезет в инвентарь; остаток остаётся.
fn pickup_items(world: &mut World, player_id: u64, spawn_seq: u64) {
    let taken = {
        let Some(items) = world.get_resource_mut::<GroundItems>() else {
            return;
        };
        let Some(index) = items
            .entries
            .iter()
            .position(|item| item.spawn_seq == spawn_seq)
        else {
            return;
        };
        items.entries.remove(index)
    };

    let catalog = catalog_of(world);
    let moved = with_player(world, player_id, |state| {
        if state.health == 0 {
            return 0;
        }
        add_stack(&catalog, &mut state.inventory, taken.stack)
    })
    .unwrap_or(0);
    if moved > 0 {
        push_inventory_update_of(world, player_id);
    }
    if moved < taken.stack.count {
        spawn_ground_item(
            world,
            Stack::new(taken.stack.item, taken.stack.count - moved),
            taken.position,
        );
    }
}

/// Позиция глаз, прицел и открытый контейнер живого игрока.
fn player_aim(world: &mut World, player_id: u64) -> Option<(Vec3, Vec3, Option<u64>)> {
    let mut query = world.query::<&PlayerState>();
    for state in query.iter() {
        if state.id == player_id && state.health > 0 {
            return Some((
                state.position + Vec3::new(0.0, EYE_HEIGHT, 0.0),
                aim_forward(state.yaw, state.pitch),
                state.open_container,
            ));
        }
    }
    None
}

/// Точка выброса: шаг вперёд от игрока на высоте ступней.
fn drop_position(world: &mut World, player_id: u64) -> Vec3 {
    let mut query = world.query::<&PlayerState>();
    for state in query.iter() {
        if state.id == player_id {
            let (sin_yaw, cos_yaw) = state.yaw.sin_cos();
            return state.position + Vec3::new(cos_yaw, 0.0, sin_yaw) * 0.6;
        }
    }
    Vec3::ZERO
}

/// Мутирует состояние игрока по id, если он существует.
fn with_player<T>(world: &mut World, id: u64, f: impl FnOnce(&mut PlayerState) -> T) -> Option<T> {
    let mut query = world.query::<&mut PlayerState>();
    for state in query.iter() {
        if state.id == id {
            return Some(f(state));
        }
    }
    None
}

fn player_state_copy(world: &mut World, id: u64) -> Option<PlayerState> {
    let mut query = world.query::<&PlayerState>();
    query.iter().find(|state| state.id == id).cloned()
}

/// Копия каталога предметов уровня.
fn catalog_of(world: &World) -> ItemCatalog {
    world
        .get_resource::<LootCatalog>()
        .map(|catalog| catalog.0.clone())
        .unwrap_or_default()
}

/// Вес одной единицы предмета в граммах — точные сравнения с лимитом.
fn item_weight_grams(item: &Item) -> u32 {
    (item.weight_kg * 1000.0).round().max(1.0) as u32
}

/// Делит стопки по весу: `percent` груза идёт в первую кучу, остаток — во
/// вторую. Граничная стопка режется по единицам веса — патроны/болты делятся,
/// артефакты либо влезают, либо остаются. Общая логика экстракции (M4d-2) и
/// сейфа базы (M5-2).
fn split_by_weight(
    items: Vec<Stack>,
    percent: f32,
    catalog: &ItemCatalog,
) -> (Vec<Stack>, Vec<Stack>) {
    let percent = percent.clamp(0.0, 1.0);
    if items.is_empty() || percent >= 1.0 {
        return (items, Vec::new());
    }
    let unit_weight = |item: u16| -> u64 {
        catalog
            .get(item)
            .map(|item| u64::from(item_weight_grams(item)))
            .unwrap_or(0)
    };
    let stack_weight =
        |stack: &Stack| unit_weight(stack.item).saturating_mul(u64::from(stack.count));
    let total: u64 = items.iter().map(&stack_weight).sum();
    let mut budget = (total as f32 * percent) as u64;
    let mut kept = Vec::new();
    let mut lost = Vec::new();
    for stack in items {
        let weight = stack_weight(&stack);
        if weight <= budget {
            budget -= weight;
            kept.push(stack);
            continue;
        }
        // Граничная стопка: режем по единицам веса, что не влезло — падает.
        let unit = unit_weight(stack.item);
        let keep_count = budget
            .checked_div(unit)
            .unwrap_or(0)
            .min(u64::from(stack.count));
        if keep_count > 0 {
            budget -= unit * keep_count;
            kept.push(Stack::new(stack.item, keep_count as u32));
            let rest = stack.count - keep_count as u32;
            if rest > 0 {
                lost.push(Stack::new(stack.item, rest));
            }
        } else {
            lost.push(stack);
        }
    }
    (kept, lost)
}

/// Вес инвентаря в граммах по каталогу.
pub fn inventory_weight_grams(
    catalog: &ItemCatalog,
    inventory: &[Option<Stack>; INVENTORY_SLOTS],
) -> u32 {
    let mut grams = 0u32;
    for stack in inventory.iter().flatten() {
        if let Some(item) = catalog.get(stack.item) {
            grams = grams.saturating_add(item_weight_grams(item).saturating_mul(stack.count));
        }
    }
    grams
}

/// Лимит переноски с учётом артефактов (M4d-3a): самый сильный `weight_cut`
/// в инвентаре поднимает потолок (без суммирования).
pub fn carry_limit_grams(
    catalog: &ItemCatalog,
    inventory: &[Option<Stack>; INVENTORY_SLOTS],
) -> u32 {
    let cut = inventory
        .iter()
        .flatten()
        .filter_map(|stack| catalog.get(stack.item))
        .filter_map(|item| item.carry_effect.as_ref())
        .filter_map(|effect| effect.weight_cut)
        .fold(0.0f32, f32::max)
        .clamp(0.0, 1.0);
    (MAX_CARRY_GRAMS as f32 * (1.0 + cut)) as u32
}

/// Множитель урона жгучих аномалий по инвентарю (M4d-3a): самый сильный
/// `heat_resist` среди артефактов (0.5 — половина урона).
pub fn heat_resist_factor(
    catalog: &ItemCatalog,
    inventory: &[Option<Stack>; INVENTORY_SLOTS],
) -> f32 {
    inventory
        .iter()
        .flatten()
        .filter_map(|stack| catalog.get(stack.item))
        .filter_map(|item| item.carry_effect.as_ref())
        .filter_map(|effect| effect.heat_resist)
        .fold(1.0f32, f32::min)
        .clamp(0.0, 1.0)
}

/// Расходует одну единицу предмета (M4d-3a: патрон за выстрел): пустые стопки
/// убираются, сеть получает обновление инвентаря.
fn consume_item(world: &mut World, player_id: u64, item: u16) {
    let spent = with_player(world, player_id, |state| {
        let mut spent = false;
        for slot in state.inventory.iter_mut().flatten() {
            if slot.item == item && slot.count > 0 {
                slot.count -= 1;
                spent = true;
                break;
            }
        }
        for slot in state.inventory.iter_mut() {
            if slot.as_ref().is_some_and(|stack| stack.count == 0) {
                *slot = None;
            }
        }
        spent
    })
    .unwrap_or(false);
    if spent {
        push_inventory_update_of(world, player_id);
    }
}

/// Кладёт стопку в инвентарь насколько влезает по весу и ячейкам: сначала
/// доливает частичные стопки того же предмета, затем занимает пустые ячейки.
/// Возвращает число перенесённых единиц (может быть меньше `stack.count`).
fn add_stack(
    catalog: &ItemCatalog,
    inventory: &mut [Option<Stack>; INVENTORY_SLOTS],
    stack: Stack,
) -> u32 {
    let Some(item) = catalog.get(stack.item) else {
        return 0;
    };
    let stack_max = item.stack_max.max(1);
    let unit_grams = item_weight_grams(item);
    let room_units = carry_limit_grams(catalog, inventory)
        .saturating_sub(inventory_weight_grams(catalog, inventory))
        / unit_grams;
    if room_units == 0 {
        return 0;
    }

    let mut left = stack.count;
    let mut moved = 0u32;

    // Доливка в частичные стопки того же предмета.
    for slot in inventory.iter_mut() {
        if left == 0 || moved >= room_units {
            break;
        }
        if let Some(existing) = slot.as_mut() {
            if existing.item == stack.item && existing.count < stack_max {
                let take = left.min(stack_max - existing.count).min(room_units - moved);
                existing.count += take;
                left -= take;
                moved += take;
            }
        }
    }

    // Новые ячейки.
    for slot in inventory.iter_mut() {
        if left == 0 || moved >= room_units {
            break;
        }
        if slot.is_none() {
            let take = left.min(stack_max).min(room_units - moved);
            *slot = Some(Stack::new(stack.item, take));
            left -= take;
            moved += take;
        }
    }

    moved
}

/// Выгребает содержимое инвентаря (лут для трупа), оставляя ячейки пустыми.
fn drain_inventory(inventory: &mut [Option<Stack>; INVENTORY_SLOTS]) -> Vec<Stack> {
    let mut items = Vec::new();
    for slot in inventory.iter_mut() {
        if let Some(stack) = slot.take() {
            items.push(stack);
        }
    }
    items
}

/// Полное состояние ячеек инвентаря в сетевом виде.
fn inventory_net(inventory: &[Option<Stack>; INVENTORY_SLOTS]) -> Vec<Option<ItemStackNet>> {
    inventory
        .iter()
        .map(|slot| {
            slot.map(|stack| ItemStackNet {
                item: stack.item,
                count: u16::try_from(stack.count).unwrap_or(u16::MAX),
            })
        })
        .collect()
}

/// Стопки в сетевом виде.
fn stacks_net(items: &[Stack]) -> Vec<ItemStackNet> {
    items
        .iter()
        .map(|stack| ItemStackNet {
            item: stack.item,
            count: u16::try_from(stack.count).unwrap_or(u16::MAX),
        })
        .collect()
}

/// Помечает инвентарь игрока «изменился»: сессия дошлёт полное состояние.
fn push_inventory_update(world: &mut World, state: &PlayerState) {
    if let Some(updates) = world.get_resource_mut::<NetUpdates>() {
        updates.push_inventory(state.id, inventory_net(&state.inventory));
    }
}

/// То же по id игрока.
fn push_inventory_update_of(world: &mut World, player_id: u64) {
    if let Some(state) = player_state_copy(world, player_id) {
        push_inventory_update(world, &state);
    }
}

/// Обновление содержимого контейнера для игрока с открытой панелью.
fn push_container_update(world: &mut World, player_id: u64, container: u64) {
    let items = container_items_net(world, container).unwrap_or_default();
    if let Some(updates) = world.get_resource_mut::<NetUpdates>() {
        updates.push_container(player_id, ContainerUpdateData { container, items });
    }
}

/// Кладёт труп игрока (тело остаётся для рендера даже без лута) и сообщает
/// сети, что инвентарь жертвы и открытый контейнер опустели.
fn spawn_corpse(
    world: &mut World,
    owner_id: u64,
    position: Vec3,
    yaw: f32,
    items: Vec<Stack>,
    tick: u64,
) {
    if let Some(corpses) = world.get_resource_mut::<Corpses>() {
        corpses.spawn(Corpse {
            id: 0,
            owner_id,
            position,
            yaw,
            items,
            despawn_tick: tick + CORPSE_LIFETIME_TICKS,
        });
    }
    push_inventory_update_of(world, owner_id);
    set_open_container(world, owner_id, None);
}

/// Кладёт предмет на землю; сверх `GROUND_ITEMS_MAX` гаснут самые старые.
fn spawn_ground_item(world: &mut World, stack: Stack, position: Vec3) {
    if stack.count == 0 {
        return;
    }
    let Some(items) = world.get_resource_mut::<GroundItems>() else {
        return;
    };
    let seq = items.next_seq;
    items.next_seq += 1;
    items.entries.push(GroundItem {
        stack,
        position,
        spawn_seq: seq,
    });
    while items.entries.len() > GROUND_ITEMS_MAX {
        let Some(oldest) = items
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, item)| item.spawn_seq)
            .map(|(index, _)| index)
        else {
            break;
        };
        items.entries.remove(oldest);
    }
}

/// id контейнера-ящика по индексу объекта сцены.
pub fn crate_container_id(scene_index: u32) -> u64 {
    u64::from(scene_index) + 1
}

/// Индекс объекта сцены по id ящика; `None` для трупов и 0.
pub fn crate_scene_index(container: u64) -> Option<u32> {
    if container == 0 || is_corpse_container(container) {
        return None;
    }
    u32::try_from(container - 1).ok()
}

/// id относится к трупу, а не к ящику сцены.
pub fn is_corpse_container(container: u64) -> bool {
    container >= CORPSE_CONTAINER_BASE
}

/// Центр контейнера (ящика или места смерти трупа).
fn container_position(world: &World, container: u64) -> Option<Vec3> {
    if container == 0 {
        return None;
    }
    if is_corpse_container(container) {
        let corpses = world.get_resource::<Corpses>()?;
        return corpses
            .entries
            .iter()
            .find(|corpse| corpse.id == container)
            .map(|corpse| corpse.position);
    }
    let scene_index = crate_scene_index(container)?;
    let containers = world.get_resource::<Containers>()?;
    containers
        .0
        .iter()
        .find(|crate_state| crate_state.scene_index == scene_index)
        .map(|crate_state| crate_state.position)
}

/// Содержимое контейнера (копия).
fn container_items_copy(world: &World, container: u64) -> Option<Vec<Stack>> {
    container_items_ref(world, container).cloned()
}

/// Содержимое контейнера в сетевом виде.
fn container_items_net(world: &World, container: u64) -> Option<Vec<ItemStackNet>> {
    container_items_copy(world, container).map(|items| stacks_net(&items))
}

fn container_items_ref(world: &World, container: u64) -> Option<&Vec<Stack>> {
    if container == 0 {
        return None;
    }
    if is_corpse_container(container) {
        let corpses = world.get_resource::<Corpses>()?;
        return corpses
            .entries
            .iter()
            .find(|corpse| corpse.id == container)
            .map(|corpse| &corpse.items);
    }
    let scene_index = crate_scene_index(container)?;
    let containers = world.get_resource::<Containers>()?;
    containers
        .0
        .iter()
        .find(|crate_state| crate_state.scene_index == scene_index)
        .map(|crate_state| &crate_state.items)
}

/// Мутабельная ссылка на содержимое контейнера (ящика или трупа).
fn container_items_mut(world: &mut World, container: u64) -> Option<&mut Vec<Stack>> {
    if container == 0 {
        return None;
    }
    if is_corpse_container(container) {
        let corpses = world.get_resource_mut::<Corpses>()?;
        return corpses
            .entries
            .iter_mut()
            .find(|corpse| corpse.id == container)
            .map(|corpse| &mut corpse.items);
    }
    let scene_index = crate_scene_index(container)?;
    let containers = world.get_resource_mut::<Containers>()?;
    containers
        .0
        .iter_mut()
        .find(|crate_state| crate_state.scene_index == scene_index)
        .map(|crate_state| &mut crate_state.items)
}

/// Забирает стопку из контейнера по индексу.
fn take_container_stack(world: &mut World, container: u64, index: usize) -> Option<Stack> {
    let items = container_items_mut(world, container)?;
    if index < items.len() {
        Some(items.remove(index))
    } else {
        None
    }
}

/// Кладёт стопку в конец содержимого контейнера.
fn push_container_stack(world: &mut World, container: u64, stack: Stack) {
    if let Some(items) = container_items_mut(world, container) {
        items.push(stack);
    }
}

/// Заменяет содержимое контейнера.
fn set_container_items(world: &mut World, container: u64, items: Vec<Stack>) {
    if let Some(target) = container_items_mut(world, container) {
        *target = items;
    }
}

/// Горизонтальная дистанция между точками (высота не учитывается).
fn horizontal_distance(left: Vec3, right: Vec3) -> f32 {
    ((left.x - right.x).powi(2) + (left.z - right.z).powi(2)).sqrt()
}

/// Аномалии сцены → состояние симуляции (центр сферы, радиус, вид).
fn build_anomalies(scene: &Scene) -> Vec<AnomalyState> {
    scene
        .anomalies
        .iter()
        .map(|anomaly| AnomalyState {
            center: anomaly.center(),
            radius: anomaly.radius,
            kind: anomaly.kind,
        })
        .collect()
}

/// Собирает ящики уровня: объекты сцены с `loot`, наполненные по каталогу.
/// Неизвестные ключи пропускаются с предупреждением, количество дробится по
/// `stack_max` предмета.
fn build_containers(scene: &Scene, catalog: &ItemCatalog) -> Vec<ContainerState> {
    let mut containers = Vec::new();
    for (index, object) in scene.objects.iter().enumerate() {
        let Some(loot) = &object.loot else {
            continue;
        };
        let Ok(scene_index) = u32::try_from(index) else {
            continue;
        };
        let mut items = Vec::new();
        for entry in loot {
            let Some(item_index) = catalog.index_of(&entry.item) else {
                log::warn!(
                    "ящик #{}: предмет '{}' отсутствует в каталоге — пропущен",
                    index,
                    entry.item
                );
                continue;
            };
            let stack_max = catalog
                .get(item_index)
                .map(|item| item.stack_max.max(1))
                .unwrap_or(1);
            let mut left = entry.count;
            while left > 0 {
                let count = left.min(stack_max);
                items.push(Stack::new(item_index, count));
                left -= count;
            }
        }
        containers.push(ContainerState {
            scene_index,
            position: object.position,
            items,
        });
    }
    containers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tick: u64) -> ClientInput {
        ClientInput::new(tick)
    }

    #[test]
    fn input_buffer_consumes_in_tick_order() {
        let mut buffer = InputBuffer::default();
        let mut a = input(3);
        a.move_forward = 0.3;
        let mut b = input(2);
        b.move_forward = 0.2;
        let mut c = input(4);
        c.move_forward = 0.4;
        buffer.queue(1, a);
        buffer.queue(1, b); // пришёл позже, но tick меньше
        buffer.queue(1, c);

        assert_eq!(buffer.consume(1).unwrap().move_forward, 0.2);
        assert_eq!(buffer.consume(1).unwrap().move_forward, 0.3);
        assert_eq!(buffer.consume(1).unwrap().move_forward, 0.4);
        assert_eq!(buffer.last_tick(1), Some(4));
    }

    #[test]
    fn input_buffer_repeats_last_and_drops_stale() {
        let mut buffer = InputBuffer::default();
        buffer.queue(1, input(5));
        let consumed = buffer.consume(1).unwrap();
        assert_eq!(consumed.tick, 5);

        // Повтор последнего при пустой очереди.
        assert_eq!(buffer.consume(1).unwrap().tick, 5);

        // Устаревший и дублирующий tick не принимаются.
        buffer.queue(1, input(5));
        buffer.queue(1, input(4));
        assert_eq!(buffer.consume(1).unwrap().tick, 5);

        buffer.queue(1, input(6));
        assert_eq!(buffer.consume(1).unwrap().tick, 6);
    }

    #[test]
    fn input_buffer_ignores_duplicate_ticks_idempotently() {
        let mut buffer = InputBuffer::default();
        let mut first = input(1);
        first.move_forward = 1.0;
        let mut duplicate = input(1);
        duplicate.move_forward = 1.0;
        buffer.queue(7, first);
        buffer.queue(7, duplicate);
        assert_eq!(buffer.consume(7).unwrap().move_forward, 1.0);
        assert!(buffer.consume(7).is_some());
    }

    #[test]
    fn input_buffer_caps_pending_queue() {
        let mut buffer = InputBuffer::default();
        for tick in 0..(INPUT_QUEUE_CAP as u64 + 10) {
            buffer.queue(1, input(tick));
        }
        // Потребляем всё — очередь не должна превышать CAP.
        let mut consumed = 0;
        while let Some(command) = buffer.consume(1) {
            consumed += 1;
            if command.tick >= INPUT_QUEUE_CAP as u64 + 9 {
                break;
            }
        }
        assert!(consumed <= INPUT_QUEUE_CAP + 1, "consumed = {consumed}");
    }

    #[test]
    fn clear_drops_player_state() {
        let mut buffer = InputBuffer::default();
        buffer.queue(1, input(1));
        buffer.consume(1);
        buffer.clear(1);
        assert!(buffer.last(1).is_none());
        assert!(buffer.consume(1).is_none());
    }

    // ── База между рейдами (M5-2): heal и сейф ──────────────────────────

    /// Каталог для тестов веса: болт 50 г, аптечка 500 г.
    fn weight_catalog() -> ItemCatalog {
        ItemCatalog::from_ron(
            r#"(
                items: [
                    (key: "bolt", display_name: "Болт", weight_kg: 0.05, stack_max: 20, value_rub: 5),
                    (key: "medkit", display_name: "Аптечка", weight_kg: 0.5, stack_max: 3, value_rub: 250),
                ],
            )"#,
        )
        .unwrap()
    }

    fn sim_with_weights() -> Simulation {
        Simulation::with_scene_and_catalog(&Scene::new("test"), weight_catalog())
    }

    #[test]
    fn heal_restores_health_and_clamps() {
        let mut sim = sim_with_weights();
        let player = sim.spawn_player(Vec3::ZERO);
        sim.surge_damage(30);
        assert_eq!(sim.player_state(player).unwrap().health, MAX_HEALTH - 30);

        // Лечение выше остатка — clamp по максимуму.
        assert!(sim.heal(player, 25));
        assert_eq!(sim.player_state(player).unwrap().health, MAX_HEALTH - 5);
        assert!(sim.heal(player, 10));
        assert_eq!(sim.player_state(player).unwrap().health, MAX_HEALTH);
        // На полном HP лечение не происходит.
        assert!(!sim.heal(player, 10));
        assert_eq!(sim.player_state(player).unwrap().health, MAX_HEALTH);
    }

    #[test]
    fn heal_skips_dead_and_missing_players() {
        let mut sim = sim_with_weights();
        let player = sim.spawn_player(Vec3::ZERO);
        // Первый смертельный удар кладёт в нокдаун, второй — добивает.
        sim.surge_damage(MAX_HEALTH);
        sim.surge_damage(MAX_HEALTH);
        assert_eq!(sim.player_state(player).unwrap().health, 0);
        assert!(!sim.heal(player, 50), "мёртвым медпункт не помогает");
        assert!(!sim.heal(player + 100, 50), "несуществующий игрок");
    }

    #[test]
    fn salvage_corpse_splits_boundary_stack_by_weight() {
        let mut sim = sim_with_weights();
        spawn_corpse(
            &mut sim.world,
            1,
            Vec3::ZERO,
            0.0,
            vec![Stack::new(0, 10)],
            0,
        );
        let corpse = sim.snapshot_corpses()[0].container;

        // 30 % от 500 г = 150 г → 3 болта в стэш, 7 остаются в трупе.
        let salvaged = sim.salvage_corpse(corpse, 0.3);
        assert_eq!(salvaged, vec![Stack::new(0, 3)]);
        assert_eq!(sim.container_items(corpse), Some(vec![Stack::new(0, 7)]));
    }

    #[test]
    fn salvage_corpse_keeps_whole_stack_when_fraction_covers_it() {
        let mut sim = sim_with_weights();
        // Аптечка (500 г) + 10 болтов (500 г) = 1000 г.
        spawn_corpse(
            &mut sim.world,
            1,
            Vec3::ZERO,
            0.0,
            vec![Stack::new(1, 1), Stack::new(0, 10)],
            0,
        );
        let corpse = sim.snapshot_corpses()[0].container;

        // 75 % = 750 г: аптечка (500 г) влезает целиком, из болтов влезает
        // ещё 250 г → 5 штук; остаток (5 болтов) остаётся в трупе.
        let salvaged = sim.salvage_corpse(corpse, 0.75);
        assert_eq!(
            salvaged,
            vec![Stack::new(1, 1), Stack::new(0, 5)],
            "стопки делятся по порядку, как в экстракции"
        );
        assert_eq!(sim.container_items(corpse), Some(vec![Stack::new(0, 5)]));
    }

    #[test]
    fn salvage_corpse_edge_cases() {
        let mut sim = sim_with_weights();
        // Несуществующий контейнер — пусто.
        assert!(sim
            .salvage_corpse(CORPSE_CONTAINER_BASE + 99, 0.5)
            .is_empty());
        // Доля 0 — труп не тронут.
        spawn_corpse(
            &mut sim.world,
            1,
            Vec3::ZERO,
            0.0,
            vec![Stack::new(0, 4)],
            0,
        );
        let corpse = sim.snapshot_corpses()[0].container;
        assert!(sim.salvage_corpse(corpse, 0.0).is_empty());
        assert_eq!(sim.container_items(corpse), Some(vec![Stack::new(0, 4)]));
        // Доля 1 — весь лут в стэш, труп пустеет.
        assert_eq!(sim.salvage_corpse(corpse, 1.0), vec![Stack::new(0, 4)]);
        assert_eq!(sim.container_items(corpse), Some(Vec::new()));
    }
}
