use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use zn_content::ItemCatalog;
use zn_net::{
    check_client_hello, decode_message, encode_message, BoltNet, ClientInput, CorpseNet, GameEvent,
    GroundItemNet, InventoryAction, ItemStackNet, Message, PlayerNetState, RejectReason,
    StashAction, INPUT_REDUNDANCY, MAX_NAME_LEN, SERVER_TIMEOUT, SIMULATION_HZ, SNAPSHOT_HZ,
};
use zn_scene::{
    distances_to_base, world_content_hash, LocationId, LocationInfo, LocationRegistry, Scene,
    LOCATION_BASE,
};
use zn_sim::{NetUpdates, PlayerAction, Simulation, Stack, Vec3, ANOMALY_HEART_HEIGHT};

/// Порт по умолчанию для выделенного сервера.
pub const DEFAULT_PORT: u16 = 7700;

/// Буфер приёма: наш протокол без фрагментации укладывается в MTU.
const RECV_BUF_SIZE: usize = 2048;

/// Снапшот каждые `SIMULATION_HZ / SNAPSHOT_HZ` тиков симуляции (2 при 60/30).
const SNAPSHOT_EVERY_TICKS: u64 = (SIMULATION_HZ / SNAPSHOT_HZ) as u64;

/// Пузырей на карту: случайно 2–3 в пределах числа кандидатов сцены.
const BUBBLE_COUNT_MIN: usize = 2;
const BUBBLE_COUNT_MAX: usize = 3;
/// Использованный пузырь возвращается в мир через 90 с.
const BUBBLE_RESPAWN_TICKS: u64 = 90 * SIMULATION_HZ as u64;

/// Параметры сессии. Всё, что влияет на детерминизм тестов (латентность/потери
/// исходящих), по умолчанию выключено.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub max_players: usize,
    /// Задержка исходящих пакетов — диагностика джиттера, не для CI.
    pub out_latency: Duration,
    /// Процент дропа исходящих пакетов (0..=100) — диагностика потерь.
    pub out_loss_pct: f32,
    /// Seed детерминированного генератора потерь (воспроизводимость).
    pub rng_seed: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_players: zn_net::MAX_PLAYERS_PER_SESSION,
            out_latency: Duration::ZERO,
            out_loss_pct: 0.0,
            rng_seed: 0x9E37_79B9,
        }
    }
}

/// Взгляд на подключённого клиента (для консоли и тестов).
#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub player_id: u64,
    pub name: String,
    pub addr: SocketAddr,
    /// Виртуальное время с момента последнего пакета клиента.
    pub idle: Duration,
    /// Локация игрока (id из реестра).
    pub location: LocationId,
    /// Название локации для консоли.
    pub location_name: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionStats {
    pub received: u64,
    pub sent: u64,
    pub dropped: u64,
}

/// Состояние пузырей одной локации (M4d): какие кандидаты сцены активны и
/// когда вернуть использованные.
#[derive(Clone, Debug, Default)]
struct BubbleField {
    /// Индексы активных кандидатов `Scene::bubbles` (без повторов).
    active: Vec<usize>,
    /// Tick-и, на которых использованные пузыри вернутся в мир.
    pending: Vec<u64>,
}

/// Симуляция, сцена и пузыри одной локации мира.
struct LocationRuntime {
    scene: Scene,
    simulation: Simulation,
    bubbles: BubbleField,
}

/// Итог перехода игрока между локациями (пешком или пузырём).
#[derive(Clone, Copy, Debug)]
struct LocationChange {
    player_id: u64,
    from: LocationId,
    to: LocationId,
    /// Точка появления в целевой локации (ступни).
    position: Vec3,
    yaw: f32,
}

/// Мир сессии (M4d): симуляция на локацию, переходы, пузыри и склад.
/// Тики всех локаций синхронны — сессия шагает их одним `dt`.
struct World {
    locations: BTreeMap<LocationId, LocationRuntime>,
    registry: LocationRegistry,
    base: LocationId,
    /// Кратчайшие расстояния до базы (в переходах); у пузырей — ориентир.
    distances: HashMap<LocationId, u32>,
    /// Где сейчас игрок (id → локация).
    placements: HashMap<u64, LocationId>,
    /// Склад по игрокам: стопки без слияния.
    stashes: HashMap<u64, Vec<Stack>>,
    /// xorshift32 для пузырей (детерминирован из seed конфигурации).
    rng: u32,
}

impl World {
    /// Мир из реестра и загруженных сцен (основной режим сервера).
    fn new(
        registry: LocationRegistry,
        scenes: Vec<(LocationId, Scene)>,
        catalog: &ItemCatalog,
        rng_seed: u32,
    ) -> Result<Self, String> {
        if scenes.is_empty() {
            return Err("мир без локаций".to_string());
        }
        let mut locations = BTreeMap::new();
        for (id, scene) in scenes {
            locations.insert(
                id,
                LocationRuntime {
                    simulation: Simulation::with_scene_and_catalog(&scene, catalog.clone()),
                    scene,
                    bubbles: BubbleField::default(),
                },
            );
        }
        let base = registry
            .base_id()
            .filter(|id| locations.contains_key(id))
            .ok_or_else(|| "в реестре нет базовой локации среди загруженных сцен".to_string())?;
        let refs: Vec<(LocationId, &Scene)> = locations
            .iter()
            .map(|(id, runtime)| (*id, &runtime.scene))
            .collect();
        let distances = distances_to_base(&registry, &refs);
        let mut world = Self {
            locations,
            registry,
            base,
            distances,
            placements: HashMap::new(),
            stashes: HashMap::new(),
            rng: rng_seed | 1,
        };
        world.seed_bubbles();
        Ok(world)
    }

    /// Одиночная сцена (тесты и `--level`): локация 0, мир без переходов,
    /// пузырей и сдачи на базу.
    fn single(scene: &Scene, catalog: ItemCatalog, rng_seed: u32) -> Self {
        let mut locations = BTreeMap::new();
        locations.insert(
            LOCATION_BASE,
            LocationRuntime {
                scene: scene.clone(),
                simulation: Simulation::with_scene_and_catalog(scene, catalog),
                bubbles: BubbleField::default(),
            },
        );
        // Реестр нужен только ради имени локации в логах и консоли.
        let registry = LocationRegistry {
            locations: vec![LocationInfo {
                id: LOCATION_BASE,
                name: scene.name.clone(),
                scene: scene.name.clone(),
                base: true,
            }],
        };
        Self {
            locations,
            registry,
            base: LOCATION_BASE,
            distances: HashMap::new(),
            placements: HashMap::new(),
            stashes: HashMap::new(),
            rng: rng_seed | 1,
        }
    }

