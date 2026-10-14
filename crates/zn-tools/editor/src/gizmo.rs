//! Кастомный gizmo редактора: 3 цветные оси, экранный drag оси (translate)
//! и горизонтальный drag (rotate). Чистая логика отделена от painter'а.

use glam::{Vec2, Vec3};

pub const GIZMO_LENGTH_PX: f32 = 80.0;
pub const GIZMO_HIT_TOLERANCE_PX: f32 = 8.0;
pub const GIZMO_ROTATE_SPEED: f32 = 0.01;

/// Цвета осей gizmo (egui-палитра): X — красный, Y — зелёный, Z — синий.
pub const AXIS_COLORS: [(u8, u8, u8); 3] = [(229, 83, 75), (126, 200, 80), (86, 149, 229)];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GizmoMode {
    #[default]
    Translate,
    Rotate,
}

impl GizmoMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Translate => "перемещение",
            Self::Rotate => "вращение",
        }
    }

    pub fn toggled(&self) -> Self {
        match self {
            Self::Translate => Self::Rotate,
            Self::Rotate => Self::Translate,
        }
    }
}

/// Состояние gizmo: режим + активный drag оси + позиция курсора.
#[derive(Clone, Debug, Default)]
pub struct GizmoState {
    pub mode: GizmoMode,
    drag_axis: Option<usize>,
    last_cursor: Option<Vec2>,
    active: bool,
    pub yaw: f32,
}

impl GizmoState {
    pub fn new(mode: GizmoMode) -> Self {
        Self {
            mode,
            drag_axis: None,
            last_cursor: None,
            active: false,
            yaw: 0.0,
        }
    }

    pub fn toggle_mode(&mut self) {
        self.mode = self.mode.toggled();
        self.reset_yaw();
    }

    pub fn is_dragging(&self) -> bool {
        self.active
    }

    /// Начинает drag: проверяет попадание курсора в оси gizmo.
    pub fn begin_drag(
        &mut self,
        camera: &zn_render::Camera,
        viewport_px: Vec2,
        center: Vec3,
        cursor: Vec2,
    ) -> bool {
        self.last_cursor = Some(cursor);
        if self.mode == GizmoMode::Rotate {
            self.active = true; // вращение начинается от любого места у объекта
            return true;
        }
        if let Some(axis) = pick_axis(camera, viewport_px, center, cursor) {
            self.drag_axis = Some(axis);
            self.active = true;
            return true;
        }
        false
    }

    /// Обновляет drag: returns мировое смещение (translate) и сбрасывает yaw-
    /// дельту (rotate) или None при отсутствии активного drag.
    pub fn update_drag(
        &mut self,
        camera: &zn_render::Camera,
        viewport_px: Vec2,
        center: Vec3,
        cursor: Vec2,
    ) -> Option<GizmoDelta> {
        if !self.active {
            self.last_cursor = Some(cursor);
            return None;
        }
        let delta_px = cursor - self.last_cursor.unwrap_or(cursor);
        self.last_cursor = Some(cursor);

        match self.mode {
            GizmoMode::Translate => {
                let axis = self.drag_axis?;
                let translation =
                    screen_delta_to_world(camera, viewport_px, axis, center, delta_px);
                Some(GizmoDelta::Translation(translation))
            }
            GizmoMode::Rotate => {
                let yaw_delta = -delta_px.x * GIZMO_ROTATE_SPEED;
                self.yaw += yaw_delta;
                Some(GizmoDelta::Rotation(yaw_delta))
            }
        }
    }

    pub fn end_drag(&mut self) {
        self.drag_axis = None;
        self.last_cursor = None;
        self.active = false;
    }

    pub fn reset_yaw(&mut self) {
        self.yaw = 0.0;
    }
}

#[derive(Clone, Copy, Debug)]
pub enum GizmoDelta {
    Translation(Vec3),
    Rotation(f32),
}

/// Экранная позиция мировой точки (None — за камерой).
pub fn world_to_screen(camera: &zn_render::Camera, viewport_px: Vec2, point: Vec3) -> Option<Vec2> {
    let clip = camera.view_projection() * point.extend(1.0);
    if clip.w <= 1e-6 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(Vec2::new(
        (ndc.x + 1.0) * 0.5 * viewport_px.x,
        (1.0 - ndc.y) * 0.5 * viewport_px.y,
    ))
}

