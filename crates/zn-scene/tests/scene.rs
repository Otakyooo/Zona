#[cfg(test)]
mod tests {
    use glam::Vec3;
    use zn_scene::{
        distances_to_base, lamp_object, world_content_hash, AnomalyKind, ColliderShape,
        LocationRegistry, LootEntry, Scene, SceneAnomaly, SceneBubble, SceneEnvironment,
        SceneExtraction, SceneObject, SceneOffer, SceneStation, SceneTransition, StationKind,
        BUILTIN_CUBE, BUILTIN_SPHERE, LOCATION_BASE, LOCATION_KORDON, LOCATION_PODZEMKA,
        LOCATION_SVALKA,
    };

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
    fn legacy_ron_missing_environment_gets_default() {
        let legacy = r#"
(
    name: "legacy",
    spawn: (0.0, 2.0, 0.0),
    objects: [],
)
"#;
        let scene = Scene::from_ron(legacy).unwrap();
        assert_eq!(scene.environment, SceneEnvironment::default());
        assert_eq!(scene.environment.fog_density, 0.0);
    }

    #[test]
    fn environment_roundtrips_through_ron() {
        let mut scene = Scene::new("atmo");
        scene.environment = SceneEnvironment {
            sun_direction: Vec3::new(-1.0, 2.0, 0.5),
            sun_color: Vec3::new(1.0, 0.8, 0.6),
            sun_intensity: 0.7,
            ambient_color: Vec3::new(0.1, 0.12, 0.2),
            ambient_intensity: 0.5,
            fog_color: Vec3::new(0.3, 0.3, 0.4),
            fog_density: 0.03,
            sky_top: Vec3::new(0.1, 0.2, 0.5),
            sky_horizon: Vec3::new(0.5, 0.6, 0.7),
        };

        let text = scene.to_ron().unwrap();
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded, scene);
        assert_eq!(loaded.environment, scene.environment);
    }

    #[test]
    fn sun_direction_normalizes_and_survives_zero() {
        let mut env = SceneEnvironment {
            sun_direction: Vec3::new(0.0, 5.0, 0.0),
            ..Default::default()
        };
        assert!((env.sun_direction_normalized() - Vec3::Y).length() < 1e-6);

        env.sun_direction = Vec3::ZERO;
        assert_eq!(env.sun_direction_normalized(), Vec3::Y);
    }

    #[test]
    fn world_locations_have_distinct_environments() {
        let base = Scene::base();
        let kordon = Scene::kordon();
        let svalka = Scene::svalka();
        let podzemka = Scene::podzemka();

        // Кордон — тот же день, что demo_zone (наследует).
        assert_eq!(kordon.environment, Scene::demo_zone().environment);
        // База светлее Кордона, туман слабее.
        assert!(base.environment.fog_density < kordon.environment.fog_density);
        assert!(
            base.environment.ambient_color.length() > kordon.environment.ambient_color.length()
        );
        // Свалка мглистее Кордона, Подземка — тёмная и самая плотная.
        assert!(svalka.environment.fog_density > kordon.environment.fog_density);
        assert!(podzemka.environment.fog_density > svalka.environment.fog_density);
        assert!(podzemka.environment.sun_intensity < 0.5);
        assert!(
            podzemka.environment.ambient_color.length() < svalka.environment.ambient_color.length()
        );
    }

    #[test]
    fn scene_bounds_cover_objects_and_empty_falls_back_to_spawn() {
        let kordon = Scene::kordon();
        let (min, max) = kordon.bounds();
        // Периметр demo_zone — стены ±30 с полутолщиной 0.5.
        assert!(min.x <= -30.5 && max.x >= 30.5, "bounds: {min:?}..{max:?}");
        assert!(min.z <= -30.5 && max.z >= 30.5);
        assert!(max.y >= 3.0);

        let mut empty = Scene::new("empty");
        empty.spawn = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(empty.bounds(), (empty.spawn, empty.spawn));
    }

    #[test]
    fn world_locations_use_builtin_textures() {
        for scene in [
            Scene::base(),
            Scene::kordon(),
            Scene::svalka(),
            Scene::podzemka(),
        ] {
            let textured: Vec<_> = scene
                .objects
                .iter()
                .filter(|object| object.texture.is_some())
                .collect();
            assert!(
                textured.len() >= 5,
                "{}: ожидались текстурированные пол и стены",
                scene.name
            );
            for object in textured {
                assert!(object.world_uv, "{}: у поверхностей мировые UV", scene.name);
                assert_eq!(object.asset_key(), BUILTIN_CUBE);
                assert!(object.has_material_overrides());
            }
        }
        // Лампы подземки (M4g) светятся emissive-материалом.
        let podzemka = Scene::podzemka();
        assert!(podzemka
            .objects
            .iter()
            .any(|object| object.emissive.is_some()));
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
        assert_eq!(object.loot, None);
        assert!(!object.is_container());
        // Материал поверхности (M4g) в старом RON отсутствует — дефолты.
        assert_eq!(object.texture, None);
        assert_eq!(object.uv_scale, 1.0);
        assert!(!object.world_uv);
        assert_eq!(object.roughness, None);
        assert_eq!(object.metallic, None);
        assert_eq!(object.emissive, None);
        assert!(!object.has_material_overrides());
        assert!(scene.anomalies.is_empty());
    }

    #[test]
    fn material_fields_roundtrip_and_defaults() {
        let mut scene = Scene::new("materials");
        scene.add(
            SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE)
                .with_texture("builtin://concrete", 8.0, true)
                .with_roughness(0.4)
                .with_metallic(0.1)
                .with_emissive(Vec3::new(0.2, 0.3, 0.4)),
        );

        let text = scene.to_ron().unwrap();
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded, scene);
        let object = &loaded.objects[0];
        assert_eq!(object.texture.as_deref(), Some("builtin://concrete"));
        assert_eq!(object.uv_scale, 8.0);
        assert!(object.world_uv);
        assert_eq!(object.roughness, Some(0.4));
        assert_eq!(object.metallic, Some(0.1));
        assert_eq!(object.emissive, Some(Vec3::new(0.2, 0.3, 0.4)));
        assert!(object.has_material_overrides());
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
    fn lamp_object_creates_emissive_sphere_marker() {
        let pos = Vec3::new(1.0, 2.5, 3.0);
        let color = Vec3::new(1.0, 0.8, 0.5);
        let lamp = lamp_object(pos, color, 2.0);
        assert_eq!(lamp.position, pos);
        assert_eq!(lamp.scale, Vec3::splat(0.25));
        assert_eq!(lamp.asset.as_deref(), Some(BUILTIN_SPHERE));
        assert_eq!(lamp.emissive, Some(color * 2.0));
        assert!(!lamp.solid, "лампа-маркер не должна иметь коллизии");
        assert_eq!(lamp.effective_collider(), None);
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
    fn container_loot_roundtrips_through_ron() {
        let mut scene = Scene::new("loot");
        scene.add(
            SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE).with_loot(vec![
                LootEntry {
                    item: "medkit".into(),
                    count: 1,
                },
                LootEntry {
                    item: "ammo_545".into(),
                    count: 60,
                },
            ]),
        );

        let text = scene.to_ron().unwrap();
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded, scene);
        let object = &loaded.objects[0];
        assert!(object.is_container());
        let loot = object.loot.as_ref().unwrap();
        assert_eq!(loot.len(), 2);
        assert_eq!(loot[1].item, "ammo_545");
        assert_eq!(loot[1].count, 60);
    }

    #[test]
    fn demo_zone_has_eight_crates_with_loot() {
        let scene = Scene::demo_zone();
        let containers: Vec<_> = scene
            .objects
            .iter()
            .filter(|object| object.is_container())
            .collect();
        assert_eq!(containers.len(), 8);
        for container in containers {
            assert!(container.solid, "ящик должен быть solid (обыск лучом)");
            assert!(!container.loot.as_ref().unwrap().is_empty());
        }
    }

    #[test]
    fn content_hash_distinguishes_loot() {
        let mut plain = Scene::new("hash");
        let base = SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE);
        plain.add(base.clone());

        let mut with_loot = Scene::new("hash");
        with_loot.add(base.with_loot(vec![LootEntry {
            item: "bolt".into(),
            count: 1,
        }]));

        assert_ne!(with_loot.content_hash(), plain.content_hash());
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

    #[test]
    fn anomaly_roundtrip_through_ron() {
        let mut scene = Scene::new("anomalies");
        scene.add_anomaly(
            SceneAnomaly::new(Vec3::new(1.0, 0.0, 2.0), 3.0, AnomalyKind::Heat)
                .with_artifact("artifact_heat"),
        );
        scene.add_anomaly(SceneAnomaly::new(
            Vec3::new(-4.0, 0.0, 5.0),
            3.5,
            AnomalyKind::Gravity,
        ));

        let text = scene.to_ron().unwrap();
        assert!(
            text.contains("heat") && text.contains("gravity"),
            "вид аномалии пишется в lowercase: {text}"
        );
        let loaded = Scene::from_ron(&text).unwrap();
        assert_eq!(loaded, scene);
        assert_eq!(
            loaded.anomalies[0].artifact.as_deref(),
            Some("artifact_heat")
        );
        assert_eq!(loaded.anomalies[1].artifact, None);

        // Центр сферы — на высоте радиуса над якорем; contains учитывает радиус.
        let gravity = &loaded.anomalies[1];
        assert_eq!(gravity.center(), Vec3::new(-4.0, 3.5, 5.0));
        assert!(gravity.contains(Vec3::new(-4.0, 3.5, 5.0)));
        assert!(!gravity.contains(Vec3::new(-4.0, 3.5, 5.0 + 3.6)));
    }

    #[test]
    fn demo_zone_has_anomalies_with_artifacts() {
        let scene = Scene::demo_zone();
        assert_eq!(scene.anomalies.len(), 3);
        let kinds: Vec<_> = scene.anomalies.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            vec![AnomalyKind::Heat, AnomalyKind::Heat, AnomalyKind::Gravity]
        );
        let artifacts: Vec<&str> = scene
            .anomalies
            .iter()
            .filter_map(|a| a.artifact.as_deref())
            .collect();
        assert_eq!(artifacts, vec!["artifact_heat", "artifact_gravity"]);
        for anomaly in &scene.anomalies {
            assert!(anomaly.radius > 0.0);
            // Якорь — на полу в пределах арены.
            assert_eq!(anomaly.position.y, 0.0);
            assert!(anomaly.position.x.abs() < 28.0 && anomaly.position.z.abs() < 28.0);
        }
    }

    #[test]
    fn content_hash_distinguishes_anomalies() {
        let mut plain = Scene::new("hash");
        plain.add(SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE));

        let mut with_anomaly = Scene::new("hash");
        with_anomaly.add(SceneObject::new(Vec3::ZERO, Vec3::ONE, glam::Vec4::ONE));
        with_anomaly.add_anomaly(SceneAnomaly::new(Vec3::ZERO, 3.0, AnomalyKind::Heat));

        assert_ne!(with_anomaly.content_hash(), plain.content_hash());
    }

    #[test]
    fn builtin_registry_roundtrip_and_base() {
        let registry = LocationRegistry::builtin();
        assert_eq!(registry.base_id(), Some(LOCATION_BASE));
        assert_eq!(registry.get(LOCATION_KORDON).unwrap().name, "Кордон");
        assert_eq!(registry.get(LOCATION_SVALKA).unwrap().scene, "svalka");

        let text = registry.to_ron().unwrap();
        let loaded = LocationRegistry::from_ron(&text).unwrap();
        assert_eq!(loaded, registry);
    }

    #[test]
    fn builtin_world_matches_registry() {
        let registry = LocationRegistry::builtin();
        let world = Scene::builtin_world();
        assert_eq!(world.len(), registry.locations.len());
        for (id, scene) in &world {
            let info = registry.get(*id).expect("локация есть в реестре");
            assert_eq!(scene.name, info.scene, "имя сцены совпадает с файлом");
        }

        // База безопасна: без аномалий и пузырей, один переход — на Кордон.
        let base = &world[0].1;
        assert!(base.anomalies.is_empty());
        assert!(base.bubbles.is_empty());
        assert!(base.bubble_arrivals.is_empty());
        assert_eq!(base.transitions.len(), 1);
        assert_eq!(base.transitions[0].target, LOCATION_KORDON);
    }

    #[test]
    fn world_hash_depends_on_registry_and_scenes() {
        let registry = LocationRegistry::builtin();
        let world = Scene::builtin_world();
        let refs: Vec<(u8, &Scene)> = world.iter().map(|(id, scene)| (*id, scene)).collect();
        let first = world_content_hash(&registry, &refs);

        let mut bumped = world.clone();
        bumped[2].1.spawn.x += 1.0;
        let bumped_refs: Vec<(u8, &Scene)> =
            bumped.iter().map(|(id, scene)| (*id, scene)).collect();
        assert_ne!(first, world_content_hash(&registry, &bumped_refs));
        assert_eq!(first, world_content_hash(&registry, &refs));
    }

    #[test]
    fn world_spawn_points_are_outside_transition_zones() {
        let world = Scene::builtin_world();
        for (id, scene) in &world {
            let mut spawn_points: Vec<Vec3> = scene
                .transitions
                .iter()
                .map(|transition| transition.target_position)
                .collect();
            spawn_points.extend(scene.bubble_arrivals.iter().copied());
            for point in spawn_points {
                for zone in &scene.transitions {
                    assert!(
                        !zone.contains(point),
                        "локация {id}: точка появления {point:?} внутри зоны {zone:?} — телепорт-петля"
                    );
                }
            }
        }
    }

    #[test]
    fn kordon_extends_demo_zone_with_transitions_and_bubbles() {
        let demo = Scene::demo_zone();
        let kordon = Scene::kordon();
        assert_eq!(
            kordon.objects.len(),
            demo.objects.len() + 3,
            "плюс три плиты переходов"
        );
        assert_eq!(kordon.anomalies.len(), demo.anomalies.len());
        assert_eq!(kordon.transitions.len(), 3);
        assert_eq!(kordon.bubbles.len(), 3);
        assert!(!kordon.bubble_arrivals.is_empty());

        let targets: Vec<u8> = kordon.transitions.iter().map(|t| t.target).collect();
        assert_eq!(
            targets,
            vec![LOCATION_BASE, LOCATION_SVALKA, LOCATION_PODZEMKA]
        );
    }

    #[test]
    fn distances_to_base_prefers_kordon() {
        let registry = LocationRegistry::builtin();
        let world = Scene::builtin_world();
        let refs: Vec<(u8, &Scene)> = world.iter().map(|(id, scene)| (*id, scene)).collect();
        let distances = distances_to_base(&registry, &refs);
        assert_eq!(distances.get(&LOCATION_BASE), Some(&0));
        assert_eq!(distances.get(&LOCATION_KORDON), Some(&1));
        assert_eq!(distances.get(&LOCATION_SVALKA), Some(&2));
        assert_eq!(distances.get(&LOCATION_PODZEMKA), Some(&2));
    }

    #[test]
    fn transition_and_bubble_containment() {
        let transition =
            SceneTransition::new(Vec3::new(0.0, 0.0, 10.0), 2.5, LOCATION_BASE, Vec3::ZERO);
        assert!(transition.contains(Vec3::new(1.0, 5.0, 10.5)));
        assert!(!transition.contains(Vec3::new(3.0, 0.0, 10.0)));

        let bubble = SceneBubble::new(Vec3::ZERO, 1.2);
        assert_eq!(bubble.center(), Vec3::new(0.0, 1.2, 0.0));
        assert!(bubble.contains(Vec3::new(0.0, 1.2, 0.5)));
        assert!(!bubble.contains(Vec3::new(2.0, 1.2, 0.0)));
    }

    #[test]
    fn extraction_circle_containment_and_lookup() {
        let mut scene = Scene::new("test");
        let circle = SceneExtraction::new(Vec3::new(-24.0, 0.0, -24.0), 3.0, 0.75)
            .with_label("Аварийная эвакуация");
        scene.add_extraction(circle.clone());

        // Горизонтальный цилиндр: высота не мешает, дальность по XZ решает.
        assert!(scene.extraction_at(Vec3::new(-24.0, 1.6, -22.0)).is_some());
        assert!(scene.extraction_at(Vec3::new(-24.5, 0.0, -25.5)).is_some());
        assert!(scene.extraction_at(Vec3::new(-20.0, 0.0, -24.0)).is_none());
        assert_eq!(
            scene
                .extraction_at(Vec3::new(-24.0, 0.0, -24.0))
                .map(|c| c.percent),
            Some(0.75)
        );
        assert_eq!(circle.label, "Аварийная эвакуация");
    }

    #[test]
    fn scene_without_extractions_field_still_loads() {
        // Обратная совместимость RON (M1): поле `extractions` необязательное.
        let ron = r#"(
            name: "legacy",
            objects: [],
        )"#;
        let scene: Scene = ron::from_str(ron).expect("сцена без extractions");
        assert!(scene.extractions.is_empty());
        assert!(scene.extraction_at(Vec3::ZERO).is_none());
    }

    #[test]
    fn builtin_world_has_extraction_on_every_raid_location() {
        let world = Scene::builtin_world();
        for (id, scene) in &world {
            if *id == LOCATION_BASE {
                assert!(scene.extractions.is_empty(), "база безопасна — кругов нет");
                continue;
            }
            assert_eq!(scene.extractions.len(), 1, "локация {id} без круга");
            let extraction = &scene.extractions[0];
            assert!(extraction.percent > 0.0 && extraction.percent < 1.0);
            assert!(!extraction.label.is_empty());
        }
    }

    #[test]
    fn station_circle_containment_and_lookup() {
        let mut scene = Scene::new("test");
        scene.add_station(
            SceneStation::new(Vec3::new(-8.0, 0.0, 10.0), 2.0, StationKind::Trader)
                .with_label("Торговец")
                .with_offers(vec![SceneOffer {
                    item: "medkit".into(),
                    count: 1,
                }]),
        );

        // Горизонтальный цилиндр, как у кругов экстракции.
        assert!(scene.station_at(Vec3::new(-8.0, 1.6, 10.0)).is_some());
        assert!(scene.station_at(Vec3::new(-9.5, 0.0, 9.0)).is_some());
        assert!(scene.station_at(Vec3::new(0.0, 0.0, 14.0)).is_none());

        let station = scene
            .station_at(Vec3::new(-8.0, 0.0, 10.0))
            .expect("станция под точкой");
        assert_eq!(station.kind, StationKind::Trader);
        assert_eq!(station.label, "Торговец");
        assert_eq!(station.offers.len(), 1);
        assert_eq!(station.offers[0].item, "medkit");
        assert_eq!(station.offers[0].count, 1);
    }

    #[test]
    fn scene_without_stations_field_still_loads() {
        // Обратная совместимость RON: поле `stations` необязательное.
        let ron = r#"(
            name: "legacy",
            objects: [],
        )"#;
        let scene: Scene = ron::from_str(ron).expect("сцена без stations");
        assert!(scene.stations.is_empty());
        assert!(scene.station_at(Vec3::ZERO).is_none());
    }

    #[test]
    fn base_has_four_stations_near_spawn() {
        let base = Scene::base();
        assert_eq!(base.stations.len(), 4, "торговец, медпункт, верстак, сейф");
        for station in &base.stations {
            assert!(!station.label.is_empty());
            // Все станции — в паре шагов от спавна.
            let flat = Vec3::new(base.spawn.x, 0.0, base.spawn.z);
            let delta = Vec3::new(
                station.position.x - flat.x,
                0.0,
                station.position.z - flat.z,
            );
            assert!(
                delta.length() < 10.0,
                "станция '{}' далеко от спавна",
                station.label
            );
        }
        let trader = base
            .stations
            .iter()
            .find(|station| station.kind == StationKind::Trader)
            .expect("торговец на базе");
        assert!(!trader.offers.is_empty(), "у торговца есть офферы");
        // Ровно одна станция каждого вида.
        for kind in [
            StationKind::Trader,
            StationKind::Workbench,
            StationKind::Medpost,
            StationKind::Safe,
        ] {
            assert_eq!(
                base.stations
                    .iter()
                    .filter(|station| station.kind == kind)
                    .count(),
                1,
                "одна станция вида {kind:?}"
            );
        }
    }
}
