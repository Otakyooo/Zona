//! Персональные материалы объектов сцены (M4g): текстура, тайлинг, факторы
//! из `SceneObject` → `CpuMaterial` для `Renderer::register_material`.
//! Клиент и редактор кэшируют материалы по [`MaterialSpec`].

use zn_assets::{builtin_texture, parse_builtin_texture_key, CpuImage, CpuMaterial};
use zn_scene::SceneObject;

use crate::renderer::material::{SLOT_ALBEDO, SLOT_EMISSIVE, TEXTURE_SLOTS};

/// Шероховатость по умолчанию для персональных материалов (неметалл).
pub const DEFAULT_ROUGHNESS: f32 = 0.85;
/// Металличность по умолчанию для персональных материалов.
pub const DEFAULT_METALLIC: f32 = 0.0;

/// Ключ кэша материалов: всё, что влияет на вид поверхности объекта.
/// f32-поля хранятся битами — нужны `Eq + Hash`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MaterialSpec {
    texture: Option<String>,
    uv_scale_bits: [u32; 2],
    world_uv: bool,
    roughness_bits: Option<u32>,
    metallic_bits: Option<u32>,
    emissive_bits: [u32; 3],
}

impl MaterialSpec {
    /// Спецификация персонального материала объекта; `None` — переопределений
    /// нет (объект рисуется материалом ассета или дефолтным).
    pub fn from_object(object: &SceneObject) -> Option<Self> {
        if !object.has_material_overrides() {
            return None;
        }
        let emissive = object.emissive.unwrap_or_default();
        Some(Self {
            texture: object.texture.clone(),
            uv_scale_bits: [object.uv_scale.to_bits(); 2],
            world_uv: object.world_uv,
            roughness_bits: object.roughness.map(f32::to_bits),
            metallic_bits: object.metallic.map(f32::to_bits),
            emissive_bits: [
                emissive.x.to_bits(),
                emissive.y.to_bits(),
                emissive.z.to_bits(),
            ],
        })
    }

    /// Ключ текстуры альбедо, если задан.
    pub fn texture(&self) -> Option<&str> {
        self.texture.as_deref()
    }

    /// Тайлинг текстуры по осям UV.
    pub fn uv_scale(&self) -> [f32; 2] {
        [
            f32::from_bits(self.uv_scale_bits[0]),
            f32::from_bits(self.uv_scale_bits[1]),
        ]
    }

