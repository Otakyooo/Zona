use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use zn_content::ItemCatalog;
use zn_net::{
    check_client_hello, decode_message, encode_message, AuthHello, BaseActionKind, BoltNet,
    ClientInput, CorpseNet, GameEvent, GroundItemNet, InventoryAction, ItemStackNet, Message,
    PlayerNetState, RejectReason, StashAction, INPUT_REDUNDANCY, MAX_NAME_LEN, SERVER_TIMEOUT,
    SIMULATION_HZ, SNAPSHOT_HZ,
};
use zn_scene::{
    distances_to_base, world_content_hash, LocationId, LocationInfo, LocationRegistry, Scene,
    StationKind, LOCATION_BASE,
};
use zn_sim::{NetUpdates, PlayerAction, Simulation, Stack, Vec3, ANOMALY_HEART_HEIGHT};

mod db;

pub use db::{AccountSummary, Database, DB_FILE_DEFAULT};

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

/// Выброс (M4d-3b): цикл ожидание → предупреждение → активная фаза.
/// Во время активной фазы игроки вне базы теряют HP (база безопасна).
const BLOWOUT_WAIT_TICKS: u64 = 180 * SIMULATION_HZ as u64;
const BLOWOUT_WARNING_TICKS: u64 = 30 * SIMULATION_HZ as u64;
const BLOWOUT_ACTIVE_TICKS: u64 = 15 * SIMULATION_HZ as u64;
/// Урон выброса: раз в полсекунды по 4 HP (8 HP/с на игрока).
const BLOWOUT_DAMAGE_INTERVAL_TICKS: u64 = SIMULATION_HZ as u64 / 2;
const BLOWOUT_DAMAGE_PER_HIT: u16 = 4;

/// Легаси-файл стэшей (M4d-2): в M5 только читается для миграции в БД.
pub const STASH_FILE_DEFAULT: &str = "stashes.ron";
/// Версия схемы легаси-файла стэшей (M4d-2).
const STASH_FILE_VERSION: u32 = 1;
/// Период автосохранения складов аккаунтов (30 с виртуального времени сессии).
const STASH_FLUSH_INTERVAL: Duration = Duration::from_secs(30);
/// Аккаунты (M5): защита от перебора пароля per-addr — после N отказов
/// проверка не выполняется, пока не истечёт окно.
const AUTH_MAX_FAILURES: u32 = 5;
const AUTH_FAIL_WINDOW: Duration = Duration::from_secs(10);

/// База между рейдами (M5-2): стартовый баланс кошелька нового аккаунта (RU).
pub const START_WALLET: u32 = 200;
/// Верстак: прибавка к доле сохраняемой добычи за каждый уровень.
const WORKBENCH_EXTRACT_BONUS: f32 = 0.1;
/// Медпункт: лечение игроков на базе — HP в секунду за каждый уровень.
const MEDPOST_HEAL_PER_SEC: f32 = 2.0;
/// Сейф: доля вещей, спасённых с трупа при смерти, за каждый уровень.
const SAFE_SALVAGE_FRACTION: f32 = 0.25;
/// Торговец: покупает по цене каталога, продаёт офферы с наценкой.
const TRADER_BUY_MARKUP: u32 = 2;
/// Максимальный уровень станции (верстак/медпункт/сейф).
const FACILITY_MAX_LEVEL: u8 = 3;
/// Цена апгрейда станции на следующий уровень: база × (текущий + 1).
const UPGRADE_COST_BASE: u32 = 300;

/// Период выбросов по умолчанию (M4d-3b): ожидание → предупреждение → актив.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlowoutTiming {
    pub wait_ticks: u64,
    pub warning_ticks: u64,
    pub active_ticks: u64,
}

impl Default for BlowoutTiming {
    fn default() -> Self {
        Self {
            wait_ticks: BLOWOUT_WAIT_TICKS,
            warning_ticks: BLOWOUT_WARNING_TICKS,
            active_ticks: BLOWOUT_ACTIVE_TICKS,
        }
    }
}

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
    /// Файл БД аккаунтов (M5). `None` — персист выключен: аккаунты живут
    /// только в памяти сессии (режим тестов).
    pub db_file: Option<PathBuf>,
    /// Гостевой вход запрещён (M5-3): `auth = None` → отказ. Для реального
    /// деплоя персист обязан быть включён — иначе аккаунты не сохраняются
    /// между перезапусками.
    pub auth_required: bool,
    /// Тайминги выброса (M4d-3b); в тестах укорачиваются.
    pub blowout: BlowoutTiming,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_players: zn_net::MAX_PLAYERS_PER_SESSION,
            out_latency: Duration::ZERO,
            out_loss_pct: 0.0,
            rng_seed: 0x9E37_79B9,
            db_file: None,
            auth_required: false,
            blowout: BlowoutTiming::default(),
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
    pub position: Vec3,
    pub yaw: f32,
}

/// Сработавшая экстракция (M4d-2): игрок в круге `Scene::extractions`.
#[derive(Clone, Copy, Debug)]
struct Extraction {
    player_id: u64,
    from: LocationId,
    /// Доля сохраняемой добычи из круга сцены.
    percent: f32,
    /// Якорь круга — сюда падает остаток добычи.
    position: Vec3,
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
    /// Фаза выброса (M4d-3b) и tick её окончания.
    blowout_phase: BlowoutPhase,
    blowout_until_tick: u64,
    /// Длительности фаз выброса (тесты укорачивают через SessionConfig).
    blowout_timing: BlowoutTiming,
}

/// Фазы выброса (M4d-3b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlowoutPhase {
    /// Между выбросами.
    Idle,
    /// Предупреждение: клиенты видят таймер, урона нет.
    Warning,
    /// Активная фаза: игроки вне базы теряют HP.
    Active,
}