    /// Раскладывает стартовые пузыри: 2–3 активных в разных кандидатах сцены.
    fn seed_bubbles(&mut self) {
        let ids: Vec<LocationId> = self.locations.keys().copied().collect();
        for id in ids {
            let candidates = self
                .locations
                .get(&id)
                .map(|runtime| runtime.scene.bubbles.len())
                .unwrap_or(0);
            if candidates == 0 {
                continue;
            }
            let count = (BUBBLE_COUNT_MIN
                + self.rand_below(BUBBLE_COUNT_MAX - BUBBLE_COUNT_MIN + 1))
            .min(candidates);
            let active: Vec<usize> = self
                .shuffled_indices(candidates)
                .into_iter()
                .take(count)
                .collect();
            if let Some(runtime) = self.locations.get_mut(&id) {
                runtime.bubbles.active = active;
            }
            log::info!(
                "пузыри: '{}' — активно {count} из {candidates}",
                self.location_name(id)
            );
        }
    }

    /// Шагает все симуляции мира одним `dt` (тики локаций синхронны).
    fn advance(&mut self, frame_delta: Duration) {
        for runtime in self.locations.values_mut() {
            runtime.simulation.advance(frame_delta);
        }
    }

    /// Tick мира — по базовой локации (все симуляции синхронны).
    fn tick(&self) -> u64 {
        self.locations
            .get(&self.base)
            .map(|runtime| runtime.simulation.tick())
            .unwrap_or(0)
    }

    fn player_count(&mut self) -> usize {
        self.locations
            .values_mut()
            .map(|runtime| runtime.simulation.player_count())
            .sum()
    }

    fn static_collider_count(&self) -> usize {
        self.locations
            .values()
            .map(|runtime| runtime.simulation.static_collider_count())
            .sum()
    }

    /// Название локации для логов и консоли.
    fn location_name(&self, location: LocationId) -> String {
        self.registry
            .get(location)
            .map(|info| info.name.clone())
            .unwrap_or_else(|| format!("локация {location}"))
    }

    fn player_location(&self, player_id: u64) -> Option<LocationId> {
        self.placements.get(&player_id).copied()
    }

    /// Спавнит игрока на базе (подключение, отладка) и запоминает локацию.
    fn spawn_on_base(&mut self) -> (u64, Vec3) {
        let base = self.base;
        let runtime = self
            .locations
            .get_mut(&base)
            .expect("базовая локация существует");
        let spawn = runtime.simulation.spawn_point();
        let player_id = runtime.simulation.spawn_player(spawn);
        self.placements.insert(player_id, base);
        (player_id, spawn)
    }

    /// Убирает игрока из мира (отключение): состояние и склад теряются.
    fn remove_player(&mut self, player_id: u64) {
        self.stashes.remove(&player_id);
        let Some(location) = self.placements.remove(&player_id) else {
            return;
        };
        if let Some(runtime) = self.locations.get_mut(&location) {
            runtime.simulation.despawn_player(player_id);
        }
    }

