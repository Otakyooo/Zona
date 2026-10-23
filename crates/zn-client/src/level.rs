use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zn_content::ItemCatalog;
use zn_render::{DrawList, InstanceRaw, MaterialId, MeshId};
use zn_scene::{LocationId, LocationRegistry, Scene, SceneObject};

/// Корень контента по умолчанию (относительно cwd).
pub const DEFAULT_CONTENT_ROOT: &str = "game/content";
/// Имя уровня по умолчанию (offline-режим).
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

/// Мир локаций для клиента (M4d): реестр `<content>/locations.ron` и сцены
/// каждой локации по id. Битые/отсутствующие файлы заменяются встроенным
/// миром — той же логикой, что у сервера, чтобы хэш мира сошёлся.
pub fn load_world(content_root: &Path) -> (LocationRegistry, HashMap<LocationId, Scene>) {
    let registry_path = content_root.join("locations.ron");
    let registry = match LocationRegistry::load_ron(&registry_path) {
        Ok(registry) if !registry.locations.is_empty() => {
            log::info!(
                "реестр локаций загружен из {} ({} локаций)",
                registry_path.display(),
                registry.locations.len()
            );
            registry
        }
        Ok(_) => {
            log::warn!(
                "реестр локаций пуст ({}) — встроенный мир",
                registry_path.display()
            );
            LocationRegistry::builtin()
        }
        Err(error) => {
            log::warn!(
                "реестр локаций не прочитан ({}): {error} — встроенный мир",
                registry_path.display()
            );
            LocationRegistry::builtin()
        }
    };

    let builtin: HashMap<LocationId, Scene> = Scene::builtin_world().into_iter().collect();
    let mut scenes = HashMap::with_capacity(registry.locations.len());
    for info in &registry.locations {
        let scene = match registry.scene_path(content_root, info.id) {
            Some(path) => match Scene::load_ron(&path) {
                Ok(scene) => {
                    log::info!(
                        "локация '{}': загружена из {} ({} объектов)",
                        info.name,
                        path.display(),
                        scene.len()
                    );
                    scene
                }
                Err(error) => {
                    log::warn!(
                        "локация '{}': {} не прочитана ({error}) — встроенная сцена",
                        info.name,
                        path.display()
                    );
                    builtin_scene(&builtin, info.id)
                }
            },
            None => builtin_scene(&builtin, info.id),
        };
        scenes.insert(info.id, scene);
    }
    (registry, scenes)
}

/// Встроенная сцена локации по id; нет такой — пустая сцена-заглушка.
fn builtin_scene(builtin: &HashMap<LocationId, Scene>, id: LocationId) -> Scene {
    builtin
        .get(&id)
        .cloned()
        .unwrap_or_else(|| Scene::new(format!("location_{id}")))
}

/// Ищет каталог предметов `<content>/items/items.ron`; при ошибке — пустой
/// каталог с предупреждением (предметы отключены, сервер отвергнет handshake).
pub fn load_catalog(content_root: &Path) -> ItemCatalog {
    let path = content_root.join("items").join("items.ron");
    match ItemCatalog::load_ron(&path) {
        Ok(catalog) => {
            log::info!(
                "каталог предметов загружен из {} ({} позиций)",
                path.display(),
                catalog.len()
            );
            catalog
        }
        Err(error) => {
            log::warn!(
                "каталог предметов не прочитан ({}): {error} — предметы отключены",
                path.display()
            );
            ItemCatalog::default()
        }
    }
}

/// Разрешает объект сцены в пары (меш, материал) для draw list: объект
/// мультиматериального glTF даёт по батчу на примитив. Вызывающий код
/// отвечает за fallback на builtin-куб/default-материал.
pub fn build_draw_list(
    scene: &Scene,
    resolve: impl Fn(&SceneObject) -> Vec<(MeshId, MaterialId)>,
) -> DrawList {
    let mut list = DrawList::new();
    for object in &scene.objects {
        let instance = InstanceRaw {
            model: object.model_matrix().to_cols_array(),
            color: object.color.into(),
        };
        for (mesh, material) in resolve(object) {
            list.push(mesh, material, instance);
        }
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
    fn load_world_falls_back_to_builtin_scenes() {
        let root = std::env::temp_dir().join("zn-client-no-such-world-root");
        let (registry, scenes) = load_world(&root);
        assert_eq!(registry.locations.len(), 5);
        assert_eq!(scenes.len(), 5);
        assert_eq!(scenes[&zn_scene::LOCATION_BASE].name, "base");
        assert!(!scenes[&zn_scene::LOCATION_SVALKA].is_empty());
    }

    #[test]
    fn load_world_prefers_scenes_from_disk() {
        let root = std::env::temp_dir().join("zn-client-world-from-disk");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("levels")).unwrap();

        // Реестр — встроенный, но сцена Базы на диске помечена позицией спавна.
        LocationRegistry::builtin()
            .save_ron(root.join("locations.ron"))
            .unwrap();
        let mut base = Scene::new("base_from_disk");
        base.spawn = glam::Vec3::new(1.0, 0.0, 2.0);
        base.add(
            SceneObject::new(glam::Vec3::ZERO, glam::Vec3::ONE, glam::Vec4::ONE)
                .with_asset(zn_scene::BUILTIN_CUBE),
        );
        base.save_ron(root.join("levels").join("base.scene.ron"))
            .unwrap();

        let (_, scenes) = load_world(&root);
        assert_eq!(scenes[&zn_scene::LOCATION_BASE].name, "base_from_disk");
        assert_eq!(
            scenes[&zn_scene::LOCATION_BASE].spawn,
            glam::Vec3::new(1.0, 0.0, 2.0)
        );
        // Остальные локации — встроенный fallback.
        assert!(!scenes[&zn_scene::LOCATION_KORDON].is_empty());
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
                vec![(1, 1)]
            } else {
                vec![(2, 2)]
            }
        });
        assert_eq!(list.instance_count(), 4);
        assert_eq!(list.batches.len(), 2);
    }

    #[test]
    fn build_draw_list_emits_all_pairs_of_multi_material_object() {
        let mut scene = Scene::new("multi");
        scene.add(object_at(0.0, Some("assets/multi.gltf")));

        let list = build_draw_list(&scene, |_| vec![(1, 10), (2, 11), (1, 11)]);
        assert_eq!(list.batches.len(), 3);
        assert_eq!(list.instance_count(), 3);
    }

    #[test]
    fn build_draw_list_fallback_for_unknown_assets() {
        let mut scene = Scene::new("fallback");
        scene.add(object_at(0.0, Some("assets/missing.gltf")));

        let list = build_draw_list(&scene, |_| vec![(99, 99)]);
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

        let list = build_draw_list(&scene, |_| vec![(1, 1)]);
        let instance = &list.batches[0].instances[0];
        assert_eq!(instance.color, [0.5, 0.25, 0.75, 1.0]);
        let model = glam::Mat4::from_cols_array(&instance.model);
        let point = model.project_point3(glam::Vec3::ZERO);
        assert!((point - object.position).length() < 1e-5);
    }
}
