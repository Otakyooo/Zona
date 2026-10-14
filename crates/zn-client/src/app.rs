use std::collections::HashMap;
use std::path::PathBuf;
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
use zn_render::{Camera, DrawList, MaterialId, MeshId, Renderer};
use zn_scene::{Scene, SceneObject};
use zn_sim::{ClientInput, Simulation, FLAG_SPRINTING};

use crate::assets::{AssetEvent, AssetLoader};
use crate::level::{build_draw_list, load_level, DEFAULT_CONTENT_ROOT, DEFAULT_LEVEL};

const LOOK_SENSITIVITY: f32 = 0.0022;
const WALK_SPEED: f32 = 5.5;
const SPRINT_SPEED: f32 = 8.5;
const JUMP_SPEED: f32 = 6.5;
const GRAVITY: f32 = 18.0;
const MAX_FALL_SPEED: f32 = 45.0;

/// Состояние загрузки ассета по ключу (путь или `builtin://…`).
pub enum AssetState {
    Pending,
    Loaded {
        meshes: Vec<MeshId>,
        materials: Vec<(Option<String>, MaterialId)>,
    },
    Failed,
}

pub struct ClientApp {
    renderer: Option<Renderer>,
    input: InputState,
    simulation: Simulation,
    scene: Scene,
    camera: Camera,
    physics: Option<PhysicsWorld>,
    asset_loader: AssetLoader,
    assets: HashMap<String, AssetState>,
    builtin_mesh_ids: HashMap<String, MeshId>,
    character_origin: Vector,
    vertical_velocity: f32,
    grounded: bool,
    cursor_grabbed: bool,
    fixed: FixedStep,
    last_frame: Option<Instant>,
    client_tick: u64,
}