    /// Хранилище игрока (склад — персональный, без слияния стопок).
    fn stash_items(&self, player_id: u64) -> &[Stack] {
        self.stashes
            .get(&player_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Активные пузыри локации (якоря на полу) — для тестов и диагностики.
    fn active_bubbles(&self, location: LocationId) -> Vec<Vec3> {
        self.locations
            .get(&location)
            .map(|runtime| {
                runtime
                    .bubbles
                    .active
                    .iter()
                    .map(|candidate| runtime.scene.bubbles[*candidate].position)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Переходы пешком: живой игрок в зоне `Scene::transitions` переезжает в
    /// целевую локацию. Возвращает сработавшие переносы для применения.
    fn check_transfers(&mut self) -> Vec<LocationChange> {
        let mut moves = Vec::new();
        let ids: Vec<LocationId> = self.locations.keys().copied().collect();
        for &id in &ids {
            let Some(runtime) = self.locations.get_mut(&id) else {
                continue;
            };
            if runtime.scene.transitions.is_empty() {
                continue;
            }
            let LocationRuntime {
                scene, simulation, ..
            } = runtime;
            for (player_id, position) in player_positions(simulation) {
                let Some(transition) = scene
                    .transitions
                    .iter()
                    .find(|transition| transition.contains(position))
                else {
                    continue;
                };
                if transition.target == id || !ids.contains(&transition.target) {
                    continue;
                }
                moves.push(LocationChange {
                    player_id,
                    from: id,
                    to: transition.target,
                    position: transition.target_position,
                    yaw: transition.target_yaw,
                });
            }
        }
        moves
    }

    /// Пузыри: живой игрок в активной сфере уносится в ближайшую к базе
    /// локацию (или к своим точкам прибытия — «почти под базу»); сработавший
    /// пузырь гаснет и возвращается через `BUBBLE_RESPAWN_TICKS`.
    fn check_bubbles(&mut self, tick: u64) -> Vec<LocationChange> {
        self.respawn_bubbles(tick);
        // Фаза 1: кто в какой пузырь попал.
        let mut hits: Vec<(LocationId, u64, usize)> = Vec::new();
        let ids: Vec<LocationId> = self.locations.keys().copied().collect();
        for id in ids {
            let Some(runtime) = self.locations.get_mut(&id) else {
                continue;
            };
            if runtime.bubbles.active.is_empty() {
                continue;
            }
            let LocationRuntime {
                scene,
                simulation,
                bubbles,
            } = runtime;
            for (player_id, position) in player_positions(simulation) {
                let heart = position + Vec3::Y * ANOMALY_HEART_HEIGHT;
                if let Some(slot) = bubbles
                    .active
                    .iter()
                    .position(|candidate| scene.bubbles[*candidate].contains(heart))
                {
                    hits.push((id, player_id, slot));
                }
            }
        }
        // Фаза 2: гаснут сработавшие пузыри, игроки уносятся.
        let mut used: BTreeMap<LocationId, Vec<usize>> = BTreeMap::new();
        let mut moves = Vec::new();
        for (location, player_id, slot) in hits {
            let (to, position, yaw) = self.bubble_destination(location);
            moves.push(LocationChange {
                player_id,
                from: location,
                to,
                position,
                yaw,
            });
            used.entry(location).or_default().push(slot);
        }
        for (location, mut slots) in used {
            slots.sort_unstable();
            slots.dedup();
            let name = self.location_name(location);
            if let Some(runtime) = self.locations.get_mut(&location) {
                for &slot in slots.iter().rev() {
                    if slot < runtime.bubbles.active.len() {
                        runtime.bubbles.active.remove(slot);
                    }
                }
                runtime
                    .bubbles
                    .pending
                    .extend(slots.iter().map(|_| tick + BUBBLE_RESPAWN_TICKS));
            }
            log::info!(
                "пузырь: '{name}' сработал {} раз(а), возврат через {} с",
                slots.len(),
                BUBBLE_RESPAWN_TICKS / SIMULATION_HZ as u64
            );
        }
        moves
    }

    /// Куда уносит пузырь из `from`: локация строго ближе к базе, кроме самой
    /// базы (в неё ведёт сигнал — M4d-5); если такой нет — свои точки прибытия
    /// («почти под базу»).
    fn bubble_destination(&mut self, from: LocationId) -> (LocationId, Vec3, f32) {
        let from_distance = self.distances.get(&from).copied().unwrap_or(u32::MAX);
        let target = self
            .locations
            .keys()
            .copied()
            .filter(|id| *id != self.base)
            .filter(|id| self.distances.get(id).copied().unwrap_or(u32::MAX) < from_distance)
            .min_by_key(|id| (self.distances.get(id).copied().unwrap_or(u32::MAX), *id));
        let target = target.unwrap_or(from);
        let (position, yaw) = self.random_arrival(target);
        (target, position, yaw)
    }

    /// Точка прибытия пузыря в локации: случайная из `bubble_arrivals` (иначе
    /// спавн сцены) и поворот лицом к переходу, ведущему к базе.
    fn random_arrival(&mut self, location: LocationId) -> (Vec3, f32) {
        let arrivals = self
            .locations
            .get(&location)
            .map(|runtime| runtime.scene.bubble_arrivals.len())
            .unwrap_or(0);
        let roll = self.rand_below(arrivals.max(1));
        let Some(runtime) = self.locations.get(&location) else {
            return (Vec3::ZERO, 0.0);
        };
        let scene = &runtime.scene;
        let position = if arrivals == 0 {
            scene.spawn
        } else {
            scene.bubble_arrivals[roll]
        };
        let from_distance = self.distances.get(&location).copied().unwrap_or(u32::MAX);
        let baseward = scene
            .transitions
            .iter()
            .filter(|transition| {
                self.distances
                    .get(&transition.target)
                    .copied()
                    .unwrap_or(u32::MAX)
                    < from_distance
            })
            .min_by_key(|transition| {
                self.distances
                    .get(&transition.target)
                    .copied()
                    .unwrap_or(u32::MAX)
            });
        let yaw = baseward.map_or(scene.spawn_yaw, |transition| {
            let to = transition.position - position;
            to.z.atan2(to.x)
        });
        (position, yaw)
    }

    /// Возвращает в мир пузыри, отлежавшиеся после использования: случайные
    /// свободные кандидаты сцены.
    fn respawn_bubbles(&mut self, tick: u64) {
        let ids: Vec<LocationId> = self.locations.keys().copied().collect();
        for id in ids {
            let due = {
                let Some(runtime) = self.locations.get_mut(&id) else {
                    continue;
                };
                let mut due = 0;
                runtime.bubbles.pending.retain(|respawn_at| {
                    if *respawn_at <= tick {
                        due += 1;
                        false
                    } else {
                        true
                    }
                });
                due
            };
            for _ in 0..due {
                let free = {
                    let Some(runtime) = self.locations.get(&id) else {
                        break;
                    };
                    (0..runtime.scene.bubbles.len())
                        .filter(|candidate| !runtime.bubbles.active.contains(candidate))
                        .count()
                };
                if free == 0 {
                    // Свободных кандидатов нет — вернёмся через секунду.
                    if let Some(runtime) = self.locations.get_mut(&id) {
                        runtime.bubbles.pending.push(tick + SIMULATION_HZ as u64);
                    }
                    continue;
                }
                let pick = self.rand_below(free);
                if let Some(runtime) = self.locations.get_mut(&id) {
                    let candidate = (0..runtime.scene.bubbles.len())
                        .filter(|candidate| !runtime.bubbles.active.contains(candidate))
                        .nth(pick)
                        .expect("pick < свободных кандидатов");
                    runtime.bubbles.active.push(candidate);
                }
                log::info!("пузырь: '{}' вернулся в мир", self.location_name(id));
            }
        }
    }

    /// Переносит состояние игрока между симуляциями: скорость и панель
    /// обыска сбрасываются; здоровье, инвентарь и цель обзора едут с игроком.
    fn apply_change(&mut self, change: &LocationChange) -> bool {
        if !self.locations.contains_key(&change.to) {
            log::warn!(
                "перенос игрока {}: локация {} не загружена",
                change.player_id,
                change.to
            );
            return false;
        }
        let Some(mut state) = self
            .locations
            .get_mut(&change.from)
            .and_then(|runtime| runtime.simulation.take_player_state(change.player_id))
        else {
            return false;
        };
        state.position = change.position;
        state.velocity = Vec3::ZERO;
        state.yaw = change.yaw;
        state.grounded = false;
        state.sprinting = false;
        state.firing = false;
        state.open_container = None;
        self.locations
            .get_mut(&change.to)
            .expect("локация проверена")
            .simulation
            .spawn_player_state(state);
        self.placements.insert(change.player_id, change.to);
        true
    }

    /// Приземление на базу: весь инвентарь уходит в склад (100 % пока;
    /// проценты по методам сейва — M4d-5). Возвращает число принятых стопок.
    fn deposit_on_base(&mut self, player_id: u64) -> usize {
        let base = self.base;
        let Some(runtime) = self.locations.get_mut(&base) else {
            return 0;
        };
        let items = runtime.simulation.take_inventory_all(player_id);
        if items.is_empty() {
            return 0;
        }
        let count = items.len();
        self.stashes.entry(player_id).or_default().extend(items);
        count
    }

    fn shuffled_indices(&mut self, n: usize) -> Vec<usize> {
        let mut order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            let j = self.rand_below(i + 1);
            order.swap(i, j);
        }
        order
    }

    fn rand_below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        self.next_rand() as usize % n
    }

    /// xorshift32: детерминированная последовательность для пузырей.
    fn next_rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }
}

/// Живые игроки симуляции: (id, позиция ступней). Мёртвые не переносятся.
fn player_positions(simulation: &mut Simulation) -> Vec<(u64, Vec3)> {
    simulation
        .player_ids()
        .into_iter()
        .filter_map(|id| {
            let state = simulation.player_state(id)?;
            (state.health > 0).then_some((id, state.position))
        })
        .collect()
}

/// Подставляет имена в события смертей: симуляция знает только id, а
/// клиентам нужен килл-фид.
fn enrich_events(events: Vec<GameEvent>, names: &HashMap<u64, String>) -> Vec<GameEvent> {
    events
        .into_iter()
        .map(|event| match event {
            GameEvent::Death { victim, killer, .. } => GameEvent::Death {
                victim,
                victim_name: names.get(&victim).cloned().unwrap_or_default(),
                killer,
                // killer = 0 — смерть без игрока-убийцы (аномалия).
                killer_name: if killer == 0 {
                    "Аномалия".to_string()
                } else {
                    names.get(&killer).cloned().unwrap_or_default()
                },
            },
            other => other,
        })
        .collect()
}

struct ClientState {
    player_id: u64,
    name: String,
    addr: SocketAddr,
    last_seen: Duration,
    /// Последний обработанный `Interact` (дедупликация повторов клиента).
    last_interact_tick: u64,
    /// Последний обработанный `InventoryAction`/`StashAction`/`ThrowBolt`.
    last_action_tick: u64,
}

/// Состояние локации для рассылки (двухфазный сбор — снапшоты шлются уже
/// без заимствований мира).
struct LocationSnapshot {
    players: Vec<PlayerNetState>,
    loot: Vec<GroundItemNet>,
    corpses: Vec<CorpseNet>,
    bolts: Vec<BoltNet>,
    events: Vec<GameEvent>,
}

/// Один процесс сервера = один матч. Сессия владеет сокетом и миром, весь
/// тайминг идёт от `poll(dt)` — время виртуальное, поэтому e2e-тесты гоняют
/// реальные сокеты без wall-clock-ожиданий.
pub struct Session {
    socket: UdpSocket,
    world: World,
    /// Мир локаций (переходы/пузыри/склад) против одиночной сцены (`--level`).
    world_mode: bool,
    content_hash: u64,
    items_hash: u64,
    /// История имён по id: килл-фид и подписи трупов переживают отключение.
    names: HashMap<u64, String>,
    config: SessionConfig,
    clients: HashMap<SocketAddr, ClientState>,
    now: Duration,
    last_snapshot_tick: u64,
    outbox: VecDeque<(Duration, SocketAddr, Vec<u8>)>,
    rng: u32,
    stats: SessionStats,
}

impl Session {
    /// Биндит UDP-сокет по адресу (`127.0.0.1:0` в тестах) и создаёт сессию
    /// одиночной сцены (тесты и `--level` сервера).
    pub fn bind(
        bind_addr: &str,
        scene: &Scene,
        catalog: ItemCatalog,
        config: SessionConfig,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;
        Self::new(socket, scene, catalog, config)
    }

    /// Сессия одиночной сцены на готовом сокете (переводится в non-blocking).
    /// Каталог предметов наполняет ящики сцены; его хэш сверяется в handshake.
    pub fn new(
        socket: UdpSocket,
        scene: &Scene,
        catalog: ItemCatalog,
        config: SessionConfig,
    ) -> io::Result<Self> {
        let items_hash = catalog.content_hash();
        let content_hash = scene.content_hash();
        let world = World::single(scene, catalog, config.rng_seed);
        Self::from_world(socket, world, content_hash, items_hash, false, config)
    }

    /// Биндит UDP-сокет и собирает мир локаций из реестра и сцен (M4d).
    pub fn bind_world(
        bind_addr: &str,
        registry: &LocationRegistry,
        scenes: Vec<(LocationId, Scene)>,
        catalog: ItemCatalog,
        config: SessionConfig,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;
        Self::new_world(socket, registry, scenes, catalog, config)
    }

    /// Мир локаций на готовом сокете: симуляция на локацию, переходы и
    /// пузыри из сцен, сдача лута на базе. Хэш handshake — `world_content_hash`.
    pub fn new_world(
        socket: UdpSocket,
        registry: &LocationRegistry,
        scenes: Vec<(LocationId, Scene)>,
        catalog: ItemCatalog,
        config: SessionConfig,
    ) -> io::Result<Self> {
        let items_hash = catalog.content_hash();
        let refs: Vec<(LocationId, &Scene)> =
            scenes.iter().map(|(id, scene)| (*id, scene)).collect();
        let content_hash = world_content_hash(registry, &refs);
        let world = World::new(registry.clone(), scenes, &catalog, config.rng_seed)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Self::from_world(socket, world, content_hash, items_hash, true, config)
    }

    fn from_world(
        socket: UdpSocket,
        world: World,
        content_hash: u64,
        items_hash: u64,
        world_mode: bool,
        config: SessionConfig,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let base = world.base;
        let colliders = world.static_collider_count();
        if world_mode {
            log::info!(
                "сессия: мир из {} локаций, база '{}', {colliders} коллайдеров, hash={content_hash:#018x}, items={items_hash:#018x}",
                world.locations.len(),
                world.location_name(base)
            );
        } else {
            let spawn = world
                .locations
                .get(&base)
                .map(|runtime| runtime.simulation.spawn_point())
                .unwrap_or(Vec3::ZERO);
            log::info!(
                "сессия: уровень '{}', {colliders} коллайдеров, спавн {spawn:?}, hash={content_hash:#018x}, items={items_hash:#018x}",
                world.location_name(base)
            );
        }
        let rng = config.rng_seed | 1;
        Ok(Self {
            socket,
            world,
            world_mode,
            content_hash,
            items_hash,
            names: HashMap::new(),
            config,
            clients: HashMap::new(),
            now: Duration::ZERO,
            last_snapshot_tick: 0,
            outbox: VecDeque::new(),
            rng,
            stats: SessionStats::default(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// Хэш каталога предметов (клиенты сверяют его в Hello).
    pub fn items_hash(&self) -> u64 {
        self.items_hash
    }

    pub fn max_players(&self) -> usize {
        self.config.max_players
    }

    pub fn tick(&self) -> u64 {
        self.world.tick()
    }

    /// Виртуальное время сессии.
    pub fn now(&self) -> Duration {
        self.now
    }

    pub fn stats(&self) -> SessionStats {
        self.stats
    }

    pub fn player_count(&mut self) -> usize {
        self.world.player_count()
    }

    pub fn clients(&self) -> Vec<ClientInfo> {
        self.clients
            .values()
            .map(|client| {
                let location = self
                    .world
                    .player_location(client.player_id)
                    .unwrap_or(self.world.base);
                ClientInfo {
                    player_id: client.player_id,
                    name: client.name.clone(),
                    addr: client.addr,
                    idle: self.now.saturating_sub(client.last_seen),
                    location,
                    location_name: self.world.location_name(location),
                }
            })
            .collect()
    }

    /// Симуляция базовой локации (диагностика/тесты). В одиночном режиме —
    /// единственная сцена сессии.
    pub fn simulation(&mut self) -> &mut Simulation {
        let base = self.world.base;
        self.world
            .locations
            .get_mut(&base)
            .map(|runtime| &mut runtime.simulation)
            .expect("базовая локация существует")
    }

    /// Симуляция указанной локации (диагностика/тесты).
    pub fn simulation_at(&mut self, location: LocationId) -> Option<&mut Simulation> {
        self.world
            .locations
            .get_mut(&location)
            .map(|runtime| &mut runtime.simulation)
    }

    /// Локация игрока (id из реестра).
    pub fn player_location(&self, player_id: u64) -> Option<LocationId> {
        self.world.player_location(player_id)
    }

    /// Название локации для консоли.
    pub fn location_name(&self, location: LocationId) -> String {
        self.world.location_name(location)
    }

    /// Число локаций мира (в одиночном режиме — 1).
    pub fn location_count(&self) -> usize {
        self.world.locations.len()
    }

    /// Идентификаторы локаций мира по возрастанию.
    pub fn location_ids(&self) -> Vec<LocationId> {
        self.world.locations.keys().copied().collect()
    }

    /// Активные пузыри локации (якоря на полу) — диагностика и тесты.
    pub fn active_bubbles(&self, location: LocationId) -> Vec<Vec3> {
        self.world.active_bubbles(location)
    }

    /// Склад игрока (склад персональный, на базе).
    pub fn stash_items(&self, player_id: u64) -> &[Stack] {
        self.world.stash_items(player_id)
    }

    /// Кикает игрока по id: без пакетов, клиент сам истечёт по таймауту.
    pub fn kick(&mut self, player_id: u64) -> bool {
        let Some(addr) = self
            .clients
            .iter()
            .find(|(_, client)| client.player_id == player_id)
            .map(|(addr, _)| *addr)
        else {
            return false;
        };
        let client = self.clients.remove(&addr).expect("найден выше");
        self.world.remove_player(client.player_id);
        log::info!(
            "игрок '{}' (id {}) кикнут ({})",
            client.name,
            client.player_id,
            client.addr
        );
        true
    }

    /// Отладочный игрок без сетевого клиента (консольная команда `spawn`).
    pub fn debug_spawn(&mut self) -> u64 {
        let (id, spawn) = self.world.spawn_on_base();
        log::info!(
            "отладочный игрок {id} заспавнен в '{}' {spawn:?}",
            self.world.location_name(self.world.base)
        );
        id
    }

    /// Отладка: перенести игрока в точку локации (без переходов и пузырей).
    pub fn debug_teleport(&mut self, player_id: u64, location: LocationId, position: Vec3) -> bool {
        let Some(from) = self.world.player_location(player_id) else {
            return false;
        };
        if !self.world.locations.contains_key(&location) {
            return false;
        }
        let yaw = self
            .world
            .locations
            .get_mut(&from)
            .and_then(|runtime| runtime.simulation.player_state(player_id))
            .map(|state| state.yaw)
            .unwrap_or(0.0);
        let change = LocationChange {
            player_id,
            from,
            to: location,
            position,
            yaw,
        };
        self.apply_location_changes(vec![change]);
        self.world.player_location(player_id) == Some(location)
    }

    /// Отладка: выдать предмет игроку по ключу каталога (консоль `loot`).
    pub fn debug_give(
        &mut self,
        player_id: u64,
        item_key: &str,
        count: u32,
    ) -> Result<u32, String> {
        let Some(location) = self.world.player_location(player_id) else {
            return Err(format!("игрок {player_id} не найден"));
        };
        let Some(simulation) = self
            .world
            .locations
            .get_mut(&location)
            .map(|runtime| &mut runtime.simulation)
        else {
            return Err(format!("локация {location} не найдена"));
        };
        let Some(item) = simulation.catalog_index_of(item_key) else {
            return Err(format!("предмет '{item_key}' не найден в каталоге"));
        };
        Ok(simulation.give_stack(player_id, Stack::new(item, count)))
    }

    /// Один шаг цикла: приём → фиксированные шаги всех локаций одним `dt` →
    /// переходы и пузыри → снапшоты (30 Гц) → таймауты → отправка отложенных.
    pub fn poll(&mut self, dt: Duration) {
        self.now += dt;
        self.receive_all();
        self.world.advance(dt);
        if self.world_mode {
            let tick = self.world.tick();
            let mut changes = self.world.check_transfers();
            changes.extend(self.world.check_bubbles(tick));
            self.apply_location_changes(changes);
        }
        if self.world.tick() - self.last_snapshot_tick >= SNAPSHOT_EVERY_TICKS {
            self.last_snapshot_tick = self.world.tick();
            self.send_snapshots();
        }
        self.check_timeouts();
        self.flush_outbox();
    }

    /// Применяет переносы мира: состояние игрока едет в симуляцию цели,
    /// клиент получает `LocationChanged`, приземление на базу сдаёт инвентарь
    /// в склад.
    fn apply_location_changes(&mut self, changes: Vec<LocationChange>) {
        for change in changes {
            if !self.world.apply_change(&change) {
                continue;
            }
            log::info!(
                "игрок {} перешёл из '{}' в '{}'",
                change.player_id,
                self.world.location_name(change.from),
                self.world.location_name(change.to)
            );
            if let Some(addr) = self.addr_of(change.player_id) {
                self.send(
                    addr,
                    Message::LocationChanged {
                        location: change.to,
                        position: change.position.to_array(),
                        yaw: change.yaw,
                    },
                );
            }
            if change.to == self.world.base {
                let moved = self.world.deposit_on_base(change.player_id);
                if moved > 0 {
                    log::info!("склад: игрок {} сдал {moved} стопок", change.player_id);
                    self.send_stash_update(change.player_id);
                }
            }
        }
    }

    fn receive_all(&mut self) {
        let mut buf = [0u8; RECV_BUF_SIZE];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    self.stats.received += 1;
                    match decode_message::<Message>(&buf[..len]) {
                        Ok(message) => self.handle_message(addr, message),
                        Err(error) => {
                            log::debug!("пакет от {addr} не декодирован: {error}");
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    log::debug!("recv: {error}");
                    break;
                }
            }
        }
    }

    fn handle_message(&mut self, addr: SocketAddr, message: Message) {
        match message {
            Message::ClientHello {
                protocol_version,
                content_hash,
                items_hash,
                name,
            } => self.handle_hello(addr, protocol_version, content_hash, items_hash, name),
            Message::InputPacket { inputs } => self.handle_inputs(addr, inputs),
            Message::Interact { tick } => self.handle_interact(addr, tick),
            Message::InventoryAction { tick, action } => {
                self.handle_inventory_action(addr, tick, action);
            }
            Message::StashAction { tick, action } => {
                self.handle_stash_action(addr, tick, action);
            }
            Message::ThrowBolt { tick } => self.handle_throw_bolt(addr, tick),
            Message::Ping { nonce } => {
                if let Some(client) = self.clients.get_mut(&addr) {
                    client.last_seen = self.now;
                }
                let server_tick = self.world.tick();
                self.send(addr, Message::Pong { nonce, server_tick });
            }
            Message::ClientBye => self.remove_client(addr, "вышел"),
            other => {
                log::debug!("игнорируем {} от {addr}", other.kind());
            }
        }
    }

    fn handle_hello(
        &mut self,
        addr: SocketAddr,
        protocol_version: u32,
        content_hash: u64,
        items_hash: u64,
        name: String,
    ) {
        // Повтор Hello (Welcome мог потеряться) — повторяем Welcome.
        if let Some(client) = self.clients.get_mut(&addr) {
            client.last_seen = self.now;
            let player_id = client.player_id;
            self.send_welcome(addr, player_id);
            return;
        }

        if self.clients.len() >= self.config.max_players {
            log::info!("отказ {addr}: сессия заполнена");
            self.send(
                addr,
                Message::ServerReject {
                    reason: RejectReason::SessionFull,
                },
            );
            return;
        }

        if let Err(reason) = check_client_hello(
            protocol_version,
            content_hash,
            items_hash,
            self.content_hash,
            self.items_hash,
        ) {
            log::info!("отказ {addr}: {reason:?}");
            self.send(addr, Message::ServerReject { reason });
            return;
        }

        let name = clamp_name(&name);
        let (player_id, _) = self.world.spawn_on_base();
        log::info!(
            "игрок '{name}' (id {player_id}) подключился с {addr} в '{}' [{} игроков]",
            self.world.location_name(self.world.base),
            self.clients.len() + 1
        );
        self.names.insert(player_id, name.clone());
        self.clients.insert(
            addr,
            ClientState {
                player_id,
                name,
                addr,
                last_seen: self.now,
                last_interact_tick: 0,
                last_action_tick: 0,
            },
        );
        self.send_welcome(addr, player_id);
    }

    fn send_welcome(&mut self, addr: SocketAddr, player_id: u64) {
        let server_tick = self.world.tick();
        let location = self
            .world
            .player_location(player_id)
            .unwrap_or(self.world.base);
        let spawn = self
            .world
            .locations
            .get(&location)
            .map(|runtime| runtime.simulation.spawn_point())
            .unwrap_or(Vec3::ZERO)
            .to_array();
        self.send(
            addr,
            Message::ServerWelcome {
                player_id,
                server_tick,
                spawn,
                location,
            },
        );
    }

    /// Симуляция локации игрока — маршрутизация инпутов и действий.
    fn simulation_of_player(&mut self, player_id: u64) -> Option<&mut Simulation> {
        let location = self.world.player_location(player_id)?;
        self.world
            .locations
            .get_mut(&location)
            .map(|runtime| &mut runtime.simulation)
    }

    fn handle_inputs(&mut self, addr: SocketAddr, inputs: Vec<ClientInput>) {
        let Some(client) = self.clients.get_mut(&addr) else {
            log::debug!("InputPacket от неизвестного адреса {addr} — игнор");
            return;
        };
        client.last_seen = self.now;
        let player_id = client.player_id;

        let Some(simulation) = self.simulation_of_player(player_id) else {
            log::debug!("InputPacket от игрока {player_id} без локации — игнор");
            return;
        };

        // Берём только последние INPUT_REDUNDANCY инпутов (redundancy против
        // потерь) и санитизируем: ничего нефинитного/запредельного в sim.
        let skip = inputs.len().saturating_sub(INPUT_REDUNDANCY);
        for mut input in inputs.into_iter().skip(skip) {
            sanitize_input(&mut input);
            simulation.queue_input(player_id, input);
        }
    }

    /// Обыск/подбор (E): повторные пакеты с тем же tick отбрасываются —
    /// клиент шлёт действие с redundancy, но обработать его нужно один раз.
    fn handle_interact(&mut self, addr: SocketAddr, tick: u64) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_interact_tick {
            return;
        }
        client.last_interact_tick = tick;
        let player_id = client.player_id;
        if let Some(simulation) = self.simulation_of_player(player_id) {
            simulation.queue_action(player_id, PlayerAction::Interact);
        }
    }

    /// Действие с инвентарём: та же дедупликация по tick, что у `Interact`.
    fn handle_inventory_action(&mut self, addr: SocketAddr, tick: u64, action: InventoryAction) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        if let Some(simulation) = self.simulation_of_player(player_id) {
            simulation.queue_action(player_id, PlayerAction::Inventory(action));
        }
    }

    /// Действие со складом базы (M4d): только на базе; дедупликация по tick
    /// общим счётчиком с прочими действиями.
    fn handle_stash_action(&mut self, addr: SocketAddr, tick: u64, action: StashAction) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        if self.world.player_location(player_id) != Some(self.world.base) {
            log::debug!("склад: игрок {player_id} не на базе — действие отброшено");
            return;
        }
        match action {
            StashAction::Take { index } => self.stash_take(player_id, index),
            StashAction::Put { slot } => self.stash_put(player_id, slot),
        }
    }

    /// Взятие со склада: сколько влезет в инвентарь, остаток остаётся.
    fn stash_take(&mut self, player_id: u64, index: u16) {
        let index = usize::from(index);
        let Some(stack) = self
            .world
            .stashes
            .get(&player_id)
            .and_then(|stash| stash.get(index))
            .copied()
        else {
            return;
        };
        let base = self.world.base;
        let Some(simulation) = self
            .world
            .locations
            .get_mut(&base)
            .map(|runtime| &mut runtime.simulation)
        else {
            return;
        };
        let moved = simulation.give_stack(player_id, stack);
        if moved == 0 {
            return;
        }
        if let Some(stash) = self.world.stashes.get_mut(&player_id) {
            if moved == stack.count {
                stash.remove(index);
            } else if let Some(entry) = stash.get_mut(index) {
                entry.count -= moved;
            }
        }
        self.send_stash_update(player_id);
    }

    /// Положение ячейки инвентаря в склад (стопки не сливаются).
    fn stash_put(&mut self, player_id: u64, slot: u8) {
        let base = self.world.base;
        let Some(simulation) = self
            .world
            .locations
            .get_mut(&base)
            .map(|runtime| &mut runtime.simulation)
        else {
            return;
        };
        let Some(stack) = simulation.take_inventory_slot(player_id, usize::from(slot)) else {
            return;
        };
        self.world.stashes.entry(player_id).or_default().push(stack);
        self.send_stash_update(player_id);
    }

    /// Полное состояние склада игроку (после сдачи/взятия/положения).
    fn send_stash_update(&mut self, player_id: u64) {
        let items: Vec<ItemStackNet> = self
            .world
            .stash_items(player_id)
            .iter()
            .map(|stack| ItemStackNet {
                item: stack.item,
                count: u16::try_from(stack.count).unwrap_or(u16::MAX),
            })
            .collect();
        if let Some(addr) = self.addr_of(player_id) {
            self.send(addr, Message::StashUpdate { items });
        }
    }

    /// Бросок болта (G): дедупликация по `tick`, общий счётчик с действиями.
    fn handle_throw_bolt(&mut self, addr: SocketAddr, tick: u64) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        if let Some(simulation) = self.simulation_of_player(player_id) {
            simulation.queue_action(player_id, PlayerAction::ThrowBolt);
        }
    }

    /// Снапшоты по локациям: сбор состояния всех локаций, затем персональная
    /// рассылка — клиент видит только свою локацию и её события.
    fn send_snapshots(&mut self) {
        let server_tick = self.world.tick();
        let ids: Vec<LocationId> = self.world.locations.keys().copied().collect();
        let mut per_location: Vec<(LocationId, LocationSnapshot)> = Vec::with_capacity(ids.len());
        let mut net_updates: Vec<NetUpdates> = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(runtime) = self.world.locations.get_mut(&id) else {
                continue;
            };
            let players = runtime.simulation.snapshot();
            let loot = runtime.simulation.snapshot_loot();
            let mut corpses = runtime.simulation.snapshot_corpses();
            // Имена жертв живут в сессии и переживают отключение.
            for corpse in &mut corpses {
                if corpse.name.is_empty() {
                    if let Some(name) = self.names.get(&corpse.owner) {
                        corpse.name = name.clone();
                    }
                }
            }
            let bolts = runtime.simulation.snapshot_bolts();
            let events = enrich_events(runtime.simulation.take_events(), &self.names);
            net_updates.push(runtime.simulation.take_net_updates());
            per_location.push((
                id,
                LocationSnapshot {
                    players,
                    loot,
                    corpses,
                    bolts,
                    events,
                },
            ));
        }

        let recipients: Vec<(SocketAddr, u64)> = self
            .clients
            .values()
            .map(|client| (client.addr, client.player_id))
            .collect();
        for (addr, player_id) in recipients {
            let Some(location) = self.world.player_location(player_id) else {
                continue;
            };
            let Some((_, snapshot)) = per_location.iter().find(|(id, _)| *id == location) else {
                continue;
            };
            // ack персональный: сколько инпутов этого игрока уже учтено.
            let acked_input_tick = self
                .world
                .locations
                .get_mut(&location)
                .and_then(|runtime| runtime.simulation.last_input_tick(player_id))
                .unwrap_or(0);
            self.send(
                addr,
                Message::Snapshot {
                    server_tick,
                    acked_input_tick,
                    players: snapshot.players.clone(),
                    loot: snapshot.loot.clone(),
                    corpses: snapshot.corpses.clone(),
                    bolts: snapshot.bolts.clone(),
                    events: snapshot.events.clone(),
                },
            );
        }

        for updates in net_updates {
            self.send_net_updates(updates);
        }
    }

