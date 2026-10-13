use std::time::Instant;

use glam::Vec3;
use rapier3d::prelude::Vector;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, WindowId};

use zn_core::FixedStep;
use zn_input::InputState;
use zn_physics::{PhysicsWorld, PLAYER_EYE_HEIGHT};
use zn_render::{instances_from_scene, Camera, InstanceRaw, Renderer};
use zn_scene::Scene;
use zn_sim::{ClientInput, Simulation, FLAG_SPRINTING};

const LOOK_SENSITIVITY: f32 = 0.0022;
const WALK_SPEED: f32 = 5.5;
const SPRINT_SPEED: f32 = 8.5;
const JUMP_SPEED: f32 = 6.5;
const GRAVITY: f32 = 18.0;
const MAX_FALL_SPEED: f32 = 45.0;

pub struct ClientApp {
    renderer: Option<Renderer>,
    input: InputState,
    simulation: Simulation,
    instances: Vec<InstanceRaw>,
    camera: Camera,
    physics: Option<PhysicsWorld>,
    character_origin: Vector,
    vertical_velocity: f32,
    grounded: bool,
    cursor_grabbed: bool,
    fixed: FixedStep,
    last_frame: Option<Instant>,
    client_tick: u64,
}

impl Default for ClientApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientApp {
    pub fn new() -> Self {
        let scene = Scene::demo_zone();
        log::info!("loading scene '{}' ({} objects)", scene.name, scene.len());
        let instances = instances_from_scene(&scene);
        let camera = Camera::new(scene.spawn, scene.spawn_yaw, 16.0 / 9.0);

        let mut physics = PhysicsWorld::with_gravity_y(-GRAVITY, 1.0 / 60.0);
        for object in scene.solids() {
            let scale = object.scale;
            physics.add_static_box(
                Vector::new(scale.x / 2.0, scale.y / 2.0, scale.z / 2.0),
                Vector::new(object.position.x, object.position.y, object.position.z),
            );
        }
        physics.step_dynamic_bodies();

        let character_origin = PhysicsWorld::character_origin(Vector::new(
            scene.spawn.x,
            scene.spawn.y,
            scene.spawn.z,
        ));

        Self {
            renderer: None,
            input: InputState::new(),
            simulation: Simulation::new(),
            instances,
            camera,
            physics: Some(physics),
            character_origin,
            vertical_velocity: 0.0,
            grounded: false,
            cursor_grabbed: false,
            fixed: FixedStep::from_hz(60),
            last_frame: None,
            client_tick: 0,
        }
    }

    fn grab_cursor(&mut self, grab: bool) {
        let Some(renderer) = self.renderer.as_ref() else { return };
        let window = renderer.window();
        let mode = if grab { CursorGrabMode::Locked } else { CursorGrabMode::None };
        let result = window.set_cursor_grab(mode);
        if result.is_err() && grab {
            let _ = window.set_cursor_grab(CursorGrabMode::Confined);
        }
        window.set_cursor_visible(!grab);
        self.cursor_grabbed = grab;
    }

    fn simulate(&mut self, frame_delta: std::time::Duration) {
        let steps = self.fixed.accumulate(frame_delta);
        let dt = self.fixed.fixed_dt().as_secs_f32();

        for _ in 0..steps {
            let (dx, dy) = self.input.take_mouse_delta();
            self.camera.look(-dx * LOOK_SENSITIVITY, -dy * LOOK_SENSITIVITY);

            let [right_axis, forward_axis] =
                self.input.move_axis(KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD);
            let (sin_yaw, cos_yaw) = self.camera.yaw.sin_cos();
            let forward = Vec3::new(cos_yaw, 0.0, sin_yaw);
            let right = Vec3::new(sin_yaw, 0.0, -cos_yaw);
            let move_dir = forward * forward_axis + right * right_axis;
            let move_dir = if move_dir.length_squared() > 1.0 { move_dir.normalize() } else { move_dir };

            let sprinting = self.input.is_key_down(KeyCode::ShiftLeft) && forward_axis.abs() > 0.1;
            let speed = if sprinting { SPRINT_SPEED } else { WALK_SPEED };

            if self.input.just_pressed(KeyCode::Space) && self.grounded {
                self.vertical_velocity = JUMP_SPEED;
                self.grounded = false;
            }
            self.vertical_velocity = (self.vertical_velocity - GRAVITY * dt).max(-MAX_FALL_SPEED);

            let desired = Vector::new(
                move_dir.x * speed * dt,
                self.vertical_velocity * dt,
                move_dir.z * speed * dt,
            );

            let command = ClientInput {
                tick: self.client_tick,
                move_forward: forward_axis,
                move_right: right_axis,
                yaw_delta: -dx * LOOK_SENSITIVITY,
                pitch_delta: -dy * LOOK_SENSITIVITY,
                jump: self.input.just_pressed(KeyCode::Space),
                buttons: if sprinting { FLAG_SPRINTING } else { 0 },
            };
            self.simulation.queue_input(1, command);

            if let Some(physics) = self.physics.as_mut() {
                physics.step_dynamic_bodies();
                let movement = physics.move_character(self.character_origin, desired);
                self.character_origin += movement.translation;
                self.grounded = movement.grounded;
                if self.grounded && self.vertical_velocity < 0.0 {
                    self.vertical_velocity = 0.0;
                }
            }

            let feet = PhysicsWorld::character_feet(self.character_origin);
            self.camera.position = Vec3::new(feet.x, feet.y + PLAYER_EYE_HEIGHT, feet.z);

            self.client_tick += 1;
            self.simulation.step_once();
        }

        self.input.end_frame();
    }
}

impl ApplicationHandler for ClientApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let mut renderer = Renderer::new(event_loop);
        renderer.update_instances(&self.instances);
        let size = renderer.surface_size();
        self.camera.set_aspect(size.width as f32 / size.height.max(1) as f32);
        log::info!(
            "renderer ready: {}x{}, {} instances",
            size.width,
            size.height,
            renderer.instance_count()
        );
        self.renderer = Some(renderer);
        self.grab_cursor(true);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(owned_window_id) = self.renderer.as_ref().map(|renderer| renderer.window().id()) else {
            return;
        };
        if window_id != owned_window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
                self.camera.set_aspect(size.width as f32 / size.height.max(1) as f32);
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
                self.simulate(frame_delta);
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.render(&self.camera);
                }
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

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _device_id: winit::event::DeviceId, event: winit::event::DeviceEvent) {
        self.input.handle_device_event(&event);
    }
}
