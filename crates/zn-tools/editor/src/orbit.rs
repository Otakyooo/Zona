//! Орбитальная камера редактора: вращение ЛКМ, панорама/полёт ПКМ/Space,
//! зум колесом. Гарантирует согласованность с `zn_render::Camera`.

use glam::{Vec2, Vec3};

const MIN_DISTANCE: f32 = 0.5;
const MAX_DISTANCE: f32 = 2000.0;
const MAX_PITCH: f32 = std::f32::consts::FRAC_PI_2 - 0.01;

#[derive(Clone, Debug)]
pub struct OrbitCamera {
    pub center: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl OrbitCamera {
    pub fn new(center: Vec3, distance: f32, yaw: f32, pitch: f32) -> Self {
        Self {
            center,
            distance,
            yaw,
            pitch: pitch.clamp(-MAX_PITCH, MAX_PITCH),
        }
    }

    /// Вектор «вперёд» от камеры к цели (совпадает с Camera::forward).
    pub fn forward(&self) -> Vec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(cos_yaw * cos_pitch, sin_pitch, sin_yaw * cos_pitch).normalize_or_zero()
    }

    /// Позиция камеры в мире.
    pub fn position(&self) -> Vec3 {
        self.center - self.forward() * self.distance
    }

    /// Применяет состояние к FPS-камере рендерера.
    pub fn apply(&self, camera: &mut zn_render::Camera) {
        camera.position = self.position();
        camera.yaw = self.yaw;
        camera.pitch = self.pitch;
    }

    /// Вращение вокруг цели: drag в пикселях → yaw/pitch.
    pub fn orbit(&mut self, drag_px: Vec2, viewport_height: f32) {
        let scale = std::f32::consts::PI / viewport_height.max(1.0);
        self.yaw = (self.yaw - drag_px.x * scale).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + drag_px.y * scale).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Зум: multiplicative factor (>1 — отдалить).
    pub fn zoom(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
        }
    }

    /// Панорама: сдвиг цели в плоскости экрана.
    pub fn pan(&mut self, right: Vec3, up: Vec3, right_amount: f32, up_amount: f32) {
        self.center += right * right_amount + up * up_amount;
    }

    /// Полёт: движение цели вдоль-поперёк view-оси + вверх/вниз.
    pub fn fly(
        &mut self,
        forward: Vec3,
        right: Vec3,
        forward_amount: f32,
        right_amount: f32,
        up_amount: f32,
    ) {
        self.center += forward * forward_amount + right * right_amount + Vec3::Y * up_amount;
    }

    /// Приближает/отодвигает центр к точке (двойной клик).
    pub fn focus(&mut self, point: Vec3) {
        self.center = point;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_is_center_minus_forward_distance() {
        let orbit = OrbitCamera::new(Vec3::new(1.0, 2.0, 3.0), 5.0, 0.0, 0.0);
        let position = orbit.position();
        let to_center = (orbit.center - position).normalize();
        assert!((to_center - orbit.forward()).length() < 1e-5);
        assert!((position.distance(orbit.center) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn zoom_clamps_distance() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0, 0.0, 0.0);
        orbit.zoom(0.00001);
        assert!(orbit.distance >= MIN_DISTANCE);
        orbit.zoom(1e9);
        assert!(orbit.distance <= MAX_DISTANCE);
    }

    #[test]
    fn orbit_clamps_pitch() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0, 0.0, 0.0);
        orbit.orbit(Vec2::new(0.0, 100000.0), 900.0);
        assert!(orbit.pitch <= MAX_PITCH);
        assert!(orbit.pitch > 0.0);
        orbit.orbit(Vec2::new(0.0, -100000.0), 900.0);
        assert!(orbit.pitch >= -MAX_PITCH);
    }

    #[test]
    fn orbit_wraps_yaw() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0, 0.0, 0.0);
        orbit.orbit(Vec2::new(-100000.0, 0.0), 900.0);
        assert!((0.0..std::f32::consts::TAU).contains(&orbit.yaw));
    }

    #[test]
    fn pan_moves_center() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0, 0.0, 0.0);
        orbit.pan(Vec3::X, Vec3::Y, 2.0, -1.0);
        assert_eq!(orbit.center, Vec3::new(2.0, -1.0, 0.0));
    }

    #[test]
    fn fly_moves_center_along_axes() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0, 0.0, 0.0);
        orbit.fly(Vec3::Z, Vec3::X, 1.0, 0.0, 5.0);
        assert_eq!(orbit.center, Vec3::new(0.0, 5.0, 1.0));
    }

    #[test]
    fn apply_syncs_camera_to_orbit() {
        let orbit = OrbitCamera::new(Vec3::new(4.0, 1.0, -2.0), 12.0, 0.7, -0.3);
        let mut camera = zn_render::Camera::new(Vec3::ZERO, 0.0, 1.0);
        orbit.apply(&mut camera);
        assert!((camera.position - orbit.position()).length() < 1e-4);
        let to_center = (orbit.center - camera.position).normalize();
        assert!((camera.forward() - to_center).length() < 1e-4);
    }

    #[test]
    fn focus_recenters() {
        let mut orbit = OrbitCamera::new(Vec3::ZERO, 10.0, 0.0, 0.0);
        orbit.focus(Vec3::new(5.0, 0.0, 5.0));
        assert_eq!(orbit.center, Vec3::new(5.0, 0.0, 5.0));
    }
}
