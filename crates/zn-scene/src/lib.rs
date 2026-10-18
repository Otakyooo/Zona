use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

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

/// Вид аномалии (M4c).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnomalyKind {
    /// Жгучая: наносит урон, пока цель внутри сферы.
    Heat,
    /// Гравитационная: тянет цель к центру, урона нет.
    Gravity,
}

/// Аномалия в сцене: сфера радиуса `radius` с центром `position + (0, radius, 0)`
/// (якорь `position` лежит на полу — сфера касается пола). Аномалии не входят в
/// геометрию столкновений: эффекты считает симуляция, визуал — клиент.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneAnomaly {
    pub position: Vec3,
    pub radius: f32,
    pub kind: AnomalyKind,
    /// Ключ предмета из каталога: артефакт появляется на полу в якоре при старте матча.
    #[serde(default)]
    pub artifact: Option<String>,
}

impl SceneAnomaly {
    pub fn new(position: Vec3, radius: f32, kind: AnomalyKind) -> Self {
        Self {
            position,
            radius,
            kind,
            artifact: None,
        }
    }

    pub fn with_artifact(mut self, item: impl Into<String>) -> Self {
        self.artifact = Some(item.into());
        self
    }

    /// Центр сферы в мире.
    pub fn center(&self) -> Vec3 {
        self.position + Vec3::Y * self.radius
    }

    /// Точка внутри сферы аномалии?
    pub fn contains(&self, point: Vec3) -> bool {
        point.distance_squared(self.center()) <= self.radius * self.radius
    }
}

// ── Атмосфера локации (M4g) ─────────────────────────────────────────────

/// Атмосфера сцены (M4g): солнце и ambient для PBR, туман и градиент неба.
/// Визуал клиента — симуляция эти поля не читает. Цвета линейные; направление
/// солнца нормализует рендер (и [`Self::sun_direction_normalized`]).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneEnvironment {
    /// Направление НА солнце (им освещаются поверхности).
    pub sun_direction: Vec3,
    /// Цвет солнечного света.
    pub sun_color: Vec3,
    /// Интенсивность солнца (масштаб прямого света).
    pub sun_intensity: f32,
    /// Цвет неба-ambient (рассеянный свет).
    pub ambient_color: Vec3,
    /// Интенсивность ambient.
    pub ambient_intensity: f32,
    /// Цвет тумана (к нему уходит дальний план).
    pub fog_color: Vec3,
    /// Плотность экспоненциального тумана (0 — туман выключен).
    pub fog_density: f32,
    /// Цвет неба в зените.
    pub sky_top: Vec3,
    /// Цвет неба у горизонта.
    pub sky_horizon: Vec3,
}

impl Default for SceneEnvironment {
    /// Нейтраль эпохи M4c: тёмный фон, один тёплый источник, без тумана.
    fn default() -> Self {
        Self {
            sun_direction: Vec3::new(0.4, 0.8, 0.3),
            sun_color: Vec3::new(1.0, 0.96, 0.88),
            sun_intensity: 1.0,
            ambient_color: Vec3::new(0.18, 0.20, 0.26),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.06, 0.07, 0.09),
            fog_density: 0.0,
            sky_top: Vec3::new(0.01, 0.012, 0.02),
            sky_horizon: Vec3::new(0.03, 0.035, 0.05),
        }
    }
}

impl SceneEnvironment {
    /// Направление НА солнце, нормализованное (защита от нулевого вектора).
    pub fn sun_direction_normalized(&self) -> Vec3 {
        let length = self.sun_direction.length();
        if length > 1e-6 {
            self.sun_direction / length
        } else {
            Vec3::Y
        }
    }
}

// ── Локации и переходы (M4d) ────────────────────────────────────────────

/// Идентификатор локации мира (`LocationRegistry`).
pub type LocationId = u8;

/// Локация встроенного мира: База.
pub const LOCATION_BASE: LocationId = 0;
/// Локация встроенного мира: Кордон.
pub const LOCATION_KORDON: LocationId = 1;
/// Локация встроенного мира: Свалка.
pub const LOCATION_SVALKA: LocationId = 2;
/// Локация встроенного мира: Подземка.
pub const LOCATION_PODZEMKA: LocationId = 3;