    /// Персональные апдейты: состояние инвентаря владельцу, содержимое
    /// открытого контейнера — открывшему его игроку.
    fn send_net_updates(&mut self, updates: NetUpdates) {
        for (player_id, slots) in updates.inventory {
            if let Some(addr) = self.addr_of(player_id) {
                self.send(addr, Message::InventoryUpdate { slots });
            }
        }
        for (player_id, update) in updates.containers {
            if let Some(addr) = self.addr_of(player_id) {
                self.send(
                    addr,
                    Message::ContainerUpdate {
                        container: update.container,
                        items: update.items,
                    },
                );
            }
        }
    }

    fn addr_of(&self, player_id: u64) -> Option<SocketAddr> {
        self.clients
            .iter()
            .find(|(_, client)| client.player_id == player_id)
            .map(|(addr, _)| *addr)
    }

    fn check_timeouts(&mut self) {
        let timed_out: Vec<SocketAddr> = self
            .clients
            .iter()
            .filter(|(_, client)| self.now.saturating_sub(client.last_seen) > SERVER_TIMEOUT)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in timed_out {
            self.remove_client(addr, "таймаут");
        }
    }

    fn remove_client(&mut self, addr: SocketAddr, reason: &str) {
        if let Some(client) = self.clients.remove(&addr) {
            self.world.remove_player(client.player_id);
            log::info!(
                "игрок '{}' (id {}) удалён: {reason}, в сессии {}",
                client.name,
                client.player_id,
                self.clients.len()
            );
        }
    }

