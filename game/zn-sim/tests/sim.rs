use std::time::Duration;

use glam::Vec3;
use zn_net::{ClientInput, GameEvent, InventoryAction, ItemStackNet};
use zn_scene::{AnomalyKind, Scene, SceneAnomaly, SceneObject};
use zn_sim::{
    aim_forward, carry_limit_grams, crate_container_id, inventory_weight_grams,
    is_corpse_container, zone_damage, PlayerAction, Simulation, Stack,
    ANOMALY_DAMAGE_INTERVAL_TICKS, BOLT_THROW_COOLDOWN_TICKS, CORPSE_LIFETIME_TICKS, EYE_HEIGHT,
    FLAG_FIRE, FLAG_SPRINTING, MAX_CARRY_GRAMS, MAX_HEALTH, RESPAWN_DELAY_TICKS, ZONE_HEAD_MIN,
    ZONE_TORSO_MIN,
};

const SIM_STEPS: u64 = 60;

fn input(tick: u64, forward: f32, right: f32, jump: bool) -> ClientInput {
    ClientInput {
        tick,
        move_forward: forward,
        move_right: right,
        jump,
        ..ClientInput::default()
    }
}

#[test]
fn players_move_forward_relative_to_yaw() {
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);

    for tick in 0..SIM_STEPS {
        sim.queue_input(id, input(tick, 1.0, 0.0, false));
        sim.step_once();
    }

    let state = sim.player_state(id).unwrap();
    assert!(
        state.position.x > 4.0,
        "player did not move: {:?}",
        state.position
    );
    assert!(
        state.position.y.abs() < 0.05,
        "player left the ground: {}",
        state.position.y
    );
    assert!(state.grounded);
}

#[test]
fn strafe_right_moves_toward_screen_right() {
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);

    for tick in 0..SIM_STEPS {
        sim.queue_input(id, input(tick, 0.0, 1.0, false));
        sim.step_once();
    }

    let state = sim.player_state(id).unwrap();
    // Конвенция: при yaw = 0 «вправо» — это +Z (глубина экрана в zn-render).
    assert!(
        state.position.z > 4.0,
        "strafe should go +Z: {:?}",
        state.position
    );
    assert!(state.position.x.abs() < 0.01, "x = {}", state.position.x);
}

#[test]
fn positive_yaw_delta_turns_toward_screen_right() {
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);

    let mut turn = input(0, 0.0, 0.0, false);
    turn.yaw_delta = std::f32::consts::FRAC_PI_2;
    sim.queue_input(id, turn);
    sim.step_once();

    for tick in 1..SIM_STEPS {
        sim.queue_input(id, input(tick, 1.0, 0.0, false));
        sim.step_once();
    }

    let position = sim.player_state(id).unwrap().position;
    assert!(position.z > 4.0, "should walk +Z after turn: {position:?}");
    assert!(position.x.abs() < 0.5, "x = {}", position.x);
}

#[test]
fn players_move_independently() {
    let mut sim = Simulation::new();
    let a = sim.spawn_player(Vec3::ZERO);
    let b = sim.spawn_player(Vec3::new(0.0, 0.0, 50.0));

    for tick in 0..SIM_STEPS {
        sim.queue_input(a, input(tick, 1.0, 0.0, false));
        sim.queue_input(b, input(tick, 0.0, 1.0, false));
        sim.step_once();
    }

    let pa = sim.player_state(a).unwrap().position;
    let pb = sim.player_state(b).unwrap().position;
    assert!(pa.x > 4.0, "a = {pa:?}");
    assert!(pb.z > 54.0, "b should have strafed +Z: {pb:?}");
    assert_ne!(pa, pb);
}

#[test]
fn jump_rises_and_lands_again() {
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);

    sim.queue_input(id, input(0, 0.0, 0.0, true));
    sim.step_once();
    let airborne = sim.player_state(id).unwrap();
    assert!(!airborne.grounded);
    assert!(airborne.velocity.y > 0.0);

    for tick in 1..SIM_STEPS {
        sim.queue_input(id, input(tick, 0.0, 0.0, false));
        sim.step_once();
    }

    let landed = sim.player_state(id).unwrap();
    assert!(landed.grounded, "player never landed");
    // Небольшой зазор — offset кинематического контроллера rapier.
    assert!(landed.position.y.abs() < 0.05, "y = {}", landed.position.y);
}

#[test]
fn sprint_flag_increases_speed() {
    fn run(buttons: u32) -> f32 {
        let mut sim = Simulation::new();
        let id = sim.spawn_player(Vec3::ZERO);
        for tick in 0..120u64 {
            let mut command = input(tick, 1.0, 0.0, false);
            command.buttons = buttons;
            sim.queue_input(id, command);
            sim.step_once();
        }
        sim.player_state(id).unwrap().position.x
    }

    let walk = run(0);
    let sprint = run(FLAG_SPRINTING);
    assert!(
        sprint > walk + 3.0,
        "walk = {walk}, sprint = {sprint} — режим бега не ускоряет игрока"
    );
}

#[test]
fn simulation_is_deterministic() {
    fn run() -> Vec<[f32; 3]> {
        let mut sim = Simulation::new();
        let id = sim.spawn_player(Vec3::new(1.0, 0.0, 2.0));
        for tick in 0..120u64 {
            let command = ClientInput {
                tick,
                move_forward: (tick as f32 * 0.1).sin(),
                move_right: (tick as f32 * 0.05).cos(),
                yaw_delta: 0.01,
                pitch_delta: 0.0,
                jump: tick % 50 == 0,
                buttons: 0,
            };
            sim.queue_input(id, command);
            sim.step_once();
        }
        sim.snapshot().into_iter().map(|s| s.position).collect()
    }

    assert_eq!(run(), run());
}

#[test]
fn fixed_tick_accumulation_drives_ticks() {
    let mut sim = Simulation::new();
    let steps = sim.advance(Duration::from_millis(100));
    assert_eq!(steps, 5);
    assert_eq!(sim.tick(), 5);
}

#[test]
fn snapshot_covers_all_players_with_ids() {
    let mut sim = Simulation::new();
    let a = sim.spawn_player(Vec3::ZERO);
    let b = sim.spawn_player(Vec3::new(5.0, 0.0, 0.0));
    let snapshot = sim.snapshot();
    assert_eq!(snapshot.len(), 2);
    assert_eq!(sim.player_count(), 2);
    let ids: Vec<u64> = snapshot.iter().map(|s| s.player_id).collect();
    assert!(ids.contains(&a) && ids.contains(&b), "ids = {ids:?}");
}