    /// Собирает материал и слоты изображений (albedo встроенной текстуры).
    /// Неизвестный ключ текстуры — материал без текстуры (с предупреждением).
    pub fn to_material(&self) -> (CpuMaterial, Vec<Option<CpuImage>>) {
        let albedo = match self.texture.as_deref() {
            None => None,
            Some(key) => {
                let image = parse_builtin_texture_key(key).and_then(builtin_texture);
                if image.is_none() {
                    log::warn!("текстура '{key}' не встроенная — объект без текстуры");
                }
                image
            }
        };

        let mut slots: Vec<Option<CpuImage>> = (0..TEXTURE_SLOTS).map(|_| None).collect();
        slots[SLOT_ALBEDO] = albedo;

        let emissive = [
            f32::from_bits(self.emissive_bits[0]),
            f32::from_bits(self.emissive_bits[1]),
            f32::from_bits(self.emissive_bits[2]),
        ];
        // Заполнитель emissive-слота чёрный; для свечения нужна белая карта,
        // иначе `emissive_factor` умножается на ноль.
        if emissive != [0.0, 0.0, 0.0] {
            slots[SLOT_EMISSIVE] = Some(CpuImage::new(1, 1, vec![255, 255, 255, 255], true));
        }
        let material = CpuMaterial {
            name: Some("scene-object".to_string()),
            metallic_factor: self
                .metallic_bits
                .map(f32::from_bits)
                .unwrap_or(DEFAULT_METALLIC),
            roughness_factor: self
                .roughness_bits
                .map(f32::from_bits)
                .unwrap_or(DEFAULT_ROUGHNESS),
            emissive_factor: emissive,
            uv_scale: self.uv_scale(),
            world_uv: self.world_uv,
            ..CpuMaterial::default()
        };
        (material, slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec3, Vec4};

    fn object() -> SceneObject {
        SceneObject::new(Vec3::ZERO, Vec3::ONE, Vec4::ONE)
    }

    #[test]
    fn plain_object_has_no_spec() {
        assert!(MaterialSpec::from_object(&object()).is_none());
    }

    #[test]
    fn texture_and_scale_create_spec() {
        let textured = object().with_texture("builtin://concrete", 4.0, true);
        let spec = MaterialSpec::from_object(&textured).expect("спека текстуры");
        assert_eq!(spec.texture(), Some("builtin://concrete"));
        assert_eq!(spec.uv_scale(), [4.0, 4.0]);

        // Спека кэшируется по значению: одинаковые объекты — один материал.
        let same =
            MaterialSpec::from_object(&object().with_texture("builtin://concrete", 4.0, true));
        assert_eq!(Some(spec), same);
        let other =
            MaterialSpec::from_object(&object().with_texture("builtin://concrete", 2.0, true));
        assert_ne!(same, other);
    }

    #[test]
    fn factors_alone_create_spec() {
        let spec =
            MaterialSpec::from_object(&object().with_roughness(0.3)).expect("спека факторов");
        assert!(spec.texture().is_none());
        let (material, slots) = spec.to_material();
        assert_eq!(material.roughness_factor, 0.3);
        assert_eq!(material.metallic_factor, DEFAULT_METALLIC);
        assert!(slots[SLOT_ALBEDO].is_none());
    }

    #[test]
    fn material_carries_texture_and_flags() {
        let spec = MaterialSpec::from_object(
            &object()
                .with_texture("builtin://metal", 3.0, true)
                .with_emissive(Vec3::new(0.1, 0.2, 0.3)),
        )
        .expect("спека");
        let (material, slots) = spec.to_material();
        assert_eq!(material.uv_scale, [3.0, 3.0]);
        assert!(material.world_uv);
        assert_eq!(material.emissive_factor, [0.1, 0.2, 0.3]);
        assert_eq!(material.roughness_factor, DEFAULT_ROUGHNESS);
        let albedo = slots[SLOT_ALBEDO].as_ref().expect("текстура альбедо");
        assert!(albedo.srgb);
        assert_eq!(
            albedo.pixels.len(),
            (zn_assets::BUILTIN_TEXTURE_SIZE.pow(2) * 4) as usize
        );
    }

    #[test]
    fn emissive_gets_white_texture_slot() {
        let spec = MaterialSpec::from_object(&object().with_emissive(Vec3::new(1.1, 0.9, 0.4)))
            .expect("спека");
        let (_, slots) = spec.to_material();
        let image = slots[SLOT_EMISSIVE].as_ref().expect("белый emissive-слот");
        assert_eq!(image.pixels, vec![255, 255, 255, 255]);
        // Без свечения слот остаётся дефолтным (чёрным) — экономия текстур.
        let plain = MaterialSpec::from_object(&object().with_roughness(0.5)).expect("спека");
        let (_, slots) = plain.to_material();
        assert!(slots[SLOT_EMISSIVE].is_none());
    }

    #[test]
    fn scene_texture_keys_are_builtin() {
        for key in [
            zn_scene::TEXTURE_ASPHALT,
            zn_scene::TEXTURE_CONCRETE,
            zn_scene::TEXTURE_DIRT,
            zn_scene::TEXTURE_METAL,
        ] {
            assert!(zn_assets::parse_builtin_texture_key(key).is_some(), "{key}");
        }
        // Текстурированные поверхности локаций получают карту альбедо.
        for scene in [zn_scene::Scene::base(), zn_scene::Scene::svalka()] {
            for object in &scene.objects {
                let Some(spec) = MaterialSpec::from_object(object) else {
                    continue;
                };
                if spec.texture().is_none() {
                    continue;
                }
                let (material, slots) = spec.to_material();
                assert!(slots[SLOT_ALBEDO].is_some(), "{}", scene.name);
                assert!(material.world_uv, "{}", scene.name);
            }
        }
    }

    #[test]
    fn unknown_texture_key_falls_back_to_plain_material() {
        let spec = MaterialSpec::from_object(&object().with_texture("builtin://nope", 1.0, false))
            .expect("спека");
        let (material, slots) = spec.to_material();
        assert!(slots[SLOT_ALBEDO].is_none());
        assert!(!material.world_uv);
    }
}
