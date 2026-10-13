#[cfg(test)]
mod tests {
    use zn_scene::{Scene, SceneObject};
    use glam::Vec3;

    #[test]
    fn ron_roundtrip_preserves_scene() {
        let mut scene = Scene::new("test");
        scene.spawn = Vec3::new(1.0, 2.0, 3.0);
        scene.add(SceneObject::new(Vec3::new(0.0, 0.0, 0.0), Vec3::ONE, glam::Vec4::ONE));

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
    }

    #[test]
    fn model_matrix_applies_scale_rotation_translation() {
        let object = SceneObject::new(Vec3::new(1.0, 2.0, 3.0), Vec3::splat(2.0), glam::Vec4::ONE)
            .with_yaw(std::f32::consts::FRAC_PI_2);
        let point = object.model_matrix().project_point3(glam::Vec3::new(0.0, 0.0, 1.0));
        assert!((point.x - 3.0).abs() < 1e-4, "x was {}", point.x);
        assert!((point.y - 2.0).abs() < 1e-4, "y was {}", point.y);
        assert!((point.z - 3.0).abs() < 1e-4, "z was {}", point.z);
    }
}
