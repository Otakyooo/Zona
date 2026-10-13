use std::path::Path;

use glam::{Vec3, Vec4};
use serde::{Deserialize, Serialize};

pub const SCENE_EXTENSION: &str = "scene.ron";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneObject {
    pub position: Vec3,
    pub scale: Vec3,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default = "default_color")]
    pub color: Vec4,
    #[serde(default = "default_true")]
    pub solid: bool,
}

fn default_color() -> Vec4 {
    Vec4::ONE
}

fn default_true() -> bool {
    true
}

impl SceneObject {
    pub fn new(position: Vec3, scale: Vec3, color: Vec4) -> Self {
        Self { position, scale, yaw: 0.0, color, solid: true }
    }

    pub fn with_yaw(mut self, yaw: f32) -> Self {
        self.yaw = yaw;
        self
    }

    pub fn invulnerable(mut self) -> Self {
        self.solid = false;
        self
    }

    pub fn model_matrix(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(
            self.scale,
            glam::Quat::from_rotation_y(self.yaw),
            self.position,
        )
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
        Self { name: name.into(), spawn: Vec3::new(0.0, 2.0, 0.0), spawn_yaw: 0.0, objects: Vec::new() }
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
            scene.add(SceneObject::new(position, scale, color));
        };

        push(Vec3::new(0.0, -0.5, 0.0), Vec3::new(60.0, 0.5, 60.0), Vec4::new(0.18, 0.19, 0.16, 1.0));

        let wall_color = Vec4::new(0.10, 0.10, 0.11, 1.0);
        push(Vec3::new(0.0, 2.5, -30.0), Vec3::new(31.0, 3.0, 1.0), wall_color);
        push(Vec3::new(0.0, 2.5, 30.0), Vec3::new(31.0, 3.0, 1.0), wall_color);
        push(Vec3::new(-30.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 31.0), wall_color);
        push(Vec3::new(30.0, 2.5, 0.0), Vec3::new(1.0, 3.0, 31.0), wall_color);

        let pillar = Vec4::new(0.24, 0.25, 0.22, 1.0);
        for x in -20..=20 {
            for z in -20..=20 {
                if x % 6 == 0 && z % 6 == 0 {
                    push(Vec3::new(x as f32, 1.75, z as f32), Vec3::new(0.5, 3.5, 0.5), pillar);
                }
            }
        }

        push(Vec3::new(0.0, 0.5, 0.0), Vec3::new(4.0, 1.0, 4.0), Vec4::new(0.35, 0.30, 0.18, 1.0));
        push(Vec3::new(-6.0, 1.0, -6.0), Vec3::new(2.0, 2.0, 2.0), Vec4::new(0.30, 0.30, 0.34, 1.0));
        push(Vec3::new(7.0, 1.0, -8.0), Vec3::new(1.5, 3.0, 1.5), Vec4::new(0.30, 0.30, 0.34, 1.0));
        push(Vec3::new(10.0, 0.75, 4.0), Vec3::new(3.0, 1.5, 3.0), Vec4::new(0.30, 0.30, 0.34, 1.0));

        let artifact = Vec4::new(0.95, 0.72, 0.15, 1.0);
        let artifacts = [
            (Vec3::new(0.0, 1.6, 0.0), Vec3::new(0.35, 0.35, 0.35)),
            (Vec3::new(-6.0, 2.75, -6.0), Vec3::new(0.35, 0.35, 0.35)),
            (Vec3::new(7.0, 3.25, -8.0), Vec3::new(0.35, 0.35, 0.35)),
            (Vec3::new(10.0, 2.25, 4.0), Vec3::new(0.35, 0.35, 0.35)),
        ];
        for (position, scale) in artifacts {
            scene.add(SceneObject::new(position, scale, artifact).invulnerable());
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
