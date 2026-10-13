use glam::camera::rh::proj::vulkan::perspective;
use glam::camera::rh::view::look_at_mat4;
use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Debug)]
pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
    pub aspect: f32,
}

impl Camera {
    pub const DEFAULT_FOV_Y: f32 = 70.0f32.to_radians();
    const MAX_PITCH: f32 = 89.0f32.to_radians();

    pub fn new(position: Vec3, yaw: f32, aspect: f32) -> Self {
        Self { position, yaw, pitch: 0.0, fov_y: Self::DEFAULT_FOV_Y, near: 0.1, far: 500.0, aspect }
    }

    pub fn forward(&self) -> Vec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(cos_yaw * cos_pitch, sin_pitch, sin_yaw * cos_pitch).normalize()
    }

    pub fn right(&self) -> Vec3 {
        Vec3::Y.cross(self.forward()).normalize()
    }

    pub fn up(&self) -> Vec3 {
        self.forward().cross(self.right()).normalize()
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        if aspect.is_finite() && aspect > 0.0 {
            self.aspect = aspect;
        }
    }

    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.yaw = (self.yaw + yaw_delta).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + pitch_delta).clamp(-Self::MAX_PITCH, Self::MAX_PITCH);
    }

    pub fn view_projection(&self) -> Mat4 {
        let view = look_at_mat4(self.position, self.position + self.forward(), Vec3::Y);
        let projection = perspective(self.fov_y, self.aspect, self.near, self.far);
        projection * view
    }

    pub fn orientation(&self) -> Quat {
        Quat::from_euler(glam::EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }
}
