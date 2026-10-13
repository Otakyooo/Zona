use std::time::Duration;

use glam::Vec3;
use zn_net::ClientInput;
use zn_sim::{InputBuffer, Simulation};

fn input(tick: u64, forward: f32, right: f32, jump: bool) -> ClientInput {
    ClientInput { tick, move_forward: forward, move_right: right, jump, ..ClientInput::default() }
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
    assert!(state.position.x > 4.0, "player did not move: {:?}", state.position);
    assert!(state.position.y.abs() < 0.01, "player left the ground: {}", state.position.y);
    assert!(state.grounded);
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
    assert!(pa.x > 4.0);
    assert!(pb.z < 45.0, "player b should have moved toward -x/z");
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
    assert!(landed.position.y.abs() < 0.01);
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
fn input_buffer_keeps_last_command_between_ticks() {
    let mut buffer = InputBuffer::default();
    let command = ClientInput::new(1);
    buffer.set(7, command);
    buffer.swap();
    assert_eq!(buffer.get(7), Some(&command));
}

#[test]
fn snapshot_covers_all_players() {
    let mut sim = Simulation::new();
    sim.spawn_player(Vec3::ZERO);
    sim.spawn_player(Vec3::new(5.0, 0.0, 0.0));
    assert_eq!(sim.snapshot().len(), 2);
    assert_eq!(sim.player_count(), 2);
}

const SIM_STEPS: u64 = 60;