#[test]
fn scene_colliders_block_movement() {
    let mut scene = Scene::new("wall");
    scene.spawn = Vec3::ZERO;
    scene.add(SceneObject::new(
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(20.0, 0.5, 20.0),
        glam::Vec4::ONE,
    ));
    scene.add(SceneObject::new(
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(0.5, 2.0, 4.0),
        glam::Vec4::ONE,
    ));

    let mut sim = Simulation::with_scene(&scene);
    assert_eq!(sim.static_collider_count(), 2);
    let spawn = sim.spawn_point();
    let id = sim.spawn_player(spawn);

    for tick in 0..120u64 {
        sim.queue_input(id, input(tick, 1.0, 0.0, false));
        sim.step_once();
    }

    let x = sim.player_state(id).unwrap().position.x;
    assert!(x < 1.9, "player walked through the wall: x = {x}");
    assert!(x > 1.0, "player never reached the wall: x = {x}");
}

#[test]
fn rebuild_static_replaces_scene_colliders() {
    let mut scene = Scene::new("reload");
    scene.add(SceneObject::new(
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(20.0, 1.0, 20.0),
        glam::Vec4::ONE,
    ));
    let mut sim = Simulation::with_scene(&scene);
    assert_eq!(sim.static_collider_count(), 1);

    // Новый состав сцены: старый коллайдер снимается, дублей нет.
    let mut next = Scene::new("reload-next");
    next.add(SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE));
    next.add(SceneObject::new(
        Vec3::new(3.0, 0.0, 0.0),
        Vec3::ONE,
        glam::Vec4::ONE,
    ));
    sim.rebuild_static(&next);
    assert_eq!(sim.static_collider_count(), 2);
}

#[test]
fn restore_and_replay_converges() {
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);
    let commands: Vec<ClientInput> = (0..35u64)
        .map(|tick| ClientInput {
            tick,
            move_forward: 1.0,
            yaw_delta: 0.01,
            ..ClientInput::default()
        })
        .collect();

    // Сервер: обработал все 35 инпутов; снапшот для клиента снят на tick 29
    // (ack = 29), а каноничное состояние — после 35.
    let mut snapshot_at_30: Option<zn_net::PlayerNetState> = None;
    for (index, command) in commands.iter().enumerate() {
        sim.queue_input(id, *command);
        sim.step_once();
        if index == 29 {
            snapshot_at_30 = sim.snapshot().into_iter().next();
        }
    }
    let authoritative = sim.player_state(id).unwrap();
    let snapshot = snapshot_at_30.expect("snapshot taken");

    // Клиент: восстановить снапшот и переиграть неподтверждённые инпуты 30..34.
    let mut replay = Simulation::new();
    let replay_id = replay.spawn_player(Vec3::ZERO);
    assert!(replay.restore_player(replay_id, &snapshot));
    assert_eq!(replay.last_input_tick(replay_id), None);
    for command in &commands[30..] {
        replay.queue_input(replay_id, *command);
        replay.step_once();
    }

    let replayed = replay.player_state(replay_id).unwrap();
    assert!(
        (replayed.position - authoritative.position).length() < 1e-5,
        "replay diverged: {:?} vs {:?}",
        replayed.position,
        authoritative.position
    );
    assert!((replayed.velocity - authoritative.velocity).length() < 1e-5);
    assert_eq!(replayed.grounded, authoritative.grounded);
}

#[test]
fn despawn_removes_player_and_inputs() {
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);
    sim.queue_input(id, input(0, 1.0, 0.0, false));
    sim.step_once();

    assert!(sim.despawn_player(id));
    assert_eq!(sim.player_count(), 0);
    assert_eq!(sim.last_input_tick(id), None);
    assert!(!sim.despawn_player(id));
}

// ── Перенос игрока между симуляциями (M4d) ──────────────────────────────

/// Перенос состояния между локациями: позиция/инвентарь едут целиком, id
/// сохраняется, новые id не сталкиваются; ячейки и весь инвентарь можно
/// вынимать под склад с обновлением сети.
#[test]
fn player_takes_state_between_simulations() {
    let catalog = test_catalog();
    let mut kordon = Simulation::with_scene_and_catalog(&Scene::kordon(), catalog.clone());
    let id = kordon.spawn_player(Vec3::new(0.0, 0.0, 22.0));
    let bolt = catalog.index_of("bolt").unwrap();
    let medkit = catalog.index_of("medkit").unwrap();
    assert_eq!(kordon.give_stack(id, Stack::new(bolt, 5)), 5);
    assert_eq!(kordon.give_stack(id, Stack::new(medkit, 1)), 1);

    let mut command = ClientInput::new(0);
    command.move_forward = 1.0;
    kordon.queue_input(id, command);
    kordon.step_once();
    let before = kordon.player_state(id).unwrap();
    assert!(before.velocity != Vec3::ZERO, "игрок в движении");
    let updates = kordon.take_net_updates();
    assert_eq!(updates.inventory.len(), 2, "выдачи пометили инвентарь");

    let moved = kordon.take_player_state(id).expect("игрок был в Кордоне");
    assert_eq!(moved.id, id);
    assert_eq!(moved.position, before.position);
    assert!(kordon.player_state(id).is_none());
    assert_eq!(kordon.player_count(), 0);

    let mut base = Simulation::with_scene_and_catalog(&Scene::base(), catalog);
    assert_eq!(base.spawn_player_state(moved), id);
    let after = base.player_state(id).unwrap();
    assert_eq!(after.position, before.position);
    assert_eq!(after.velocity, before.velocity);
    assert_eq!(after.inventory[0], Some(Stack::new(bolt, 5)));
    assert_eq!(after.inventory[1], Some(Stack::new(medkit, 1)));

    // Свежие игроки не занимают id перенесённого.
    let fresh = base.spawn_player(Vec3::ZERO);
    assert!(fresh > id);

    // Ячейка — в склад, остаток — авто-сдачей; сеть видит пустые ячейки.
    assert_eq!(base.take_inventory_slot(id, 0), Some(Stack::new(bolt, 5)));
    assert_eq!(base.take_inventory_all(id), vec![Stack::new(medkit, 1)]);
    assert!(base
        .player_state(id)
        .unwrap()
        .inventory
        .iter()
        .all(Option::is_none));
    let updates = base.take_net_updates();
    assert!(updates
        .inventory
        .iter()
        .any(|(who, slots)| *who == id && slots.iter().all(Option::is_none)));
}

