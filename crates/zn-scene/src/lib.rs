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
/// Встроенная процедурная сфера (маркеры, лампы, аномалии).
pub const BUILTIN_SPHERE: &str = "builtin://sphere";

/// Встроенные текстуры поверхностей (M4g): ключи совпадают с zn-assets.
pub const TEXTURE_CONCRETE: &str = "builtin://concrete";
pub const TEXTURE_ASPHALT: &str = "builtin://asphalt";
pub const TEXTURE_METAL: &str = "builtin://metal";
pub const TEXTURE_DIRT: &str = "builtin://dirt";

/// Куб-поверхность в текстуре (M4g): мировые (box) UV — плотность текселей
/// не зависит от размеров объекта, стены и полы текстурируются одинаково.
fn textured_cube(
    position: Vec3,
    scale: Vec3,
    color: Vec4,
    texture: &str,
    uv_scale: f32,
) -> SceneObject {
    SceneObject::new(position, scale, color)
        .with_asset(BUILTIN_CUBE)
        .with_texture(texture, uv_scale, true)
}

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
/// Локация встроенного мира: Полигон (G5: испытательный полигон всех систем).
pub const LOCATION_POLYGON: LocationId = 4;

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

/// Круг экстракции (M4d-2): вход со добычей сохраняет `percent` инвентаря
/// в персональный стэш базы (смерть или конец сессии — потеря всего, 0 %).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneExtraction {
    /// Якорь круга на полу.
    pub position: Vec3,
    pub radius: f32,
    /// Доля сохраняемого инвентаря, 0..=1 (1 — сохраняется всё).
    pub percent: f32,
    /// Подпись для рендера/UI (например, «Эвакуация»).
    #[serde(default)]
    pub label: String,
}

impl SceneExtraction {
    pub fn new(position: Vec3, radius: f32, percent: f32) -> Self {
        Self {
            position,
            radius,
            percent,
            label: String::new(),
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Точка в зоне экстракции? Расстояние по горизонтали: зона — цилиндр.
    pub fn contains(&self, point: Vec3) -> bool {
        let delta = Vec3::new(point.x - self.position.x, 0.0, point.z - self.position.z);
        delta.length_squared() <= self.radius * self.radius
    }
}

/// Тип станции базы (M5-2).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StationKind {
    /// Торговец: продажа лута по `value_rub`, покупка офферов по `value×2`.
    Trader,
    /// Верстак: +10 % сохраняемой добычи за уровень (к `percent` круга).
    Workbench,
    /// Медпункт: реген HP игрокам на базе, сила — уровень.
    Medpost,
    /// Сейф: доля лута, спасённая со трупа при смерти, — уровень.
    Safe,
}

/// Оффер торговца (M5-2): предмет каталога по ключу и количество в партии.
/// Сервер резолвит ключ в индекс каталога при старте (сцена не знает индексов).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneOffer {
    /// Ключ предмета в `items.ron`.
    pub item: String,
    /// Сколько штук в одной покупке.
    #[serde(default = "default_offer_count")]
    pub count: u32,
}

fn default_offer_count() -> u32 {
    1
}

/// Станция базы (M5-2): круг радиуса `radius` вокруг `position`; E внутри —
/// взаимодействие (`BaseInteract`). У торговца — офферы; у верстака/медпункта/
/// сейфа — только вид и уровень из `facilities` аккаунта (не в сцене).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneStation {
    /// Якорь круга на полу.
    pub position: Vec3,
    pub radius: f32,
    pub kind: StationKind,
    /// Подпись для UI («Торговец», «Верстак», «Медпункт», «Сейф»).
    #[serde(default)]
    pub label: String,
    /// Офферы торговца; у остальных станций пусто.
    #[serde(default)]
    pub offers: Vec<SceneOffer>,
}

