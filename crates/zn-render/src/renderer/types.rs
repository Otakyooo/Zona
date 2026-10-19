use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct CameraUniform {
    /// Матрица теневого прохода (M4g) — первой: shadow.wgsl читает только её.
    pub light_view_projection: [f32; 16],
    pub view_projection: [f32; 16],
    /// Обратная view-projection — для лучей неба по экрану (M4g).
    pub inverse_view_projection: [f32; 16],
    pub camera_position: [f32; 4],
    pub light_direction: [f32; 4],
    /// rgb — цвет солнца, w — интенсивность (M4g).
    pub light_color: [f32; 4],
    /// rgb — цвет ambient, w — интенсивность (M4g).
    pub ambient_color: [f32; 4],
    /// rgb — цвет тумана, w — плотность (M4g).
    pub fog_color: [f32; 4],
    /// Цвет неба в зените (M4g).
    pub sky_top: [f32; 4],
    /// Цвет неба у горизонта (M4g).
    pub sky_horizon: [f32; 4],
}

impl CameraUniform {
    /// Кладёт атмосферу локации (M4g) в uniform-поля: направление солнца
    /// нормализуется, интенсивности едут в w-компонентах.
    pub(crate) fn apply_environment(&mut self, environment: &zn_scene::SceneEnvironment) {
        let sun = environment.sun_direction_normalized();
        self.light_direction = [sun.x, sun.y, sun.z, 0.0];
        self.light_color = [
            environment.sun_color.x,
            environment.sun_color.y,
            environment.sun_color.z,
            environment.sun_intensity,
        ];
        self.ambient_color = [
            environment.ambient_color.x,
            environment.ambient_color.y,
            environment.ambient_color.z,
            environment.ambient_intensity,
        ];
        self.fog_color = [
            environment.fog_color.x,
            environment.fog_color.y,
            environment.fog_color.z,
            environment.fog_density,
        ];
        self.sky_top = [
            environment.sky_top.x,
            environment.sky_top.y,
            environment.sky_top.z,
            0.0,
        ];
        self.sky_horizon = [
            environment.sky_horizon.x,
            environment.sky_horizon.y,
            environment.sky_horizon.z,
            0.0,
        ];
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct PostUniform {
    /// Сила виньетки 0..1 (0 — выключена), зеркало `PostSettings`.
    pub vignette: f32,
    /// 1.0 — поверхность не-sRGB, пост-пасс кодирует линейный цвет вручную.
    pub output_srgb: f32,
    /// Интенсивность подмешивания bloom (0 — выключен).
    pub bloom_intensity: f32,
    pub _padding: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct BloomBrightUniform {
    /// Порог яркости bloom по максимальному каналу HDR.
    pub threshold: f32,
    pub _padding: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct BloomBlurUniform {
    /// Шаг выборки размытия в UV: (1/ширина, 0) или (0, 1/высота).
    pub step: [f32; 2],
    pub _padding: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct InstanceRaw {
    pub model: [f32; 16],
    pub color: [f32; 4],
}

impl InstanceRaw {
    // locations 0-2 — position/normal/uv вершины меша.
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 0,
            shader_location: 3,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 16,
            shader_location: 4,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 32,
            shader_location: 5,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 48,
            shader_location: 6,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 64,
            shader_location: 7,
        },
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_uniform_is_16_byte_aligned() {
        // 3×[f32;16] + 7×[f32;4].
        let size = std::mem::size_of::<CameraUniform>();
        assert_eq!(size % 16, 0, "uniform должен быть кратен 16: {size}");
        assert_eq!(size, 304);
    }

    #[test]
    fn post_uniform_is_16_bytes() {
        assert_eq!(std::mem::size_of::<PostUniform>(), 16);
        assert_eq!(std::mem::size_of::<BloomBrightUniform>(), 16);
        assert_eq!(std::mem::size_of::<BloomBlurUniform>(), 16);
    }

    #[test]
    fn instance_raw_is_80_bytes() {
        assert_eq!(std::mem::size_of::<InstanceRaw>(), 16 * 4 + 16);
    }

    #[test]
    fn apply_environment_packs_intensities_and_normalizes_sun() {
        let mut uniform = CameraUniform::zeroed();
        let environment = zn_scene::SceneEnvironment {
            sun_direction: glam::Vec3::new(0.0, 10.0, 0.0),
            sun_intensity: 0.75,
            ambient_intensity: 0.5,
            fog_density: 0.03,
            sky_top: glam::Vec3::new(0.1, 0.2, 0.3),
            ..Default::default()
        };
        uniform.apply_environment(&environment);

        assert_eq!(uniform.light_direction, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(uniform.light_color[3], 0.75);
        assert_eq!(uniform.ambient_color[3], 0.5);
        assert_eq!(uniform.fog_color[3], 0.03);
        assert_eq!(uniform.sky_top, [0.1, 0.2, 0.3, 0.0]);
        // Матрицы и позиция камеры — не трогаются.
        assert_eq!(uniform.camera_position, [0.0; 4]);
    }
}