// ── Бой (M4a) ───────────────────────────────────────────────────────────

/// Инпут с удержанием огня.
fn fire_input(tick: u64) -> ClientInput {
    ClientInput {
        tick,
        buttons: FLAG_FIRE,
        ..ClientInput::default()
    }
}

/// Один выстрел стрелка (`shooter_x`, yaw = 0 → +X) с прицелом на высоту
/// `hit_height` цели на дистанции. Возвращает здоровье цели после выстрела.
fn health_after_one_shot(shooter_x: f32, target_x: f32, hit_height: f32) -> u16 {
    let mut sim = Simulation::new();
    let shooter = sim.spawn_player(Vec3::new(shooter_x, 0.0, 0.0));
    let target = sim.spawn_player(Vec3::new(target_x, 0.0, 0.0));
    let mut command = fire_input(0);
    command.pitch_delta = (hit_height - EYE_HEIGHT).atan2(target_x - shooter_x);
    sim.queue_input(shooter, command);
    sim.step_once();
    sim.player_state(target).unwrap().health
}

#[test]
fn zone_damage_boundaries() {
    assert_eq!(zone_damage(ZONE_HEAD_MIN + 0.05), 50);
    assert_eq!(zone_damage(ZONE_TORSO_MIN + 0.05), 25);
    assert_eq!(zone_damage(ZONE_TORSO_MIN - 0.05), 15);
}

#[test]
fn damage_zones_head_torso_legs() {
    // Голова ×2 → 50, торс ×1 → 25, ноги ×0.6 → 15.
    assert_eq!(health_after_one_shot(-5.0, 5.0, 1.65), MAX_HEALTH - 50);
    assert_eq!(health_after_one_shot(-5.0, 5.0, 1.15), MAX_HEALTH - 25);
    assert_eq!(health_after_one_shot(-5.0, 5.0, 0.4), MAX_HEALTH - 15);
}

#[test]
fn torso_shots_kill_and_respawn_at_spawn() {
    let mut sim = Simulation::new();
    let shooter = sim.spawn_player(Vec3::new(-5.0, 0.0, 0.0));
    let target = sim.spawn_player(Vec3::new(5.0, 0.0, 0.0));

    // Первый выстрел — в торс.
    let mut command = fire_input(0);
    command.pitch_delta = (1.15 - EYE_HEIGHT).atan2(10.0);
    sim.queue_input(shooter, command);
    sim.step_once();

    assert_eq!(
        sim.player_state(target).unwrap().health,
        MAX_HEALTH - 25,
        "первый выстрел в торс — 25 урона"
    );
    let events = sim.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::Shot { shooter: id, .. } if *id == shooter)),
        "событие Shot потерялось: {events:?}"
    );

    // Удержание огня: выстрелы на тиках 8/16/24 — цель умирает.
    for tick in 1..30u64 {
        sim.queue_input(shooter, fire_input(tick));
        sim.step_once();
    }
    let dead = sim.player_state(target).unwrap();
    assert_eq!(dead.health, 0, "четыре торс-попадания должны убивать");

    let death_events = sim.take_events();
    assert!(
        death_events.iter().any(|event| matches!(
            event,
            GameEvent::Death { victim, killer, .. }
                if *victim == target && *killer == shooter
        )),
        "событие Death потерялось: {death_events:?}"
    );

    // Мёртвый заморожен: инпуты движения не действуют.
    let frozen = dead.position;
    for tick in 30..60u64 {
        sim.queue_input(shooter, input(tick, 0.0, 0.0, false)); // прекратить огонь
        sim.queue_input(target, input(tick, 1.0, 0.0, false));
        sim.step_once();
    }
    let still = sim.player_state(target).unwrap();
    assert_eq!(still.health, 0);
    assert_eq!(still.position, frozen, "мёртвый не должен двигаться");

    // Через 3 с (180 тиков) после смерти — респавн на точке спавна уровня.
    while sim.tick() <= 24 + 180 {
        sim.queue_input(target, input(sim.tick(), 0.0, 0.0, false));
        sim.step_once();
    }
    let respawned = sim.player_state(target).unwrap();
    assert_eq!(respawned.health, MAX_HEALTH, "респавн возвращает здоровье");
    assert!(
        (respawned.position - Vec3::ZERO).length() < 1e-3,
        "респавн должен быть на точке спавна: {:?}",
        respawned.position
    );
}

#[test]
fn wall_blocks_bullet() {
    let mut scene = Scene::new("combat-wall");
    scene.spawn = Vec3::ZERO;
    scene.add(SceneObject::new(
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(30.0, 1.0, 30.0),
        glam::Vec4::ONE,
    ));
    // Стена между стрелком и целью.
    scene.add(SceneObject::new(
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.0, 3.0, 4.0),
        glam::Vec4::ONE,
    ));

    let mut sim = Simulation::with_scene(&scene);
    let shooter = sim.spawn_player(Vec3::new(-5.0, 0.0, 0.0));
    let target = sim.spawn_player(Vec3::new(5.0, 0.0, 0.0));

    let mut command = fire_input(0);
    command.pitch_delta = (1.15 - EYE_HEIGHT).atan2(10.0);
    sim.queue_input(shooter, command);
    for tick in 1..10u64 {
        sim.queue_input(shooter, fire_input(tick));
        sim.step_once();
    }

    assert_eq!(
        sim.player_state(target).unwrap().health,
        MAX_HEALTH,
        "стена должна блокировать выстрел"
    );
    let events = sim.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::Shot { .. })),
        "выстрел в стену всё равно должен быть виден: {events:?}"
    );
}

#[test]
fn fire_rate_is_limited() {
    let mut sim = Simulation::new();
    let shooter = sim.spawn_player(Vec3::ZERO);
    for tick in 0..25u64 {
        sim.queue_input(shooter, fire_input(tick));
        sim.step_once();
    }
    let shots = sim
        .take_events()
        .iter()
        .filter(|event| matches!(event, GameEvent::Shot { .. }))
        .count();
    // 25 тиков удержания — выстрелы только на тиках 0, 8, 16, 24.
    assert_eq!(shots, 4, "авто-огонь должен ограничиваться кулдауном");
}

