use std::path::Path;

use glam::{Vec3, Vec4};
use serde::{Deserialize, Serialize};

pub const SCENE_EXTENSION: &str = "scene.ron";

/// Константы FNV-1a (64 бита) для [`Scene::content_hash`].
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Ключ ассета по умолчанию: встроенный процедурный куб.
pub const BUILTIN_CUBE: &str = "builtin://cube";

/// Форма прокси-коллайдера объекта сцены. Размеры берутся из `scale` объекта.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ColliderShape {
    #[default]
    Box,
    Cylinder,
    Sphere,
}

impl ColliderShape {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Box => "box",
            Self::Cylinder => "cylinder",
            Self::Sphere => "sphere",
        }
    }
}

impl std::fmt::Display for ColliderShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Запись лута контейнера: ключ предмета из каталога `zn-content` и количество.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LootEntry {
    pub item: String,
    pub count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneObject {
    pub position: Vec3,
    pub scale: Vec3,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default = "default_color")]
    pub color: Vec4,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub collider: Option<ColliderShape>,
    #[serde(default = "default_true")]
    pub solid: bool,
    /// Фиксированный лут: наличие превращает объект в обыскиваемый контейнер.
    #[serde(default)]
    pub loot: Option<Vec<LootEntry>>,
}

fn default_color() -> Vec4 {
    Vec4::ONE
}

fn default_true() -> bool {
    true
}

/// Ящик лут-контейнера: куб 0.8 м у пола с фиксированным содержимым.
fn crate_object(x: f32, z: f32, loot: &[(&str, u32)]) -> SceneObject {
    let entries = loot
        .iter()
        .map(|(item, count)| LootEntry {
            item: (*item).to_string(),
            count: *count,
        })
        .collect();
    SceneObject::new(
        Vec3::new(x, 0.4, z),
        Vec3::splat(0.8),
        Vec4::new(0.45, 0.33, 0.18, 1.0),
    )
    .with_asset(BUILTIN_CUBE)
    .with_loot(entries)
}

impl SceneObject {
    pub fn new(position: Vec3, scale: Vec3, color: Vec4) -> Self {
        Self {
            position,
            scale,
            yaw: 0.0,
            color,
            asset: None,
            material: None,
            collider: None,
            solid: true,
            loot: None,
        }
    }

    pub fn with_yaw(mut self, yaw: f32) -> Self {
        self.yaw = yaw;
        self
    }

    pub fn with_position(mut self, position: Vec3) -> Self {
        self.position = position;
        self
    }

    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.scale = scale;
        self
    }

    pub fn with_color(mut self, color: Vec4) -> Self {
        self.color = color;
        self
    }

    pub fn with_asset(mut self, asset: impl Into<String>) -> Self {
        self.asset = Some(asset.into());
        self
    }

    pub fn with_material(mut self, material: impl Into<String>) -> Self {
        self.material = Some(material.into());
        self
    }

    pub fn with_collider(mut self, collider: ColliderShape) -> Self {
        self.collider = Some(collider);
        self
    }

    pub fn with_solid(mut self, solid: bool) -> Self {
        self.solid = solid;
        self
    }

    pub fn with_loot(mut self, loot: Vec<LootEntry>) -> Self {
        self.loot = Some(loot);
        self
    }

    /// Обыскиваемый контейнер: у объекта задан фиксированный лут.
    pub fn is_container(&self) -> bool {
        self.loot.is_some()
    }

    pub fn invulnerable(mut self) -> Self {
        self.solid = false;
        self
    }

    /// Ключ ассета объекта; `None` означает встроенный куб.
    pub fn asset_key(&self) -> &str {
        self.asset.as_deref().unwrap_or(BUILTIN_CUBE)
    }

    /// Эффективная форма коллайдера: `None`, если объект не solid.
    pub fn effective_collider(&self) -> Option<ColliderShape> {
        if self.solid {
            Some(self.collider.unwrap_or(ColliderShape::Box))
        } else {
            None
        }
    }

    pub fn model_matrix(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(
            self.scale,
            glam::Quat::from_rotation_y(self.yaw),
            self.position,
        )
    }

    /// AABB объекта в мире: центр ± половины размеров с учётом поворота по yaw.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let half = self.scale * 0.5;
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let extent = Vec3::new(
            (half.x * cos_yaw).abs() + (half.z * sin_yaw).abs(),
            half.y,
            (half.x * sin_yaw).abs() + (half.z * cos_yaw).abs(),
        );
        (self.position - extent, self.position + extent)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Scene {
    pub name: String,
    #[serde(default)]
    pub spawn: Vec3,
    #[serde(default)]
    pub spawn_yaw: f32,
    #[serde(default)]
    pub objects: Vec<SceneObject>,
}