impl World {
    /// Мир из реестра и загруженных сцен (основной режим сервера).
    fn new(
        registry: LocationRegistry,
        scenes: Vec<(LocationId, Scene)>,
        catalog: &ItemCatalog,
        rng_seed: u32,
        timing: BlowoutTiming,
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
            blowout_phase: BlowoutPhase::Idle,
            blowout_until_tick: timing.wait_ticks,
            blowout_timing: timing,
        };
        world.seed_bubbles();
        Ok(world)
    }

    /// Одиночная сцена (тесты и `--level`): локация 0, мир без переходов,
    /// пузырей и сдачи на базу.
    fn single(scene: &Scene, catalog: ItemCatalog, rng_seed: u32) -> Self {
        let timing = BlowoutTiming::default();
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
            blowout_phase: BlowoutPhase::Idle,
            blowout_until_tick: timing.wait_ticks,
            blowout_timing: timing,
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

    /// Круги экстракции (M4d-2): живой игрок в круге эвакуируется. Возвращает
    /// сработавшие круги для применения (депозит + перенос на базу).
    fn check_extractions(&mut self) -> Vec<Extraction> {
        let mut found = Vec::new();
        let ids: Vec<LocationId> = self.locations.keys().copied().collect();
        for &id in &ids {
            let Some(runtime) = self.locations.get_mut(&id) else {
                continue;
            };
            if runtime.scene.extractions.is_empty() {
                continue;
            }
            let LocationRuntime {
                scene, simulation, ..
            } = runtime;
            for (player_id, position) in player_positions(simulation) {
                if let Some(extraction) = scene
                    .extractions
                    .iter()
                    .find(|extraction| extraction.contains(position))
                {
                    found.push(Extraction {
                        player_id,
                        from: id,
                        percent: extraction.percent,
                        position: extraction.position,
                    });
                }
            }
        }
        found
    }

    /// Выполняет эвакуацию: делит инвентарь по проценту круга, сохраняемую
    /// часть кладёт в склад сессии, остаток роняет на землю в круге.
    /// Возвращает (сохранено стопок, потеряно стопок).
    fn extract_player(&mut self, extraction: Extraction) -> (usize, usize) {
        let Some(runtime) = self.locations.get_mut(&extraction.from) else {
            return (0, 0);
        };
        let (saved, lost) = runtime
            .simulation
            .extract_inventory(extraction.player_id, extraction.percent);
        for stack in &lost {
            runtime
                .simulation
                .drop_stack_at(*stack, extraction.position);
        }
        let saved_count = saved.len();
        let lost_count = lost.len();
        if !saved.is_empty() {
            self.stashes
                .entry(extraction.player_id)
                .or_default()
                .extend(saved);
        }
        (saved_count, lost_count)
    }

    /// Фаза выброса (M4d-3b): ожидание → предупреждение → активная.
    fn advance_blowout(&mut self) {
        let tick = self.tick();
        if tick < self.blowout_until_tick {
            return;
        }
        let timing = self.blowout_timing;
        let (next, duration) = match self.blowout_phase {
            BlowoutPhase::Idle => (BlowoutPhase::Warning, timing.warning_ticks),
            BlowoutPhase::Warning => (BlowoutPhase::Active, timing.active_ticks),
            BlowoutPhase::Active => (BlowoutPhase::Idle, timing.wait_ticks),
        };
        self.blowout_phase = next;
        self.blowout_until_tick = tick + duration;
        log::info!(
            "выброс: фаза {:?}, длительность {} с",
            next,
            duration / SIMULATION_HZ as u64
        );
    }

    /// Текущее состояние выброса для снапшотов: `(активен, секунд до конца)`.
    /// Между выбросами — `(false, 0.0)`: клиент гасит баннер.
    fn blowout_state(&self) -> (bool, f32) {
        let seconds_left =
            self.blowout_until_tick.saturating_sub(self.tick()) as f32 / SIMULATION_HZ as f32;
        match self.blowout_phase {
            BlowoutPhase::Idle => (false, 0.0),
            BlowoutPhase::Warning => (false, seconds_left),
            BlowoutPhase::Active => (true, seconds_left),
        }
    }

    /// Урон выброса (M4d-3b): каждые `BLOWOUT_DAMAGE_INTERVAL_TICKS` все живые
    /// игроки вне базы теряют HP. База — безопасная зона.
    fn apply_blowout_damage(&mut self) {
        if self.blowout_phase != BlowoutPhase::Active {
            return;
        }
        let tick = self.tick();
        if !tick.is_multiple_of(BLOWOUT_DAMAGE_INTERVAL_TICKS) {
            return;
        }
        let base = self.base;
        for (id, runtime) in self.locations.iter_mut() {
            if *id == base {
                continue;
            }
            runtime.simulation.surge_damage(BLOWOUT_DAMAGE_PER_HIT);
        }
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

/// Цена апгрейда станции с текущего `level` на следующий (M5-2).
fn upgrade_cost(level: u8) -> u32 {
    UPGRADE_COST_BASE * u32::from(level + 1)
}

/// Название станции по индексу facilities (M5-2): уведомления и логи.
fn facility_name(facility: usize) -> &'static str {
    match facility {
        0 => "верстак",
        1 => "медпункт",
        _ => "сейф",
    }
}

/// Подставляет имена в события смертей и нокдаунов: симуляция знает только
/// id, а клиентам нужен килл-фид.
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
            GameEvent::KnockedDown {
                player,
                ticks,
                player_name,
            } => GameEvent::KnockedDown {
                player,
                player_name: if player_name.is_empty() {
                    names.get(&player).cloned().unwrap_or_default()
                } else {
                    player_name
                },
                ticks,
            },
            GameEvent::GotUp {
                player,
                player_name,
            } => GameEvent::GotUp {
                player,
                player_name: if player_name.is_empty() {
                    names.get(&player).cloned().unwrap_or_default()
                } else {
                    player_name
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
    /// Аккаунт игрока (M5): имя из `AuthHello`. `None` — гостевой вход
    /// (склад живёт только в памяти сессии).
    account: Option<String>,
    /// `id` аккаунта в БД (ключ стэша/кошелька/апгрейдов). `None` — гость
    /// или персист выключен.
    account_id: Option<i64>,
    /// Аккаунт создан этим подключением (M5): уведомление в Welcome.
    account_created: bool,
    /// Персональное состояние базы (M5-2): кошелёк и уровни станций.
    base: BaseState,
}

/// Персональное состояние базы (M5-2): кошелёк, уровни станций и панель.
/// У гостей и при выключенном персисте живёт только в памяти сессии.
#[derive(Clone, Debug)]
struct BaseState {
    /// Баланс кошелька аккаунта (RU).
    wallet: u32,
    /// Уровни станций: [верстак, медпункт, сейф].
    facilities: [u8; 3],
    /// Открытая панель станции (индекс в `Scene::stations`); `None` — закрыта.
    station: Option<u8>,
    /// Дробный остаток лечения медпункта (HP < 1) между тиками.
    heal_debt: f32,
    /// Контейнеры трупов игрока, с которых сейф уже спас долю (раз за труп).
    salvaged: HashSet<u64>,
}

impl BaseState {
    fn new(wallet: u32, facilities: [u8; 3]) -> Self {
        Self {
            wallet,
            facilities,
            station: None,
            heal_debt: 0.0,
            salvaged: HashSet::new(),
        }
    }
}

/// Состояние локации для рассылки (двухфазный сбор — снапшоты шлются уже
/// без заимствований мира).
struct LocationSnapshot {
    players: Vec<PlayerNetState>,
    loot: Vec<GroundItemNet>,
    corpses: Vec<CorpseNet>,
    bolts: Vec<BoltNet>,
    /// Активные пузыри этой локации (квантованные якоря).
    bubbles: Vec<[i16; 3]>,
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
    /// Персист аккаунтов (M5): БД со стэшами (и в M5-2 — кошельками/апгрейдами).
    /// `None` — персист выключен (`--no-db`, режим тестов).
    db: Option<Database>,
    /// Есть несохранённые изменения складов.
    stash_dirty: bool,
    last_stash_flush: Duration,
    /// Неудачные попытки входа per-addr: (число отказов, начало окна).
    auth_failures: HashMap<SocketAddr, (u32, Duration)>,
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
        let world = World::new(
            registry.clone(),
            scenes,
            &catalog,
            config.rng_seed,
            config.blowout,
        )
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
        // Персист аккаунтов (M5): БД + миграция легаси-стэшей (M4d-2).
        let (db, migrated) = match config.db_file.clone() {
            Some(path) => {
                let database = Database::open(&path)
                    .map_err(|error| io::Error::other(format!("бд {}: {error}", path.display())))?;
                // Миграция только на пустую БД: повторный старт не двоит.
                let mut migrated = 0usize;
                if database.account_count().unwrap_or(0) == 0 {
                    let legacy_path = path.with_file_name(STASH_FILE_DEFAULT);
                    let legacy = load_stash_file(&legacy_path);
                    if !legacy.is_empty() {
                        match database.import_legacy_stashes(&legacy) {
                            Ok(count) => {
                                migrated = count;
                                let imported = legacy_path.with_extension("ron.imported");
                                match std::fs::rename(&legacy_path, &imported) {
                                    Ok(()) => log::info!(
                                        "бд: миграция — {count} профилей из {} (исходник → {})",
                                        legacy_path.display(),
                                        imported.display()
                                    ),
                                    Err(error) => log::warn!(
                                        "бд: миграция — {count} профилей из {}, но переименовать не удалось: {error}",
                                        legacy_path.display()
                                    ),
                                }
                            }
                            Err(error) => {
                                log::error!(
                                    "бд: миграция из {} не удалась: {error}",
                                    legacy_path.display()
                                )
                            }
                        }
                    }
                }
                (Some(database), migrated)
            }
            None => (None, 0),
        };
        if let Some(path) = &config.db_file {
            log::info!(
                "бд: {} (аккаунтов: {}, миграция: {migrated})",
                path.display(),
                db.as_ref().map_or(0, |db| db.account_count().unwrap_or(0))
            );
        }
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
            db,
            stash_dirty: false,
            last_stash_flush: Duration::ZERO,
            auth_failures: HashMap::new(),
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

    /// Сводка аккаунтов для консоли `accounts` (M5-3). Персист выключен —
    /// пустой список: аккаунты живут только в сессиях.
    pub fn account_summaries(&self) -> Vec<AccountSummary> {
        let Some(db) = self.db.as_ref() else {
            return Vec::new();
        };
        match db.list_accounts() {
            Ok(accounts) => accounts,
            Err(error) => {
                log::warn!("бд: не удалось выгрузить список аккаунтов: {error}");
                Vec::new()
            }
        }
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
    /// переходы и пузыри → экстракции → снапшоты (30 Гц) → таймауты → отправка
    /// отложенных.
    pub fn poll(&mut self, dt: Duration) {
        self.now += dt;
        self.receive_all();
        self.world.advance(dt);
        if self.world_mode {
            let tick = self.world.tick();
            let mut changes = self.world.check_transfers();
            changes.extend(self.world.check_bubbles(tick));
            self.apply_location_changes(changes);
            let extractions = self.world.check_extractions();
            if !extractions.is_empty() {
                self.apply_extractions(extractions);
            }
            // Выброс (M4d-3b): смена фаз и поражение вне базы.
            self.world.advance_blowout();
            self.world.apply_blowout_damage();
            // База между рейдами (M5-2): реген медпункта и страховка сейфа.
            self.apply_medpost_heal(dt);
            self.apply_safe_salvage();
        }
        self.flush_accounts_if_dirty();
        if self.world.tick() - self.last_snapshot_tick >= SNAPSHOT_EVERY_TICKS {
            self.last_snapshot_tick = self.world.tick();
            self.send_snapshots();
        }
        self.check_timeouts();
        self.flush_outbox();
    }

    /// Пишет склады аккаунтов в БД, если есть изменения и включён персист.
    fn flush_accounts_if_dirty(&mut self) {
        if !self.stash_dirty || self.now < self.last_stash_flush + STASH_FLUSH_INTERVAL {
            return;
        }
        self.flush_accounts();
    }

    /// Принудительно пишет склады аккаунтов в БД (quit консоли, отключение).
    pub fn flush_accounts(&mut self) {
        self.stash_dirty = false;
        self.last_stash_flush = self.now;
        let Some(db) = self.db.as_ref() else {
            return;
        };
        for (player_id, items) in &self.world.stashes {
            if let Some(account_id) = self.account_id_of(*player_id) {
                if let Err(error) = db.save_stash(account_id, items) {
                    log::error!("бд: склад игрока {player_id} не сохранён: {error}");
                }
            }
        }
    }

    /// `id` аккаунта в БД по игроку (ключ персиста).
    fn account_id_of(&self, player_id: u64) -> Option<i64> {
        self.clients
            .values()
            .find(|client| client.player_id == player_id)
            .and_then(|client| client.account_id)
    }

    /// Экстракция (M4d-2): депозит доли добычи, событие в снапшот локации,
    /// перенос на базу. Круги сцены читает клиент локально — рендер без сети.
    /// Верстак (M5-2): каждый его уровень прибавляет к доле сохраняемой добычи.
    fn apply_extractions(&mut self, extractions: Vec<Extraction>) {
        for mut extraction in extractions {
            let workbench = self.facility_of(extraction.player_id, 0);
            if workbench > 0 {
                extraction.percent =
                    (extraction.percent + f32::from(workbench) * WORKBENCH_EXTRACT_BONUS).min(1.0);
            }
            let name = self
                .names
                .get(&extraction.player_id)
                .cloned()
                .unwrap_or_default();
            let (saved, lost) = self.world.extract_player(extraction);
            // Событие уходит всем в локации: остальные видят эвакуацию.
            if let Some(runtime) = self.world.locations.get_mut(&extraction.from) {
                runtime.simulation.push_event(GameEvent::Extracted {
                    player: extraction.player_id,
                    player_name: name.clone(),
                    percent: extraction.percent,
                });
            }
            // Перенос на базу: спавн базовой сцены, инвентарь уже разделён.
            let base = self.world.base;
            let (spawn, spawn_yaw) = self
                .world
                .locations
                .get(&base)
                .map(|runtime| (runtime.scene.spawn, runtime.scene.spawn_yaw))
                .unwrap_or((Vec3::ZERO, 0.0));
            let change = LocationChange {
                player_id: extraction.player_id,
                from: extraction.from,
                to: base,
                position: spawn,
                yaw: spawn_yaw,
            };
            self.apply_location_changes(vec![change]);
            self.stash_dirty = true;
            log::info!(
                "эвакуация: игрок {name} из '{}' сохранил {saved} стопок ({}%), потеряно {lost}",
                self.world.location_name(extraction.from),
                (extraction.percent * 100.0).round()
            );
        }
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
                }
                // Даже пустая сдача освежает панель: клиент всегда видит склад.
                self.send_stash_update(change.player_id);
                // Кошелёк и уровни станций — тоже актуальны после рейда.
                self.send_base_update(change.player_id);
            } else if change.from == self.world.base {
                // Уход с базы закрывает панель станции: на выезде её нет.
                let open = self
                    .clients
                    .values_mut()
                    .find(|client| client.player_id == change.player_id)
                    .is_some_and(|client| client.base.station.take().is_some());
                if open {
                    self.send_base_update(change.player_id);
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
                auth,
            } => self.handle_hello(addr, protocol_version, content_hash, items_hash, name, auth),
            Message::InputPacket { inputs } => self.handle_inputs(addr, inputs),
            Message::Interact { tick } => self.handle_interact(addr, tick),
            Message::InventoryAction { tick, action } => {
                self.handle_inventory_action(addr, tick, action);
            }
            Message::StashAction { tick, action } => {
                self.handle_stash_action(addr, tick, action);
            }
            Message::BaseInteract { tick, station } => {
                self.handle_base_interact(addr, tick, station);
            }
            Message::BaseAction { tick, action } => {
                self.handle_base_action(addr, tick, action);
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
        auth: Option<AuthHello>,
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

        // Гостевой вход запрещён (M5-3): `--auth-required` закрывает сервер для
        // анонимных подключений. Аккаунты при выключенном персисте живут только
        // в сессии — для реального деплоя БД обязательна.
        if auth.is_none() && self.config.auth_required {
            log::info!("отказ {addr}: гостевой вход запрещён (--auth-required)");
            self.send(
                addr,
                Message::ServerReject {
                    reason: RejectReason::AuthFailed,
                },
            );
            return;
        }

        // Аккаунты (M5): гость / проверка пароля / claim миграции /
        // авто-регистрация. БД выключена — аккаунт живёт только в сессии.
        let mut account = None;
        let mut account_id = None;
        let mut account_created = false;
        let mut stash_from_db = None;
        if let Some(auth) = &auth {
            if self.auth_rate_limited(addr) {
                log::warn!("отказ {addr}: слишком много неудачных входов");
                self.send(
                    addr,
                    Message::ServerReject {
                        reason: RejectReason::AuthFailed,
                    },
                );
                return;
            }
            let found = self
                .db
                .as_ref()
                .and_then(|db| db.find_account(&auth.account));
            match found {
                Some(known) => match &known.password_hash {
                    // Известный аккаунт с паролем — сверяем дайджест.
                    Some(password_hash) => {
                        if !verify_password(&auth.password_digest, password_hash) {
                            self.note_auth_failure(addr);
                            log::info!("отказ {addr}: неверный пароль '{}'", known.name);
                            self.send(
                                addr,
                                Message::ServerReject {
                                    reason: RejectReason::AuthFailed,
                                },
                            );
                            return;
                        }
                        if let Some(db) = self.db.as_ref() {
                            let _ = db.touch(&known.name);
                            stash_from_db = db.load_stash(known.id).ok();
                        }
                        account = Some(known.name);
                        account_id = Some(known.id);
                    }
                    // Мигрированный из stashes.ron: первый вход задаёт пароль.
                    None => {
                        let claimed = self
                            .db
                            .as_ref()
                            .and_then(|db| hash_password(&auth.password_digest).map(|h| (db, h)))
                            .and_then(|(db, hash)| db.set_password(&auth.account, &hash).ok());
                        if claimed.is_some() {
                            log::info!("аккаунт '{}': пароль задан (миграция)", auth.account);
                            if let Some(db) = self.db.as_ref() {
                                stash_from_db = db.load_stash(known.id).ok();
                            }
                        } else {
                            log::error!("бд: не удалось задать пароль аккаунту '{}'", auth.account);
                        }
                        account = Some(auth.account.clone());
                        account_id = Some(known.id);
                    }
                },
                // Неизвестный аккаунт — авто-регистрация (как профили в M4d-2).
                None => match self.db.as_ref() {
                    Some(db) => {
                        let hash = hash_password(&auth.password_digest);
                        match db.insert_account(&auth.account, hash.as_deref()) {
                            Ok(id) => {
                                log::info!("аккаунт '{}' создан", auth.account);
                                account = Some(auth.account.clone());
                                account_id = Some(id);
                                account_created = true;
                            }
                            Err(error) => {
                                log::error!("бд: аккаунт '{}' не создан: {error}", auth.account);
                                account = Some(auth.account.clone());
                            }
                        }
                    }
                    None => {
                        log::info!(
                            "персист выключен — '{}' подключается без сохранения",
                            auth.account
                        );
                        account = Some(auth.account.clone());
                    }
                },
            }
        }

        // Кошелёк и уровни станций (M5-2): из БД аккаунта; гость/выключенный
        // персист — стартовый баланс и нули (живут в памяти сессии).
        let mut wallet = START_WALLET;
        let mut facilities = [0u8; 3];
        if let (Some(db), Some(id)) = (self.db.as_ref(), account_id) {
            wallet = db.load_wallet(id).unwrap_or(START_WALLET);
            facilities = db.load_facilities(id).unwrap_or_default();
        }

        let (player_id, _) = self.world.spawn_on_base();
        // Склад с прошлых сессий едет с игроком.
        if let Some(items) = stash_from_db {
            if !items.is_empty() {
                self.world.stashes.insert(player_id, items);
                log::info!(
                    "стэши: аккаунт '{}' восстановил склад игрока {player_id}",
                    account.as_deref().unwrap_or("?")
                );
            }
        }
        log::info!(
            "игрок '{name}' (id {player_id}{}) подключился с {addr} в '{}' [{} игроков]",
            account
                .as_ref()
                .map(|account| format!(", аккаунт '{account}'"))
                .unwrap_or_default(),
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
                account,
                account_id,
                account_created,
                base: BaseState::new(wallet, facilities),
            },
        );
        self.send_welcome(addr, player_id);
        // Кошелёк и уровни станций — сразу: HUD не ждёт первой панели.
        self.send_base_update(player_id);
    }

    /// Превышен ли лимит неудачных входов с адреса (окно `AUTH_FAIL_WINDOW`).
    fn auth_rate_limited(&self, addr: SocketAddr) -> bool {
        self.auth_failures.get(&addr).is_some_and(|(count, since)| {
            *count >= AUTH_MAX_FAILURES && self.now.saturating_sub(*since) < AUTH_FAIL_WINDOW
        })
    }

    /// Учитывает неудачный вход: окно сбрасывает счётчик.
    fn note_auth_failure(&mut self, addr: SocketAddr) {
        let entry = self.auth_failures.entry(addr).or_insert((0, self.now));
        if self.now.saturating_sub(entry.1) >= AUTH_FAIL_WINDOW {
            *entry = (1, self.now);
        } else {
            entry.0 += 1;
        }
    }

    fn send_welcome(&mut self, addr: SocketAddr, player_id: u64) {
        let account_created = self
            .clients
            .get(&addr)
            .is_some_and(|client| client.account_created);
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
                account_created,
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

    /// Сцена локации мира (круги экстракции, переходы — для клиента и тестов).
    pub fn world_scene(&self, location: LocationId) -> &Scene {
        &self.world.locations[&location].scene
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

    // --- База между рейдами (M5-2): станции, торговец, апгрейды -------------

    /// Уровень станции игрока: 0 — верстак, 1 — медпункт, 2 — сейф.
    /// Неизвестный игрок (отключился между тиками) — 0.
    fn facility_of(&self, player_id: u64, facility: usize) -> u8 {
        self.clients
            .values()
            .find(|client| client.player_id == player_id)
            .map_or(0, |client| client.base.facilities[facility])
    }

    /// Вид станции базы по индексу в `Scene::stations` (сцена базы).
    fn station_kind_at(&self, station: u8) -> Option<StationKind> {
        let base = self.world.base;
        self.world
            .locations
            .get(&base)
            .and_then(|runtime| runtime.scene.stations.get(usize::from(station)))
            .map(|station| station.kind)
    }

    /// Офферы торговца станции: (индекс предмета в каталоге, количество).
    fn trader_offers(&self, station: u8) -> Vec<ItemStackNet> {
        let base = self.world.base;
        let Some(runtime) = self.world.locations.get(&base) else {
            return Vec::new();
        };
        let Some(scene_station) = runtime.scene.stations.get(usize::from(station)) else {
            return Vec::new();
        };
        scene_station
            .offers
            .iter()
            .filter_map(|offer| {
                let item = runtime.simulation.catalog_index_of(&offer.item)?;
                Some(ItemStackNet {
                    item,
                    count: u16::try_from(offer.count).unwrap_or(u16::MAX),
                })
            })
            .collect()
    }

    /// Открытие/закрытие панели станции (M5-2): E внутри круга. Повторное
    /// взаимодействие с той же станцией закрывает панель. Дедупликация по
    /// `tick` — общим счётчиком с прочими действиями.
    fn handle_base_interact(&mut self, addr: SocketAddr, tick: u64, station: u8) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        let was_open = client.base.station;
        // Вне базы станций нет: панель не открывается.
        if self.world.player_location(player_id) != Some(self.world.base) {
            log::debug!("база: игрок {player_id} не на базе — панель не открыта");
            return;
        }
        // Станция должна существовать в сцене, а игрок — стоять в её круге.
        let base = self.world.base;
        let position = self
            .world
            .locations
            .get_mut(&base)
            .and_then(|runtime| runtime.simulation.player_state(player_id))
            .map(|state| state.position);
        let near_station = position.is_some_and(|position| {
            self.world
                .locations
                .get(&base)
                .and_then(|runtime| runtime.scene.stations.get(usize::from(station)))
                .is_some_and(|scene_station| scene_station.contains(position))
        });
        if !near_station {
            self.send_base_notice(player_id, "подойди к станции ближе");
            return;
        }
        // Тумблер: повторное E у той же станции закрывает панель.
        let open = if was_open == Some(station) {
            None
        } else {
            Some(station)
        };
        if let Some(client) = self.clients.get_mut(&addr) {
            client.base.station = open;
        }
        self.send_base_update(player_id);
    }

    /// Действие в панели станции (M5-2): продажа/покупка у торговца, апгрейд
    /// верстака/медпункта/сейфа. Дедупликация по `tick`, как у stash-действий.
    fn handle_base_action(&mut self, addr: SocketAddr, tick: u64, action: BaseActionKind) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        let Some(station) = client.base.station else {
            self.send_base_notice(player_id, "панель станции закрыта");
            return;
        };
        if self.world.player_location(player_id) != Some(self.world.base) {
            return;
        }
        let kind = self.station_kind_at(station);
        match action {
            BaseActionKind::Sell { from_stash, slot } => {
                if kind != Some(StationKind::Trader) {
                    self.send_base_notice(player_id, "продаёт только торговец");
                    return;
                }
                self.trader_sell(player_id, from_stash, slot);
            }
            BaseActionKind::Buy { offer } => {
                if kind != Some(StationKind::Trader) {
                    self.send_base_notice(player_id, "продаёт только торговец");
                    return;
                }
                self.trader_buy(player_id, station, offer);
            }
            BaseActionKind::Upgrade => self.upgrade_facility(player_id, kind),
        }
        // Панель освежается после любого действия: кошелёк/уровни могли
        // измениться, офферы остались те же.
        self.send_base_update(player_id);
    }

    /// Продажа торговцу: стопка из склада или инвентаря по цене каталога.
    fn trader_sell(&mut self, player_id: u64, from_stash: bool, slot: u8) {
        let slot = usize::from(slot);
        // Стопка и её цена читаются без изменений: отказ по пустой ячейке
        // или нулевой цене не должен ничего трогать.
        let stash_stack = if from_stash {
            self.world
                .stashes
                .get(&player_id)
                .and_then(|stash| stash.get(slot))
                .copied()
        } else {
            None
        };
        let base = self.world.base;
        let peeked = self.world.locations.get_mut(&base).and_then(|runtime| {
            let stack =
                stash_stack.or_else(|| runtime.simulation.inventory_slot(player_id, slot))?;
            let value = runtime.simulation.item_value_rub(stack.item).unwrap_or(0);
            Some((stack, value))
        });
        let Some((stack, value)) = peeked else {
            self.send_base_notice(player_id, "ячейка пуста");
            return;
        };
        if value == 0 {
            self.send_base_notice(player_id, "торговец не берёт этот хлам");
            return;
        }
        let price = u64::from(value) * u64::from(stack.count);

        // Забираем стопку: склад — из списка, инвентарь — из ячейки.
        let taken = if from_stash {
            self.world.stashes.get_mut(&player_id).is_some_and(|stash| {
                slot < stash.len() && {
                    stash.remove(slot);
                    true
                }
            })
        } else {
            self.world
                .locations
                .get_mut(&base)
                .and_then(|runtime| runtime.simulation.take_inventory_slot(player_id, slot))
                .is_some()
        };
        if !taken {
            return;
        }

        self.add_wallet(player_id, price);
        if from_stash {
            self.send_stash_update(player_id);
        }
        self.send_base_notice(player_id, &format!("продано: +{price} RU"));
        log::info!("торговец: игрок {player_id} продал стопку за {price} RU");
    }

    /// Покупка оффера торговца: цена каталога × наценка × количество.
    /// Что не влезло в инвентарь — уходит на склад базы.
    fn trader_buy(&mut self, player_id: u64, station: u8, offer: u8) {
        // Оффер и его цена читаются без изменений: отказ ничего не трогает.
        let base = self.world.base;
        let goods = self.world.locations.get(&base).and_then(|runtime| {
            let scene_station = runtime.scene.stations.get(usize::from(station))?;
            let scene_offer = scene_station.offers.get(usize::from(offer))?;
            let item = runtime.simulation.catalog_index_of(&scene_offer.item)?;
            let value = runtime.simulation.item_value_rub(item).unwrap_or(0);
            Some((item, scene_offer.count, value))
        });
        let Some((item, count, value)) = goods else {
            self.send_base_notice(player_id, "торговец не предлагает этого");
            return;
        };
        let price = u64::from(value) * u64::from(TRADER_BUY_MARKUP) * u64::from(count);
        if price == 0 {
            self.send_base_notice(player_id, "торговец не продаёт хлам");
            return;
        }
        let wallet = self
            .clients
            .values()
            .find(|client| client.player_id == player_id)
            .map_or(0, |client| client.base.wallet);
        if u64::from(wallet) < price {
            self.send_base_notice(player_id, "не хватает RU");
            return;
        }

        let base = self.world.base;
        let moved = self
            .world
            .locations
            .get_mut(&base)
            .map(|runtime| {
                runtime
                    .simulation
                    .give_stack(player_id, Stack::new(item, count))
            })
            .unwrap_or(0);
        if moved == 0 {
            self.send_base_notice(player_id, "инвентарь полон");
            return;
        }
        // Остаток партии — на склад: покупка на базе, склад под рукой.
        if moved < count {
            self.world
                .stashes
                .entry(player_id)
                .or_default()
                .push(Stack::new(item, count - moved));
            self.stash_dirty = true;
            self.send_stash_update(player_id);
        }
        self.spend_wallet(player_id, price);
        self.send_base_notice(player_id, &format!("куплено: {} шт", moved));
        log::info!("торговец: игрок {player_id} купил {moved} шт за {price} RU");
    }

    /// Апгрейд станции базы: уровень +1 за RU. Торговец не улучшается.
    fn upgrade_facility(&mut self, player_id: u64, kind: Option<StationKind>) {
        let facility = match kind {
            Some(StationKind::Workbench) => 0,
            Some(StationKind::Medpost) => 1,
            Some(StationKind::Safe) => 2,
            _ => {
                self.send_base_notice(player_id, "тут нечего улучшать");
                return;
            }
        };
        let (level, wallet) = self
            .clients
            .values()
            .find(|client| client.player_id == player_id)
            .map_or((0, 0), |client| {
                (client.base.facilities[facility], client.base.wallet)
            });
        if level >= FACILITY_MAX_LEVEL {
            self.send_base_notice(player_id, "уже максимальный уровень");
            return;
        }
        let cost = upgrade_cost(level);
        if u64::from(wallet) < u64::from(cost) {
            self.send_base_notice(player_id, "не хватает RU");
            return;
        }
        if let Some(client) = self
            .clients
            .values_mut()
            .find(|client| client.player_id == player_id)
        {
            client.base.facilities[facility] = level + 1;
        }
        self.spend_wallet(player_id, u64::from(cost));
        self.save_facilities(player_id);
        let name = facility_name(facility);
        self.send_base_notice(player_id, &format!("{name} → уровень {}", level + 1));
        log::info!(
            "база: игрок {player_id} улучшил {name} до уровня {}",
            level + 1
        );
    }

    /// Медпункт (M5-2): лечение игроков на базе — HP/с за уровень станции.
    fn apply_medpost_heal(&mut self, dt: Duration) {
        let base = self.world.base;
        let seconds = dt.as_secs_f32();
        // Кого лечим: подключённые игроки на базе (дробный остаток копится
        // в `heal_debt` — лечение называется реже раза в тик).
        let mut jobs: Vec<(u64, u16)> = Vec::new();
        for client in self.clients.values_mut() {
            if self.world.player_location(client.player_id) != Some(base) {
                client.base.heal_debt = 0.0;
                continue;
            }
            let medpost = client.base.facilities[1];
            if medpost == 0 {
                continue;
            }
            client.base.heal_debt += f32::from(medpost) * MEDPOST_HEAL_PER_SEC * seconds;
            let amount = client.base.heal_debt.floor();
            if amount >= 1.0 {
                client.base.heal_debt -= amount;
                jobs.push((client.player_id, amount as u16));
            }
        }
        if jobs.is_empty() {
            return;
        }
        if let Some(runtime) = self.world.locations.get_mut(&base) {
            for (player_id, amount) in jobs {
                runtime.simulation.heal(player_id, amount);
            }
        }
    }

    /// Сейф (M5-2): при смерти игрока доля вещей с трупа уходит в склад
    /// страховки — раз за труп, сила спасения — уровень станции.
    fn apply_safe_salvage(&mut self) {
        let mut jobs: Vec<(LocationId, u64, u64)> = Vec::new();
        for (id, runtime) in &self.world.locations {
            for corpse in runtime.simulation.snapshot_corpses() {
                if corpse.owner == 0 {
                    continue;
                }
                let already = self
                    .clients
                    .values()
                    .find(|client| client.player_id == corpse.owner)
                    .is_some_and(|client| client.base.salvaged.contains(&corpse.container));
                if !already {
                    jobs.push((*id, corpse.container, corpse.owner));
                }
            }
        }
        for (location, container, owner) in jobs {
            let fraction = f32::from(self.facility_of(owner, 2)) * SAFE_SALVAGE_FRACTION;
            let salvaged = self
                .world
                .locations
                .get_mut(&location)
                .map(|runtime| runtime.simulation.salvage_corpse(container, fraction))
                .unwrap_or_default();
            // Трупом занялись всё равно: даже без сейфа второй раз не
            // обрабатываем — иначе уровень, купленный позже, спасёт дважды.
            if let Some(client) = self
                .clients
                .values_mut()
                .find(|client| client.player_id == owner)
            {
                client.base.salvaged.insert(container);
            }
            if salvaged.is_empty() {
                continue;
            }
            let count = salvaged.len();
            self.world
                .stashes
                .entry(owner)
                .or_default()
                .extend(salvaged);
            self.stash_dirty = true;
            self.send_stash_update(owner);
            self.send_base_notice(owner, &format!("сейф: сохранено {count} стопок"));
            log::info!(
                "сейф: игрок {owner} спас со трупа {count} стопок (уровень {})",
                self.facility_of(owner, 2)
            );
        }
    }

    /// Полное состояние базы игроку: панель (или её закрытие), кошелёк,
    /// уровни станций и офферы торговца.
    fn send_base_update(&mut self, player_id: u64) {
        let Some(addr) = self.addr_of(player_id) else {
            return;
        };
        let Some(client) = self.clients.get(&addr) else {
            return;
        };
        let station = client.base.station;
        let wallet = client.base.wallet;
        let facilities = client.base.facilities;
        // Офферы — только у торговца: у прочих станций панель без товара.
        let offers = match station {
            Some(station) if self.station_kind_at(station) == Some(StationKind::Trader) => {
                self.trader_offers(station)
            }
            _ => Vec::new(),
        };
        self.send(
            addr,
            Message::BaseUpdate {
                station,
                wallet,
                facilities,
                offers,
            },
        );
    }

    /// Всплывающее уведомление базы: отказ или итог действия.
    fn send_base_notice(&mut self, player_id: u64, text: &str) {
        if let Some(addr) = self.addr_of(player_id) {
            self.send(
                addr,
                Message::BaseNotice {
                    text: text.to_string(),
                },
            );
        }
    }

    /// Зачисление на кошелёк (продажа) с немедленным сохранением в БД.
    fn add_wallet(&mut self, player_id: u64, amount: u64) {
        let Some(client) = self
            .clients
            .values_mut()
            .find(|client| client.player_id == player_id)
        else {
            return;
        };
        client.base.wallet = client
            .base
            .wallet
            .saturating_add(u32::try_from(amount).unwrap_or(u32::MAX));
        self.save_wallet(player_id);
    }

    /// Списание с кошелька (покупка/апгрейд) с немедленным сохранением.
    fn spend_wallet(&mut self, player_id: u64, amount: u64) {
        let Some(client) = self
            .clients
            .values_mut()
            .find(|client| client.player_id == player_id)
        else {
            return;
        };
        client.base.wallet = client
            .base
            .wallet
            .saturating_sub(u32::try_from(amount).unwrap_or(u32::MAX));
        self.save_wallet(player_id);
    }

    /// Пишет кошелёк игрока в БД (если есть аккаунт и персист включён).
    fn save_wallet(&self, player_id: u64) {
        let Some(account_id) = self.account_id_of(player_id) else {
            return;
        };
        let Some(wallet) = self
            .clients
            .values()
            .find(|client| client.player_id == player_id)
            .map(|client| client.base.wallet)
        else {
            return;
        };
        if let Some(db) = self.db.as_ref() {
            if let Err(error) = db.save_wallet(account_id, wallet) {
                log::error!("бд: кошелёк игрока {player_id} не сохранён: {error}");
            }
        }
    }

    /// Пишет уровни станций игрока в БД (если есть аккаунт и персист).
    fn save_facilities(&self, player_id: u64) {
        let Some(account_id) = self.account_id_of(player_id) else {
            return;
        };
        let Some(facilities) = self
            .clients
            .values()
            .find(|client| client.player_id == player_id)
            .map(|client| client.base.facilities)
        else {
            return;
        };
        if let Some(db) = self.db.as_ref() {
            if let Err(error) = db.save_facilities(account_id, facilities) {
                log::error!("бд: станции игрока {player_id} не сохранены: {error}");
            }
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
        let blowout = self.world.blowout_state();
        let ids: Vec<LocationId> = self.world.locations.keys().copied().collect();
        let mut per_location: Vec<(LocationId, LocationSnapshot)> = Vec::with_capacity(ids.len());
        let mut net_updates: Vec<NetUpdates> = Vec::with_capacity(ids.len());
        for id in ids {
            // Пузыри — из BubbleField локации (набор меняется со временем).
            let bubbles: Vec<[i16; 3]> = self
                .world
                .active_bubbles(id)
                .into_iter()
                .map(|anchor| zn_net::quantize_pos(anchor.to_array()))
                .collect();
            let Some(runtime) = self.world.locations.get_mut(&id) else {
                continue;
            };
            // Выброс (M4d-3b): свежий таймер в каждом снапшоте — клиент не
            // считает сам, только рисует.
            let (active, seconds_left) = blowout;
            runtime.simulation.push_event(GameEvent::Blowout {
                active,
                seconds_left,
            });
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
                    bubbles,
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
                    bubbles: snapshot.bubbles.clone(),
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
            // Персист (M5): склад аккаунта уезжает из сессии в БД.
            if let Some(account_id) = client.account_id {
                if let Some(items) = self.world.stashes.remove(&client.player_id) {
                    if let Some(db) = self.db.as_ref() {
                        if let Err(error) = db.save_stash(account_id, &items) {
                            log::error!(
                                "бд: склад игрока {} не сохранён: {error}",
                                client.player_id
                            );
                        }
                    }
                }
                // Кошелёк и уровни станций (M5-2): страховка на отключении —
                // в обычной игре они пишутся сразу при тратах.
                if let Some(db) = self.db.as_ref() {
                    if let Err(error) = db.save_wallet(account_id, client.base.wallet) {
                        log::error!(
                            "бд: кошелёк игрока {} не сохранён: {error}",
                            client.player_id
                        );
                    }
                    if let Err(error) = db.save_facilities(account_id, client.base.facilities) {
                        log::error!(
                            "бд: станции игрока {} не сохранены: {error}",
                            client.player_id
                        );
                    }
                }
            }
            self.auth_failures.remove(&addr);
            self.world.remove_player(client.player_id);
            log::info!(
                "игрок '{}' (id {}{}) удалён: {reason}, в сессии {}",
                client.name,
                client.player_id,
                client
                    .account
                    .as_ref()
                    .map(|account| format!(", аккаунт '{account}'"))
                    .unwrap_or_default(),
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

// ─── легаси-стэши (M4d-2): только чтение для миграции в БД ──────────────

/// Файл стэшей: версия схемы и записи по профилям. Профиль — ключ игрока
/// (`ZN_PROFILE` клиента); в M5 файл читается один раз для миграции.
#[derive(Serialize, Deserialize)]
struct StashFile {
    version: u32,
    stashes: Vec<StashRecord>,
}

/// Склад одного профиля: стопки без слияния (порядок — как в панели).
#[derive(Serialize, Deserialize)]
struct StashRecord {
    profile: String,
    /// Пары (индекс предмета в каталоге, количество).
    items: Vec<(u16, u32)>,
}

/// Читает файл стэшей. Отсутствие файла — не ошибка (первый запуск); битый
/// файл — тоже не ошибка: предупреждение и пустой склад (fail-soft).
fn load_stash_file(path: &Path) -> HashMap<String, Vec<Stack>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    match ron::from_str::<StashFile>(&text) {
        Ok(file) if file.version == STASH_FILE_VERSION => file
            .stashes
            .into_iter()
            .map(|record| {
                (
                    record.profile,
                    record
                        .items
                        .into_iter()
                        .map(|(item, count)| Stack::new(item, count))
                        .collect(),
                )
            })
            .collect(),
        Ok(file) => {
            log::warn!(
                "стэши: версия файла {} != {} — склад не загружен",
                file.version,
                STASH_FILE_VERSION
            );
            HashMap::new()
        }
        Err(error) => {
            log::warn!("стэши: не удалось прочесть {}: {error}", path.display());
            HashMap::new()
        }
    }
}

// ─── пароли аккаунтов (M5) ──────────────────────────────────────────────

/// argon2id-хэш дайджеста пароля. Соль случайная на аккаунт; результат —
/// PHC-строка. `None` — argon2 недоступен (не должно случаться).
fn hash_password(digest: &[u8; 32]) -> Option<String> {
    let salt = SaltString::generate(&mut OsRng);
    match Argon2::default().hash_password(digest, &salt) {
        Ok(hash) => Some(hash.to_string()),
        Err(error) => {
            log::error!("argon2: не удалось захэшировать пароль: {error}");
            None
        }
    }
}

/// Проверка дайджеста против PHC-строки. Пустая/битая строка — отказ.
fn verify_password(digest: &[u8; 32], password_hash: &str) -> bool {
    if password_hash.is_empty() {
        return false;
    }
    PasswordHash::new(password_hash)
        .and_then(|hash| Argon2::default().verify_password(digest, &hash))
        .is_ok()
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
        let mut world = World::new(
            registry,
            scenes,
            &test_catalog(),
            7,
            BlowoutTiming::default(),
        )
        .expect("мир");
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
        let mut world = World::new(
            registry,
            scenes,
            &test_catalog(),
            7,
            BlowoutTiming::default(),
        )
        .expect("мир");
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

    #[test]
    fn world_extract_deposits_percent_and_drops_rest_in_circle() {
        let registry = LocationRegistry::builtin();
        let scenes = Scene::builtin_world();
        let mut world = World::new(
            registry,
            scenes,
            &test_catalog(),
            7,
            BlowoutTiming::default(),
        )
        .expect("мир");
        let (player_id, _) = world.spawn_on_base();
        let catalog = test_catalog();
        let medkit = catalog.index_of("medkit").unwrap();
        let canned = catalog.index_of("canned_food").unwrap();
        let bolt = catalog.index_of("bolt").unwrap();
        {
            let runtime = world.locations.get_mut(&world.base).unwrap();
            runtime
                .simulation
                .give_stack(player_id, Stack::new(medkit, 2));
            runtime
                .simulation
                .give_stack(player_id, Stack::new(canned, 2));
            runtime
                .simulation
                .give_stack(player_id, Stack::new(bolt, 4));
        }

        let extraction = Extraction {
            player_id,
            from: world.base,
            percent: 0.75,
            position: Vec3::new(2.0, 0.0, 3.0),
        };
        let (saved, lost) = world.extract_player(extraction);
        // Бюджет 1.65 кг из 2.2: medkit целиком, canned 1/1, bolt 3/1.
        assert_eq!((saved, lost), (3, 2));
        assert_eq!(
            world.stashes[&player_id],
            vec![
                Stack::new(medkit, 2),
                Stack::new(canned, 1),
                Stack::new(bolt, 3),
            ]
        );
        let loot = world.locations[&world.base].simulation.snapshot_loot();
        assert_eq!(loot.len(), 2, "остаток добычи лежит в круге");
        assert!(loot
            .iter()
            .any(|item| item.item == canned && item.count == 1));
        assert!(loot.iter().any(|item| item.item == bolt && item.count == 1));
    }

    #[test]
    fn world_check_extractions_finds_player_in_circle() {
        let registry = LocationRegistry::builtin();
        let scenes = Scene::builtin_world();
        let mut world = World::new(
            registry,
            scenes,
            &test_catalog(),
            7,
            BlowoutTiming::default(),
        )
        .expect("мир");
        let (player_id, _) = world.spawn_on_base();
        let circle = world.locations[&zn_scene::LOCATION_KORDON]
            .scene
            .extractions[0]
            .position;
        // Переносим игрока на Кордон и ставим точно в круг экстракции.
        let move_change = LocationChange {
            player_id,
            from: world.base,
            to: zn_scene::LOCATION_KORDON,
            position: Vec3::new(0.0, 0.0, 22.0),
            yaw: 0.0,
        };
        assert!(world.apply_change(&move_change));
        {
            let runtime = world.locations.get_mut(&zn_scene::LOCATION_KORDON).unwrap();
            let mut players = runtime.simulation.snapshot();
            let state = players
                .iter_mut()
                .find(|player| player.player_id == player_id)
                .expect("игрок на Кордоне");
            state.position = circle.to_array();
            runtime.simulation.restore_player(player_id, state);
        }
        let found = world.check_extractions();
        assert_eq!(found.len(), 1, "круг экстракции сработал");
        assert_eq!(found[0].player_id, player_id);
        assert_eq!(found[0].from, zn_scene::LOCATION_KORDON);
        assert!(found[0].percent > 0.0);
    }

    #[test]
    fn legacy_stash_file_reads_and_fails_soft() {
        let dir = std::env::temp_dir().join(format!("zn_stash_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stashes.ron");

        // Валидный легаси-файл читается (миграция в M5-1).
        std::fs::write(
            &path,
            "(version: 1, stashes: [(profile: \"alpha\", items: [(3, 30)])])",
        )
        .unwrap();
        let loaded = load_stash_file(&path);
        assert_eq!(
            loaded.get("alpha"),
            Some(&vec![Stack::new(3, 30)]),
            "легаси-запись читается"
        );

        // Битый файл — предупреждение и пустой склад, без паники.
        std::fs::write(&path, "(((").unwrap();
        assert!(load_stash_file(&path).is_empty());
        // Несовпадение версии — отказ загрузки.
        std::fs::write(&path, "(version: 99, stashes: [])").unwrap();
        assert!(load_stash_file(&path).is_empty());
        // Отсутствующий файл — не ошибка.
        assert!(load_stash_file(&dir.join("nope.ron")).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn password_hash_verifies_and_rejects() {
        let digest = zn_net::password_digest("zona-2026");
        let wrong = zn_net::password_digest("Zona-2026");

        let hash = hash_password(&digest).expect("argon2 доступен");
        assert!(hash.starts_with("$argon2"), "PHC-строка argon2id: {hash}");
        assert!(verify_password(&digest, &hash), "свой дайджест проходит");
        assert!(!verify_password(&wrong, &hash), "чужой дайджест — отказ");
        // Соль случайна: два хэша одного пароля differ.
        let other = hash_password(&digest).unwrap();
        assert_ne!(hash, other, "соль случайна на аккаунт");
        assert!(verify_password(&digest, &other));
        // Мусор и пустая строка — отказ, без паники.
        assert!(!verify_password(&digest, ""));
        assert!(!verify_password(&digest, "not-a-phc-string"));
    }
}
