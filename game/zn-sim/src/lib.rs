use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use glam::Vec3;
use zn_core::{Entity, FixedStep, Schedule, World, SIMULATION_HZ, SIMULATION_STEP};
use zn_net::GameEvent;
pub use zn_net::{ClientInput, PlayerNetState, FLAG_FIRE, FLAG_GROUNDED, FLAG_SPRINTING};
use zn_physics::{PhysicsWorld, PLAYER_HALF_HEIGHT, PLAYER_RADIUS};
use zn_scene::Scene;

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

/// Максимум отложенных инпутов на игрока (защита от переполнения при джиттере).
pub const INPUT_QUEUE_CAP: usize = 64;

/// Полуразмер невидимого пола «пустой» сцены (`Simulation::new`); верх — y = 0.
const EMPTY_SCENE_GROUND_HALF_EXTENT: f32 = 256.0;

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
    /// Раньше этого tick стрелять нельзя (кулдаун оружия).
    pub next_fire_tick: u64,
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
            next_fire_tick: 0,
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

/// Очередь событий боя (выстрелы, смерти) с прошлого снапшота; забирает
/// серверная сессия. Имена в `Death` заполняет сессия — симуляция их не знает.
#[derive(Clone, Debug, Default)]
pub struct CombatEvents(pub Vec<GameEvent>);

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
    /// (`solid`), точка спавна — из сцены. Пол должен быть объектом сцены —
    /// как в `demo_zone`.
    pub fn with_scene(scene: &Scene) -> Self {
        let mut simulation = Self::build(scene.spawn, scene.spawn_yaw, false);
        simulation.rebuild_static(scene);
        simulation
    }

    /// Пересобирает статические коллайдеры сцены и точку спавна: единый путь
    /// загрузки уровня (ручная пол-пластина пустой сцены не трогается).
    pub fn rebuild_static(&mut self, scene: &Scene) {
        self.spawn = scene.spawn;
        self.spawn_yaw = scene.spawn_yaw;
        if let Some(spawn_point) = self.world.get_resource_mut::<SpawnPoint>() {
            spawn_point.position = scene.spawn;
            spawn_point.yaw = scene.spawn_yaw;
        }
        let physics = self
            .world
            .get_resource_mut::<PhysicsWorld>()
            .expect("physics resource initialized in build()");
        physics.rebuild_static(&scene.objects);
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

    /// Убирает игрока и его буфер ввода. `false`, если игрока не было.
    pub fn despawn_player(&mut self, id: u64) -> bool {
        let entity = self
            .world
            .query::<(Entity, &PlayerState)>()
            .iter()
            .find(|(_, state)| state.id == id)
            .map(|(entity, _)| entity);
        let Some(entity) = entity else {
            return false;
        };
        self.clear_inputs(id);
        self.world.despawn(entity)
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
    schedule.add("player_move", move_players);
    schedule.add("player_combat", player_combat);
    schedule.add("player_respawn", player_respawn);
    schedule
}

fn tick_dt(world: &World) -> f32 {
    world
        .get_resource::<TickInfo>()
        .map(|info| info.dt)
        .unwrap_or_else(|| SIMULATION_STEP.as_secs_f32())
}

fn apply_input(world: &mut World) {
    let dt = tick_dt(world);

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

            if state.health == 0 {
                // Мёртвый может осматриваться, но не двигаться и не стрелять.
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

    let requests: Vec<(Entity, Vec3, Vec3)> = {
        let mut query = world.query::<(Entity, &PlayerState)>();
        query
            .iter()
            .filter(|(_, state)| state.health > 0)
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
    let shooters: Vec<(u64, Vec3, Vec3)> = world
        .query::<&PlayerState>()
        .iter()
        .filter(|state| state.health > 0 && state.firing && tick >= state.next_fire_tick)
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

    // Фаза 2: кулдаун, урон, смерти (мутабельные заимствования).
    let mut deaths: Vec<(u64, u64)> = Vec::new(); // (жертва, убийца)
    {
        let mut query = world.query::<&mut PlayerState>();
        for state in query.iter() {
            if fired.contains(&state.id) {
                state.next_fire_tick = tick + FIRE_INTERVAL_TICKS;
            }
            for (_, killer, damage) in hits.iter().filter(|(victim, _, _)| *victim == state.id) {
                if state.health == 0 {
                    // Добивание в том же тике смертью не считается.
                    continue;
                }
                state.health = state.health.saturating_sub(*damage);
                if state.health == 0 {
                    state.velocity = Vec3::ZERO;
                    state.sprinting = false;
                    state.firing = false;
                    state.respawn_at = tick + RESPAWN_DELAY_TICKS;
                    deaths.push((state.id, *killer));
                }
            }
        }
    }

    for (victim, killer) in deaths {
        events.push(GameEvent::Death {
            victim,
            victim_name: String::new(),
            killer,
            killer_name: String::new(),
        });
    }

    if let Some(queue) = world.get_resource_mut::<CombatEvents>() {
        queue.0.extend(events);
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
        }
    }
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
}