impl Scene {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            spawn: Vec3::new(0.0, 2.0, 0.0),
            spawn_yaw: 0.0,
            objects: Vec::new(),
        }
    }

    pub fn add(&mut self, object: SceneObject) -> &mut Self {
        self.objects.push(object);
        self
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn solids(&self) -> impl Iterator<Item = &SceneObject> {
        self.objects.iter().filter(|o| o.solid)
    }

    pub fn to_ron(&self) -> Result<String, ron::Error> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
    }

    /// Детерминированный хэш содержимого уровня (FNV-1a по каноническому
    /// RON-представлению). Не зависит от пути к файлу; клиент и сервер
    /// сверяют его при сетевом handshake, чтобы коллайдеры совпадали.
    pub fn content_hash(&self) -> u64 {
        let text = self.to_ron().unwrap_or_else(|_| self.name.clone());
        let mut hash = FNV_OFFSET_BASIS;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    pub fn save_ron(&self, path: impl AsRef<Path>) -> Result<(), SceneError> {
        let text = self.to_ron().map_err(SceneError::Serialize)?;
        std::fs::write(path, text).map_err(SceneError::Io)
    }

    pub fn from_ron(text: &str) -> Result<Self, SceneError> {
        ron::from_str(text).map_err(SceneError::Deserialize)
    }

    pub fn load_ron(path: impl AsRef<Path>) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path).map_err(SceneError::Io)?;
        Self::from_ron(&text)
    }

    pub fn demo_zone() -> Self {
        let mut scene = Scene::new("demo_zone");
        scene.spawn = Vec3::new(0.0, 0.0, 14.0);
        scene.spawn_yaw = std::f32::consts::PI;

        let mut push = |position: Vec3, scale: Vec3, color: Vec4| {
            scene.add(SceneObject::new(position, scale, color).with_asset(BUILTIN_CUBE));
        };

        push(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(60.0, 0.5, 60.0),
            Vec4::new(0.18, 0.19, 0.16, 1.0),
        );

        let wall_color = Vec4::new(0.10, 0.10, 0.11, 1.0);
        push(
            Vec3::new(0.0, 2.5, -30.0),
            Vec3::new(31.0, 3.0, 1.0),
            wall_color,
        );
        push(
            Vec3::new(0.0, 2.5, 30.0),
            Vec3::new(31.0, 3.0, 1.0),
            wall_color,
        );
        push(
            Vec3::new(-30.0, 2.5, 0.0),
            Vec3::new(1.0, 3.0, 31.0),
            wall_color,
        );
        push(
            Vec3::new(30.0, 2.5, 0.0),
            Vec3::new(1.0, 3.0, 31.0),
            wall_color,
        );

        let pillar = Vec4::new(0.24, 0.25, 0.22, 1.0);
        for x in -20..=20 {
            for z in -20..=20 {
                if x % 6 == 0 && z % 6 == 0 {
                    push(
                        Vec3::new(x as f32, 1.75, z as f32),
                        Vec3::new(0.5, 3.5, 0.5),
                        pillar,
                    );
                }
            }
        }

        push(
            Vec3::new(0.0, 0.5, 0.0),
            Vec3::new(4.0, 1.0, 4.0),
            Vec4::new(0.35, 0.30, 0.18, 1.0),
        );
        push(
            Vec3::new(-6.0, 1.0, -6.0),
            Vec3::new(2.0, 2.0, 2.0),
            Vec4::new(0.30, 0.30, 0.34, 1.0),
        );
        push(
            Vec3::new(7.0, 1.0, -8.0),
            Vec3::new(1.5, 3.0, 1.5),
            Vec4::new(0.30, 0.30, 0.34, 1.0),
        );
        push(
            Vec3::new(10.0, 0.75, 4.0),
            Vec3::new(3.0, 1.5, 3.0),
            Vec4::new(0.30, 0.30, 0.34, 1.0),
        );

        let artifact = Vec4::new(0.95, 0.72, 0.15, 1.0);
        let artifacts = [
            (Vec3::new(0.0, 1.6, 0.0), Vec3::new(0.35, 0.35, 0.35)),
            (Vec3::new(-6.0, 2.75, -6.0), Vec3::new(0.35, 0.35, 0.35)),
            (Vec3::new(7.0, 3.25, -8.0), Vec3::new(0.35, 0.35, 0.35)),
            (Vec3::new(10.0, 2.25, 4.0), Vec3::new(0.35, 0.35, 0.35)),
        ];
        for (position, scale) in artifacts {
            scene.add(
                SceneObject::new(position, scale, artifact)
                    .with_asset(BUILTIN_CUBE)
                    .invulnerable(),
            );
        }

        // Ящики M4b — обыскиваемые контейнеры с фиксированным лутом:
        // два у спавна, шесть по маршруту. Итог: аптечек 5, бинтов 5,
        // патронов 180, болтов 30, консервов 3, водки 4.
        let crates = [
            crate_object(2.5, 12.0, &[("medkit", 1), ("bandage", 2)]),
            crate_object(-3.0, 12.5, &[("ammo_545", 60), ("bolt", 10)]),
            crate_object(8.0, 16.0, &[("canned_food", 2), ("vodka", 1)]),
            crate_object(-9.0, 6.0, &[("medkit", 2), ("bandage", 2)]),
            crate_object(12.0, 2.0, &[("bolt", 20), ("ammo_545", 30)]),
            crate_object(-13.0, -11.0, &[("vodka", 2), ("canned_food", 1)]),
            crate_object(
                9.5,
                -16.0,
                &[("medkit", 1), ("bandage", 1), ("ammo_545", 60)],
            ),
            crate_object(0.0, -10.0, &[("medkit", 1), ("vodka", 1), ("ammo_545", 30)]),
        ];
        for object in crates {
            scene.add(object);
        }

        scene
    }
}

#[derive(Debug)]
pub enum SceneError {
    Io(std::io::Error),
    Serialize(ron::Error),
    Deserialize(ron::error::SpannedError),
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Serialize(e) => write!(f, "serialize error: {e}"),
            Self::Deserialize(e) => write!(f, "deserialize error: {e}"),
        }
    }
}

impl std::error::Error for SceneError {}