/// Проекция точки на экран для отрисовки оси фиксированной длины.
pub fn axis_endpoint(
    camera: &zn_render::Camera,
    viewport_px: Vec2,
    center: Vec3,
    axis: Vec3,
) -> Option<Vec2> {
    let start = world_to_screen(camera, viewport_px, center)?;
    let end_world = center + axis.normalize_or_zero();
    let end = world_to_screen(camera, viewport_px, end_world)?;
    let dir = (end - start).normalize_or_zero();
    Some(start + dir * GIZMO_LENGTH_PX)
}

/// Ближайшая ось под курсором (0=X, 1=Y, 2=Z) или None.
pub fn pick_axis(
    camera: &zn_render::Camera,
    viewport_px: Vec2,
    center: Vec3,
    cursor: Vec2,
) -> Option<usize> {
    let start = world_to_screen(camera, viewport_px, center)?;
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    for (index, axis) in axes.into_iter().enumerate() {
        let end = axis_endpoint(camera, viewport_px, center, axis)?;
        if point_segment_distance(cursor, start, end) <= GIZMO_HIT_TOLERANCE_PX {
            return Some(index);
        }
    }
    None
}

/// Перевод экранной дельты drag в мировое смещение вдоль оси `axis`.
pub fn screen_delta_to_world(
    camera: &zn_render::Camera,
    viewport_px: Vec2,
    axis: usize,
    center: Vec3,
    delta_px: Vec2,
) -> Vec3 {
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let axis_dir = axes[axis];
    let start = world_to_screen(camera, viewport_px, center).unwrap_or_default();
    let end = axis_endpoint(camera, viewport_px, center, axis_dir).unwrap_or(start + Vec2::X);
    let screen_dir = (end - start).normalize_or_zero();
    let along = delta_px.dot(screen_dir);

    // Пиксели на мировую единицу на глубине центра: h = 2*d*tan(fov/2).
    let distance = center.distance(camera.position).max(0.001);
    let world_per_pixel = (2.0 * distance * (camera.fov_y / 2.0).tan()) / viewport_px.y.max(1.0);
    axis_dir * (along * world_per_pixel)
}