/// Точка пузыря (M4d): сфера-телепорт. `position` — якорь на полу (центр —
/// `position + (0, radius, 0)`), вход игрока уносит его в ближайшую к базе
/// локацию. Кандидаты появления задаются в сцене; какие активны и когда
/// перерождаются — решает сервер.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneBubble {
    pub position: Vec3,
    pub radius: f32,
}

impl SceneBubble {
    pub fn new(position: Vec3, radius: f32) -> Self {
        Self { position, radius }
    }

    /// Центр сферы в мире (как у аномалий: сфера касается пола).
    pub fn center(&self) -> Vec3 {
        self.position + Vec3::Y * self.radius
    }

    /// Точка внутри сферы пузыря? Проверяется «сердце» игрока (ступни + 0.9).
    pub fn contains(&self, point: Vec3) -> bool {
        point.distance_squared(self.center()) <= self.radius * self.radius
    }
}

/// Переход между локациями (M4d): круг радиуса `radius` на полу вокруг
/// `position`. Вход игрока переключает его в локацию `target`, в точку
/// `target_position` с поворотом `target_yaw` (пешком, без таймера).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneTransition {
    pub position: Vec3,
    pub radius: f32,
    /// id целевой локации из `LocationRegistry`.
    pub target: LocationId,
    /// Точка появления в целевой локации (ступни).
    pub target_position: Vec3,
    #[serde(default)]
    pub target_yaw: f32,
    /// Подпись для рендера/UI (например, «База»).
    #[serde(default)]
    pub label: String,
}

impl SceneTransition {
    pub fn new(position: Vec3, radius: f32, target: LocationId, target_position: Vec3) -> Self {
        Self {
            position,
            radius,
            target,
            target_position,
            target_yaw: 0.0,
            label: String::new(),
        }
    }

    pub fn with_yaw(mut self, yaw: f32) -> Self {
        self.target_yaw = yaw;
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Точка в зоне перехода? Расстояние по горизонтали: зона — цилиндр.
    pub fn contains(&self, point: Vec3) -> bool {
        let delta = Vec3::new(point.x - self.position.x, 0.0, point.z - self.position.z);
        delta.length_squared() <= self.radius * self.radius
    }
}

/// Запись реестра локаций: название для UI и файл сцены без расширения
/// (`levels/<scene>.scene.ron`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LocationInfo {
    pub id: LocationId,
    pub name: String,
    pub scene: String,
    /// База — безопасная локация: спавн, склад, без аномалий.
    #[serde(default)]
    pub base: bool,
}

/// Реестр локаций мира (M4d): `locations.ron`. Клиент и сервер читают его при
/// старте; сцены локаций и сам реестр покрывает `world_content_hash`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LocationRegistry {
    pub locations: Vec<LocationInfo>,
}

impl LocationRegistry {
    /// Встроенный мир цикла M4d: База — Кордон — Свалка/Подземка.
    pub fn builtin() -> Self {
        Self {
            locations: vec![
                LocationInfo {
                    id: LOCATION_BASE,
                    name: "База".to_string(),
                    scene: "base".to_string(),
                    base: true,
                },
                LocationInfo {
                    id: LOCATION_KORDON,
                    name: "Кордон".to_string(),
                    scene: "kordon".to_string(),
                    base: false,
                },
                LocationInfo {
                    id: LOCATION_SVALKA,
                    name: "Свалка".to_string(),
                    scene: "svalka".to_string(),
                    base: false,
                },
                LocationInfo {
                    id: LOCATION_PODZEMKA,
                    name: "Подземка".to_string(),
                    scene: "podzemka".to_string(),
                    base: false,
                },
            ],
        }
    }

