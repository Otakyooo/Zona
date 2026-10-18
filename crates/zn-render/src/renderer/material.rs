use bytemuck::{Pod, Zeroable};

use crate::renderer::cache::MaterialId;
use zn_assets::{AlphaMode, CpuMaterial};

/// Параметры материала для uniform-буфера (совпадает со структурой Material
/// в pbr.wgsl; флаги: 1 = alpha mask, 2 = double sided, 4 = blend,
/// 8 = мировые (box) UV).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, Default)]
pub struct MaterialParams {
    pub base_color_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    _pad0: f32,
    _pad1: f32,
    pub emissive_factor: [f32; 4],
    pub alpha_cutoff: f32,
    pub flags: u32,
    /// Тайлинг UV (M4g): WGSL `vec2<f32>` на смещении 56, итого 64 байта.
    pub uv_scale: [f32; 2],
}

impl From<&CpuMaterial> for MaterialParams {
    fn from(material: &CpuMaterial) -> Self {
        let flags = match material.alpha_mode {
            AlphaMode::Opaque => 0u32,
            AlphaMode::Mask => 1,
            AlphaMode::Blend => FLAG_BLEND,
        } | if material.double_sided { 2 } else { 0 }
            | if material.world_uv { FLAG_WORLD_UV } else { 0 };
        Self {
            base_color_factor: material.base_color_factor,
            metallic_factor: material.metallic_factor,
            roughness_factor: material.roughness_factor,
            _pad0: 0.0,
            _pad1: 0.0,
            emissive_factor: [
                material.emissive_factor[0],
                material.emissive_factor[1],
                material.emissive_factor[2],
                1.0,
            ],
            alpha_cutoff: ALPHA_CUTOFF,
            flags,
            uv_scale: [
                // Нулевой/отрицательный масштаб из контента — как 1.0.
                if material.uv_scale[0] > 0.0 {
                    material.uv_scale[0]
                } else {
                    1.0
                },
                if material.uv_scale[1] > 0.0 {
                    material.uv_scale[1]
                } else {
                    1.0
                },
            ],
        }
    }
}

/// Порог discard для alpha-mask материалов.
pub const ALPHA_CUTOFF: f32 = 0.5;

/// Флаг blend-конвейера: инстансы рисуются вторым проходом с альфа-блендингом.
pub const FLAG_BLEND: u32 = 4;

/// Флаг мировых UV (M4g): box-проекция по мировой позиции вместо UV меша.
pub const FLAG_WORLD_UV: u32 = 8;

/// Число текстурных слотов материала.
pub const TEXTURE_SLOTS: usize = 5;

/// Слоты текстур: albedo, metallic-roughness, normal, occlusion, emissive.
pub const SLOT_ALBEDO: usize = 0;
pub const SLOT_METALLIC_ROUGHNESS: usize = 1;
pub const SLOT_NORMAL: usize = 2;
pub const SLOT_OCCLUSION: usize = 3;
pub const SLOT_EMISSIVE: usize = 4;

/// GPU-материал: bind group с uniform + 4 карты + сэмплер + параметры.
/// Сэмплер и bind group layout принадлежат Renderer'у (общие для всех).
pub struct GpuMaterial {
    pub id: MaterialId,
    pub params: MaterialParams,
    pub texture_bytes: u64,
    pub bind_group: wgpu::BindGroup,
    /// Владеет текстурами: пока запись живёт в кэше, GPU-память занята.
    /// Сознательно не читается — компилятор видит только владение.
    #[allow(dead_code)]
    owned_textures: Vec<wgpu::Texture>,
}

impl GpuMaterial {
    pub fn new(
        id: MaterialId,
        params: MaterialParams,
        texture_bytes: u64,
        bind_group: wgpu::BindGroup,
        owned_textures: Vec<wgpu::Texture>,
    ) -> Self {
        Self {
            id,
            params,
            texture_bytes,
            bind_group,
            owned_textures,
        }
    }
}

impl std::fmt::Debug for GpuMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuMaterial")
            .field("id", &self.id)
            .field("texture_bytes", &self.texture_bytes)
            .finish_non_exhaustive()
    }
}

