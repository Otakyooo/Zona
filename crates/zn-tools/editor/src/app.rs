use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, RichText, Stroke};
use egui_wgpu::ScreenDescriptor;
use glam::{Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::WindowId;

use zn_render::{Camera, DrawList, InstanceRaw, MaterialId, MaterialSpec, MeshId, Renderer};
use zn_scene::{ColliderShape, Scene, SceneEnvironment, SceneObject};

use crate::gizmo::{self, GizmoDelta, GizmoMode, GizmoState};
use crate::history::{
    environment_edit_label, object_edit_label, EditCommand, History, HISTORY_LIMIT,
};
use crate::orbit::OrbitCamera;
use crate::picking::{cursor_ray, pick_object, placement_point};

const DEFAULT_CONTENT_ROOT: &str = "game/content";
const CLICK_MOVE_TOLERANCE_PX: f32 = 4.0;
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(350);
const DOUBLE_CLICK_DISTANCE_PX: f32 = 4.0;
const FLY_SPEED: f32 = 12.0;
const PLACE_SCALE_DEFAULT: f32 = 1.0;
/// Шаг привязки по умолчанию (G4), метры.
const SNAP_DEFAULT: f32 = 0.5;

/// Двойной клик: тот же участок экрана в пределах `DOUBLE_CLICK_INTERVAL`.
fn is_double_click(last: Option<(Instant, Vec2)>, now: Instant, cursor: Vec2) -> bool {
    match last {
        Some((time, position)) => {
            now.duration_since(time) <= DOUBLE_CLICK_INTERVAL
                && position.distance(cursor) <= DOUBLE_CLICK_DISTANCE_PX
        }
        None => false,
    }
}

/// Загруженный в GPU ассет редактора.
pub struct LoadedAsset {
    pub meshes: Vec<MeshId>,
    pub materials: Vec<(Option<String>, MaterialId)>,
}

/// Текущий drag операции мышью.
#[derive(Clone, Copy, Debug)]
enum CameraDrag {
    Orbit { last: Vec2 },
    Pan { last: Vec2 },
    Gizmo,
}

pub struct EditorApp {
    renderer: Option<Renderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    ui_renderer: Option<egui_wgpu::Renderer>,
    camera: Camera,
    orbit: OrbitCamera,
    scene: Scene,
    /// История правок (M4g‑G4a): undo/redo объектов.
    history: History,
    content_root: PathBuf,
    current_path: Option<PathBuf>,
    path_buffer: String,
    selected: Option<usize>,
    gizmo: GizmoState,
    /// Объект до начала drag gizmo — команда истории на отпускании ЛКМ.
    gizmo_before: Option<(usize, SceneObject)>,
    drag: Option<CameraDrag>,
    cursor_px: Option<Vec2>,
    last_click: Option<(Instant, Vec2)>,
    keys_down: HashSet<KeyCode>,
    pending_place: Option<String>,
    assets: HashMap<String, LoadedAsset>,
    builtin_meshes: HashMap<String, MeshId>,
    /// Персональные материалы объектов (M4g): спека → GPU-материал.
    scene_materials: HashMap<MaterialSpec, MaterialId>,
    asset_files: Vec<String>,
    place_scale: f32,
    place_color: [f32; 4],
    /// Шаг привязки к сетке (G4), 0 — выключена. Округляется x/z; y не
    /// трогаем — объекты стоят на поверхностях.
    snap: f32,
    status: String,
    ctrl_down: bool,
}

impl EditorApp {
    pub fn new(scene: Scene, current_path: Option<PathBuf>, content_root: PathBuf) -> Self {
        let orbit = OrbitCamera::new(
            scene.spawn + Vec3::new(0.0, 14.0, 16.0),
            24.0,
            scene.spawn_yaw,
            -0.6,
        );
        let mut camera = Camera::new(orbit.position(), orbit.yaw, 16.0 / 9.0);
        orbit.apply(&mut camera);
        let egui_ctx = egui::Context::default();
        Self {
            renderer: None,
            egui_ctx,
            egui_state: None,
            ui_renderer: None,
            camera,
            orbit,
            scene,
            history: History::new(HISTORY_LIMIT),
            content_root,
            current_path,
            path_buffer: String::new(),
            selected: None,
            gizmo: GizmoState::new(GizmoMode::Translate),
            gizmo_before: None,
            drag: None,
            cursor_px: None,
            last_click: None,
            keys_down: HashSet::new(),
            pending_place: None,
            assets: HashMap::new(),
            builtin_meshes: HashMap::new(),
            scene_materials: HashMap::new(),
            asset_files: Vec::new(),
            place_scale: PLACE_SCALE_DEFAULT,
            place_color: [0.55, 0.55, 0.6, 1.0],
            snap: SNAP_DEFAULT,
            status: "готов".into(),
            ctrl_down: false,
        }
    }

    fn renderer(&self) -> &Renderer {
        self.renderer.as_ref().expect("renderer")
    }

    fn viewport_px(&self) -> Vec2 {
        let size = self.renderer().surface_size();
        Vec2::new(size.width as f32, size.height as f32)
    }

    // --- файловые операции ------------------------------------------------

    fn refresh_asset_files(&mut self) {
        self.asset_files = zn_assets::scan_asset_directory(&self.content_root.join("assets"))
            .unwrap_or_default()
            .iter()
            .filter(|path| zn_assets::is_supported_source(path))
            .filter_map(|path| {
                path.strip_prefix(&self.content_root)
                    .ok()
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        self.asset_files.sort();
    }

    fn scene_asset_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for object in &self.scene.objects {
            let key = object.asset_key().to_string();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    /// Синхронно грузит ассет (builtin или glTF) в GPU-кэши.
    fn ensure_asset(&mut self, key: &str) -> bool {
        if self.assets.contains_key(key) {
            return true;
        }
        if let Some(name) = zn_assets::parse_builtin_key(key) {
            let mut meshes = Vec::new();
            if name == "cube" {
                meshes.push(self.renderer().builtin_cube_mesh());
            } else {
                let mesh_id = if let Some(&id) = self.builtin_meshes.get(name) {
                    id
                } else {
                    let Some(mesh) = zn_assets::builtin_mesh(name) else {
                        return false;
                    };
                    let id = self.renderer.as_mut().unwrap().register_mesh(&mesh);
                    self.builtin_meshes.insert(name.to_string(), id);
                    id
                };
                meshes.push(mesh_id);
            }
            self.assets.insert(
                key.to_string(),
                LoadedAsset {
                    meshes,
                    materials: Vec::new(),
                },
            );
            return true;
        }

        let path = self.content_root.join(key);
        match zn_assets::import_gltf(&path) {
            Ok(asset) => {
                let renderer = self.renderer.as_mut().unwrap();
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
                self.assets
                    .insert(key.to_string(), LoadedAsset { meshes, materials });
                true
            }
            Err(error) => {
                log::warn!("ассет '{key}' не загружен: {error}");
                false
            }
        }
    }

    fn ensure_scene_assets(&mut self) {
        for key in self.scene_asset_keys() {
            if !self.ensure_asset(&key) {
                self.status = format!("ассет '{key}' не загружен — объекты показаны кубом");
            }
        }
        self.ensure_scene_materials();
    }

    /// Регистрирует персональные материалы объектов сцены (M4g): текстура,
    /// тайлинг, факторы. Кэш по спеке — как в клиенте; вызывается каждый кадр,
    /// чтобы правки свойств подхватывались сразу.
    fn ensure_scene_materials(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        for object in &self.scene.objects {
            let Some(spec) = MaterialSpec::from_object(object) else {
                continue;
            };
            if self.scene_materials.contains_key(&spec) {
                continue;
            }
            let (material, images) = spec.to_material();
            let refs: Vec<Option<&zn_assets::CpuImage>> =
                images.iter().map(|image| image.as_ref()).collect();
            let id = renderer.register_material(&material, &refs);
            self.scene_materials.insert(spec, id);
        }
    }

    fn save_scene(&mut self, path: &Path) {
        match self.scene.save_ron(path) {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                self.status = format!("сохранено: {}", path.display());
            }
            Err(error) => {
                self.status = format!("ошибка сохранения: {error}");
            }
        }
    }

    fn new_scene(&mut self) {
        self.scene = Scene::new("untitled");
        self.selected = None;
        self.current_path = None;
        self.assets.clear();
        self.history.clear();
        self.gizmo_before = None;
        self.status = "новая сцена".into();
    }

    fn open_scene(&mut self, path: &Path) {
        match Scene::load_ron(path) {
            Ok(scene) => {
                self.scene = scene;
                self.selected = None;
                self.current_path = Some(path.to_path_buf());
                self.assets.clear();
                self.ensure_scene_assets();
                self.history.clear();
                self.gizmo_before = None;
                self.status = format!("открыто: {}", path.display());
            }
            Err(error) => {
                self.status = format!("ошибка открытия: {error}");
            }
        }
    }

    // --- взаимодействие ----------------------------------------------------

    fn pick_at_cursor(&mut self) {
        let Some(cursor) = self.cursor_px else { return };
        let now = Instant::now();
        let double_click = is_double_click(self.last_click, now, cursor);
        self.last_click = Some((now, cursor));
        let (origin, dir) = cursor_ray(&self.camera, cursor, self.viewport_px());
        let bounds: Vec<_> = self.scene.objects.iter().map(scene_bounds).collect();
        self.selected = pick_object(bounds.iter(), origin, dir).map(|(index, _)| index);
        match self.selected {
            Some(index) => {
                self.status = format!("выделен объект {index}");
                // Двойной клик центрирует орбиту на объекте (одиночный — только выделяет).
                if double_click {
                    if let Some(object) = self.scene.objects.get(index) {
                        self.orbit.focus(scene_bounds(object).center());
                    }
                }
            }
            None => self.status = "выделение снято".into(),
        }
    }

    fn place_pending(&mut self) {
        let Some(key) = self.pending_place.take() else {
            return;
        };
        if !self.ensure_asset(&key) {
            self.status = format!("ассет '{key}' недоступен — размещение отменено");
            return;
        }
        let Some(cursor) = self.cursor_px else { return };
        let (origin, dir) = cursor_ray(&self.camera, cursor, self.viewport_px());
        let colliders: Vec<_> = self
            .scene
            .objects
            .iter()
            .filter(|o| o.effective_collider().is_some())
            .map(scene_bounds)
            .collect();
        let scale = Vec3::splat(self.place_scale.max(0.05));
        let Some(point) = placement_point(origin, dir, 0.0, &colliders, scale.y * 0.5) else {
            self.status = "нет поверхности под курсором".into();
            return;
        };
        let point = snap_xz(point, self.snap);
        let object = SceneObject::new(point, scale, glam::Vec4::from_array(self.place_color))
            .with_asset(key.clone())
            .with_yaw(0.0);
        let index = self.scene.objects.len();
        self.scene.add(object.clone());
        self.history
            .push(EditCommand::add(index, &object), Instant::now());
        self.selected = Some(index);
        self.status = format!("размещён объект {index} ({key})");
    }

    /// Дублирует выделенный объект (G4): клон встык за оригиналом, копия
    /// становится выделенной — её сразу можно тащить gizmo или править.
    fn duplicate_selected(&mut self) {
        let Some(index) = self.selected else {
            self.status = "нечего дублировать — выделите объект".into();
            return;
        };
        let Some(object) = self.scene.objects.get(index) else {
            return;
        };
        let copy = object.clone();
        let insert_at = index + 1;
        self.scene.objects.insert(insert_at, copy.clone());
        self.history
            .push(EditCommand::add(insert_at, &copy), Instant::now());
        self.selected = Some(insert_at);
        self.status = format!("объект {index} продублирован → {insert_at}");
    }

    fn delete_selected(&mut self) {
        if let Some(index) = self.selected {
            if let Some(object) = self.scene.objects.get(index) {
                let removed = object.clone();
                self.scene.objects.remove(index);
                self.history
                    .push(EditCommand::remove(index, &removed), Instant::now());
                self.status = format!("объект {index} удалён");
            }
        }
        self.selected = None;
        self.gizmo.end_drag();
        self.gizmo_before = None;
    }

    /// Отменяет последнюю правку сцены.
    fn undo_last(&mut self) {
        match self.history.undo(&mut self.scene, Instant::now()) {
            Some(label) => {
                if self
                    .selected
                    .is_some_and(|index| index >= self.scene.objects.len())
                {
                    self.selected = None;
                }
                self.status = format!("отменено: {label}");
            }
            None => self.status = "нечего отменять".into(),
        }
    }

    /// Повторяет отменённую правку сцены.
    fn redo_last(&mut self) {
        match self.history.redo(&mut self.scene, Instant::now()) {
            Some(label) => {
                if self
                    .selected
                    .is_some_and(|index| index >= self.scene.objects.len())
                {
                    self.selected = None;
                }
                self.status = format!("повтор: {label}");
            }
            None => self.status = "нечего повторять".into(),
        }
    }

    /// Завершает drag gizmo: сдвиг/поворот объекта — команда истории.
    fn finish_gizmo_drag(&mut self) {
        if let Some((index, before)) = self.gizmo_before.take() {
            if let Some(after) = self.scene.objects.get(index) {
                if &before != after {
                    let label = object_edit_label(&before, after);
                    self.history.push(
                        EditCommand::change(label, index, &before, after),
                        Instant::now(),
                    );
                }
            }
        }
        self.gizmo.end_drag();
    }

    fn handle_fly_keys(&mut self) {
        if self.keys_down.is_empty() {
            return;
        }
        let fly = self.keys_down.contains(&KeyCode::Space);
        if !fly {
            // WASD без Space — панорама по земле.
            let mut right_amount = 0.0;
            let mut up_amount = 0.0;
            if self.keys_down.contains(&KeyCode::KeyW) {
                up_amount += 1.0;
            }
            if self.keys_down.contains(&KeyCode::KeyS) {
                up_amount -= 1.0;
            }
            if self.keys_down.contains(&KeyCode::KeyD) {
                right_amount += 1.0;
            }
            if self.keys_down.contains(&KeyCode::KeyA) {
                right_amount -= 1.0;
            }
            let world_per_pixel = self.world_per_pixel();
            self.orbit.pan(
                self.camera.right(),
                self.camera.up(),
                right_amount * 12.0 * world_per_pixel,
                -up_amount * 12.0 * world_per_pixel,
            );
            return;
        }
        let forward = self.camera.forward();
        let right = self.camera.right();
        let mut forward_amount = 0.0;
        let mut right_amount = 0.0;
        let mut vertical = 0.0;
        if self.keys_down.contains(&KeyCode::KeyW) {
            forward_amount += 1.0;
        }
        if self.keys_down.contains(&KeyCode::KeyS) {
            forward_amount -= 1.0;
        }
        if self.keys_down.contains(&KeyCode::KeyD) {
            right_amount += 1.0;
        }
        if self.keys_down.contains(&KeyCode::KeyA) {
            right_amount -= 1.0;
        }
        if self.keys_down.contains(&KeyCode::KeyE) {
            vertical += 1.0;
        }
        if self.keys_down.contains(&KeyCode::KeyQ) {
            vertical -= 1.0;
        }
        self.orbit.fly(
            forward,
            right,
            forward_amount * FLY_SPEED,
            right_amount * FLY_SPEED,
            vertical * FLY_SPEED,
        );
    }

    /// Мировых единиц на пиксель экрана на глубине орбиты.
    fn world_per_pixel(&self) -> f32 {
        let height = self.viewport_px().y.max(1.0);
        2.0 * self.orbit.distance * (self.camera.fov_y / 2.0).tan() / height
    }

    fn apply_gizmo(&mut self, cursor: Vec2) {
        if !self.gizmo.is_dragging() {
            return;
        }
        let Some(index) = self.selected else { return };
        if index >= self.scene.objects.len() {
            return;
        }
        let bounds = scene_bounds(&self.scene.objects[index]);
        let center = bounds.center();
        match self
            .gizmo
            .update_drag(&self.camera, self.viewport_px(), center, cursor)
        {
            Some(GizmoDelta::Translation(translation)) => {
                let object = &mut self.scene.objects[index];
                object.position = snap_xz(object.position + translation, self.snap);
            }
            Some(GizmoDelta::Rotation(yaw_delta)) => {
                self.scene.objects[index].yaw += yaw_delta;
            }
            Some(GizmoDelta::Scale(factor)) => {
                // Uniform-масштаб: пропорции сохраняются, разумные пределы.
                let scale = &mut self.scene.objects[index].scale;
                *scale = (*scale * factor).clamp(Vec3::splat(0.01), Vec3::splat(100.0));
            }
            None => {}
        }
        self.status = format!("объект {index}: gizmo {}", self.gizmo.mode.label());
    }

    // --- egui ---------------------------------------------------------------

    fn draw_ui(&mut self, ctx: &egui::Context) {
        // max_rect корневого Ui: размер окна в логических точках.
        let size = self.renderer().surface_size();
        let ppp = ctx.pixels_per_point().max(1e-3);
        let screen_rect = egui::Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(size.width as f32 / ppp, size.height as f32 / ppp),
        );
        let mut root = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("editor_root"),
            egui::UiBuilder::new().max_rect(screen_rect),
        );

        egui::Panel::top("menu")
            .resizable(false)
            .show(&mut root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Новый").clicked() {
                        self.new_scene();
                    }
                    if ui.button("Открыть").clicked() {
                        let path = self.path_buffer.clone();
                        if !path.is_empty() {
                            self.open_scene(Path::new(&path));
                        } else if let Some(current) = &self.current_path {
                            let current = current.clone();
                            self.open_scene(&current);
                        } else {
                            self.status = "укажите путь к .scene.ron в поле пути".into();
                        }
                    }
                    if ui.button("Сохранить").clicked() {
                        if let Some(path) = self.current_path.clone() {
                            self.save_scene(&path);
                        } else {
                            let path = self.path_buffer.clone();
                            if path.is_empty() {
                                self.status = "путь не задан — используйте «Сохранить как»".into();
                            } else {
                                self.save_scene(Path::new(&path));
                            }
                        }
                    }
                    if ui.button("Сохранить как").clicked() {
                        let path = self.path_buffer.clone();
                        if path.is_empty() {
                            self.status = "введите путь для сохранения".into();
                        } else {
                            self.save_scene(Path::new(&path));
                        }
                    }
                    ui.separator();
                    let undo_label = match self.history.undo_label() {
                        Some(label) => format!("↶ Отменить: {label}"),
                        None => "↶ Отменить".to_string(),
                    };
                    if ui
                        .add_enabled(
                            self.history.can_undo(),
                            egui::Button::new(RichText::new(undo_label)),
                        )
                        .clicked()
                    {
                        self.undo_last();
                    }
                    let redo_label = match self.history.redo_label() {
                        Some(label) => format!("↷ Повторить: {label}"),
                        None => "↷ Повторить".to_string(),
                    };
                    if ui
                        .add_enabled(
                            self.history.can_redo(),
                            egui::Button::new(RichText::new(redo_label)),
                        )
                        .clicked()
                    {
                        self.redo_last();
                    }
                    ui.separator();
                    let mode_label = format!("Gizmo: {}", self.gizmo.mode.label());
                    if ui.button(mode_label).clicked() {
                        self.gizmo.toggle_mode();
                        self.status = format!("режим gizmo: {}", self.gizmo.mode.label());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("путь:");
                    let mut path = self.path_buffer.clone();
                    ui.add(egui::TextEdit::singleline(&mut path).desired_width(400.0));
                    if path != self.path_buffer {
                        self.path_buffer = path;
                    }
                    if let Some(current) = &self.current_path {
                        ui.label(RichText::new(current.to_string_lossy().to_string()).weak());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    let history = self.history.depth();
                    let history_note = if history > 0 {
                        format!(" · история: {history}")
                    } else {
                        String::new()
                    };
                    ui.label(format!(
                        "сцена: {} ({} объектов){} — {}",
                        self.scene.name,
                        self.scene.len(),
                        history_note,
                        self.status
                    ));
                });
            });

        egui::Panel::left("objects")
            .default_size(260.0)
            .resizable(true)
            .show(&mut root, |ui| {
                ui.label(RichText::new("Объекты").strong());
                let mut selection = self.selected;
                egui::ScrollArea::vertical()
                    .id_salt("objects-list")
                    .max_height(280.0)
                    .show(ui, |ui| {
                        for (index, object) in self.scene.objects.iter().enumerate() {
                            let label = format!("#{index} {}", object.asset_key());
                            if ui
                                .selectable_label(self.selected == Some(index), label)
                                .clicked()
                            {
                                selection = Some(index);
                            }
                        }
                    });
                self.selected = selection;

                ui.horizontal(|ui| {
                    if ui.button("Удалить").clicked() {
                        self.delete_selected();
                    }
                    if ui.button("Дублировать").clicked() {
                        self.duplicate_selected();
                    }
                    if ui.button("Снять выделение").clicked() {
                        self.selected = None;
                    }
                });

                ui.separator();
                ui.label(RichText::new("Ассеты").strong());
                ui.horizontal(|ui| {
                    ui.label("размер:");
                    ui.add(
                        egui::DragValue::new(&mut self.place_scale)
                            .speed(0.1)
                            .range(0.05..=32.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("цвет:");
                    let mut color = self.place_color;
                    if ui.color_edit_button_rgba_unmultiplied(&mut color).changed() {
                        self.place_color = color;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("привязка:");
                    let mut snap = self.snap;
                    egui::ComboBox::from_id_salt("snap")
                        .selected_text(if snap > 0.0 {
                            format!("{snap} м")
                        } else {
                            "выкл".to_string()
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut snap, 0.0, "выкл");
                            for step in [0.25, 0.5, 1.0, 2.0] {
                                ui.selectable_value(&mut snap, step, format!("{step} м"));
                            }
                        });
                    if snap != self.snap {
                        self.snap = snap;
                        self.status = if snap > 0.0 {
                            format!("привязка к сетке {snap} м")
                        } else {
                            "привязка выключена".into()
                        };
                    }
                });

                egui::ScrollArea::vertical()
                    .id_salt("assets-list")
                    .show(ui, |ui| {
                        ui.label(RichText::new("Встроенные примитивы").weak());
                        for name in zn_assets::BUILTIN_MESH_NAMES {
                            let key = zn_assets::builtin_key(name);
                            ui.horizontal(|ui| {
                                ui.label(&key);
                                if ui.small_button("поставить").clicked() {
                                    self.pending_place = Some(key.clone());
                                    self.status = format!("клик по сцене разместит {key}");
                                }
                            });
                        }
                        ui.label(RichText::new("Файлы (content/assets)").weak());
                        if self.asset_files.is_empty() {
                            ui.label(RichText::new("<пусто>").weak());
                        }
                        for key in self.asset_files.clone() {
                            ui.horizontal(|ui| {
                                let label = key.replace('\\', "/");
                                if ui.small_button(&label).clicked() {
                                    self.pending_place = Some(key.clone());
                                    self.status = format!("клик по сцене разместит {key}");
                                }
                            });
                        }
                    });
            });

        egui::Panel::right("properties")
            .default_size(300.0)
            .resizable(true)
            .show(&mut root, |ui| {
                ui.label(RichText::new("Свойства").strong());
                // Свойства выделенного объекта и атмосфера сцены (G4) — в одной
                // прокрутке: длинные панели больше не обрезаются по высоте.
                egui::ScrollArea::vertical()
                    .id_salt("properties-scroll")
                    .show(ui, |ui| {
                        if let Some(index) = self.selected {
                            // Имена материалов импортированного ассета объекта — для комбо.
                            let asset_material_names: Vec<String> = self
                                .scene
                                .objects
                                .get(index)
                                .and_then(|object| object.asset.as_deref())
                                .and_then(|key| self.assets.get(key))
                                .map(|asset| {
                                    let mut names: Vec<String> = asset
                                        .materials
                                        .iter()
                                        .filter_map(|(name, _)| name.clone())
                                        .collect();
                                    names.sort();
                                    names.dedup();
                                    names
                                })
                                .unwrap_or_default();

                            // Снимок «до» — правки за кадр сравниваются с ним и
                            // уходят в историю одной командой (склейка по окну).
                            let before = self.scene.objects.get(index).cloned();
                            if let Some(object) = self.scene.objects.get_mut(index) {
                                ui.label(format!("Объект #{index}"));

                                ui.horizontal(|ui| {
                                    ui.label("position");
                                    // Правка идёт по копии: при включённой
                                    // привязке округляем x/z до сетки (G4).
                                    let mut position = object.position;
                                    let mut edited = false;
                                    edited |= ui
                                        .add(
                                            egui::DragValue::new(&mut position.x)
                                                .speed(0.1)
                                                .prefix("x "),
                                        )
                                        .changed();
                                    edited |= ui
                                        .add(
                                            egui::DragValue::new(&mut position.y)
                                                .speed(0.1)
                                                .prefix("y "),
                                        )
                                        .changed();
                                    edited |= ui
                                        .add(
                                            egui::DragValue::new(&mut position.z)
                                                .speed(0.1)
                                                .prefix("z "),
                                        )
                                        .changed();
                                    if edited {
                                        object.position = snap_xz(position, self.snap);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("scale");
                                    ui.add(
                                        egui::DragValue::new(&mut object.scale.x)
                                            .speed(0.1)
                                            .range(0.01..=100.0)
                                            .prefix("x "),
                                    );
                                    ui.add(
                                        egui::DragValue::new(&mut object.scale.y)
                                            .speed(0.1)
                                            .range(0.01..=100.0)
                                            .prefix("y "),
                                    );
                                    ui.add(
                                        egui::DragValue::new(&mut object.scale.z)
                                            .speed(0.1)
                                            .range(0.01..=100.0)
                                            .prefix("z "),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("yaw");
                                    let mut yaw = object.yaw.to_degrees();
                                    if ui
                                        .add(egui::DragValue::new(&mut yaw).speed(1.0).suffix("°"))
                                        .changed()
                                    {
                                        object.yaw = yaw.to_radians();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("tint");
                                    let mut color = object.color.to_array();
                                    if ui.color_edit_button_rgba_unmultiplied(&mut color).changed()
                                    {
                                        object.color = glam::Vec4::from_array(color);
                                    }
                                });

                                let mut asset = object.asset.clone().unwrap_or_default();
                                if ui
                                    .horizontal(|ui| {
                                        ui.label("asset");
                                        ui.text_edit_singleline(&mut asset).changed()
                                    })
                                    .inner
                                {
                                    object.asset =
                                        if asset.is_empty() { None } else { Some(asset) };
                                }

                                ui.horizontal(|ui| {
                                    ui.label("material");
                                    let current = object.material.clone().unwrap_or_default();
                                    let mut material = current.clone();
                                    let edited = ui.text_edit_singleline(&mut material).changed();
                                    let mut picked = false;
                                    if !asset_material_names.is_empty() {
                                        // Имена материалов ассета — помимо ручного ввода.
                                        egui::ComboBox::from_id_salt("object-material")
                                            .selected_text("▾")
                                            .show_ui(ui, |ui| {
                                                if ui
                                                    .selectable_label(
                                                        material.is_empty(),
                                                        "<по мешу>",
                                                    )
                                                    .clicked()
                                                {
                                                    material.clear();
                                                    picked = true;
                                                }
                                                for name in &asset_material_names {
                                                    if ui
                                                        .selectable_label(material == *name, name)
                                                        .clicked()
                                                    {
                                                        material = name.clone();
                                                        picked = true;
                                                    }
                                                }
                                            });
                                    }
                                    if edited || picked {
                                        object.material = if material.is_empty() {
                                            None
                                        } else {
                                            Some(material)
                                        };
                                    }
                                });

                                // Материал поверхности (M4g): текстура, тайлинг, факторы.
                                ui.separator();
                                ui.label(RichText::new("Материал поверхности").strong());
                                ui.horizontal(|ui| {
                                    ui.label("texture");
                                    let mut texture = object.texture.clone().unwrap_or_default();
                                    let mut picked = false;
                                    egui::ComboBox::from_id_salt("object-texture")
                                        .selected_text(if texture.is_empty() {
                                            "—".to_string()
                                        } else {
                                            texture.trim_start_matches("builtin://").to_string()
                                        })
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(texture.is_empty(), "—")
                                                .clicked()
                                            {
                                                texture.clear();
                                                picked = true;
                                            }
                                            for name in zn_assets::BUILTIN_TEXTURE_NAMES {
                                                let key = zn_assets::builtin_key(name);
                                                if ui
                                                    .selectable_label(texture == key, *name)
                                                    .clicked()
                                                {
                                                    texture = key;
                                                    picked = true;
                                                }
                                            }
                                        });
                                    if picked {
                                        let had_texture = object.texture.is_some();
                                        object.texture = if texture.is_empty() {
                                            None
                                        } else {
                                            Some(texture)
                                        };
                                        // Первая текстура на кубах — сразу мировые UV: без
                                        // растяжения по граням (флажок ниже это переопределяет).
                                        if !had_texture && object.texture.is_some() {
                                            object.world_uv = true;
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("uv scale");
                                    ui.add(
                                        egui::DragValue::new(&mut object.uv_scale)
                                            .speed(0.1)
                                            .range(0.05..=64.0),
                                    );
                                    let mut world_uv = object.world_uv;
                                    if ui.checkbox(&mut world_uv, "мировые UV").changed() {
                                        object.world_uv = world_uv;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("roughness");
                                    let mut enabled = object.roughness.is_some();
                                    if ui.checkbox(&mut enabled, "").changed() {
                                        object.roughness = if enabled { Some(0.85) } else { None };
                                    }
                                    if let Some(value) = object.roughness.as_mut() {
                                        ui.add(
                                            egui::DragValue::new(value)
                                                .speed(0.01)
                                                .range(0.0..=1.0),
                                        );
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("metallic");
                                    let mut enabled = object.metallic.is_some();
                                    if ui.checkbox(&mut enabled, "").changed() {
                                        object.metallic = if enabled { Some(0.0) } else { None };
                                    }
                                    if let Some(value) = object.metallic.as_mut() {
                                        ui.add(
                                            egui::DragValue::new(value)
                                                .speed(0.01)
                                                .range(0.0..=1.0),
                                        );
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("emissive");
                                    let mut enabled = object.emissive.is_some();
                                    if ui.checkbox(&mut enabled, "").changed() {
                                        object.emissive = if enabled {
                                            Some(Vec3::new(0.8, 0.7, 0.4))
                                        } else {
                                            None
                                        };
                                    }
                                    if let Some(value) = object.emissive.as_mut() {
                                        let mut color = [value.x, value.y, value.z];
                                        if ui.color_edit_button_rgb(&mut color).changed() {
                                            *value = Vec3::from_array(color);
                                        }
                                    }
                                });
                                if object.has_material_overrides() {
                                    ui.label(RichText::new("персональный материал (M4g)").weak());
                                }
                                ui.separator();

                                let mut collider = object.collider;
                                egui::ComboBox::from_id_salt("collider")
                                    .selected_text(
                                        object
                                            .collider
                                            .map(|shape| shape.as_str())
                                            .unwrap_or("по умолчанию (box)"),
                                    )
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut collider,
                                            None,
                                            "по умолчанию (box)",
                                        );
                                        for shape in [
                                            ColliderShape::Box,
                                            ColliderShape::Cylinder,
                                            ColliderShape::Sphere,
                                        ] {
                                            ui.selectable_value(
                                                &mut collider,
                                                Some(shape),
                                                shape.as_str(),
                                            );
                                        }
                                    });
                                if collider != object.collider {
                                    object.collider = collider;
                                }

                                let mut solid = object.solid;
                                if ui.checkbox(&mut solid, "solid").changed() {
                                    object.solid = solid;
                                }
                            }
                            // Итог правок за кадр — команда истории (склейка по окну).
                            if let (Some(before), Some(after)) =
                                (before, self.scene.objects.get(index))
                            {
                                if before != *after {
                                    let label = object_edit_label(&before, after);
                                    self.history.push(
                                        EditCommand::change(label, index, &before, after),
                                        Instant::now(),
                                    );
                                }
                            }
                        } else {
                            ui.label(RichText::new("ничего не выделено").weak());
                        }
                        ui.separator();
                        self.draw_environment(ui);
                    });
            });

        egui::Panel::bottom("hints").resizable(false).show(&mut root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("ЛКМ-драг: орбита · ЛКМ-клик: выделить · ПКМ-драг: панорама · Space+WASD: полёт · колесо: зум · G: gizmo · Del: удалить · Ctrl+D: дублировать · Ctrl+Z/Ctrl+Y: отменить/повторить · Ctrl+S: сохранить");
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
            .show(&mut root, |ui| {
                let painter = ui.painter();
                let viewport = self.viewport_px();

                if let Some(index) = self.selected {
                    if let Some(object) = self.scene.objects.get(index) {
                        draw_selection_outline(painter, &self.camera, viewport, object);
                    }
                }
                if let Some(index) = self.selected {
                    if let Some(object) = self.scene.objects.get(index) {
                        draw_gizmo(
                            painter,
                            &self.camera,
                            viewport,
                            self.gizmo.mode,
                            object.position,
                        );
                    }
                }
            });
    }

    /// Панель атмосферы (G4): солнце, ambient, туман, небо. Правки уходят в
    /// историю одной командой (серии DragValue склеиваются по окну), превью
    /// живое — `set_atmosphere` вызывается каждый кадр отрисовки.
    fn draw_environment(&mut self, ui: &mut egui::Ui) {
        let before = self.scene.environment.clone();
        ui.label(RichText::new("Атмосфера").strong());
        ui.horizontal_wrapped(|ui| {
            for (preset, label) in [
                ("clear", "ясный день"),
                ("dust", "пыльная мгла"),
                ("underground", "подземка"),
                ("overcast", "пасмурно"),
            ] {
                if ui.small_button(label).clicked() {
                    if let Some(environment) = environment_preset(preset) {
                        self.scene.environment = environment;
                        self.status = format!("атмосфера: {label}");
                    }
                }
            }
            if ui.small_button("сброс").clicked() {
                self.scene.environment = SceneEnvironment::default();
                self.status = "атмосфера: по умолчанию".into();
            }
        });

        {
            let environment = &mut self.scene.environment;
            ui.label(RichText::new("Солнце").weak());
            ui.horizontal(|ui| {
                ui.label("направление");
                ui.add(
                    egui::DragValue::new(&mut environment.sun_direction.x)
                        .speed(0.01)
                        .range(-1.0..=1.0)
                        .prefix("x "),
                );
                ui.add(
                    egui::DragValue::new(&mut environment.sun_direction.y)
                        .speed(0.01)
                        .range(-1.0..=1.0)
                        .prefix("y "),
                );
                ui.add(
                    egui::DragValue::new(&mut environment.sun_direction.z)
                        .speed(0.01)
                        .range(-1.0..=1.0)
                        .prefix("z "),
                );
            });
            ui.horizontal(|ui| {
                ui.label("цвет");
                let mut color = environment.sun_color.to_array();
                if ui.color_edit_button_rgb(&mut color).changed() {
                    environment.sun_color = Vec3::from_array(color);
                }
                ui.label("яркость");
                ui.add(
                    egui::DragValue::new(&mut environment.sun_intensity)
                        .speed(0.05)
                        .range(0.0..=4.0),
                );
            });

            ui.label(RichText::new("Ambient").weak());
            ui.horizontal(|ui| {
                ui.label("цвет");
                let mut color = environment.ambient_color.to_array();
                if ui.color_edit_button_rgb(&mut color).changed() {
                    environment.ambient_color = Vec3::from_array(color);
                }
                ui.label("яркость");
                ui.add(
                    egui::DragValue::new(&mut environment.ambient_intensity)
                        .speed(0.05)
                        .range(0.0..=4.0),
                );
            });

            ui.label(RichText::new("Туман").weak());
            ui.horizontal(|ui| {
                ui.label("цвет");
                let mut color = environment.fog_color.to_array();
                if ui.color_edit_button_rgb(&mut color).changed() {
                    environment.fog_color = Vec3::from_array(color);
                }
                ui.label("плотность");
                ui.add(
                    egui::DragValue::new(&mut environment.fog_density)
                        .speed(0.002)
                        .range(0.0..=0.2),
                );
            });

            ui.label(RichText::new("Небо").weak());
            ui.horizontal(|ui| {
                ui.label("зенит");
                let mut color = environment.sky_top.to_array();
                if ui.color_edit_button_rgb(&mut color).changed() {
                    environment.sky_top = Vec3::from_array(color);
                }
            });
            ui.horizontal(|ui| {
                ui.label("горизонт");
                let mut color = environment.sky_horizon.to_array();
                if ui.color_edit_button_rgb(&mut color).changed() {
                    environment.sky_horizon = Vec3::from_array(color);
                }
            });
        }

        // Итог правок за кадр — команда истории (склейка серий по окну).
        let after = self.scene.environment.clone();
        if before != after {
            let label = environment_edit_label(&before, &after);
            self.history.push(
                EditCommand::environment(label, &before, &after),
                Instant::now(),
            );
        }
    }

    fn build_draw_list(&self) -> DrawList {
        let renderer = self.renderer();
        let mut list = DrawList::new();
        for object in &self.scene.objects {
            let key = object.asset_key();
            // Персональный материал объекта (M4g) перекрывает материал ассета.
            let override_material = MaterialSpec::from_object(object)
                .and_then(|spec| self.scene_materials.get(&spec).copied());
            let (mesh, material) = self
                .assets
                .get(key)
                .map(|asset| {
                    let mesh = asset
                        .meshes
                        .first()
                        .copied()
                        .unwrap_or(renderer.builtin_cube_mesh());
                    let material = match object.material.as_deref() {
                        Some(name) => asset
                            .materials
                            .iter()
                            .find(|(material_name, _)| material_name.as_deref() == Some(name))
                            .map(|(_, id)| *id),
                        None => asset.materials.first().map(|(_, id)| *id),
                    }
                    .unwrap_or(renderer.default_material());
                    (mesh, material)
                })
                .unwrap_or((renderer.builtin_cube_mesh(), renderer.default_material()));
            list.push(
                mesh,
                override_material.unwrap_or(material),
                InstanceRaw {
                    model: object.model_matrix().to_cols_array(),
                    color: object.color.into(),
                },
            );
        }
        list
    }
}

impl Default for EditorApp {
    fn default() -> Self {
        let scene = Scene::demo_zone();
        Self::new(scene, None, PathBuf::from(DEFAULT_CONTENT_ROOT))
    }
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let renderer = Renderer::new(event_loop);
        renderer.set_window_title("ZONA — редактор уровней");
        let pixels_per_point = renderer.window().scale_factor() as f32;
        self.egui_ctx.set_pixels_per_point(pixels_per_point);
        let egui_viewport_id = egui::ViewportId::ROOT;
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui_viewport_id,
            renderer.window(),
            Some(pixels_per_point),
            None,
            None,
        );
        self.egui_state = Some(egui_state);
        // UI: egui-wgpu поверх сцены. msaa 1 — кадр без мультисэмплинга;
        // depth не нужен: проход оверлея идёт без depth attach.
        let ui_renderer = egui_wgpu::Renderer::new(
            renderer.device(),
            renderer.surface_format(),
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        );
        self.renderer = Some(renderer);
        self.ui_renderer = Some(ui_renderer);
        if let Some(path) = self.current_path.clone() {
            self.path_buffer = path.to_string_lossy().to_string();
        }
        self.refresh_asset_files();
        self.ensure_scene_assets();
        let size = self.renderer().surface_size();
        self.camera
            .set_aspect(size.width as f32 / size.height.max(1) as f32);
        log::info!("editor ready");
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        if window_id != renderer.window().id() {
            return;
        }
        let window = renderer.window();
        let egui_consumed = self
            .egui_state
            .as_mut()
            .map(|state| state.on_window_event(window, &event).consumed)
            .unwrap_or(false);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
                self.camera
                    .set_aspect(size.width as f32 / size.height.max(1) as f32);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let cursor = Vec2::new(position.x as f32, position.y as f32);
                self.cursor_px = Some(cursor);
                if egui_consumed {
                    return;
                }
                match self.drag {
                    Some(CameraDrag::Orbit { last }) => {
                        let delta = cursor - last;
                        self.orbit.orbit(delta, self.viewport_px().y);
                        self.drag = Some(CameraDrag::Orbit { last: cursor });
                    }
                    Some(CameraDrag::Pan { last }) => {
                        let delta = cursor - last;
                        let world_per_pixel = self.world_per_pixel();
                        let right = self.camera.right();
                        let up = self.camera.up();
                        self.orbit.pan(
                            right,
                            up,
                            delta.x * world_per_pixel,
                            -delta.y * world_per_pixel,
                        );
                        self.drag = Some(CameraDrag::Pan { last: cursor });
                    }
                    Some(CameraDrag::Gizmo) => self.apply_gizmo(cursor),
                    None => {}
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if egui_consumed {
                    return;
                }
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        let Some(cursor) = self.cursor_px else { return };
                        if self.selected.is_some()
                            && self.gizmo.begin_drag(
                                &self.camera,
                                self.viewport_px(),
                                self.selected_center(),
                                cursor,
                            )
                        {
                            // Снимок «до» — команда истории на отпускании ЛКМ.
                            self.gizmo_before = self.selected.and_then(|index| {
                                self.scene
                                    .objects
                                    .get(index)
                                    .map(|object| (index, object.clone()))
                            });
                            self.drag = Some(CameraDrag::Gizmo);
                        } else {
                            self.drag = Some(CameraDrag::Orbit { last: cursor });
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        let was_drag = match self.drag {
                            Some(CameraDrag::Orbit { last }) => {
                                self.cursor_px.is_some_and(|cursor| {
                                    cursor.distance(last) > CLICK_MOVE_TOLERANCE_PX
                                })
                            }
                            Some(CameraDrag::Gizmo) => {
                                self.finish_gizmo_drag();
                                true
                            }
                            _ => false,
                        };
                        self.drag = None;
                        if !was_drag {
                            self.pick_at_cursor();
                            if self.pending_place.is_some() {
                                self.place_pending();
                            }
                        }
                    }
                    (MouseButton::Right, ElementState::Pressed) => {
                        if let Some(cursor) = self.cursor_px {
                            self.drag = Some(CameraDrag::Pan { last: cursor });
                        }
                    }
                    (MouseButton::Right, ElementState::Released) => {
                        if matches!(self.drag, Some(CameraDrag::Pan { .. })) {
                            self.drag = None;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if egui_consumed {
                    return;
                }
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(offset) => offset.y as f32 / 40.0,
                };
                self.orbit.zoom((1.0 - lines * 0.1).clamp(0.1, 10.0));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.ctrl_down = modifiers.state().control_key();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        ..
                    },
                ..
            } => {
                if matches!(state, ElementState::Pressed) {
                    if self.ctrl_down && code == KeyCode::KeyS {
                        if let Some(path) = self.current_path.clone() {
                            self.save_scene(&path);
                        } else {
                            self.status =
                                "Ctrl+S: путь не задан — используйте «Сохранить как»".into();
                        }
                        return;
                    }
                    if egui_consumed {
                        return;
                    }
                    self.keys_down.insert(code);
                    match code {
                        KeyCode::KeyG => {
                            self.gizmo.toggle_mode();
                            self.status = format!("режим gizmo: {}", self.gizmo.mode.label());
                        }
                        KeyCode::KeyZ if self.ctrl_down => self.undo_last(),
                        KeyCode::KeyY if self.ctrl_down => self.redo_last(),
                        KeyCode::KeyD if self.ctrl_down => self.duplicate_selected(),
                        KeyCode::Delete | KeyCode::Backspace => self.delete_selected(),
                        KeyCode::Escape => {
                            self.selected = None;
                            self.status = "выделение снято".into();
                        }
                        _ => {}
                    }
                } else {
                    self.keys_down.remove(&code);
                }
            }
            WindowEvent::RedrawRequested => {
                let raw_input = {
                    let renderer = self.renderer.as_ref().unwrap();
                    let window = renderer.window();
                    let mut input = self
                        .egui_state
                        .as_mut()
                        .expect("egui state")
                        .take_egui_input(window);
                    let size = renderer.surface_size();
                    let pixels_per_point = window.scale_factor() as f32;
                    let logical = egui::Rect::from_min_size(
                        Pos2::ZERO,
                        egui::vec2(size.width as f32, size.height as f32) / pixels_per_point,
                    );
                    input.screen_rect = Some(logical);
                    input
                };
                self.egui_ctx.begin_pass(raw_input);
                let ctx = self.egui_ctx.clone();
                self.draw_ui(&ctx);
                let mut output = self.egui_ctx.end_pass();
                if let (Some(state), Some(renderer)) =
                    (self.egui_state.as_mut(), self.renderer.as_ref())
                {
                    state.handle_platform_output(renderer.window(), output.platform_output);
                }

                self.handle_fly_keys();
                self.orbit.apply(&mut self.camera);

                let clipped = self
                    .egui_ctx
                    .tessellate(output.shapes, self.egui_ctx.pixels_per_point());
                let (screen_size_px, pixels_per_point) = {
                    let renderer = self.renderer();
                    let size = renderer.surface_size();
                    ([size.width, size.height], self.egui_ctx.pixels_per_point())
                };
                let screen_descriptor = ScreenDescriptor {
                    size_in_pixels: screen_size_px,
                    pixels_per_point,
                };

                // Текстурные дельты egui (атлас шрифта и user-текстуры) — до
                // рисования. Дельты необходимо ПОТРЕБИТЬ (set + free), иначе
                // Drop TexturesDelta падает на debug_assert.
                if let (Some(ui_renderer), Some(renderer)) =
                    (self.ui_renderer.as_mut(), self.renderer.as_ref())
                {
                    let mut textures = std::mem::take(&mut output.textures_delta);
                    for (id, deltas) in textures.set.drain() {
                        for delta in deltas {
                            ui_renderer.update_texture(
                                renderer.device(),
                                renderer.queue(),
                                id,
                                &delta,
                            );
                        }
                    }
                    for texture_id in textures.free.drain() {
                        ui_renderer.free_texture(&texture_id);
                    }
                }

                // Правки свойств материалов (M4g) подхватываются в этом кадре.
                self.ensure_scene_materials();
                let list = self.build_draw_list();
                if let (Some(renderer), Some(ui_renderer)) =
                    (self.renderer.as_mut(), self.ui_renderer.as_mut())
                {
                    // Атмосфера редактируемой локации (M4g) — как в игре.
                    renderer.set_atmosphere(&self.scene.environment);
                    let (bounds_min, bounds_max) = self.scene.bounds();
                    renderer.set_shadow_bounds(bounds_min, bounds_max);
                    renderer.update_draw_list(&list);
                    let device = renderer.device().clone();
                    let queue = renderer.queue().clone();
                    // Аплоад буферов egui отдельным энкодером до кадра:
                    // render() требует свежего update_buffers в этом кадре.
                    let mut upload =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("zn-editor-ui-upload"),
                        });
                    let user_buffers = ui_renderer.update_buffers(
                        &device,
                        &queue,
                        &mut upload,
                        &clipped,
                        &screen_descriptor,
                    );
                    queue.submit(user_buffers.into_iter().chain([upload.finish()]));
                    let camera = &self.camera;
                    renderer.render_with(camera, |pass| {
                        let mut pass = pass.forget_lifetime();
                        ui_renderer.render(&mut pass, &clipped, &screen_descriptor);
                    });
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.window().request_redraw();
        }
    }
}

impl EditorApp {
    fn selected_center(&self) -> Vec3 {
        self.selected
            .and_then(|index| self.scene.objects.get(index))
            .map(scene_bounds)
            .map(|bounds| bounds.center())
            .unwrap_or(Vec3::ZERO)
    }
}

// --- помощники отрисовки/геометрии ------------------------------------------

/// Привязка к сетке (G4): округление x/z до шага `snap` (0 — выключена).
/// y не трогаем: объекты стоят на поверхностях, высоту задаёт placement.
fn snap_xz(point: Vec3, snap: f32) -> Vec3 {
    if snap <= 0.0 {
        return point;
    }
    Vec3::new(
        (point.x / snap).round() * snap,
        point.y,
        (point.z / snap).round() * snap,
    )
}

fn scene_bounds(object: &SceneObject) -> zn_assets::Bounds {
    let (min, max) = object.bounds();
    zn_assets::Bounds { min, max }
}

fn draw_selection_outline(
    painter: &egui::Painter,
    camera: &Camera,
    viewport: Vec2,
    object: &SceneObject,
) {
    let bounds = scene_bounds(object);
    let corners = [
        Vec3::new(bounds.min.x, bounds.min.y, bounds.min.z),
        Vec3::new(bounds.max.x, bounds.min.y, bounds.min.z),
        Vec3::new(bounds.max.x, bounds.max.y, bounds.min.z),
        Vec3::new(bounds.min.x, bounds.max.y, bounds.min.z),
        Vec3::new(bounds.min.x, bounds.min.y, bounds.max.z),
        Vec3::new(bounds.max.x, bounds.min.y, bounds.max.z),
        Vec3::new(bounds.max.x, bounds.max.y, bounds.max.z),
        Vec3::new(bounds.min.x, bounds.max.y, bounds.max.z),
    ];
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    let screens: Vec<Option<Pos2>> = corners
        .iter()
        .map(|corner| {
            gizmo::world_to_screen(camera, viewport, *corner)
                .map(|point| Pos2::new(point.x, point.y))
        })
        .collect();
    let stroke = Stroke::new(1.5, Color32::from_rgb(255, 214, 90));
    for (a, b) in edges {
        if let (Some(a), Some(b)) = (screens[a], screens[b]) {
            painter.line_segment([a, b], stroke);
        }
    }
}

/// Пресеты атмосферы (G4) — настроения встроенных локаций: быстрый старт
/// дизайна новой сцены без ручной подгонки девяти полей. Значения зеркалят
/// `SceneEnvironment` встроенных сцен (`zn_scene::Scene::base` и др.).
fn environment_preset(name: &str) -> Option<SceneEnvironment> {
    Some(match name {
        // База: ясный день, синее небо, слабая дымка.
        "clear" => SceneEnvironment {
            sun_direction: Vec3::new(-0.35, 0.65, 0.45),
            sun_color: Vec3::new(1.0, 0.94, 0.82),
            sun_intensity: 0.95,
            ambient_color: Vec3::new(0.28, 0.30, 0.35),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.55, 0.60, 0.68),
            fog_density: 0.006,
            sky_top: Vec3::new(0.12, 0.22, 0.45),
            sky_horizon: Vec3::new(0.55, 0.62, 0.72),
        },
        // Свалка: низкое пыльное солнце, рыжая мгла, тяжёлый туман.
        "dust" => SceneEnvironment {
            sun_direction: Vec3::new(-0.25, 0.62, 0.50),
            sun_color: Vec3::new(1.0, 0.88, 0.68),
            sun_intensity: 0.8,
            ambient_color: Vec3::new(0.30, 0.27, 0.22),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.50, 0.44, 0.35),
            fog_density: 0.020,
            sky_top: Vec3::new(0.30, 0.29, 0.26),
            sky_horizon: Vec3::new(0.60, 0.54, 0.44),
        },
        // Подземка: холодный полумрак, плотный тёмный туман коридоров.
        "underground" => SceneEnvironment {
            sun_direction: Vec3::new(0.0, 1.0, 0.0),
            sun_color: Vec3::new(0.70, 0.78, 0.95),
            sun_intensity: 0.24,
            ambient_color: Vec3::new(0.22, 0.24, 0.29),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.04, 0.05, 0.07),
            fog_density: 0.035,
            sky_top: Vec3::new(0.004, 0.005, 0.009),
            sky_horizon: Vec3::new(0.02, 0.025, 0.04),
        },
        // Пасмурный день: мягкое солнце, серая мгла.
        "overcast" => SceneEnvironment {
            sun_direction: Vec3::new(0.4, 0.8, 0.3),
            sun_color: Vec3::new(1.0, 0.95, 0.85),
            sun_intensity: 0.9,
            ambient_color: Vec3::new(0.24, 0.25, 0.28),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.62, 0.63, 0.60),
            fog_density: 0.010,
            sky_top: Vec3::new(0.35, 0.42, 0.52),
            sky_horizon: Vec3::new(0.68, 0.68, 0.66),
        },
        _ => return None,
    })
}