/// Расстояние от точки до отрезка (2D).
pub fn point_segment_distance(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let length_squared = ab.length_squared();
    if length_squared < 1e-8 {
        return point.distance(a);
    }
    let t = (point - a).dot(ab) / length_squared;
    let t = t.clamp(0.0, 1.0);
    point.distance(a + ab * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Камера в `position`, направленная в `target`.
    fn test_camera(position: Vec3, target: Vec3) -> zn_render::Camera {
        let dir = (target - position).normalize();
        let yaw = dir.z.atan2(dir.x);
        let pitch = dir.y.clamp(-1.0, 1.0).asin();
        let mut camera = zn_render::Camera::new(position, yaw, 1.0);
        camera.pitch = pitch;
        camera
    }

    #[test]
    fn world_to_screen_projects_center() {
        let camera = test_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let screen =
            world_to_screen(&camera, Vec2::new(1600.0, 900.0), Vec3::ZERO).expect("перед камерой");
        assert!((screen.x - 800.0).abs() < 1.0, "x = {}", screen.x);
        assert!((screen.y - 450.0).abs() < 1.0, "y = {}", screen.y);
    }

    #[test]
    fn world_to_screen_rejects_points_behind() {
        let camera = test_camera(Vec3::ZERO, Vec3::X);
        let screen = world_to_screen(
            &camera,
            Vec2::new(1600.0, 900.0),
            Vec3::new(-10.0, 0.0, 0.0),
        );
        assert!(screen.is_none());
    }

    #[test]
    fn point_segment_distance_matches_geometry() {
        let start = Vec2::new(0.0, 0.0);
        let end = Vec2::new(10.0, 0.0);
        assert!((point_segment_distance(Vec2::new(5.0, 3.0), start, end) - 3.0).abs() < 1e-5);
        assert!((point_segment_distance(Vec2::new(-5.0, 0.0), start, end) - 5.0).abs() < 1e-5);
        assert!((point_segment_distance(Vec2::new(15.0, 0.0), start, end) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn pick_axis_finds_nearest_axis() {
        let camera = test_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let viewport = Vec2::new(1600.0, 900.0);
        let center = Vec3::ZERO;
        let origin = world_to_screen(&camera, viewport, center).unwrap();
        let x_end = axis_endpoint(&camera, viewport, center, Vec3::X).unwrap();
        let y_end = axis_endpoint(&camera, viewport, center, Vec3::Y).unwrap();
        assert!(x_end.x > origin.x, "экранный +X должен быть правее центра");
        assert!(y_end.y < origin.y, "экранный +Y должен быть выше центра");

        let near_x = origin + (x_end - origin).normalize() * 30.0;
        assert_eq!(pick_axis(&camera, viewport, center, near_x), Some(0));
        let near_y = origin + (y_end - origin).normalize() * 30.0;
        assert_eq!(pick_axis(&camera, viewport, center, near_y), Some(1));
        let outside = origin + Vec2::new(300.0, 300.0);
        assert!(pick_axis(&camera, viewport, center, outside).is_none());
    }

    #[test]
    fn screen_delta_to_world_is_directional() {
        let camera = test_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let viewport = Vec2::new(1600.0, 900.0);
        let center = Vec3::ZERO;

        let x_end = axis_endpoint(&camera, viewport, center, Vec3::X).unwrap();
        let origin = world_to_screen(&camera, viewport, center).unwrap();
        let delta = (x_end - origin).normalize() * 10.0; // 10 пикселей вдоль экранного +X

        let world = screen_delta_to_world(&camera, viewport, 0, center, delta);
        assert!(world.x > 0.0, "delta должен двигать по +X: {world}");
        assert!(world.y.abs() < 1e-5 && world.z.abs() < 1e-5);
    }

    #[test]
    fn gizmo_translate_drag() {
        let camera = test_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let viewport = Vec2::new(1600.0, 900.0);
        let center = Vec3::ZERO;
        let origin = world_to_screen(&camera, viewport, center).unwrap();
        let x_end = axis_endpoint(&camera, viewport, center, Vec3::X).unwrap();
        let start = origin + (x_end - origin).normalize() * 20.0;

        let mut gizmo = GizmoState::new(GizmoMode::Translate);
        assert!(gizmo.begin_drag(&camera, viewport, center, start));
        let delta = gizmo.update_drag(&camera, viewport, center, start + Vec2::new(5.0, 0.0));
        assert!(matches!(delta, Some(GizmoDelta::Translation(_))));
        if let Some(GizmoDelta::Translation(translation)) = delta {
            assert!(translation.x > 0.0);
        }
        gizmo.end_drag();
        assert!(!gizmo.is_dragging());
    }

    #[test]
    fn gizmo_rotate_drag_accumulates_yaw() {
        let mut gizmo = GizmoState::new(GizmoMode::Rotate);
        let camera = test_camera(Vec3::ZERO, Vec3::X);
        let viewport = Vec2::new(1600.0, 900.0);
        let center = Vec3::new(5.0, 0.0, 0.0);

        assert!(gizmo.begin_drag(&camera, viewport, center, Vec2::splat(100.0)));
        let start = Vec2::new(100.0, 100.0);
        let delta = gizmo.update_drag(&camera, viewport, center, start + Vec2::new(-20.0, 0.0));
        assert!(matches!(delta, Some(GizmoDelta::Rotation(yaw)) if yaw > 0.0));
        assert!(gizmo.yaw > 0.0);
        gizmo.reset_yaw();
        assert_eq!(gizmo.yaw, 0.0);
    }

    #[test]
    fn gizmo_miss_does_not_start_drag() {
        let camera = test_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let viewport = Vec2::new(1600.0, 900.0);
        let mut gizmo = GizmoState::new(GizmoMode::Translate);
        assert!(!gizmo.begin_drag(&camera, viewport, Vec3::ZERO, Vec2::new(1500.0, 800.0)));
        assert!(gizmo
            .update_drag(&camera, viewport, Vec3::ZERO, Vec2::new(1510.0, 800.0))
            .is_none());
    }

    #[test]
    fn gizmo_mode_toggles() {
        let mut gizmo = GizmoState::new(GizmoMode::Translate);
        assert_eq!(gizmo.mode, GizmoMode::Translate);
        gizmo.toggle_mode();
        assert_eq!(gizmo.mode, GizmoMode::Rotate);
        assert_eq!(gizmo.mode.label(), "вращение");
    }
}
