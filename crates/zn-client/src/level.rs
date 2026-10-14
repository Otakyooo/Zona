use std::path::{Path, PathBuf};

use zn_render::{DrawList, InstanceRaw, MaterialId, MeshId};
use zn_scene::{Scene, SceneObject};

/// Корень контента по умолчанию (относительно cwd).
pub const DEFAULT_CONTENT_ROOT: &str = "game/content";
/// Имя уровня по умолчанию.
pub const DEFAULT_LEVEL: &str = "demo_zone";

/// Ищет файл уровня `<content>/<levels>/<name>.scene.ron`; если нет — fallback
/// на `Scene::demo_zone()` с предупреждением.
pub fn load_level(content_root: &Path, level: &str) -> (Scene, bool) {
    let path = level_path(content_root, level);
    match Scene::load_ron(&path) {
        Ok(scene) => {
            log::info!(
                "уровень '{level}' загружен из {} ({} объектов)",
                path.display(),
                scene.len()
            );
            (scene, true)
        }
        Err(error) => {
            log::warn!(
                "уровень '{level}' не прочитан ({}): {error} — fallback на demo_zone()",
                path.display()
            );
            (Scene::demo_zone(), false)
        }
    }
}

pub fn level_path(content_root: &Path, level: &str) -> PathBuf {
    content_root
        .join("levels")
        .join(format!("{level}.scene.ron"))
}

/// Разрешает ключ ассета объекта в пару (меш, материал) для draw list.
/// Вызывающий код отвечает за fallback на builtin-куб/default-материал.
pub fn build_draw_list(
    scene: &Scene,
    resolve: impl Fn(&SceneObject) -> (MeshId, MaterialId),
) -> DrawList {
    let mut list = DrawList::new();
    for object in &scene.objects {
        let (mesh, material) = resolve(object);
        list.push(
            mesh,
            material,
            InstanceRaw {
                model: object.model_matrix().to_cols_array(),
                color: object.color.into(),
            },
        );
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use zn_scene::{ColliderShape, SceneObject, BUILTIN_CUBE};

    fn object_at(x: f32, asset: Option<&str>) -> SceneObject {
        SceneObject::new(
            glam::Vec3::new(x, 0.0, 0.0),
            glam::Vec3::ONE,
            glam::Vec4::ONE,
        )
        .with_asset(asset.unwrap_or(BUILTIN_CUBE))
    }

    #[test]
    fn load_level_falls_back_to_demo_zone() {
        let root = std::env::temp_dir().join("zn-client-no-such-content-root");
        let (scene, loaded) = load_level(&root, "demo_zone");
        assert!(!loaded, "файла уровня нет — должен быть fallback");
        assert_eq!(scene.name, "demo_zone");
        assert!(!scene.is_empty());
    }

    #[test]
    fn load_level_reads_scene_from_disk() {
        let root = std::env::temp_dir().join("zn-client-level-roundtrip");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("levels")).unwrap();

        let mut scene = Scene::new("test_level");
        scene.spawn = glam::Vec3::new(1.0, 2.0, 3.0);
        scene.add(
            SceneObject::new(glam::Vec3::ZERO, glam::Vec3::splat(2.0), glam::Vec4::ONE)
                .with_asset("assets/box.gltf")
                .with_material("Metal")
                .with_collider(ColliderShape::Sphere)
                .with_yaw(0.5),
        );
        scene.save_ron(level_path(&root, "test_level")).unwrap();

        let (loaded, ok) = load_level(&root, "test_level");
        assert!(ok);
        assert_eq!(loaded, scene);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn build_draw_list_groups_by_asset() {
        let mut scene = Scene::new("mix");
        scene.add(object_at(0.0, Some("builtin://cube")));
        scene.add(object_at(1.0, Some("assets/box.gltf")));
        scene.add(object_at(2.0, Some("assets/box.gltf")));
        scene.add(object_at(3.0, Some("builtin://cube")));

        let list = build_draw_list(&scene, |object| {
            if object.asset_key() == "builtin://cube" {
                (1, 1)
            } else {
                (2, 2)
            }
        });
        assert_eq!(list.instance_count(), 4);
        assert_eq!(list.batches.len(), 2);
    }

    #[test]
    fn build_draw_list_fallback_for_unknown_assets() {
        let mut scene = Scene::new("fallback");
        scene.add(object_at(0.0, Some("assets/missing.gltf")));

        let list = build_draw_list(&scene, |_| (99, 99));
        assert_eq!(list.batches.len(), 1);
        assert_eq!(list.batches[0].mesh, 99);
        assert_eq!(list.instance_count(), 1);
    }

    #[test]
    fn build_draw_list_uses_object_transform() {
        let mut scene = Scene::new("transform");
        let object = SceneObject::new(
            glam::Vec3::new(3.0, 4.0, 5.0),
            glam::Vec3::splat(2.0),
            glam::Vec4::new(0.5, 0.25, 0.75, 1.0),
        )
        .with_yaw(1.0);
        scene.add(object.clone());

        let list = build_draw_list(&scene, |_| (1, 1));
        let instance = &list.batches[0].instances[0];
        assert_eq!(instance.color, [0.5, 0.25, 0.75, 1.0]);
        let model = glam::Mat4::from_cols_array(&instance.model);
        let point = model.project_point3(glam::Vec3::ZERO);
        assert!((point - object.position).length() < 1e-5);
    }
}