#[test]
fn snapshot_death_does_not_respawn_locally() {
    // Предсказание клиента: смерть пришла снапшотом (respawn_at не задан) —
    // локального респавна быть не должно, игрока вернёт сервер.
    let mut sim = Simulation::new();
    let id = sim.spawn_player(Vec3::ZERO);
    let mut dead = sim.player_state(id).unwrap().net_state();
    dead.health = 0;
    assert!(sim.restore_player(id, &dead));

    for _ in 0..300 {
        sim.step_once();
    }

    let state = sim.player_state(id).unwrap();
    assert_eq!(state.health, 0, "локальный респавн без respawn_at запрещён");
    assert!(
        state.position.length() < 1e-3,
        "позиция мёртвого не меняется: {:?}",
        state.position
    );
}

#[test]
fn aim_forward_matches_camera_convention() {
    let forward = aim_forward(0.0, 0.0);
    assert!((forward - Vec3::X).length() < 1e-5, "{forward:?}");

    // Положительный pitch — вверх; yaw = +90° переводит прицел на +Z
    // (экранное «вправо», как в zn-render).
    assert!(aim_forward(0.0, 0.5).y > 0.4, "pitch вверх");
    let right = aim_forward(std::f32::consts::FRAC_PI_2, 0.0);
    assert!((right - Vec3::Z).length() < 1e-5, "{right:?}");
}

// ── Лут (M4b) ───────────────────────────────────────────────────────────

fn test_catalog() -> zn_content::ItemCatalog {
    zn_content::ItemCatalog::load_ron("../content/items/items.ron").expect("каталог предметов")
}

/// Сцена теста лута: пол (верх на y = 0) и ящик-куб 0.8 в `position`
/// (как `demo_zone`); спавн в начале координат, прицел yaw = 0 → +X.
fn loot_scene(position: Vec3, loot: &[(&str, u32)]) -> Scene {
    let mut scene = Scene::new("loot");
    scene.spawn = Vec3::ZERO;
    scene.add(SceneObject::new(
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(30.0, 1.0, 30.0),
        glam::Vec4::ONE,
    ));
    let entries = loot
        .iter()
        .map(|(item, count)| zn_scene::LootEntry {
            item: (*item).to_string(),
            count: *count,
        })
        .collect();
    scene.add(SceneObject::new(position, Vec3::splat(0.8), glam::Vec4::ONE).with_loot(entries));
    scene
}

/// Ящик тестовой сцены — второй объект (индекс 1) → id контейнера 2.
fn test_crate_container() -> u64 {
    crate_container_id(1)
}

#[test]
fn interact_opens_crate_and_take_all_moves_loot() {
    let catalog = test_catalog();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("ammo_545", 60), ("bolt", 10)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    let container = test_crate_container();

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert_eq!(
        sim.player_state(id).unwrap().open_container,
        Some(container)
    );

    // Открытие прислало содержимое ящика.
    let updates = sim.take_net_updates();
    let open = updates
        .containers
        .iter()
        .find(|(who, _)| *who == id)
        .expect("апдейт открытия");
    assert_eq!(open.1.container, container);
    assert_eq!(open.1.items.len(), 2);

    sim.queue_action(
        id,
        PlayerAction::Inventory(InventoryAction::TakeAll { container }),
    );
    sim.step_once();

    let ammo = catalog.index_of("ammo_545").unwrap();
    let bolt = catalog.index_of("bolt").unwrap();
    let state = sim.player_state(id).unwrap();
    assert_eq!(state.inventory[0], Some(Stack::new(ammo, 60)));
    assert_eq!(state.inventory[1], Some(Stack::new(bolt, 10)));
    assert_eq!(
        state.inventory.iter().flatten().count(),
        2,
        "остальные ячейки пусты"
    );

    // Контейнер опустел, инвентарь разослан.
    let updates = sim.take_net_updates();
    assert!(updates.containers.iter().any(|(who, update)| *who == id
        && update.container == container
        && update.items.is_empty()));
    assert!(updates.inventory.iter().any(|(who, slots)| *who == id
        && slots[0]
            == Some(ItemStackNet {
                item: ammo,
                count: 60
            })));
}

#[test]
fn carry_weight_limit_caps_take_all_and_leaves_leftover() {
    let catalog = test_catalog();
    let canned = catalog.index_of("canned_food").unwrap();
    // 120 консервов = 60 кг: по весу влезает ровно 100 (50 кг).
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("canned_food", 120)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    let container = test_crate_container();

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    // 120 шт. дробятся по stack_max = 5 → 24 стопки.
    let updates = sim.take_net_updates();
    let open = updates
        .containers
        .iter()
        .find(|(who, _)| *who == id)
        .expect("апдейт открытия");
    assert_eq!(open.1.items.len(), 24);

    sim.queue_action(
        id,
        PlayerAction::Inventory(InventoryAction::TakeAll { container }),
    );
    sim.step_once();

    let state = sim.player_state(id).unwrap();
    let total: u32 = state
        .inventory
        .iter()
        .flatten()
        .map(|stack| stack.count)
        .sum();
    assert_eq!(total, 100, "по весу влезает ровно 100 консервов");
    assert!(state
        .inventory
        .iter()
        .flatten()
        .all(|stack| stack.item == canned));
    assert_eq!(
        inventory_weight_grams(&catalog, &state.inventory),
        MAX_CARRY_GRAMS,
        "инвентарь заполнен ровно до лимита"
    );

    // В ящике осталось 20 консервов.
    let updates = sim.take_net_updates();
    let last = updates
        .containers
        .iter()
        .rev()
        .find(|(who, update)| *who == id && update.container == container)
        .map(|(_, update)| update)
        .expect("контейнер обновлён");
    let leftover: u32 = last.items.iter().map(|stack| u32::from(stack.count)).sum();
    assert_eq!(leftover, 20);
}

