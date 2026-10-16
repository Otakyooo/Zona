#[cfg(test)]
mod tests {
    use glam::Vec3;
    use zn_scene::{ColliderShape, Scene, SceneObject, BUILTIN_CUBE};

    #[test]
    fn ron_roundtrip_preserves_scene() {
        let mut scene = Scene::new("test");
        scene.spawn = Vec3::new(1.0, 2.0, 3.0);
        scene.add(SceneObject::new(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::ONE,
            glam::Vec4::ONE,
        ));

        let text = scene.to_ron().unwrap();
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded, scene);
    }

    #[test]
    fn demo_zone_has_solids_and_spawn() {
        let scene = Scene::demo_zone();
        assert!(!scene.is_empty());
        assert!(scene.solids().count() >= 5);
        assert!(scene.spawn.y >= 0.0);
        for object in &scene.objects {
            assert_eq!(object.asset_key(), BUILTIN_CUBE);
        }
    }

    #[test]
    fn model_matrix_applies_scale_rotation_translation() {
        let object = SceneObject::new(Vec3::new(1.0, 2.0, 3.0), Vec3::splat(2.0), glam::Vec4::ONE)
            .with_yaw(std::f32::consts::FRAC_PI_2);
        let point = object
            .model_matrix()
            .project_point3(glam::Vec3::new(0.0, 0.0, 1.0));
        assert!((point.x - 3.0).abs() < 1e-4, "x was {}", point.x);
        assert!((point.y - 2.0).abs() < 1e-4, "y was {}", point.y);
        assert!((point.z - 3.0).abs() < 1e-4, "z was {}", point.z);
    }

    #[test]
    fn legacy_ron_missing_fields_get_defaults() {
        let legacy = r#"
(
    name: "legacy",
    spawn: (0.0, 2.0, 0.0),
    objects: [
        (
            position: (1.0, 2.0, 3.0),
            scale: (2.0, 2.0, 2.0),
            color: (0.5, 0.5, 0.5, 1.0),
        ),
    ],
)
"#;
        let scene = Scene::from_ron(legacy).unwrap();
        let object = &scene.objects[0];
        assert_eq!(object.yaw, 0.0);
        assert_eq!(object.color, glam::Vec4::new(0.5, 0.5, 0.5, 1.0));
        assert_eq!(object.asset, None);
        assert_eq!(object.material, None);
        assert_eq!(object.collider, None);
        assert!(object.solid);
        assert_eq!(object.asset_key(), BUILTIN_CUBE);
        assert_eq!(object.effective_collider(), Some(ColliderShape::Box));
    }

    #[test]
    fn new_fields_roundtrip_through_ron() {
        let mut scene = Scene::new("fields");
        scene.add(
            SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE)
                .with_asset("builtin://sphere")
                .with_material("Metal")
                .with_collider(ColliderShape::Cylinder)
                .with_yaw(0.75),
        );

        let text = scene.to_ron().unwrap();
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded, scene);
        let object = &loaded.objects[0];
        assert_eq!(object.asset.as_deref(), Some("builtin://sphere"));
        assert_eq!(object.material.as_deref(), Some("Metal"));
        assert_eq!(object.collider, Some(ColliderShape::Cylinder));
        assert_eq!(object.effective_collider(), Some(ColliderShape::Cylinder));
    }

    #[test]
    fn effective_collider_respects_solid_flag() {
        let solid_sphere = SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE)
            .with_collider(ColliderShape::Sphere);
        assert_eq!(
            solid_sphere.effective_collider(),
            Some(ColliderShape::Sphere)
        );

        let non_solid = SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE)
            .with_collider(ColliderShape::Sphere)
            .invulnerable();
        assert_eq!(non_solid.effective_collider(), None);

        let default_box = SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE);
        assert_eq!(default_box.effective_collider(), Some(ColliderShape::Box));
    }

    #[test]
    fn collider_shape_ron_roundtrip() {
        for shape in [
            ColliderShape::Box,
            ColliderShape::Cylinder,
            ColliderShape::Sphere,
        ] {
            let text = ron::to_string(&shape).unwrap();
            let loaded: ColliderShape = ron::from_str(&text).unwrap();
            assert_eq!(loaded, shape);
        }
    }

    #[test]
    fn bounds_account_for_yaw() {
        let object = SceneObject::new(Vec3::ZERO, Vec3::splat(2.0), glam::Vec4::ONE)
            .with_yaw(std::f32::consts::FRAC_PI_4);
        let (min, max) = object.bounds();
        let cos = std::f32::consts::FRAC_PI_4.cos();
        let expected = (2.0f32 * 0.5 * cos) + (2.0 * 0.5 * cos);
        assert!((max.x - expected).abs() < 1e-5, "max.x = {}", max.x);
        assert!((min.x + expected).abs() < 1e-5);
        assert!((max.y - 1.0).abs() < 1e-5);
        assert!((min.y + 1.0).abs() < 1e-5);
    }

    #[test]
    fn content_hash_is_stable_across_roundtrip() {
        let scene = Scene::demo_zone();
        let text = scene.to_ron().unwrap();
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded.content_hash(), scene.content_hash());
    }

    #[test]
    fn content_hash_distinguishes_levels() {
        let a = Scene::new("a");
        let mut b = Scene::new("b");
        b.add(SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE));
        assert_ne!(a.content_hash(), b.content_hash());

        // Изменение геометрии объекта меняет хэш.
        let mut c = b.clone();
        c.objects[0].position.x = 5.0;
        assert_ne!(c.content_hash(), b.content_hash());
    }
}