/// Стандартные RGBA8-заполнители для отсутствующих карт.
pub fn default_texture_pixels(slot: usize) -> [u8; 4] {
    match slot {
        SLOT_ALBEDO => [255, 255, 255, 255],
        SLOT_METALLIC_ROUGHNESS => [255, 255, 255, 255], // metallic=1, roughness=1
        SLOT_NORMAL => [128, 128, 255, 255],             // +Z в tangent space
        SLOT_OCCLUSION => [255, 255, 255, 255],
        SLOT_EMISSIVE => [0, 0, 0, 255],
        _ => [255, 255, 255, 255],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zn_assets::CpuImage;

    #[test]
    fn params_layout_is_64_bytes() {
        assert_eq!(std::mem::size_of::<MaterialParams>(), 64);
        assert_eq!(std::mem::size_of::<MaterialParams>() % 16, 0);
    }

    #[test]
    fn flags_encode_alpha_and_sides() {
        let opaque = CpuMaterial::default();
        let mask = CpuMaterial {
            alpha_mode: AlphaMode::Mask,
            ..Default::default()
        };
        let masked_double = CpuMaterial {
            alpha_mode: AlphaMode::Mask,
            double_sided: true,
            ..Default::default()
        };
        let blend_double = CpuMaterial {
            alpha_mode: AlphaMode::Blend,
            double_sided: true,
            ..Default::default()
        };
        assert_eq!(MaterialParams::from(&opaque).flags, 0);
        assert_eq!(MaterialParams::from(&mask).flags, 1);
        assert_eq!(MaterialParams::from(&masked_double).flags, 3);
        // Blend больше не понижается до opaque — флаг для второго прохода.
        assert_eq!(MaterialParams::from(&blend_double).flags, FLAG_BLEND | 2);
    }

    #[test]
    fn from_cpu_material_copies_factors() {
        let cpu = CpuMaterial {
            base_color_factor: [0.5, 0.25, 0.75, 1.0],
            metallic_factor: 0.1,
            roughness_factor: 0.9,
            emissive_factor: [0.2, 0.4, 0.6],
            ..Default::default()
        };
        let params = MaterialParams::from(&cpu);
        assert_eq!(params.base_color_factor, [0.5, 0.25, 0.75, 1.0]);
        assert_eq!(params.metallic_factor, 0.1);
        assert_eq!(params.roughness_factor, 0.9);
        assert_eq!(params.emissive_factor, [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn uv_scale_and_world_flag_flow_into_params() {
        let cpu = CpuMaterial {
            uv_scale: [4.0, 2.0],
            world_uv: true,
            roughness_factor: 0.5,
            metallic_factor: 0.0,
            ..Default::default()
        };
        let params = MaterialParams::from(&cpu);
        assert_eq!(params.uv_scale, [4.0, 2.0]);
        assert_eq!(params.flags & FLAG_WORLD_UV, FLAG_WORLD_UV);
        assert_eq!(params.roughness_factor, 0.5);
        assert_eq!(params.metallic_factor, 0.0);
        // Дефолт — без мировых UV и с единичным тайлингом.
        assert_eq!(MaterialParams::from(&CpuMaterial::default()).flags, 0);
        // Нулевой/отрицательный масштаб из контента выправляется в 1.0.
        let zero = CpuMaterial {
            uv_scale: [0.0, -2.0],
            ..Default::default()
        };
        assert_eq!(MaterialParams::from(&zero).uv_scale, [1.0, 1.0]);
    }

    #[test]
    fn default_pixels_match_slots() {
        assert_eq!(default_texture_pixels(SLOT_NORMAL), [128, 128, 255, 255]);
        assert_eq!(default_texture_pixels(SLOT_EMISSIVE), [0, 0, 0, 255]);
        assert_eq!(default_texture_pixels(99), [255, 255, 255, 255]);
    }

    #[test]
    fn cpu_image_roundtrip_scale() {
        // 2x2 белая текстура — 16 байт RGBA8.
        let image = CpuImage::new(2, 2, vec![255; 16], true);
        assert_eq!(image.pixels.len() as u32, image.width * image.height * 4);
    }
}