    pub fn to_ron(&self) -> Result<String, ron::Error> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
    }

    pub fn from_ron(text: &str) -> Result<Self, SceneError> {
        ron::from_str(text).map_err(SceneError::Deserialize)
    }

    pub fn load_ron(path: impl AsRef<Path>) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path).map_err(SceneError::Io)?;
        Self::from_ron(&text)
    }

    pub fn save_ron(&self, path: impl AsRef<Path>) -> Result<(), SceneError> {
        let text = self.to_ron().map_err(SceneError::Serialize)?;
        std::fs::write(path, text).map_err(SceneError::Io)
    }

    pub fn get(&self, id: LocationId) -> Option<&LocationInfo> {
        self.locations.iter().find(|info| info.id == id)
    }

    /// Путь к сцене локации: `<content_root>/levels/<scene>.scene.ron`.
    pub fn scene_path(&self, content_root: &Path, id: LocationId) -> Option<PathBuf> {
        let info = self.get(id)?;
        Some(
            content_root
                .join("levels")
                .join(format!("{}.scene.ron", info.scene)),
        )
    }

    /// Первая безопасная база (обычно единственная).
    pub fn base_id(&self) -> Option<LocationId> {
        self.locations
            .iter()
            .find(|info| info.base)
            .map(|info| info.id)
    }

    /// Детерминированный хэш реестра (FNV-1a по каноническому RON).
    pub fn content_hash(&self) -> u64 {
        let text = self.to_ron().unwrap_or_default();
        let mut hash = FNV_OFFSET_BASIS;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
}