#[test]
fn drop_and_pickup_ground_item() {
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);

    assert_eq!(sim.debug_give(id, medkit, 1), 1);
    sim.queue_action(
        id,
        PlayerAction::Inventory(InventoryAction::Drop { slot: 0 }),
    );
    sim.step_once();

    assert!(
        sim.player_state(id).unwrap().inventory[0].is_none(),
        "ячейка опустела"
    );
    let loot = sim.snapshot_loot();
    assert_eq!(loot.len(), 1);
    assert_eq!(loot[0].item, medkit);
    assert_eq!(loot[0].count, 1);

    // Смотрим вниз и подбираем: предмет появляется в первой свободной ячейке.
    let mut look_down = input(1, 0.0, 0.0, false);
    look_down.pitch_delta = -1.0;
    sim.queue_input(id, look_down);
    sim.step_once();

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();

    assert!(sim.snapshot_loot().is_empty(), "предмет подобран");
    assert_eq!(
        sim.player_state(id).unwrap().inventory[0],
        Some(Stack::new(medkit, 1))
    );
}

#[test]
fn interact_requires_aim_and_range() {
    let catalog = test_catalog();

    // Ящик дальше радиуса 2 м.
    let far = loot_scene(Vec3::new(3.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&far, catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert_eq!(
        sim.player_state(id).unwrap().open_container,
        None,
        "далёкий ящик не открывается"
    );

    // Ящик за спиной (прицел yaw = 0 → +X).
    let behind = loot_scene(Vec3::new(-1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&behind, catalog);
    let id = sim.spawn_player(Vec3::ZERO);
    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert_eq!(
        sim.player_state(id).unwrap().open_container,
        None,
        "ящик позади не открывается"
    );
}

#[test]
fn interact_toggles_open_container() {
    let catalog = test_catalog();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert!(sim.player_state(id).unwrap().open_container.is_some());
    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert!(sim.player_state(id).unwrap().open_container.is_none());
}

#[test]
fn open_container_closes_when_walking_away() {
    let catalog = test_catalog();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert!(sim.player_state(id).unwrap().open_container.is_some());

    // Уходим назад (−X): дальше 3 м панель закрывается сама.
    for tick in 1..60u64 {
        sim.queue_input(id, input(tick, -1.0, 0.0, false));
        sim.step_once();
    }
    let state = sim.player_state(id).unwrap();
    assert!(
        state.position.x < -3.0,
        "игрок не отошёл: {:?}",
        state.position
    );
    assert_eq!(state.open_container, None, "панель закрылась по дистанции");
}

#[test]
fn unknown_loot_keys_are_skipped() {
    let catalog = test_catalog();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("nope", 5), ("bolt", 3)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    let updates = sim.take_net_updates();
    let open = updates
        .containers
        .iter()
        .find(|(who, _)| *who == id)
        .expect("апдейт открытия");
    assert_eq!(open.1.items.len(), 1, "неизвестный предмет пропущен");
    assert_eq!(open.1.items[0].item, catalog.index_of("bolt").unwrap());
}

#[test]
fn medkit_heals_and_is_consumed() {
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);

    assert_eq!(sim.debug_give(id, medkit, 1), 1);
    let mut hurt = sim.player_state(id).unwrap().net_state();
    hurt.health = 40;
    assert!(sim.restore_player(id, &hurt));

    sim.queue_action(
        id,
        PlayerAction::Inventory(InventoryAction::Use { slot: 0 }),
    );
    sim.step_once();
    let healing = sim
        .player_state(id)
        .unwrap()
        .healing
        .expect("лечение началось");
    assert!(
        sim.take_events()
            .iter()
            .any(|event| matches!(event, GameEvent::UseStarted { player, ticks } if *player == id && *ticks == 120)),
        "событие начала лечения потерялось"
    );

    let until = healing.until_tick;
    while sim.tick() <= until {
        sim.step_once();
    }
    let state = sim.player_state(id).unwrap();
    assert_eq!(state.health, 90, "40 + 50 аптечки");
    assert_eq!(state.healing, None);
    assert_eq!(state.inventory[0], None, "аптечка израсходована");
}

#[test]
fn healing_capped_at_max_health() {
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);

    assert_eq!(sim.debug_give(id, medkit, 1), 1);
    let mut hurt = sim.player_state(id).unwrap().net_state();
    hurt.health = 80;
    assert!(sim.restore_player(id, &hurt));

    sim.queue_action(
        id,
        PlayerAction::Inventory(InventoryAction::Use { slot: 0 }),
    );
    sim.step_once();
    let until = sim.player_state(id).unwrap().healing.unwrap().until_tick;
    while sim.tick() <= until {
        sim.step_once();
    }
    assert_eq!(sim.player_state(id).unwrap().health, MAX_HEALTH);
}

#[test]
fn death_moves_inventory_into_corpse_that_decays() {
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    let ammo = catalog.index_of("ammo_545").unwrap();
    let scene = loot_scene(Vec3::new(100.0, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let shooter = sim.spawn_player(Vec3::new(-5.0, 0.0, 0.0));
    let target = sim.spawn_player(Vec3::new(5.0, 0.0, 0.0));
    assert_eq!(sim.debug_give(target, medkit, 2), 2);
    // Патроны стрелку (M4d-3a): без них выстрелы не происходит.
    assert_eq!(sim.debug_give(shooter, ammo, 30), 30);

    // Четыре торс-попадания подряд убивают цель.
    let mut command = fire_input(0);
    command.pitch_delta = (1.15 - EYE_HEIGHT).atan2(10.0);
    sim.queue_input(shooter, command);
    sim.step_once();
    for tick in 1..30u64 {
        sim.queue_input(shooter, fire_input(tick));
        sim.step_once();
    }
    assert_eq!(sim.player_state(target).unwrap().health, 0);

    // Прекращаем огонь: иначе последний инпут (FLAG_FIRE) повторяется вечно и
    // возродившаяся цель гибнет снова, плодя трупы.
    sim.queue_input(shooter, input(sim.tick(), 0.0, 0.0, false));
    sim.step_once();

    // Труп: тело в снапшоте, лут — в контейнере.
    let corpses = sim.snapshot_corpses();
    assert_eq!(corpses.len(), 1);
    assert_eq!(corpses[0].owner, target);
    let container = corpses[0].container;
    assert!(is_corpse_container(container));
    assert_eq!(
        sim.container_items(container),
        Some(vec![Stack::new(medkit, 2)]),
        "инвентарь жертвы переехал в труп"
    );
    assert!(
        sim.player_state(target)
            .unwrap()
            .inventory
            .iter()
            .all(|slot| slot.is_none()),
        "инвентарь жертвы пуст после смерти"
    );

    // Через 5 минут труп исчезает вместе с контейнером.
    let death_tick = sim.tick();
    while sim.tick() <= death_tick + CORPSE_LIFETIME_TICKS {
        sim.step_once();
    }
    assert!(sim.snapshot_corpses().is_empty(), "труп должен исчезнуть");
    assert!(sim.container_items(container).is_none());
}

#[test]
fn dead_player_cannot_loot() {
    let catalog = test_catalog();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);

    let mut dead = sim.player_state(id).unwrap().net_state();
    dead.health = 0;
    assert!(sim.restore_player(id, &dead));

    sim.queue_action(id, PlayerAction::Interact);
    sim.step_once();
    assert_eq!(sim.player_state(id).unwrap().open_container, None);
}

#[test]
fn demo_zone_crates_fill_from_catalog() {
    let catalog = test_catalog();
    let scene = Scene::demo_zone();
    let sim = Simulation::with_scene_and_catalog(&scene, catalog);

    let index = scene
        .objects
        .iter()
        .position(|object| object.loot.is_some())
        .expect("в demo_zone есть ящики");
    let container = crate_container_id(u32::try_from(index).unwrap());
    let items = sim.container_items(container).expect("ящик существует");
    assert!(!items.is_empty(), "ящик наполнен по каталогу");
}

// ── Аномалии и болты (M4c) ──────────────────────────────────────────────

/// Сцена теста аномалий: пол 60×60 (верх на y = 0), без ящиков; спавн в
/// начале координат, прицел yaw = 0 → +X. Опционально — одна аномалия.
fn anomaly_scene(anomaly: Option<(AnomalyKind, Vec3, f32, Option<&str>)>) -> Scene {
    let mut scene = Scene::new("anomaly");
    scene.spawn = Vec3::ZERO;
    scene.add(SceneObject::new(
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(60.0, 1.0, 60.0),
        glam::Vec4::ONE,
    ));
    if let Some((kind, position, radius, artifact)) = anomaly {
        let mut anomaly = SceneAnomaly::new(position, radius, kind);
        if let Some(key) = artifact {
            anomaly = anomaly.with_artifact(key);
        }
        scene.add_anomaly(anomaly);
    }
    scene
}

/// Сколько болтов (предметов `item`) лежит в инвентаре игрока.
fn bolt_count(sim: &mut Simulation, id: u64, item: u16) -> u32 {
    sim.player_state(id)
        .unwrap()
        .inventory
        .iter()
        .filter_map(|slot| *slot)
        .filter(|stack| stack.item == item)
        .map(|stack| stack.count)
        .sum()
}

#[test]
fn artifact_spawns_in_anomaly_at_start() {
    let catalog = test_catalog();
    let artifact = catalog.index_of("artifact_heat").unwrap();
    let scene = anomaly_scene(Some((
        AnomalyKind::Heat,
        Vec3::new(5.0, 0.0, 0.0),
        2.5,
        Some("artifact_heat"),
    )));
    let sim = Simulation::with_scene_and_catalog(&scene, catalog);

    assert_eq!(sim.anomaly_count(), 1);
    let loot = sim.snapshot_loot();
    assert_eq!(loot.len(), 1, "артефакт лежит в аномалии: {loot:?}");
    assert_eq!(loot[0].item, artifact);
    assert_eq!(loot[0].count, 1);
    // Якорь аномалии — на полу.
    assert_eq!(loot[0].pos, [50, 0, 0]);
}

#[test]
fn heat_anomaly_damages_and_kills_player() {
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    let scene = anomaly_scene(Some((AnomalyKind::Heat, Vec3::ZERO, 3.0, None)));
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);
    assert_eq!(sim.debug_give(id, medkit, 1), 1);

    // Тик 0 — первый тик урона: «сердце» (0, 0.9, 0) в 2.1 м от центра
    // (0, 3, 0) при радиусе 3 → урон 2 + 4·0.3 ≈ 3.
    sim.step_once();
    assert_eq!(sim.player_state(id).unwrap().health, MAX_HEALTH - 3);

    // Следующий тик урона — только через ANOMALY_DAMAGE_INTERVAL_TICKS.
    for _ in 0..ANOMALY_DAMAGE_INTERVAL_TICKS {
        sim.step_once();
    }
    assert_eq!(sim.player_state(id).unwrap().health, MAX_HEALTH - 6);

    // Досчитываем до смерти (с запасом на тики урона).
    for _ in 0..ANOMALY_DAMAGE_INTERVAL_TICKS * 40 {
        if sim.player_state(id).unwrap().health == 0 {
            break;
        }
        sim.step_once();
    }
    assert_eq!(sim.player_state(id).unwrap().health, 0, "жгучая убивает");

    // Смерть без игрока-убийцы (killer = 0) и обычный труп с лутом.
    let corpses = sim.snapshot_corpses();
    assert_eq!(corpses.len(), 1);
    assert_eq!(corpses[0].owner, id);
    assert_eq!(
        sim.container_items(corpses[0].container),
        Some(vec![Stack::new(medkit, 1)])
    );
    let events = sim.take_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::Death { victim, killer: 0, .. } if *victim == id
        )),
        "событие смерти от аномалии: {events:?}"
    );

    // Респавн — на точке спавна с полным здоровьем.
    let death_tick = sim.tick();
    while sim.tick() <= death_tick + RESPAWN_DELAY_TICKS {
        sim.step_once();
    }
    let state = sim.player_state(id).unwrap();
    assert_eq!(state.health, MAX_HEALTH);
    assert!(state.position.distance(Vec3::ZERO) < 0.5);
}

