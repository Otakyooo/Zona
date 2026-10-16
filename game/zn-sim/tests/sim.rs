use std::time::Duration;

use glam::Vec3;
use zn_net::ClientInput;
use zn_scene::{Scene, SceneObject};
use zn_sim::{Simulation, FLAG_SPRINTING};

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