fn draw_gizmo(
    painter: &egui::Painter,
    camera: &Camera,
    viewport: Vec2,
    mode: gizmo::GizmoMode,
    center: Vec3,
) {
    let Some(start) = gizmo::world_to_screen(camera, viewport, center) else {
        return;
    };
    let start = Pos2::new(start.x, start.y);
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    for (index, axis) in axes.into_iter().enumerate() {
        let Some(end) = gizmo::axis_endpoint(camera, viewport, center, axis) else {
            continue;
        };
        let (r, g, b) = gizmo::AXIS_COLORS[index];
        painter.line_segment(
            [start, Pos2::new(end.x, end.y)],
            Stroke::new(2.5, Color32::from_rgb(r, g, b)),
        );
    }
    if mode == gizmo::GizmoMode::Rotate {
        painter.circle_stroke(
            start,
            gizmo::GIZMO_LENGTH_PX * 0.5,
            Stroke::new(1.5, Color32::from_rgb(200, 200, 200)),
        );
    }
    if mode == gizmo::GizmoMode::Scale {
        // Квадрат в центре — метка масштаба (оси те же, что у перемещения).
        let half = gizmo::GIZMO_LENGTH_PX * 0.25;
        painter.rect_stroke(
            egui::Rect::from_center_size(start, egui::vec2(half * 2.0, half * 2.0)),
            2.0,
            Stroke::new(1.5, Color32::from_rgb(220, 200, 120)),
            egui::StrokeKind::Middle,
        );
    }
}

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
    use glam::Vec4;

    #[test]
    fn snap_rounds_xz_and_keeps_height() {
        let point = Vec3::new(0.3, 1.7, -0.6);
        assert_eq!(
            snap_xz(point, 0.5),
            Vec3::new(0.5, 1.7, -0.5),
            "x/z округлены, y не тронут"
        );
        assert_eq!(snap_xz(point, 0.0), point, "выкл — без изменений");
        assert_eq!(
            snap_xz(Vec3::new(-0.3, 0.0, -1.2), 1.0),
            Vec3::new(0.0, 0.0, -1.0),
            "отрицательные координаты округляются к ближайшему узлу"
        );
    }

    #[test]
    fn duplicate_inserts_copy_after_original() {
        let mut scene = Scene::new("тест");
        scene.add(SceneObject::new(Vec3::ZERO, Vec3::ONE, Vec4::ONE));
        scene.add(SceneObject::new(
            Vec3::new(5.0, 0.0, 0.0),
            Vec3::ONE,
            Vec4::ONE,
        ));
        let mut app = EditorApp::new(scene, None, PathBuf::from("game/content"));
        app.selected = Some(0);

        app.duplicate_selected();
        assert_eq!(app.scene.len(), 3, "копия добавлена");
        assert_eq!(app.selected, Some(1), "копия выделена");
        assert_eq!(app.scene.objects[1].position, Vec3::ZERO, "клон встык");
        assert_eq!(app.scene.objects[2].position.x, 5.0, "хвост сдвинут");
        assert_eq!(app.history.depth(), 1, "одна команда истории");

        // Откат убирает копию, хвост возвращается на место.
        app.undo_last();
        assert_eq!(app.scene.len(), 2);
        assert_eq!(app.scene.objects[1].position.x, 5.0);
    }

    #[test]
    fn environment_presets_mirror_builtin_locations() {
        // Пресеты панели атмосферы (G4) — те же настроения, что у встроенных
        // сцен: правки пресета не должны расходиться с локациями.
        assert_eq!(
            environment_preset("clear"),
            Some(zn_scene::Scene::base().environment)
        );
        assert_eq!(
            environment_preset("dust"),
            Some(zn_scene::Scene::svalka().environment)
        );
        assert_eq!(
            environment_preset("underground"),
            Some(zn_scene::Scene::podzemka().environment)
        );
        assert_eq!(
            environment_preset("overcast"),
            Some(zn_scene::Scene::demo_zone().environment)
        );
        assert!(environment_preset("такого нет").is_none());
    }

    #[test]
    fn double_click_requires_recent_same_spot() {
        let now = Instant::now();
        let cursor = Vec2::new(100.0, 200.0);

        // Первый клик — не двойной.
        assert!(!is_double_click(None, now, cursor));

        // Быстрый клик в том же месте — двойной.
        let recent = (now - Duration::from_millis(100), cursor);
        assert!(is_double_click(Some(recent), now, cursor));

        // Слишком далеко по времени — нет.
        let old = (now - Duration::from_secs(1), cursor);
        assert!(!is_double_click(Some(old), now, cursor));

        // Слишком далеко по расстоянию — нет.
        let far = (now - Duration::from_millis(50), Vec2::new(140.0, 200.0));
        assert!(!is_double_click(Some(far), now, cursor));
    }
}