impl SceneStation {
    pub fn new(position: Vec3, radius: f32, kind: StationKind) -> Self {
        Self {
            position,
            radius,
            kind,
            label: String::new(),
            offers: Vec::new(),
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_offers(mut self, offers: Vec<SceneOffer>) -> Self {
        self.offers = offers;
        self
    }

    /// Точка в зоне станции? Расстояние по горизонтали: зона — цилиндр.
    pub fn contains(&self, point: Vec3) -> bool {
        let delta = Vec3::new(point.x - self.position.x, 0.0, point.z - self.position.z);
        delta.length_squared() <= self.radius * self.radius
    }
}

/// Точка спавна мутантов на локации (PvE).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneMutantSpawn {
    pub position: Vec3,
    #[serde(default)]
    pub kind: u8, // 0 = Слепой пёс
}

impl SceneMutantSpawn {
    pub fn new(position: Vec3, kind: u8) -> Self {
        Self { position, kind }
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
                LocationInfo {
                    id: LOCATION_POLYGON,
                    name: "Полигон".to_string(),
                    scene: "polygon".to_string(),
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
    /// Текстура альбедо (M4g): `builtin://…` или ключ ассета.
    #[serde(default)]
    pub texture: Option<String>,
    /// Тайлинг текстур (M4g): множитель текстурных координат.
    #[serde(default = "default_uv_scale")]
    pub uv_scale: f32,
    /// Мировые (box) UV вместо UV меша (M4g) — для стен, полов и завалов.
    #[serde(default)]
    pub world_uv: bool,
    /// Переопределение шероховатости (M4g).
    #[serde(default)]
    pub roughness: Option<f32>,
    /// Переопределение металличности (M4g).
    #[serde(default)]
    pub metallic: Option<f32>,
    /// Свечение (M4g), линейный цвет: лампы, знаки, экраны.
    #[serde(default)]
    pub emissive: Option<Vec3>,
}

fn default_color() -> Vec4 {
    Vec4::ONE
}

fn default_true() -> bool {
    true
}

fn default_uv_scale() -> f32 {
    1.0
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

/// Светящаяся лампа-маркер: небольшая сфера с emissive без коллизии.
pub fn lamp_object(position: Vec3, color: Vec3, intensity: f32) -> SceneObject {
    SceneObject::new(
        position,
        Vec3::splat(0.25),
        Vec4::new(color.x, color.y, color.z, 1.0),
    )
    .with_asset(BUILTIN_SPHERE)
    .with_emissive(color * intensity)
    .invulnerable()
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
            texture: None,
            uv_scale: 1.0,
            world_uv: false,
            roughness: None,
            metallic: None,
            emissive: None,
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

    /// Текстура альбедо (M4g) с тайлингом; `world_uv` — box-проекция по миру.
    pub fn with_texture(
        mut self,
        texture: impl Into<String>,
        uv_scale: f32,
        world_uv: bool,
    ) -> Self {
        self.texture = Some(texture.into());
        self.uv_scale = uv_scale;
        self.world_uv = world_uv;
        self
    }

    /// Шероховатость поверхности (M4g), 0..1.
    pub fn with_roughness(mut self, roughness: f32) -> Self {
        self.roughness = Some(roughness);
        self
    }

    /// Металличность поверхности (M4g), 0..1.
    pub fn with_metallic(mut self, metallic: f32) -> Self {
        self.metallic = Some(metallic);
        self
    }

    /// Свечение (M4g), линейный цвет — лампы и светящиеся знаки.
    pub fn with_emissive(mut self, emissive: Vec3) -> Self {
        self.emissive = Some(emissive);
        self
    }

    /// Задан ли персональный материал (M4g): текстура, тайлинг или факторы.
    pub fn has_material_overrides(&self) -> bool {
        self.texture.is_some()
            || self.world_uv
            || self.roughness.is_some()
            || self.metallic.is_some()
            || self.emissive.is_some()
            || (self.uv_scale - 1.0).abs() > f32::EPSILON
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
    /// Круги экстракции (M4d-2): вход сохраняет `percent` добычи в стэш.
    #[serde(default)]
    pub extractions: Vec<SceneExtraction>,
    /// Станции базы (M5-2): торговец, верстак, медпункт, сейф.
    #[serde(default)]
    pub stations: Vec<SceneStation>,
    /// Точки спавна мутантов на локации (PvE).
    #[serde(default)]
    pub mutant_spawns: Vec<SceneMutantSpawn>,
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
            extractions: Vec::new(),
            stations: Vec::new(),
            mutant_spawns: Vec::new(),
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

    pub fn add_extraction(&mut self, extraction: SceneExtraction) -> &mut Self {
        self.extractions.push(extraction);
        self
    }

    pub fn add_station(&mut self, station: SceneStation) -> &mut Self {
        self.stations.push(station);
        self
    }

    pub fn add_mutant_spawn(&mut self, spawn: SceneMutantSpawn) -> &mut Self {
        self.mutant_spawns.push(spawn);
        self
    }

    /// Станция, накрывающая точку (ступни игрока), если есть.
    pub fn station_at(&self, point: Vec3) -> Option<&SceneStation> {
        self.stations.iter().find(|station| station.contains(point))
    }

    /// Круг экстракции, накрывающий точку (ступни игрока), если есть.
    pub fn extraction_at(&self, point: Vec3) -> Option<&SceneExtraction> {
        self.extractions
            .iter()
            .find(|extraction| extraction.contains(point))
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

    /// AABB всех объектов сцены (M4g: охват теневой карты). Для пустой
    /// сцены — точка спавна.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for object in &self.objects {
            let (object_min, object_max) = object.bounds();
            min = min.min(object_min);
            max = max.max(object_max);
        }
        if !min.is_finite() || !max.is_finite() {
            return (self.spawn, self.spawn);
        }
        (min, max)
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

        // Пол и периметр — текстуры (M4g): асфальт площадки, бетон стен.
        scene.add(textured_cube(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(60.0, 0.5, 60.0),
            Vec4::new(0.95, 0.95, 0.90, 1.0),
            TEXTURE_ASPHALT,
            0.25,
        ));

        let wall_color = Vec4::new(0.30, 0.30, 0.32, 1.0);
        for (position, scale) in [
            (Vec3::new(0.0, 2.5, -30.0), Vec3::new(31.0, 3.0, 1.0)),
            (Vec3::new(0.0, 2.5, 30.0), Vec3::new(31.0, 3.0, 1.0)),
            (Vec3::new(-30.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 31.0)),
            (Vec3::new(30.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 31.0)),
        ] {
            scene.add(textured_cube(
                position,
                scale,
                wall_color,
                TEXTURE_CONCRETE,
                0.5,
            ));
        }

        let mut push = |position: Vec3, scale: Vec3, color: Vec4| {
            scene.add(SceneObject::new(position, scale, color).with_asset(BUILTIN_CUBE));
        };

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

        // Площадка, периметр и постройки — текстуры бетона и асфальта (M4g, G5).
        let ground = Vec4::new(0.62, 0.64, 0.60, 1.0);
        let wall = Vec4::new(0.45, 0.46, 0.45, 1.0);
        let building = Vec4::new(0.78, 0.78, 0.75, 1.0);
        let metal = Vec4::new(0.55, 0.52, 0.50, 1.0);

        // Периметр базы (бетонные плиты).
        scene.add(textured_cube(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(48.0, 0.5, 48.0),
            ground,
            TEXTURE_CONCRETE,
            0.25,
        ));
        // Центральный асфальтовый плац.
        scene.add(textured_cube(
            Vec3::new(0.0, -0.48, 4.0),
            Vec3::new(28.0, 0.5, 30.0),
            Vec4::new(0.35, 0.35, 0.36, 1.0),
            TEXTURE_ASPHALT,
            0.3,
        ));

        // Внешние стены базы по периметру (удерживают игрока на уровне).
        for (position, scale) in [
            (Vec3::new(0.0, 2.5, -24.0), Vec3::new(49.0, 3.0, 1.0)),
            (Vec3::new(0.0, 2.5, 24.0), Vec3::new(49.0, 3.0, 1.0)),
            (Vec3::new(-24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0)),
            (Vec3::new(24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0)),
        ] {
            scene.add(textured_cube(position, scale, wall, TEXTURE_CONCRETE, 0.5));
        }

        // КПП / Северные ворота на Кордон (декоративное оформление проезда):
        scene.add(
            textured_cube(
                Vec3::new(-4.5, 2.5, -22.5),
                Vec3::new(1.5, 5.0, 1.6),
                wall,
                TEXTURE_CONCRETE,
                0.5,
            )
            .invulnerable(),
        );
        scene.add(
            textured_cube(
                Vec3::new(4.5, 2.5, -22.5),
                Vec3::new(1.5, 5.0, 1.6),
                wall,
                TEXTURE_CONCRETE,
                0.5,
            )
            .invulnerable(),
        );
        scene.add(
            textured_cube(
                Vec3::new(0.0, 4.5, -22.5),
                Vec3::new(10.5, 1.0, 1.2),
                metal,
                TEXTURE_METAL,
                0.4,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(-4.5, 4.8, -21.5),
            Vec3::new(1.2, 1.0, 0.6),
            1.8,
        ));
        scene.add(lamp_object(
            Vec3::new(4.5, 4.8, -21.5),
            Vec3::new(1.2, 1.0, 0.6),
            1.8,
        ));
        scene.add(
            textured_cube(
                Vec3::new(9.0, 1.8, -20.0),
                Vec3::new(3.5, 3.6, 4.0),
                building,
                TEXTURE_CONCRETE,
                0.4,
            )
            .invulnerable(),
        );

        // Постройки: казарма и ангар.
        for (position, scale) in [
            (Vec3::new(-12.0, 2.5, -8.0), Vec3::new(12.0, 5.0, 10.0)),
            (Vec3::new(10.0, 2.0, 6.0), Vec3::new(8.0, 4.0, 8.0)),
        ] {
            scene.add(textured_cube(
                position,
                scale,
                building,
                TEXTURE_CONCRETE,
                0.4,
            ));
        }

        // Осветительные мачты плаца:
        scene.add(
            textured_cube(
                Vec3::new(-11.0, 3.0, 2.0),
                Vec3::new(0.3, 6.0, 0.3),
                metal,
                TEXTURE_METAL,
                0.2,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(-11.0, 6.1, 2.0),
            Vec3::new(1.1, 1.0, 0.8),
            2.0,
        ));
        scene.add(
            textured_cube(
                Vec3::new(11.0, 3.0, 2.0),
                Vec3::new(0.3, 6.0, 0.3),
                metal,
                TEXTURE_METAL,
                0.2,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(11.0, 6.1, 2.0),
            Vec3::new(1.1, 1.0, 0.8),
            2.0,
        ));

        // Склад (декор): контейнер у спавна — ржавый металл; склад-панель —
        // по действию игрока.
        scene.add(textured_cube(
            Vec3::new(4.0, 0.6, 10.0),
            Vec3::new(1.6, 1.2, 1.6),
            Vec4::new(0.90, 0.66, 0.36, 1.0),
            TEXTURE_METAL,
            0.5,
        ));

        // Антураж станций (декорации с invulnerable(), чтобы не мешать навигации игроков):
        scene.add(
            textured_cube(
                Vec3::new(-8.0, 0.5, 7.8),
                Vec3::new(2.4, 1.0, 0.8),
                metal,
                TEXTURE_METAL,
                0.5,
            )
            .invulnerable(),
        );
        scene.add(
            textured_cube(
                Vec3::new(-8.0, 1.5, 12.4),
                Vec3::new(2.8, 3.0, 0.6),
                wall,
                TEXTURE_CONCRETE,
                0.5,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(-8.0, 2.4, 8.8),
            Vec3::new(1.2, 1.0, 0.5),
            1.5,
        ));

        scene.add(
            textured_cube(
                Vec3::new(8.0, 0.5, 11.8),
                Vec3::new(2.2, 1.0, 0.8),
                building,
                TEXTURE_CONCRETE,
                0.5,
            )
            .invulnerable(),
        );
        scene.add(
            textured_cube(
                Vec3::new(8.0, 1.5, 16.4),
                Vec3::new(2.4, 3.0, 0.6),
                metal,
                TEXTURE_METAL,
                0.5,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(8.0, 2.4, 13.8),
            Vec3::new(0.4, 1.1, 1.1),
            1.6,
        ));

        scene.add(
            textured_cube(
                Vec3::new(-8.0, 0.5, 15.8),
                Vec3::new(2.2, 1.0, 0.8),
                metal,
                TEXTURE_METAL,
                0.4,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(-8.0, 2.2, 17.0),
            Vec3::new(1.1, 0.95, 0.7),
            1.4,
        ));

        scene.add(
            textured_cube(
                Vec3::new(8.0, 1.0, 20.4),
                Vec3::new(1.6, 2.0, 0.8),
                Vec4::new(0.2, 0.2, 0.22, 1.0),
                TEXTURE_METAL,
                0.3,
            )
            .invulnerable(),
        );
        scene.add(lamp_object(
            Vec3::new(8.0, 2.2, 19.8),
            Vec3::new(1.2, 0.7, 0.2),
            1.7,
        ));

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

        // Станции базы (M5-2): четыре круга у спавна. Торговец несёт офферы
        // (покупка по двойной цене каталога), остальные — уровни из аккаунта.
        scene.add_station(
            SceneStation::new(Vec3::new(-8.0, 0.0, 10.0), 2.0, StationKind::Trader)
                .with_label("Торговец")
                .with_offers(vec![
                    SceneOffer {
                        item: "medkit".into(),
                        count: 1,
                    },
                    SceneOffer {
                        item: "bandage".into(),
                        count: 1,
                    },
                    SceneOffer {
                        item: "ammo_545".into(),
                        count: 30,
                    },
                    SceneOffer {
                        item: "canned_food".into(),
                        count: 1,
                    },
                    SceneOffer {
                        item: "vodka".into(),
                        count: 1,
                    },
                ]),
        );
        scene.add_station(
            SceneStation::new(Vec3::new(8.0, 0.0, 14.0), 2.0, StationKind::Medpost)
                .with_label("Медпункт"),
        );
        scene.add_station(
            SceneStation::new(Vec3::new(-8.0, 0.0, 18.0), 2.0, StationKind::Workbench)
                .with_label("Верстак"),
        );
        scene.add_station(
            SceneStation::new(Vec3::new(8.0, 0.0, 18.0), 2.0, StationKind::Safe).with_label("Сейф"),
        );

        // Ворота на Полигон (G5: испытательный полигон у западной стены).
        scene.add(pad(-20.0, 0.0, 5.0, Vec4::new(0.25, 0.45, 0.65, 1.0)));
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(-20.0, 0.0, 0.0),
                2.5,
                LOCATION_POLYGON,
                Vec3::new(0.0, 0.0, 16.0),
            )
            .with_yaw(std::f32::consts::PI)
            .with_label("Полигон"),
        );

        scene
    }

    /// Кордон (M4d, G5): открытая локация с дорогой, КПП у ворот Базы, переходам
    /// на Свалку и в Подземку, радиомачтой эвакуации, пузырями и аномалиями.
    pub fn kordon() -> Self {
        let mut scene = Self::demo_zone();
        scene.name = "kordon".to_string();

        let ground = Vec4::new(0.62, 0.64, 0.60, 1.0);
        let wall = Vec4::new(0.45, 0.46, 0.45, 1.0);
        let metal = Vec4::new(0.55, 0.52, 0.50, 1.0);

        // Главная дорога Кордона: асфальтовое полотно с севера (от ворот Базы) на юг.
        scene.add(textured_cube(
            Vec3::new(0.0, -0.47, 0.0),
            Vec3::new(6.0, 0.5, 54.0),
            ground,
            TEXTURE_ASPHALT,
            0.25,
        ));

        // Северный КПП перед воротами Базы (z = 24..26):
        scene.add(textured_cube(
            Vec3::new(-4.5, 1.2, 24.0),
            Vec3::new(1.2, 2.4, 3.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        scene.add(textured_cube(
            Vec3::new(4.5, 1.2, 24.0),
            Vec3::new(1.2, 2.4, 3.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        // Фонари на КПП:
        scene.add(lamp_object(
            Vec3::new(-4.5, 2.6, 24.0),
            Vec3::new(1.2, 1.0, 0.6),
            1.6,
        ));
        scene.add(lamp_object(
            Vec3::new(4.5, 2.6, 24.0),
            Vec3::new(1.2, 1.0, 0.6),
            1.6,
        ));

        // Западная дорога к переходу на Свалку (x = -26, z = 0):
        scene.add(textured_cube(
            Vec3::new(-13.0, -0.48, 0.0),
            Vec3::new(20.0, 0.5, 5.0),
            ground,
            TEXTURE_DIRT,
            0.3,
        ));
        // Разрушенное укрытие / блокпост у перехода на Свалку:
        scene.add(textured_cube(
            Vec3::new(-24.0, 1.5, 4.0),
            Vec3::new(2.0, 3.0, 5.0),
            wall,
            TEXTURE_CONCRETE,
            0.4,
        ));
        scene.add(lamp_object(
            Vec3::new(-24.0, 3.2, 4.0),
            Vec3::new(1.1, 0.8, 0.4),
            1.5,
        ));

        // Восточный вход в Подземку (x = 26, z = 0):
        scene.add(textured_cube(
            Vec3::new(13.0, -0.48, 0.0),
            Vec3::new(20.0, 0.5, 5.0),
            ground,
            TEXTURE_CONCRETE,
            0.3,
        ));
        // Бункерный портал вокруг перехода в Подземку:
        scene.add(textured_cube(
            Vec3::new(26.0, 2.0, -4.0),
            Vec3::new(3.0, 4.0, 2.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        scene.add(textured_cube(
            Vec3::new(26.0, 2.0, 4.0),
            Vec3::new(3.0, 4.0, 2.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        scene.add(textured_cube(
            Vec3::new(26.0, 4.2, 0.0),
            Vec3::new(3.0, 1.0, 6.0),
            metal,
            TEXTURE_METAL,
            0.4,
        ));
        // Янтарный аварийный фонарь над порталом Подземки:
        scene.add(lamp_object(
            Vec3::new(24.2, 3.2, 0.0),
            Vec3::new(1.3, 0.7, 0.2),
            1.8,
        ));

        // Маяк аварийной экстракции (у границы круга эвакуации (-24, -24)):
        // Высокая металлическая мачта с ярким пульсирующим красным огнём:
        scene.add(textured_cube(
            Vec3::new(-26.5, 2.5, -26.5),
            Vec3::new(0.35, 5.0, 0.35),
            metal,
            TEXTURE_METAL,
            0.2,
        ));
        scene.add(lamp_object(
            Vec3::new(-26.5, 5.2, -26.5),
            Vec3::new(1.5, 0.25, 0.25),
            2.5,
        ));

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

        // Аварийная эвакуация (M4d-2): дальний угол карты, сейв 75 % добычи
        // (остаток падает в круге). Ворота базы — бесплатный выход на 100 %.
        scene.add_extraction(
            SceneExtraction::new(Vec3::new(-24.0, 0.0, -24.0), 3.0, 0.75)
                .with_label("Аварийная эвакуация"),
        );

        // Стая слепых псов у восточных холмов (в секторе перехода в Подземку):
        scene.add_mutant_spawn(SceneMutantSpawn::new(Vec3::new(16.0, 0.0, -12.0), 0));
        scene.add_mutant_spawn(SceneMutantSpawn::new(Vec3::new(20.0, 0.0, -10.0), 0));

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

        // Пол и периметр — текстуры (M4g): земля Свалки, бетон стен.
        let ground = Vec4::new(0.78, 0.72, 0.62, 1.0);
        let wall = Vec4::new(0.34, 0.34, 0.32, 1.0);
        let pile = Vec4::new(0.90, 0.88, 0.85, 1.0);

        scene.add(textured_cube(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(96.0, 0.5, 96.0),
            ground,
            TEXTURE_DIRT,
            0.25,
        ));
        for (position, scale) in [
            (Vec3::new(0.0, 2.5, -48.0), Vec3::new(97.0, 3.0, 1.0)),
            (Vec3::new(0.0, 2.5, 48.0), Vec3::new(97.0, 3.0, 1.0)),
            (Vec3::new(-48.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 97.0)),
            (Vec3::new(48.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 97.0)),
        ] {
            scene.add(textured_cube(position, scale, wall, TEXTURE_CONCRETE, 0.5));
        }

        // Башни и завалы: на вершинах ключевых башен — сигнальные маяки,
        // служащие визуальными ориентирами в густом тумане Свалки.
        let piles = [
            (Vec3::new(-10.0, 4.0, -12.0), Vec3::new(6.0, 8.0, 6.0)),
            (Vec3::new(12.0, 3.0, 14.0), Vec3::new(5.0, 6.0, 5.0)),
            (Vec3::new(-24.0, 2.5, 20.0), Vec3::new(4.0, 5.0, 4.0)),
            (Vec3::new(26.0, 2.0, -26.0), Vec3::new(6.0, 4.0, 6.0)),
            (Vec3::new(-34.0, 1.5, -30.0), Vec3::new(5.0, 3.0, 7.0)),
        ];
        for (position, scale) in piles {
            scene.add(textured_cube(position, scale, pile, TEXTURE_METAL, 0.35));
        }

        // Габаритные огни-маяки на вершинах главных свальных башен:
        scene.add(lamp_object(
            Vec3::new(-10.0, 8.2, -12.0),
            Vec3::new(1.3, 0.7, 0.1),
            2.0,
        ));
        scene.add(lamp_object(
            Vec3::new(12.0, 6.2, 14.0),
            Vec3::new(1.4, 0.25, 0.1),
            2.0,
        ));
        scene.add(lamp_object(
            Vec3::new(26.0, 4.2, -26.0),
            Vec3::new(1.3, 0.7, 0.1),
            2.0,
        ));

        let debris = [
            (Vec3::new(4.0, 0.75, -20.0), Vec3::new(3.0, 1.5, 2.0)),
            (Vec3::new(-18.0, 1.0, 2.0), Vec3::new(2.0, 2.0, 3.0)),
            (Vec3::new(20.0, 0.6, 30.0), Vec3::new(4.0, 1.2, 2.0)),
            (Vec3::new(-6.0, 0.9, 26.0), Vec3::new(2.5, 1.8, 2.5)),
            (Vec3::new(30.0, 1.2, 8.0), Vec3::new(3.0, 2.4, 3.0)),
            (Vec3::new(-28.0, 0.7, -8.0), Vec3::new(2.0, 1.4, 4.0)),
            // Лабиринт контейнеров и обломков в центрально-северной зоне:
            (Vec3::new(-2.0, 1.2, 12.0), Vec3::new(5.0, 2.4, 2.4)),
            (Vec3::new(8.0, 1.2, -8.0), Vec3::new(2.4, 2.4, 5.0)),
            (Vec3::new(-14.0, 1.0, -22.0), Vec3::new(4.0, 2.0, 2.0)),
        ];
        for (position, scale) in debris {
            scene.add(textured_cube(position, scale, pile, TEXTURE_METAL, 0.35));
        }

        // Заборы-стенки, делящие карту.
        for (position, scale) in [
            (Vec3::new(-2.0, 1.5, -30.0), Vec3::new(20.0, 3.0, 1.0)),
            (Vec3::new(-20.0, 1.5, 8.0), Vec3::new(1.0, 3.0, 18.0)),
        ] {
            scene.add(textured_cube(position, scale, wall, TEXTURE_CONCRETE, 0.5));
        }

        // Укрепление и фонарь у перехода на Кордон (x = 43, z = 0):
        scene.add(textured_cube(
            Vec3::new(39.0, 1.0, -3.5),
            Vec3::new(2.0, 2.0, 4.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        scene.add(textured_cube(
            Vec3::new(39.0, 1.0, 3.5),
            Vec3::new(2.0, 2.0, 4.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        scene.add(lamp_object(
            Vec3::new(39.0, 2.5, 0.0),
            Vec3::new(1.1, 0.9, 0.5),
            1.6,
        ));

        // Мачта маяка аварийной эвакуации на Свалке (у границы круга эвакуации (-30, -38)):
        // Высокий зелёный спасательный маяк, заметный сквозь рыжую пыльную мглу:
        scene.add(textured_cube(
            Vec3::new(-32.5, 3.0, -40.0),
            Vec3::new(0.4, 6.0, 0.4),
            pile,
            TEXTURE_METAL,
            0.2,
        ));
        scene.add(lamp_object(
            Vec3::new(-32.5, 6.2, -40.0),
            Vec3::new(0.25, 1.6, 0.35),
            2.5,
        ));

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

        // Аварийная эвакуация (M4d-2): юго-западный угол за свальными башнями.
        scene.add_extraction(
            SceneExtraction::new(Vec3::new(-30.0, 0.0, -38.0), 3.0, 0.75)
                .with_label("Аварийная эвакуация"),
        );

        // Стая слепых псов среди ржавых завалов:
        scene.add_mutant_spawn(SceneMutantSpawn::new(Vec3::new(-15.0, 0.0, 16.0), 0));
        scene.add_mutant_spawn(SceneMutantSpawn::new(Vec3::new(10.0, 0.0, -20.0), 0));
        scene.add_mutant_spawn(SceneMutantSpawn::new(Vec3::new(20.0, 0.0, 18.0), 0));

        scene
    }

    /// Подземка (M4d): тесные коридоры 48×48, одна аномалия, лут.
    /// Переход на запад — к Кордону.
    pub fn podzemka() -> Self {
        let mut scene = Scene::new("podzemka");
        scene.spawn = Vec3::new(-15.0, 0.0, 0.0);
        scene.spawn_yaw = std::f32::consts::FRAC_PI_2;
        // Подземка: почти нет солнца, холодный ambient, тёмная мгла коридоров.
        // Читаемость держат фактуры бетона и лампы (emissive) на стенах.
        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(0.0, 1.0, 0.0),
            sun_color: Vec3::new(0.70, 0.78, 0.95),
            sun_intensity: 0.24,
            ambient_color: Vec3::new(0.22, 0.24, 0.29),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.04, 0.05, 0.07),
            fog_density: 0.035,
            sky_top: Vec3::new(0.004, 0.005, 0.009),
            sky_horizon: Vec3::new(0.02, 0.025, 0.04),
        };

        // Пол, периметр и коридоры — бетон (M4g): фактура читается даже при
        // слабом свете подземки.
        let ground = Vec4::new(0.42, 0.42, 0.41, 1.0);
        let wall = Vec4::new(0.34, 0.34, 0.33, 1.0);
        let inner = Vec4::new(0.48, 0.48, 0.47, 1.0);

        scene.add(textured_cube(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(48.0, 0.5, 48.0),
            ground,
            TEXTURE_CONCRETE,
            0.4,
        ));
        for (position, scale) in [
            (Vec3::new(0.0, 2.5, -24.0), Vec3::new(49.0, 3.0, 1.0)),
            (Vec3::new(0.0, 2.5, 24.0), Vec3::new(49.0, 3.0, 1.0)),
            (Vec3::new(-24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0)),
            (Vec3::new(24.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 49.0)),
        ] {
            scene.add(textured_cube(position, scale, wall, TEXTURE_CONCRETE, 0.5));
        }

        // Коридоры: длинные стены с проходами.
        for (position, scale) in [
            (Vec3::new(-8.0, 2.0, -9.0), Vec3::new(1.0, 4.0, 18.0)),
            (Vec3::new(8.0, 2.0, 9.0), Vec3::new(1.0, 4.0, 18.0)),
            (Vec3::new(0.0, 2.0, -16.0), Vec3::new(14.0, 4.0, 1.0)),
            (Vec3::new(0.0, 2.0, 16.0), Vec3::new(14.0, 4.0, 1.0)),
        ] {
            scene.add(textured_cube(position, scale, inner, TEXTURE_CONCRETE, 0.5));
        }

        // Опорные пилоны центрального зала и машинного отсека:
        for position in [
            Vec3::new(-4.0, 2.0, 0.0),
            Vec3::new(4.0, 2.0, 0.0),
            Vec3::new(-14.0, 2.0, 8.0),
            Vec3::new(14.0, 2.0, -8.0),
        ] {
            scene.add(textured_cube(
                position,
                Vec3::new(1.2, 4.0, 1.2),
                inner,
                TEXTURE_CONCRETE,
                0.5,
            ));
        }

        // Техническое оборудование: распределительные шкафы и вентиляция:
        scene.add(textured_cube(
            Vec3::new(14.0, 1.5, -4.0),
            Vec3::new(2.0, 3.0, 3.6),
            Vec4::new(0.3, 0.32, 0.35, 1.0),
            TEXTURE_METAL,
            0.4,
        ));
        scene.add(textured_cube(
            Vec3::new(0.0, 3.7, 0.0),
            Vec3::new(1.2, 0.6, 26.0),
            Vec4::new(0.4, 0.4, 0.42, 1.0),
            TEXTURE_METAL,
            0.3,
        ));

        // Лампы (M4g, G5): светящиеся плафоны и аварийные маркеры.
        for (position, scale) in [
            (Vec3::new(-7.35, 2.6, -9.0), Vec3::new(0.3, 0.5, 0.3)),
            (Vec3::new(7.35, 2.6, 9.0), Vec3::new(0.3, 0.5, 0.3)),
            (Vec3::new(-3.0, 2.6, -15.35), Vec3::new(0.3, 0.3, 0.3)),
            (Vec3::new(3.0, 2.6, 15.35), Vec3::new(0.3, 0.3, 0.3)),
        ] {
            scene.add(
                SceneObject::new(position, scale, Vec4::new(0.10, 0.09, 0.08, 1.0))
                    .with_asset(BUILTIN_CUBE)
                    .with_emissive(Vec3::new(1.1, 0.85, 0.45)),
            );
        }

        // Аварийные маркеры и указатели:
        // 1. Зелёный фонарь «Выход» над переходом на Кордон:
        scene.add(lamp_object(
            Vec3::new(-19.5, 2.6, 0.0),
            Vec3::new(0.25, 1.5, 0.35),
            2.2,
        ));
        // 2. Тревожный красный фонарь в опасном секторе аномалии:
        scene.add(lamp_object(
            Vec3::new(16.0, 2.8, 12.0),
            Vec3::new(1.5, 0.2, 0.1),
            2.0,
        ));
        // 3. Янтарный маяк эвакуации в техническом тупике:
        scene.add(lamp_object(
            Vec3::new(18.0, 2.6, -18.0),
            Vec3::new(1.4, 0.9, 0.2),
            2.2,
        ));

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

        // Аварийная эвакуация (M4d-2): юго-восточный угол коридоров.
        scene.add_extraction(
            SceneExtraction::new(Vec3::new(18.0, 0.0, -18.0), 2.5, 0.75)
                .with_label("Аварийная эвакуация"),
        );

        scene
    }

    /// Полигон (G5): тестовая локация 64×64 «все системы в одном месте».
    /// Включает стрельбище с мишенями на 10/15/20 м, аномальный сектор
    /// с Жгучей и Гравитационной аномалиями, тестовые станции базы,
    /// пространственный пузырь, круг эвакуации (90%) и ящики со всеми предметами.
    pub fn polygon() -> Self {
        let mut scene = Scene::new("polygon");
        scene.spawn = Vec3::new(0.0, 0.0, 20.0);
        scene.spawn_yaw = std::f32::consts::PI;

        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(-0.4, 0.7, 0.4),
            sun_color: Vec3::new(1.0, 0.96, 0.90),
            sun_intensity: 1.05,
            ambient_color: Vec3::new(0.26, 0.28, 0.32),
            ambient_intensity: 1.0,
            fog_color: Vec3::new(0.65, 0.70, 0.78),
            fog_density: 0.005,
            sky_top: Vec3::new(0.14, 0.28, 0.58),
            sky_horizon: Vec3::new(0.62, 0.72, 0.82),
        };

        let ground = Vec4::new(0.62, 0.64, 0.60, 1.0);
        let wall = Vec4::new(0.45, 0.46, 0.45, 1.0);
        let target_red = Vec4::new(0.85, 0.20, 0.15, 1.0);

        // Периметр полигона 64×64:
        scene.add(textured_cube(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(64.0, 0.5, 64.0),
            ground,
            TEXTURE_CONCRETE,
            0.25,
        ));
        for (position, scale) in [
            (Vec3::new(0.0, 2.5, -32.0), Vec3::new(65.0, 3.0, 1.0)),
            (Vec3::new(0.0, 2.5, 32.0), Vec3::new(65.0, 3.0, 1.0)),
            (Vec3::new(-32.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 65.0)),
            (Vec3::new(32.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 65.0)),
        ] {
            scene.add(textured_cube(position, scale, wall, TEXTURE_CONCRETE, 0.5));
        }

        // 1. СТРЕЛЬБИЩЕ (Северный сектор: z < 0):
        // Огневой рубеж:
        scene.add(textured_cube(
            Vec3::new(0.0, -0.47, -10.0),
            Vec3::new(20.0, 0.5, 4.0),
            Vec4::new(0.35, 0.35, 0.36, 1.0),
            TEXTURE_ASPHALT,
            0.3,
        ));
        scene.add(textured_cube(
            Vec3::new(0.0, 0.5, -8.0),
            Vec3::new(18.0, 1.0, 0.8),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));

        // Пулеулавливатель за мишенями:
        scene.add(textured_cube(
            Vec3::new(0.0, 2.0, -30.0),
            Vec3::new(24.0, 4.0, 2.0),
            Vec4::new(0.5, 0.45, 0.35, 1.0),
            TEXTURE_DIRT,
            0.5,
        ));

        // Мишени на рубежах 10м (z = -18), 15м (z = -23), 20м (z = -28):
        // Мишень 10м (бокс ростовой):
        scene.add(
            SceneObject::new(
                Vec3::new(-5.0, 1.0, -18.0),
                Vec3::new(0.5, 1.8, 0.3),
                target_red,
            )
            .with_collider(ColliderShape::Box),
        );
        // Мишень 10м (сфера в голове):
        scene.add(
            SceneObject::new(Vec3::new(-5.0, 2.1, -18.0), Vec3::splat(0.35), target_red)
                .with_asset(BUILTIN_SPHERE)
                .with_collider(ColliderShape::Sphere),
        );

        // Мишень 15м (цилиндр):
        scene.add(
            SceneObject::new(
                Vec3::new(0.0, 1.0, -23.0),
                Vec3::new(0.6, 1.8, 0.6),
                target_red,
            )
            .with_collider(ColliderShape::Cylinder),
        );

        // Мишень 20м (дальняя стойка):
        scene.add(
            SceneObject::new(
                Vec3::new(5.0, 1.0, -28.0),
                Vec3::new(0.5, 1.8, 0.3),
                target_red,
            )
            .with_collider(ColliderShape::Box),
        );

        // Освещение стрельбища:
        scene.add(lamp_object(
            Vec3::new(-8.0, 3.5, -8.0),
            Vec3::new(1.2, 1.0, 0.7),
            1.8,
        ));
        scene.add(lamp_object(
            Vec3::new(8.0, 3.5, -8.0),
            Vec3::new(1.2, 1.0, 0.7),
            1.8,
        ));

        // 2. АНОМАЛЬНЫЙ СЕКТОР (Восточный сектор: x > 10, z > -5):
        // Ограждение аномального загона:
        scene.add(textured_cube(
            Vec3::new(10.0, 1.0, 5.0),
            Vec3::new(1.0, 2.0, 22.0),
            wall,
            TEXTURE_CONCRETE,
            0.5,
        ));
        // Жгучая аномалия с Жар-камнем:
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(20.0, 0.0, 0.0), 3.0, AnomalyKind::Heat)
                .with_artifact("artifact_heat"),
        );
        // Гравитационная аномалия с Тягуном:
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(20.0, 0.0, 10.0), 3.5, AnomalyKind::Gravity)
                .with_artifact("artifact_gravity"),
        );
        // Предупредительный фонарь над аномалиями:
        scene.add(lamp_object(
            Vec3::new(10.0, 2.6, 5.0),
            Vec3::new(1.4, 0.3, 0.1),
            2.0,
        ));

        // 3. ПЕРЕДВИЖЕНИЕ И ЭВАКУАЦИЯ (Западный сектор: x < -10):
        // Пространственный пузырь:
        scene.add_bubble(SceneBubble::new(Vec3::new(-20.0, 0.0, 4.0), 1.2));
        scene.add_bubble_arrival(Vec3::new(-14.0, 0.0, 4.0));
        // Круг эвакуации (90% сейв для тестирования экстракции):
        scene.add_extraction(
            SceneExtraction::new(Vec3::new(-20.0, 0.0, -4.0), 3.0, 0.90)
                .with_label("Тестовая экстракция (90%)"),
        );
        scene.add(lamp_object(
            Vec3::new(-20.0, 3.5, -4.0),
            Vec3::new(0.2, 1.5, 0.4),
            2.2,
        ));

        // 4. ТЕСТОВЫЙ ХАБ И СТАНЦИИ (Южный сектор: z > 10):
        scene.add_station(
            SceneStation::new(Vec3::new(-6.0, 0.0, 16.0), 2.0, StationKind::Trader)
                .with_label("Тест: Торговец")
                .with_offers(vec![
                    SceneOffer {
                        item: "medkit".into(),
                        count: 2,
                    },
                    SceneOffer {
                        item: "ammo_545".into(),
                        count: 60,
                    },
                    SceneOffer {
                        item: "bolt".into(),
                        count: 30,
                    },
                ]),
        );
        scene.add_station(
            SceneStation::new(Vec3::new(6.0, 0.0, 16.0), 2.0, StationKind::Medpost)
                .with_label("Тест: Медпункт"),
        );
        scene.add_station(
            SceneStation::new(Vec3::new(-6.0, 0.0, 24.0), 2.0, StationKind::Workbench)
                .with_label("Тест: Верстак"),
        );
        scene.add_station(
            SceneStation::new(Vec3::new(6.0, 0.0, 24.0), 2.0, StationKind::Safe)
                .with_label("Тест: Сейф"),
        );

        // Ящики со ВСЕМИ предметами игры:
        let crates = [
            crate_object(
                -2.0,
                14.0,
                &[
                    ("ammo_545", 120),
                    ("bolt", 40),
                    ("medkit", 3),
                    ("bandage", 4),
                ],
            ),
            crate_object(
                2.0,
                14.0,
                &[
                    ("canned_food", 4),
                    ("vodka", 3),
                    ("artifact_heat", 1),
                    ("artifact_gravity", 1),
                ],
            ),
        ];
        for object in crates {
            scene.add(object);
        }

        // Переход обратно на Базу:
        scene.add(pad(0.0, 28.0, 5.0, Vec4::new(0.20, 0.55, 0.25, 1.0)));
        scene.add_transition(
            SceneTransition::new(
                Vec3::new(0.0, 0.0, 28.0),
                2.5,
                LOCATION_BASE,
                Vec3::new(-16.0, 0.0, 0.0),
            )
            .with_yaw(0.0)
            .with_label("База"),
        );

        // Испытательный загон для мутантов (отработка боя и хитмаркеров):
        scene.add_mutant_spawn(SceneMutantSpawn::new(Vec3::new(20.0, 0.0, -20.0), 0));

        scene
    }

    /// Встроенный мир цикла M4d: сцены локаций `LocationRegistry::builtin()`.
    pub fn builtin_world() -> Vec<(LocationId, Scene)> {
        vec![
            (LOCATION_BASE, Scene::base()),
            (LOCATION_KORDON, Scene::kordon()),
            (LOCATION_SVALKA, Scene::svalka()),
            (LOCATION_PODZEMKA, Scene::podzemka()),
            (LOCATION_POLYGON, Scene::polygon()),
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
