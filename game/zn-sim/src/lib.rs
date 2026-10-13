use std::collections::HashMap;
use std::time::Duration;

use glam::Vec3;
use zn_core::{FixedStep, Schedule, World, SIMULATION_HZ, SIMULATION_STEP};
pub use zn_net::{ClientInput, FLAG_GROUNDED, FLAG_SPRINTING, PlayerSnapshot};

pub const WALK_SPEED: f32 = 5.5;
pub const SPRINT_SPEED: f32 = 8.5;
pub const JUMP_SPEED: f32 = 6.5;
pub const GRAVITY: f32 = 18.0;
pub const MAX_PITCH: f32 = 89.0f32.to_radians();

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub id: u64,
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
    pub sprinting: bool,
}

impl PlayerState {
    pub fn new(id: u64, spawn: Vec3) -> Self {
        Self { id, position: spawn, velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, grounded: false, sprinting: false }
    }

    pub fn snapshot(&self, entity_id: u32) -> PlayerSnapshot {
        let mut flags = 0;
        if self.grounded {
            flags |= FLAG_GROUNDED;
        }
        if self.sprinting {
            flags |= FLAG_SPRINTING;
        }
        PlayerSnapshot {
            entity_id,
            position: self.position.to_array(),
            yaw: self.yaw,
            pitch: self.pitch,
            flags,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TickInfo {
    pub dt: f32,
    pub tick: u64,
}

#[derive(Clone, Debug, Default)]
pub struct InputBuffer {
    commands: HashMap<u64, ClientInput>,
    previous: HashMap<u64, ClientInput>,
}

impl InputBuffer {
    pub fn set(&mut self, id: u64, command: ClientInput) {
        self.commands.insert(id, command);
    }

    pub fn get(&self, id: u64) -> Option<&ClientInput> {
        self.commands.get(&id).or_else(|| self.previous.get(&id))
    }

    pub fn swap(&mut self) {
        std::mem::swap(&mut self.commands, &mut self.previous);
        self.commands.clear();
    }

    pub fn clear(&mut self) {
        self.commands.clear();
        self.previous.clear();
    }
}

pub struct Simulation {
    world: World,
    schedule: Schedule,
    fixed: FixedStep,
    tick: u64,
    next_player_id: u64,
}

impl Default for Simulation {
    fn default() -> Self {
        Self::new()
    }
}

impl Simulation {
    pub fn new() -> Self {
        let mut simulation = Self {
            world: World::new(),
            schedule: build_schedule(),
            fixed: FixedStep::new(SIMULATION_STEP),
            tick: 0,
            next_player_id: 1,
        };
        simulation.world.insert_resource(TickInfo::default());
        simulation.world.insert_resource(InputBuffer::default());
        simulation
    }

    pub fn tick_hz(&self) -> u32 {
        SIMULATION_HZ
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn player_count(&mut self) -> usize {
        self.world.query::<&PlayerState>().iter().len()
    }

    pub fn spawn_player(&mut self, spawn: Vec3) -> u64 {
        let id = self.next_player_id;
        self.next_player_id += 1;
        self.world.spawn((PlayerState::new(id, spawn),));
        id
    }

    pub fn queue_input(&mut self, id: u64, command: ClientInput) {
        self.world
            .get_resource_or_insert_with(InputBuffer::default)
            .set(id, command);
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
        if let Some(buffer) = self.world.get_resource_mut::<InputBuffer>() {
            buffer.swap();
        }
        self.tick += 1;
        self.world.advance_tick();
    }

    pub fn snapshot(&mut self) -> Vec<PlayerSnapshot> {
        let mut query = self.world.query::<&PlayerState>();
        query.iter().map(|state| state.snapshot(0)).collect()
    }

    pub fn player_state(&mut self, id: u64) -> Option<PlayerState> {
        self.world
            .query::<&PlayerState>()
            .iter()
            .find(|state| state.id == id)
            .cloned()
    }
}

fn build_schedule() -> Schedule {
    let mut schedule = Schedule::new();
    schedule.add("player_apply_input", apply_input);
    schedule.add("player_integrate", integrate);
    schedule
}

fn apply_input(world: &mut World) {
    let dt = world.get_resource::<TickInfo>().map(|info| info.dt).unwrap_or(1.0 / 60.0);
    let inputs = world
        .get_resource::<InputBuffer>()
        .map(|buffer| buffer.commands.clone())
        .unwrap_or_default();

    let mut query = world.query::<&mut PlayerState>();
    for state in query.iter() {
        let Some(command) = inputs.get(&state.id) else { continue };
        state.yaw = (state.yaw + command.yaw_delta).rem_euclid(std::f32::consts::TAU);
        state.pitch = (state.pitch + command.pitch_delta).clamp(-MAX_PITCH, MAX_PITCH);

        let (sin_yaw, cos_yaw) = state.yaw.sin_cos();
        let forward = Vec3::new(cos_yaw, 0.0, sin_yaw);
        let right = Vec3::new(sin_yaw, 0.0, -cos_yaw);
        let move_dir = forward * command.move_forward + right * command.move_right;
        let speed = if command.buttons & FLAG_SPRINTING != 0 { SPRINT_SPEED } else { WALK_SPEED };
        state.sprinting = command.buttons & FLAG_SPRINTING != 0 && move_dir.length_squared() > 1e-6;

        state.velocity.x = move_dir.x * speed;
        state.velocity.z = move_dir.z * speed;

        if command.jump && state.grounded {
            state.velocity.y = JUMP_SPEED;
            state.grounded = false;
        }
        if !state.grounded {
            state.velocity.y -= GRAVITY * dt;
        }
    }
}

fn integrate(world: &mut World) {
    let dt = world.get_resource::<TickInfo>().map(|info| info.dt).unwrap_or(1.0 / 60.0);
    let mut query = world.query::<&mut PlayerState>();
    for state in query.iter() {
        state.position += state.velocity * dt;

        if state.position.y <= 0.0 {
            state.position.y = 0.0;
            state.velocity.y = 0.0;
            state.grounded = true;
        } else if state.velocity.y > 0.0 || state.position.y > 0.001 {
            state.grounded = false;
        }
    }
}