/// Хэш мира (реестр + сцены локаций, отсортированные по id). Клиент и сервер
/// сверяют его в handshake: расхождение — разные переходы/коллайдеры.
pub fn world_content_hash(registry: &LocationRegistry, scenes: &[(LocationId, &Scene)]) -> u64 {
    let mut entries: Vec<(LocationId, u64)> = scenes
        .iter()
        .map(|(id, scene)| (*id, scene.content_hash()))
        .collect();
    entries.sort_unstable_by_key(|(id, _)| *id);
    let mut hash = registry.content_hash();
    for (id, scene_hash) in entries {
        hash ^= scene_hash;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= u64::from(id);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Кратчайшие расстояния (в переходах) от каждой локации до базы; рёбра —
/// переходы сцен. Пузырь уносит игрока в локацию с минимальным расстоянием
/// (ближайшую к базе). Недостижимые локации в карту не попадают.
pub fn distances_to_base(
    registry: &LocationRegistry,
    scenes: &[(LocationId, &Scene)],
) -> HashMap<LocationId, u32> {
    let mut distances = HashMap::new();
    let Some(base) = registry.base_id() else {
        return distances;
    };
    let mut adjacency: HashMap<LocationId, Vec<LocationId>> = HashMap::new();
    for (id, scene) in scenes {
        let entry = adjacency.entry(*id).or_default();
        for transition in &scene.transitions {
            entry.push(transition.target);
        }
    }
    let mut queue = VecDeque::from([(base, 0u32)]);
    distances.insert(base, 0);
    while let Some((id, distance)) = queue.pop_front() {
        let neighbours = adjacency.get(&id).cloned().unwrap_or_default();
        for neighbour in neighbours {
            if distances.contains_key(&neighbour) || registry.get(neighbour).is_none() {
                continue;
            }
            distances.insert(neighbour, distance + 1);
            queue.push_back((neighbour, distance + 1));
        }
    }
    distances
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

/// Плоская плита-маркер (переход/зона): лежит на полу, не мешает движению.
fn pad(x: f32, z: f32, size: f32, color: Vec4) -> SceneObject {
    SceneObject::new(Vec3::new(x, 0.06, z), Vec3::new(size, 0.12, size), color).invulnerable()
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
    /// Аномалии уровня (M4c): не коллайдеры, влияют на игроков и болты.
    #[serde(default)]
    pub anomalies: Vec<SceneAnomaly>,
    /// Переходы в другие локации (M4d), пешком.
    #[serde(default)]
    pub transitions: Vec<SceneTransition>,
    /// Кандидаты появления пузырей (M4d).
    #[serde(default)]
    pub bubbles: Vec<SceneBubble>,
    /// Точки прибытия пузырей (M4d): сюда сервер высаживает игрока после ТП.
    #[serde(default)]
    pub bubble_arrivals: Vec<Vec3>,
    /// Атмосфера локации (M4g): солнце, ambient, туман, небо.
    #[serde(default)]
    pub environment: SceneEnvironment,
}

impl Scene {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            spawn: Vec3::new(0.0, 2.0, 0.0),
            spawn_yaw: 0.0,
            objects: Vec::new(),
            anomalies: Vec::new(),
            transitions: Vec::new(),
            bubbles: Vec::new(),
            bubble_arrivals: Vec::new(),
            environment: SceneEnvironment::default(),
        }
    }

    /// Задаёт атмосферу локации (M4g).
    pub fn with_environment(mut self, environment: SceneEnvironment) -> Self {
        self.environment = environment;
        self
    }

    pub fn add(&mut self, object: SceneObject) -> &mut Self {
        self.objects.push(object);
        self
    }

    pub fn add_anomaly(&mut self, anomaly: SceneAnomaly) -> &mut Self {
        self.anomalies.push(anomaly);
        self
    }

    pub fn add_transition(&mut self, transition: SceneTransition) -> &mut Self {
        self.transitions.push(transition);
        self
    }

    pub fn add_bubble(&mut self, bubble: SceneBubble) -> &mut Self {
        self.bubbles.push(bubble);
        self
    }

    pub fn add_bubble_arrival(&mut self, position: Vec3) -> &mut Self {
        self.bubble_arrivals.push(position);
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
        // Пасмурный день Зоны: мягкое солнце, серый горизонт, лёгкая дымка.
        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(0.4, 0.8, 0.3),
            sun_color: Vec3::new(1.0, 0.95, 0.85),
            sun_intensity: 0.9,
            ambient_color: Vec3::new(0.24, 0.25, 0.28),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.62, 0.63, 0.60),
            fog_density: 0.010,
            sky_top: Vec3::new(0.35, 0.42, 0.52),
            sky_horizon: Vec3::new(0.68, 0.68, 0.66),
        };

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
            crate_object(-1.3, 15.0, &[("ammo_545", 60), ("bolt", 10)]),
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

        // Аномалии M4c: две жгучие (урон в радиусе) и одна гравитационная
        // (тянет к центру). В двух из них с начала матча лежат артефакты
        // («Жар-камень» и «Тягун»); появление предметов — работа симуляции.
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(15.0, 0.0, 15.0), 3.0, AnomalyKind::Heat)
                .with_artifact("artifact_heat"),
        );
        scene.add_anomaly(SceneAnomaly::new(
            Vec3::new(3.0, 0.0, -21.0),
            3.0,
            AnomalyKind::Heat,
        ));
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(-15.0, 0.0, 15.0), 3.5, AnomalyKind::Gravity)
                .with_artifact("artifact_gravity"),
        );

        scene
    }

    /// База (M4d): безопасная локация — спавн, склад, ворота на Кордон.
    /// Аномалий, пузырей и лута нет.
    pub fn base() -> Self {
        let mut scene = Scene::new("base");
        scene.spawn = Vec3::new(0.0, 0.0, 14.0);
        scene.spawn_yaw = std::f32::consts::PI;
        // Ясный день (безопасная зона): выше солнце, синее небо, дымка слабая.
        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(-0.35, 0.75, 0.45),
            sun_color: Vec3::new(1.0, 0.95, 0.86),
            sun_intensity: 1.1,
            ambient_color: Vec3::new(0.28, 0.30, 0.36),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.60, 0.66, 0.75),
            fog_density: 0.006,
            sky_top: Vec3::new(0.12, 0.26, 0.55),
            sky_horizon: Vec3::new(0.60, 0.70, 0.80),
        };

        let mut push = |position: Vec3, scale: Vec3, color: Vec4| {
            scene.add(SceneObject::new(position, scale, color).with_asset(BUILTIN_CUBE));
        };

        let ground = Vec4::new(0.20, 0.21, 0.18, 1.0);
        let wall = Vec4::new(0.12, 0.13, 0.12, 1.0);
        let building = Vec4::new(0.26, 0.26, 0.24, 1.0);

        // Площадка 48×48 и периметр.
        push(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(48.0, 0.5, 48.0),
            ground,
        );
        push(Vec3::new(0.0, 2.5, -24.0), Vec3::new(49.0, 3.0, 1.0), wall);
        push(Vec3::new(0.0, 2.5, 24.0), Vec3::new(49.0, 3.0, 1.0), wall);
        push(Vec3::new(-24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0), wall);
        push(Vec3::new(24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0), wall);

        // Постройки: казарма и ангар.
        push(
            Vec3::new(-12.0, 2.5, -8.0),
            Vec3::new(12.0, 5.0, 10.0),
            building,
        );
        push(
            Vec3::new(10.0, 2.0, 6.0),
            Vec3::new(8.0, 4.0, 8.0),
            building,
        );

        // Склад (декор): контейнер у спавна; склад-панель — по действию игрока.
        push(
            Vec3::new(4.0, 0.6, 10.0),
            Vec3::new(1.6, 1.2, 1.6),
            Vec4::new(0.45, 0.33, 0.18, 1.0),
        );

        // Ворота на Кордон (у северной стены).
        scene.add(pad(0.0, -20.0, 5.0, Vec4::new(0.20, 0.55, 0.25, 1.0)));
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(0.0, 0.0, -20.0),
                2.5,
                LOCATION_KORDON,
                Vec3::new(0.0, 0.0, 22.0),
            )
            .with_yaw(std::f32::consts::PI)
            .with_label("Кордон"),
        );

        scene
    }

    /// Кордон (M4d): нынешний demo_zone с переходами (База/Свалка/Подземка),
    /// кандидатами пузырей и точками прибытия у ворот базы.
    pub fn kordon() -> Self {
        let mut scene = Self::demo_zone();
        scene.name = "kordon".to_string();

        scene.add(pad(0.0, 26.0, 5.0, Vec4::new(0.20, 0.55, 0.25, 1.0)));
        scene.add(pad(-26.0, 0.0, 5.0, Vec4::new(0.25, 0.35, 0.60, 1.0)));
        scene.add(pad(26.0, 0.0, 5.0, Vec4::new(0.60, 0.40, 0.18, 1.0)));

        scene.add_transition(
            SceneTransition::new(
                Vec3::new(0.0, 0.0, 26.0),
                2.5,
                LOCATION_BASE,
                Vec3::new(0.0, 0.0, -16.0),
            )
            .with_label("База"),
        );
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(-26.0, 0.0, 0.0),
                2.5,
                LOCATION_SVALKA,
                Vec3::new(38.0, 0.0, 0.0),
            )
            .with_yaw(-std::f32::consts::FRAC_PI_2)
            .with_label("Свалка"),
        );
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(26.0, 0.0, 0.0),
                2.5,
                LOCATION_PODZEMKA,
                Vec3::new(-15.0, 0.0, 0.0),
            )
            .with_yaw(std::f32::consts::FRAC_PI_2)
            .with_label("Подземка"),
        );

        // Кандидаты пузырей: свободные точки карты.
        scene.add_bubble(SceneBubble::new(Vec3::new(12.0, 0.0, -16.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(-16.0, 0.0, 10.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(18.0, 0.0, 20.0), 1.2));

        // Точки прибытия: пузыри из Свалки/Подземки высаживают здесь —
        // почти под базой, до ворот идти пешком.
        scene.add_bubble_arrival(Vec3::new(6.0, 0.0, 19.0));
        scene.add_bubble_arrival(Vec3::new(-8.0, 0.0, 20.0));

        scene
    }

    /// Свалка (M4d): открытая локация 96×96 — завалы, три аномалии, лут.
    /// Переход на восток — к Кордону.
    pub fn svalka() -> Self {
        let mut scene = Scene::new("svalka");
        scene.spawn = Vec3::new(38.0, 0.0, 0.0);
        scene.spawn_yaw = -std::f32::consts::FRAC_PI_2;
        // Свалка: низкое пыльное солнце, рыжая мгла, тяжёлый туман.
        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(-0.25, 0.62, 0.50),
            sun_color: Vec3::new(1.0, 0.88, 0.68),
            sun_intensity: 0.8,
            ambient_color: Vec3::new(0.30, 0.27, 0.22),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.50, 0.44, 0.35),
            fog_density: 0.020,
            sky_top: Vec3::new(0.30, 0.29, 0.26),
            sky_horizon: Vec3::new(0.60, 0.54, 0.44),
        };

        let mut push = |position: Vec3, scale: Vec3, color: Vec4| {
            scene.add(SceneObject::new(position, scale, color).with_asset(BUILTIN_CUBE));
        };

        let ground = Vec4::new(0.20, 0.19, 0.16, 1.0);
        let wall = Vec4::new(0.11, 0.11, 0.10, 1.0);
        let pile = Vec4::new(0.28, 0.27, 0.23, 1.0);

        push(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(96.0, 0.5, 96.0),
            ground,
        );
        push(Vec3::new(0.0, 2.5, -48.0), Vec3::new(97.0, 3.0, 1.0), wall);
        push(Vec3::new(0.0, 2.5, 48.0), Vec3::new(97.0, 3.0, 1.0), wall);
        push(Vec3::new(-48.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 97.0), wall);
        push(Vec3::new(48.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 97.0), wall);

        // Завалы: башни и разбросанные блоки.
        let piles = [
            (Vec3::new(-10.0, 4.0, -12.0), Vec3::new(6.0, 8.0, 6.0)),
            (Vec3::new(12.0, 3.0, 14.0), Vec3::new(5.0, 6.0, 5.0)),
            (Vec3::new(-24.0, 2.5, 20.0), Vec3::new(4.0, 5.0, 4.0)),
            (Vec3::new(26.0, 2.0, -26.0), Vec3::new(6.0, 4.0, 6.0)),
            (Vec3::new(-34.0, 1.5, -30.0), Vec3::new(5.0, 3.0, 7.0)),
        ];
        for (position, scale) in piles {
            push(position, scale, pile);
        }
        let debris = [
            (Vec3::new(4.0, 0.75, -20.0), Vec3::new(3.0, 1.5, 2.0)),
            (Vec3::new(-18.0, 1.0, 2.0), Vec3::new(2.0, 2.0, 3.0)),
            (Vec3::new(20.0, 0.6, 30.0), Vec3::new(4.0, 1.2, 2.0)),
            (Vec3::new(-6.0, 0.9, 26.0), Vec3::new(2.5, 1.8, 2.5)),
            (Vec3::new(30.0, 1.2, 8.0), Vec3::new(3.0, 2.4, 3.0)),
            (Vec3::new(-28.0, 0.7, -8.0), Vec3::new(2.0, 1.4, 4.0)),
        ];
        for (position, scale) in debris {
            push(position, scale, pile);
        }

        // Заборы-стенки, делящие карту.
        push(Vec3::new(-2.0, 1.5, -30.0), Vec3::new(20.0, 3.0, 1.0), wall);
        push(Vec3::new(-20.0, 1.5, 8.0), Vec3::new(1.0, 3.0, 18.0), wall);

        // Ящики: лут по всей карте.
        let crates = [
            crate_object(30.0, 20.0, &[("medkit", 1), ("ammo_545", 30)]),
            crate_object(-20.0, -20.0, &[("bolt", 20), ("bandage", 2)]),
            crate_object(0.0, 34.0, &[("canned_food", 2), ("vodka", 1)]),
            crate_object(26.0, -10.0, &[("medkit", 2), ("ammo_545", 60)]),
            crate_object(-30.0, 34.0, &[("bolt", 30), ("bandage", 2)]),
            crate_object(14.0, -36.0, &[("vodka", 2), ("canned_food", 1)]),
        ];
        for object in crates {
            scene.add(object);
        }

        // Аномалии: две с артефактами, одна пустая.
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(20.0, 0.0, 25.0), 3.0, AnomalyKind::Heat)
                .with_artifact("artifact_heat"),
        );
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(-18.0, 0.0, -22.0), 3.5, AnomalyKind::Gravity)
                .with_artifact("artifact_gravity"),
        );
        scene.add_anomaly(SceneAnomaly::new(
            Vec3::new(-34.0, 0.0, 24.0),
            3.0,
            AnomalyKind::Heat,
        ));

        // Переход к Кордону (у восточной стены).
        scene.add(pad(43.0, 0.0, 5.0, Vec4::new(0.25, 0.35, 0.60, 1.0)));
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(43.0, 0.0, 0.0),
                2.5,
                LOCATION_KORDON,
                Vec3::new(-21.0, 0.0, 0.0),
            )
            .with_yaw(std::f32::consts::FRAC_PI_2)
            .with_label("Кордон"),
        );

        // Кандидаты пузырей.
        scene.add_bubble(SceneBubble::new(Vec3::new(30.0, 0.0, 30.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(-30.0, 0.0, -30.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(-34.0, 0.0, -4.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(26.0, 0.0, -34.0), 1.2));

        scene
    }

    /// Подземка (M4d): тесные коридоры 48×48, одна аномалия, лут.
    /// Переход на запад — к Кордону.
    pub fn podzemka() -> Self {
        let mut scene = Scene::new("podzemka");
        scene.spawn = Vec3::new(-15.0, 0.0, 0.0);
        scene.spawn_yaw = std::f32::consts::FRAC_PI_2;
        // Подземка: почти нет солнца, холодный ambient, тёмная мгла коридоров.
        // Лампы (emissive) появятся в G2/G5 — пока ambient чуть выше минимума,
        // чтобы коридоры читались.
        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(0.0, 1.0, 0.0),
            sun_color: Vec3::new(0.70, 0.78, 0.95),
            sun_intensity: 0.20,
            ambient_color: Vec3::new(0.20, 0.22, 0.26),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.04, 0.05, 0.07),
            fog_density: 0.035,
            sky_top: Vec3::new(0.004, 0.005, 0.009),
            sky_horizon: Vec3::new(0.02, 0.025, 0.04),
        };

        let mut push = |position: Vec3, scale: Vec3, color: Vec4| {
            scene.add(SceneObject::new(position, scale, color).with_asset(BUILTIN_CUBE));
        };

        let ground = Vec4::new(0.16, 0.16, 0.15, 1.0);
        let wall = Vec4::new(0.10, 0.10, 0.09, 1.0);
        let inner = Vec4::new(0.22, 0.22, 0.21, 1.0);

        push(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(48.0, 0.5, 48.0),
            ground,
        );
        push(Vec3::new(0.0, 2.5, -24.0), Vec3::new(49.0, 3.0, 1.0), wall);
        push(Vec3::new(0.0, 2.5, 24.0), Vec3::new(49.0, 3.0, 1.0), wall);
        push(Vec3::new(-24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0), wall);
        push(Vec3::new(24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0), wall);

        // Коридоры: длинные стены с проходами.
        push(Vec3::new(-8.0, 2.0, -9.0), Vec3::new(1.0, 4.0, 18.0), inner);
        push(Vec3::new(8.0, 2.0, 9.0), Vec3::new(1.0, 4.0, 18.0), inner);
        push(Vec3::new(0.0, 2.0, -16.0), Vec3::new(14.0, 4.0, 1.0), inner);
        push(Vec3::new(0.0, 2.0, 16.0), Vec3::new(14.0, 4.0, 1.0), inner);

        let crates = [
            crate_object(-16.0, 20.0, &[("medkit", 1), ("bandage", 2)]),
            crate_object(14.0, -14.0, &[("bolt", 15), ("ammo_545", 30)]),
            crate_object(4.0, 4.0, &[("canned_food", 1), ("vodka", 1)]),
        ];
        for object in crates {
            scene.add(object);
        }

        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(16.0, 0.0, 16.0), 2.5, AnomalyKind::Heat)
                .with_artifact("artifact_heat"),
        );

        // Переход к Кордону (у западной стены).
        scene.add(pad(-20.0, 0.0, 5.0, Vec4::new(0.60, 0.40, 0.18, 1.0)));
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(-20.0, 0.0, 0.0),
                2.5,
                LOCATION_KORDON,
                Vec3::new(21.0, 0.0, 0.0),
            )
            .with_yaw(-std::f32::consts::FRAC_PI_2)
            .with_label("Кордон"),
        );

        // Кандидаты пузырей.
        scene.add_bubble(SceneBubble::new(Vec3::new(12.0, 0.0, -12.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(-12.0, 0.0, 12.0), 1.2));
        scene.add_bubble(SceneBubble::new(Vec3::new(0.0, 0.0, 20.0), 1.2));

        scene
    }

    /// Встроенный мир цикла M4d: сцены локаций `LocationRegistry::builtin()`.
    pub fn builtin_world() -> Vec<(LocationId, Scene)> {
        vec![
            (LOCATION_BASE, Scene::base()),
            (LOCATION_KORDON, Scene::kordon()),
            (LOCATION_SVALKA, Scene::svalka()),
            (LOCATION_PODZEMKA, Scene::podzemka()),
        ]
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