#[test]
fn gravity_anomaly_pulls_and_sprint_escapes() {
    let scene = anomaly_scene(Some((AnomalyKind::Gravity, Vec3::ZERO, 4.0, None)));

    // Без движения притяжение тащит к оси аномалии (инпуты, как у клиента,
    // приходят каждый тик: нулевые).
    let mut sim = Simulation::with_scene_and_catalog(&scene, test_catalog());
    let id = sim.spawn_player(Vec3::new(0.0, 0.0, 2.0));
    for tick in 0..120u64 {
        sim.queue_input(id, input(tick, 0.0, 0.0, false));
        sim.step_once();
    }
    let pulled = sim.player_state(id).unwrap().position;
    assert!(
        pulled.length() < 0.8,
        "должно притянуть к центру: {pulled:?}"
    );

    // Спринт наружу (8.5 м/с против тяги до 5) выносит из сферы.
    let mut sim = Simulation::with_scene_and_catalog(&scene, test_catalog());
    let id = sim.spawn_player(Vec3::new(0.0, 0.0, 2.0));
    let mut turn = input(0, 0.0, 0.0, false);
    turn.yaw_delta = std::f32::consts::FRAC_PI_2; // прицел на +Z — прочь от центра
    sim.queue_input(id, turn);
    sim.step_once();
    for tick in 1..90u64 {
        let mut run = input(tick, 1.0, 0.0, false);
        run.buttons = FLAG_SPRINTING;
        sim.queue_input(id, run);
        sim.step_once();
    }
    let escaped = sim.player_state(id).unwrap().position;
    assert!(
        escaped.z > 6.0,
        "спринт должен вынести из сферы: {escaped:?}"
    );
}