impl ClientApp {
    /// Новая сессия клиента: уровень с диска (или demo_zone-fallback),
    /// физика из прокси-коллайдеров, фоновый загрузчик ассетов.
    pub fn with_content(content_root: impl Into<PathBuf>, level: &str) -> Self {
        let content_root = content_root.into();
        let (scene, _) = load_level(&content_root, level);
        log::info!("уровень '{}': {} объектов", scene.name, scene.len());
        let camera = Camera::new(scene.spawn, scene.spawn_yaw, 16.0 / 9.0);

        let mut physics = PhysicsWorld::with_gravity_y(-GRAVITY, 1.0 / 60.0);
        for object in &scene.objects {
            physics.add_scene_object(object);
        }
        physics.step_dynamic_bodies();

        let character_origin = PhysicsWorld::character_origin(Vector::new(
            scene.spawn.x,
            scene.spawn.y,
            scene.spawn.z,
        ));

        let mut assets = HashMap::new();
        let loader = AssetLoader::new(&content_root);
        for object in &scene.objects {
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

        Self {
            renderer: None,
            input: InputState::new(),
            simulation: Simulation::new(),
            scene,
            camera,
            physics: Some(physics),
            asset_loader: loader,
            assets,
            builtin_mesh_ids: HashMap::new(),
            character_origin,
            vertical_velocity: 0.0,
            grounded: false,
            cursor_grabbed: false,
            fixed: FixedStep::from_hz(60),
            last_frame: None,
            client_tick: 0,
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
            if !self
                .scene
                .objects
                .iter()
                .any(|object| object.asset_key() == format!("builtin://{name}"))
            {
                continue;
            }
            if let Some(mesh) = zn_assets::builtin_mesh(name) {
                let id = renderer.register_mesh(&mesh);
                self.builtin_mesh_ids.insert(name.to_string(), id);
            }
        }

        for event in self.asset_loader.drain_events() {
            match event {
                AssetEvent::Loaded { key, asset } => {
                    let meshes: Vec<MeshId> = asset
                        .meshes
                        .iter()
                        .map(|mesh| renderer.register_mesh(mesh))
                        .collect();
                    let materials: Vec<(Option<String>, MaterialId)> = asset
                        .materials
                        .iter()
                        .map(|material| {
                            let images = material_images(&asset, material);
                            let id = renderer.register_material(material, &images);
                            (material.name.clone(), id)
                        })
                        .collect();
                    log::info!(
                        "ассет '{key}' зарегистрирован: мешей {}, материалов {}",
                        meshes.len(),
                        materials.len()
                    );
                    self.assets
                        .insert(key, AssetState::Loaded { meshes, materials });
                }
                AssetEvent::Failed { key, error } => {
                    log::warn!("ассет '{key}' не загружен: {error} — fallback на builtin");
                    self.assets.insert(key, AssetState::Failed);
                }
            }
        }
    }

    /// Пары (меш, материал) для draw list; незагруженное → builtin fallback.
    fn resolve_asset(&self, object: &SceneObject) -> (MeshId, MaterialId) {
        let Some(renderer) = self.renderer.as_ref() else {
            return (0, 0);
        };
        let cube = renderer.builtin_cube_mesh();
        let default = renderer.default_material();
        let key = object.asset_key();

        if let Some(name) = zn_assets::parse_builtin_key(key) {
            return if name == "cube" {
                (cube, default)
            } else {
                self.builtin_mesh_ids
                    .get(name)
                    .map_or((cube, default), |id| (*id, default))
            };
        }

        match self.assets.get(key) {
            Some(AssetState::Loaded { meshes, materials }) => {
                let mesh = meshes.first().copied().unwrap_or(cube);
                let material = match object.material.as_deref() {
                    Some(name) => materials
                        .iter()
                        .find(|(material_name, _)| material_name.as_deref() == Some(name))
                        .map(|(_, id)| *id),
                    None => materials.first().map(|(_, id)| *id),
                }
                .unwrap_or(default);
                (mesh, material)
            }
            _ => (cube, default),
        }
    }

    fn build_frame_draw_list(&self) -> DrawList {
        build_draw_list(&self.scene, |object| self.resolve_asset(object))
    }

    fn simulate(&mut self, frame_delta: std::time::Duration) {
        let steps = self.fixed.accumulate(frame_delta);
        let dt = self.fixed.fixed_dt().as_secs_f32();

        for _ in 0..steps {
            let (dx, dy) = self.input.take_mouse_delta();
            self.camera
                .look(-dx * LOOK_SENSITIVITY, -dy * LOOK_SENSITIVITY);

            let [right_axis, forward_axis] =
                self.input
                    .move_axis(KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD);
            let (sin_yaw, cos_yaw) = self.camera.yaw.sin_cos();
            let forward = Vec3::new(cos_yaw, 0.0, sin_yaw);
            let right = Vec3::new(sin_yaw, -cos_yaw, 0.0);
            let move_dir = forward * forward_axis + right * right_axis;
            let move_dir = if move_dir.length_squared() > 1.0 {
                move_dir.normalize()
            } else {
                move_dir
            };

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
        let mut renderer = Renderer::new(event_loop);
        self.register_loaded_assets();
        let list = self.build_frame_draw_list();
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
        self.renderer = Some(renderer);
        self.grab_cursor(true);
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
            WindowEvent::CloseRequested => event_loop.exit(),
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
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let frame_delta = self
                    .last_frame
                    .map(|last| now - last)
                    .unwrap_or(std::time::Duration::from_millis(16));
                self.last_frame = Some(now);
                self.register_loaded_assets();
                self.simulate(frame_delta);
                let list = self.build_frame_draw_list();
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.update_draw_list(&list);
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

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        self.input.handle_device_event(&event);
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
        let app = ClientApp::with_content("game/content", "demo_zone");
        assert_eq!(app.scene.name, "demo_zone");
        assert!(!app.scene.is_empty());
        let physics = app.physics.as_ref().unwrap();
        // Коллайдеры созданы для всех solid-объектов сцены.
        assert!(physics.static_collider_count() > 0);
    }

    #[test]
    fn app_requires_loader_events_only_for_external_assets() {
        let app = ClientApp::with_content("game/content", "demo_zone");
        // demo_zone целиком на builtin-кубах — внешних запросов нет.
        assert!(app.assets.is_empty());
    }
}