    fn send(&mut self, addr: SocketAddr, message: Message) {
        let kind = message.kind();
        let bytes = match encode_message(&message) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::error!("{kind} не закодирован: {error}");
                return;
            }
        };

        if self.should_drop_outgoing() {
            self.stats.dropped += 1;
            log::debug!("[диагностика] {kind} для {addr} потерян");
            return;
        }

        if self.config.out_latency.is_zero() {
            self.send_now(addr, &bytes, kind);
        } else {
            self.outbox
                .push_back((self.now + self.config.out_latency, addr, bytes));
        }
    }

    fn send_now(&mut self, addr: SocketAddr, bytes: &[u8], kind: &'static str) {
        match self.socket.send_to(bytes, addr) {
            Ok(_) => self.stats.sent += 1,
            Err(error) => log::debug!("{kind} для {addr} не отправлен: {error}"),
        }
    }

    fn flush_outbox(&mut self) {
        while let Some((send_at, _, _)) = self.outbox.front() {
            if *send_at > self.now {
                break;
            }
            let (_, addr, bytes) = self.outbox.pop_front().expect("проверено выше");
            self.send_now(addr, &bytes, "отложенный пакет");
        }
    }

    fn should_drop_outgoing(&mut self) -> bool {
        let pct = self.config.out_loss_pct.clamp(0.0, 100.0);
        if pct <= 0.0 {
            return false;
        }
        // xorshift32: детерминированная последовательность для smoke-прогонов.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / 16_777_216.0 * 100.0 < pct
    }
}