#[test]
fn throw_bolt_consumes_item_and_lands_with_cooldown() {
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();
    let scene = anomaly_scene(None);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);
    assert_eq!(sim.debug_give(id, bolt, 2), 2);

    // Бросок: болт в полёте, из инвентаря ушла одна штука.
    sim.queue_action(id, PlayerAction::ThrowBolt);
    sim.step_once();
    assert_eq!(sim.snapshot_bolts().len(), 1, "болт летит");
    assert_eq!(bolt_count(&mut sim, id, bolt), 1);

    // Кулдаун: повторный бросок раньше срока игнорируется.
    for _ in 0..5 {
        sim.step_once();
    }
    assert!(sim.tick() < BOLT_THROW_COOLDOWN_TICKS);
    sim.queue_action(id, PlayerAction::ThrowBolt);
    sim.step_once();
    assert_eq!(sim.snapshot_bolts().len(), 1, "кулдаун держит");
    assert_eq!(bolt_count(&mut sim, id, bolt), 1, "второй болт не потрачен");

    // Полёт ~0.42 с: болт приземляется и лежит как обычный предмет.
    for _ in 0..120 {
        if sim.snapshot_bolts().is_empty() {
            break;
        }
        sim.step_once();
    }
    assert!(sim.snapshot_bolts().is_empty(), "болт должен приземлиться");
    let loot = sim.snapshot_loot();
    assert_eq!(loot.len(), 1, "подобранный болт на земле: {loot:?}");
    assert_eq!(loot[0].item, bolt);

    // После кулдауна второй бросок срабатывает и дотрачивает запас.
    while sim.tick() < BOLT_THROW_COOLDOWN_TICKS {
        sim.step_once();
    }
    sim.queue_action(id, PlayerAction::ThrowBolt);
    sim.step_once();
    assert_eq!(sim.snapshot_bolts().len(), 1);
    assert_eq!(bolt_count(&mut sim, id, bolt), 0);
}

#[test]
fn bolt_dies_in_anomaly_and_reveals_it() {
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();
    let scene = anomaly_scene(Some((
        AnomalyKind::Heat,
        Vec3::new(10.0, 0.0, 0.0),
        3.0,
        None,
    )));
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    // Игрок у границы сферы (снаружи) бросает болт в аномалию.
    let id = sim.spawn_player(Vec3::new(7.0, 0.0, 0.0));
    assert_eq!(sim.debug_give(id, bolt, 1), 1);

    sim.queue_action(id, PlayerAction::ThrowBolt);
    sim.step_once();
    assert!(
        sim.snapshot_bolts().is_empty(),
        "болт гибнет в аномалии сразу"
    );
    assert!(
        sim.snapshot_loot().is_empty(),
        "болт в аномалии не падает на землю"
    );
    let events = sim.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::AnomalyRevealed { anomaly: 0, .. })),
        "вспышка обнаружения: {events:?}"
    );
    assert_eq!(
        sim.player_state(id).unwrap().health,
        MAX_HEALTH,
        "сам игрок снаружи сферы — невредим"
    );
}

// ── Экстракция (M4d-2) ──────────────────────────────────────────────────

#[test]
fn extract_inventory_keeps_stacks_until_weight_budget() {
    let catalog = test_catalog();
    let mut sim = Simulation::with_scene_and_catalog(&Scene::base(), catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    let medkit = catalog.index_of("medkit").unwrap();
    let canned = catalog.index_of("canned_food").unwrap();
    let bolt = catalog.index_of("bolt").unwrap();
    // medkit×2 = 1.0 кг, canned×2 = 1.0 кг, bolt×4 = 0.2 кг; всего 2.2 кг.
    assert_eq!(sim.give_stack(id, Stack::new(medkit, 2)), 2);
    assert_eq!(sim.give_stack(id, Stack::new(canned, 2)), 2);
    assert_eq!(sim.give_stack(id, Stack::new(bolt, 4)), 4);

    let (kept, lost) = sim.extract_inventory(id, 0.75);
    // Бюджет 1.65 кг: medkit влезает целиком, canned режется 1/1, bolt — 3/1.
    assert_eq!(
        kept,
        vec![
            Stack::new(medkit, 2),
            Stack::new(canned, 1),
            Stack::new(bolt, 3),
        ]
    );
    assert_eq!(lost, vec![Stack::new(canned, 1), Stack::new(bolt, 1)]);
    assert!(
        sim.player_state(id)
            .unwrap()
            .inventory
            .iter()
            .all(Option::is_none),
        "инвентарь пуст после экстракции"
    );
}

#[test]
fn extract_inventory_keeps_everything_at_full_percent_and_drops_at_zero() {
    let catalog = test_catalog();
    let mut sim = Simulation::with_scene_and_catalog(&Scene::base(), catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    assert_eq!(
        sim.give_stack(id, Stack::new(catalog.index_of("bolt").unwrap(), 3)),
        3
    );

    let (kept, lost) = sim.extract_inventory(id, 1.0);
    assert_eq!(kept.len(), 1);
    assert!(lost.is_empty());

    // Одна стопка при 0 %: бюджет нулевой — всё падает на землю.
    assert_eq!(
        sim.give_stack(id, Stack::new(catalog.index_of("bolt").unwrap(), 3)),
        3
    );
    let (kept, lost) = sim.extract_inventory(id, 0.0);
    assert!(kept.is_empty());
    assert_eq!(lost.len(), 1);
}

#[test]
fn extract_inventory_splits_boundary_stack_by_units() {
    let catalog = test_catalog();
    let mut sim = Simulation::with_scene_and_catalog(&Scene::base(), catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    let bolt = catalog.index_of("bolt").unwrap();
    // bolt×5 = 0.25 кг; бюджет 75 % = 0.1875 кг → 3 болта в склад, 2 падают.
    assert_eq!(sim.give_stack(id, Stack::new(bolt, 5)), 5);
    let (kept, lost) = sim.extract_inventory(id, 0.75);
    assert_eq!(kept, vec![Stack::new(bolt, 3)]);
    assert_eq!(lost, vec![Stack::new(bolt, 2)]);
}

#[test]
fn push_event_reaches_next_snapshot_events() {
    let mut sim = Simulation::new();
    assert!(sim.take_events().is_empty());
    sim.push_event(GameEvent::Extracted {
        player: 7,
        player_name: "Сталкер".to_string(),
        percent: 0.75,
    });
    let events = sim.take_events();
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], GameEvent::Extracted { player, percent, .. } if *player == 7 && (*percent - 0.75).abs() < 1e-6),
        "событие экстракции доехало: {events:?}"
    );
    assert!(sim.take_events().is_empty(), "очередь опустошена");
}

