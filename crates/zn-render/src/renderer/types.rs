use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct CameraUniform {
    pub view_projection: [f32; 16],
    pub camera_position: [f32; 4],
    pub light_direction: [f32; 4],
    pub light_color: [f32; 4],
    pub ambient_color: [f32; 4],
    /// 1.0 — поверхность не-sRGB, шейдер кодирует линейный цвет вручную.
    pub output_srgb: f32,
    pub _padding: [f32; 3],
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
        // [f32;16] + 4×[f32;4] + f32 + [f32;3].
        let size = std::mem::size_of::<CameraUniform>();
        assert_eq!(size % 16, 0, "uniform должен быть кратен 16: {size}");
        assert_eq!(size, 144);
    }

    #[test]
    fn instance_raw_is_80_bytes() {
        assert_eq!(std::mem::size_of::<InstanceRaw>(), 16 * 4 + 16);
    }
}