/// Держит инпуты в разумных границах: NaN/бесконечности → 0, оси [-1, 1],
/// углы за тик не больше 90°. Не анти-чит, а защита предсказуемости движения.
fn sanitize_input(input: &mut ClientInput) {
    let clamp_unit = |value: f32| {
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    let clamp_look = |value: f32| {
        if value.is_finite() {
            value.clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2)
        } else {
            0.0
        }
    };
    input.move_forward = clamp_unit(input.move_forward);
    input.move_right = clamp_unit(input.move_right);
    input.yaw_delta = clamp_look(input.yaw_delta);
    input.pitch_delta = clamp_look(input.pitch_delta);
}

/// Обрезает имя до `MAX_NAME_LEN` байт по границе символа.
fn clamp_name(name: &str) -> String {
    if name.len() <= MAX_NAME_LEN {
        return name.to_string();
    }
    let mut end = MAX_NAME_LEN;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог предметов из контента репозитория (cwd теста — crate root).
    fn test_catalog() -> ItemCatalog {
        ItemCatalog::load_ron("../../game/content/items/items.ron").expect("каталог предметов")
    }

    #[test]
    fn sanitize_clamps_and_defuses_input() {
        let mut input = ClientInput {
            move_forward: 17.0,
            move_right: -3.5,
            yaw_delta: f32::NAN,
            pitch_delta: 9.0,
            ..ClientInput::new(1)
        };
        sanitize_input(&mut input);
        assert_eq!(input.move_forward, 1.0);
        assert_eq!(input.move_right, -1.0);
        assert_eq!(input.yaw_delta, 0.0);
        assert!((input.pitch_delta - std::f32::consts::FRAC_PI_2).abs() < 1e-6);

        let mut input = ClientInput {
            move_forward: f32::INFINITY,
            pitch_delta: f32::NEG_INFINITY,
            ..ClientInput::new(2)
        };
        sanitize_input(&mut input);
        assert_eq!(input.move_forward, 0.0);
        assert_eq!(input.pitch_delta, 0.0);
    }

    #[test]
    fn name_clamps_to_byte_budget_on_char_boundary() {
        let long = "я".repeat(64); // 128 байт
        let clamped = clamp_name(&long);
        assert!(clamped.len() <= MAX_NAME_LEN);
        assert_eq!(clamped, "я".repeat(16)); // 32 байта

        let short = "сталкер";
        assert_eq!(clamp_name(short), short);
    }

    #[test]
    fn loss_rng_is_deterministic_per_seed() {
        let scene = Scene::demo_zone();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let config = SessionConfig {
            out_loss_pct: 50.0,
            rng_seed: 12345,
            ..SessionConfig::default()
        };
        let mut session = Session::new(socket, &scene, test_catalog(), config).unwrap();
        let rolls_a: Vec<bool> = (0..16).map(|_| session.should_drop_outgoing()).collect();
        let mut session = Session::new(
            UdpSocket::bind("127.0.0.1:0").unwrap(),
            &scene,
            test_catalog(),
            SessionConfig {
                out_loss_pct: 50.0,
                rng_seed: 12345,
                ..SessionConfig::default()
            },
        )
        .unwrap();
        let rolls_b: Vec<bool> = (0..16).map(|_| session.should_drop_outgoing()).collect();
        assert_eq!(rolls_a, rolls_b);
        assert!(
            rolls_a.iter().any(|drop| *drop),
            "при 50% потерь должны быть дропы"
        );
    }

    #[test]
    fn bubble_destination_prefers_closer_non_base_locations() {
        let registry = LocationRegistry::builtin();
        let scenes = Scene::builtin_world();
        let mut world = World::new(registry, scenes, &test_catalog(), 7).expect("мир");
        // Из Свалки пузырь тянет в Кордон (ближе к базе), не на базу.
        let (target, position, _) = world.bubble_destination(zn_scene::LOCATION_SVALKA);
        assert_eq!(target, zn_scene::LOCATION_KORDON);
        assert!(
            position.z > 10.0,
            "точка прибытия — у ворот базы: {position:?}"
        );
        // В Кордоне ближе базы никого нет: свои прибытия, «почти под базу».
        let (target, _, _) = world.bubble_destination(zn_scene::LOCATION_KORDON);
        assert_eq!(target, zn_scene::LOCATION_KORDON);
    }

    #[test]
    fn bubble_respawn_returns_to_free_candidate() {
        let registry = LocationRegistry::builtin();
        let scenes = Scene::builtin_world();
        let mut world = World::new(registry, scenes, &test_catalog(), 7).expect("мир");
        let location = zn_scene::LOCATION_SVALKA;
        let candidates = world.locations[&location].scene.bubbles.len();
        let before = world.locations[&location].bubbles.active.len();
        assert!(before >= BUBBLE_COUNT_MIN.min(candidates));

        // «Используем» один пузырь и откладываем возврат на 100-й тик.
        {
            let field = &mut world.locations.get_mut(&location).unwrap().bubbles;
            field.active.remove(0);
            field.pending.push(100);
        }
        world.respawn_bubbles(100);
        let field = &world.locations[&location].bubbles;
        assert_eq!(field.active.len(), before, "пузырь вернулся");
        assert!(field.pending.is_empty(), "очередь возврата пуста");
        let mut unique = field.active.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), field.active.len(), "без дублей точек");
        assert!(field.active.iter().all(|candidate| *candidate < candidates));
    }
}