#[test]
fn drop_stack_at_lands_on_ground() {
    let mut sim = Simulation::new();
    sim.drop_stack_at(Stack::new(3, 2), Vec3::new(1.0, 0.0, -2.0));
    let loot = sim.snapshot_loot();
    assert_eq!(loot.len(), 1);
    assert_eq!((loot[0].item, loot[0].count), (3, 2));
    let pos = zn_net::dequantize_pos(loot[0].pos);
    assert!(
        (pos[0] - 1.0).abs() < 0.1 && (pos[2] + 2.0).abs() < 0.1,
        "{pos:?}"
    );
}

// ── Выживание, данные (M4d-3a) ───────────────────────────────────────────

#[test]
fn firing_consumes_ammo_and_goes_dry() {
    let catalog = test_catalog();
    let mut sim = Simulation::with_scene_and_catalog(&Scene::base(), catalog.clone());
    let shooter = sim.spawn_player(Vec3::ZERO);
    let ammo = catalog.index_of("ammo_545").unwrap();
    assert_eq!(sim.debug_give(shooter, ammo, 3), 3);

    // Три патрона — три выстрела, дальше «сухой» курок (48 тиков при 8-тике).
    let mut shots = 0;
    for tick in 0..48u64 {
        sim.queue_input(shooter, fire_input(tick));
        sim.step_once();
        shots += sim
            .take_events()
            .iter()
            .filter(|event| matches!(event, GameEvent::Shot { .. }))
            .count();
    }
    assert_eq!(shots, 3, "три патрона — три выстрела, потом тишина");
    let state = sim.player_state(shooter).unwrap();
    assert!(
        state
            .inventory
            .iter()
            .flatten()
            .all(|stack| stack.item != ammo),
        "магазин пуст"
    );
}

#[test]
fn weight_cut_artifact_raises_carry_limit() {
    let catalog = test_catalog();
    let mut sim = Simulation::with_scene_and_catalog(&Scene::base(), catalog.clone());
    let id = sim.spawn_player(Vec3::ZERO);
    let limit = carry_limit_grams(&catalog, &sim.player_state(id).unwrap().inventory);
    assert_eq!(limit, MAX_CARRY_GRAMS, "без артефактов — стандартный лимит");

    let gravity = catalog.index_of("artifact_gravity").unwrap();
    sim.debug_give(id, gravity, 1);
    let lifted = carry_limit_grams(&catalog, &sim.player_state(id).unwrap().inventory);
    assert_eq!(
        lifted,
        (MAX_CARRY_GRAMS as f32 * 1.2) as u32,
        "Тягун: +20 % к весу"
    );
    assert!(lifted > limit);
}

#[test]
fn heat_resist_artifact_halves_burn_damage() {
    let catalog = test_catalog();
    let scene = anomaly_scene(Some((AnomalyKind::Heat, Vec3::ZERO, 3.0, None)));
    let mut plain = Simulation::with_scene_and_catalog(&scene, catalog.clone());
    let plain_id = plain.spawn_player(Vec3::ZERO);
    let mut protected = Simulation::with_scene_and_catalog(&scene, catalog.clone());
    let protected_id = protected.spawn_player(Vec3::ZERO);
    protected.debug_give(protected_id, catalog.index_of("artifact_heat").unwrap(), 1);

    // Крутим ровно до первого тика урона (tick 0 — кратен интервалу), но не
    // дальше: иначе второе срабатывание внесёт ошибки округления в сравнение.
    for _ in 0..ANOMALY_DAMAGE_INTERVAL_TICKS {
        plain.step_once();
        protected.step_once();
    }
    let plain_damage = MAX_HEALTH - plain.player_state(plain_id).unwrap().health;
    let protected_damage = MAX_HEALTH - protected.player_state(protected_id).unwrap().health;
    assert!(plain_damage > 0, "жгучая аномалия жжёт: {plain_damage}");
    assert_eq!(
        protected_damage,
        (plain_damage as f32 * 0.5).round() as u16,
        "Жар-камень вдвое режет урон: {plain_damage} → {protected_damage}"
    );
}

#[test]
fn canned_food_heals_slowly() {
    let catalog = test_catalog();
    let canned = catalog.index_of("canned_food").unwrap();
    let scene = loot_scene(Vec3::new(1.5, 0.4, 0.0), &[("bolt", 1)]);
    let mut sim = Simulation::with_scene_and_catalog(&scene, catalog);
    let id = sim.spawn_player(Vec3::ZERO);
    assert_eq!(sim.debug_give(id, canned, 1), 1);
    let mut hurt = sim.player_state(id).unwrap().net_state();
    hurt.health = 30;
    assert!(sim.restore_player(id, &hurt));

    sim.queue_action(
        id,
        PlayerAction::Inventory(InventoryAction::Use { slot: 0 }),
    );
    sim.step_once();
    let until = sim
        .player_state(id)
        .unwrap()
        .healing
        .expect("консервы начали лечение")
        .until_tick;
    while sim.tick() <= until {
        sim.step_once();
    }
    let state = sim.player_state(id).unwrap();
    assert_eq!(state.health, 40, "30 + 10 консервов");
    assert_eq!(state.healing, None);
    assert_eq!(state.inventory[0], None, "консервы съедены");
}
