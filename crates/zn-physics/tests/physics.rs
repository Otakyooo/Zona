use rapier3d::prelude::*;

use zn_physics::{PhysicsWorld, PLAYER_HALF_HEIGHT, PLAYER_RADIUS};

const DT: f32 = 1.0 / 60.0;

fn world_with_ground() -> PhysicsWorld {
    let mut world = PhysicsWorld::with_gravity_y(-9.81, DT);
    world.add_static_box(Vector::new(10.0, 0.5, 10.0), Vector::new(0.0, 0.0, 0.0));
    world.step_dynamic_bodies();
    world
}

#[test]
fn player_lands_on_box_and_reports_grounded() {
    let mut world = world_with_ground();
    let mut feet = Vector::new(0.0, 5.0, 0.0);
    let mut grounded = false;

    for _ in 0..240 {
        let origin = PhysicsWorld::character_origin(feet);
        let movement = world.move_character(origin, Vector::new(0.0, -9.81 * DT, 0.0));
        feet = PhysicsWorld::character_feet(origin + movement.translation);
        grounded = movement.grounded;
        if grounded {
            break;
        }
    }

    assert!(grounded, "character never became grounded");
    assert!((feet.y - 0.5).abs() < 0.05, "feet y = {}", feet.y);
}

#[test]
fn wall_blocks_horizontal_movement() {
    let mut world = world_with_ground();
    world.add_static_box(Vector::new(0.5, 3.0, 10.0), Vector::new(2.0, 3.0, 0.0));
    world.step_dynamic_bodies();

    let mut feet = Vector::new(0.0, 1.0, 0.0);
    for _ in 0..120 {
        let origin = PhysicsWorld::character_origin(feet);
        let movement = world.move_character(origin, Vector::new(6.0 * DT, 0.0, 0.0));
        feet = PhysicsWorld::character_feet(origin + movement.translation);
    }

    assert!(
        feet.x < 1.3,
        "character walked through wall, x = {}",
        feet.x
    );
}

#[test]
fn autostep_climbs_small_obstacle() {
    let mut world = world_with_ground();
    world.add_static_box(Vector::new(0.5, 0.1, 0.5), Vector::new(0.0, 0.2, 0.0));
    world.step_dynamic_bodies();

    let mut feet = Vector::new(-1.0, 1.0, 0.0);
    for _ in 0..120 {
        let origin = PhysicsWorld::character_origin(feet);
        let movement = world.move_character(origin, Vector::new(6.0 * DT, 0.0, 0.0));
        feet = PhysicsWorld::character_feet(origin + movement.translation);
    }

    assert!(feet.x > 0.6, "autostep failed, x = {}", feet.x);
}

#[test]
fn steep_slope_slides_character_down() {
    let mut world = PhysicsWorld::with_gravity_y(-9.81, DT);
    let slope = ColliderBuilder::cuboid(6.0, 0.5, 6.0)
        .rotation(rapier3d::glamx::Vec3::new(0.0, 0.0, 0.6))
        .build();
    world.colliders_mut().insert(slope);
    world.step_dynamic_bodies();

    let mut feet = Vector::new(0.0, 5.0, 0.0);
    let mut grounded = false;
    for _ in 0..300 {
        let origin = PhysicsWorld::character_origin(feet);
        let step = world.move_character(origin, Vector::new(0.0, -9.81 * DT, 0.0));
        feet = PhysicsWorld::character_feet(origin + step.translation);
        grounded = step.grounded;
        if grounded {
            break;
        }
    }
    assert!(grounded, "character never touched the slope");
    assert!((feet.y - 0.5).abs() < 0.3, "feet y = {}", feet.y);
}

#[test]
fn character_dimensions_are_stable() {
    assert!((2.0 * (PLAYER_HALF_HEIGHT + PLAYER_RADIUS) - 1.8).abs() < 1e-5);
}

#[test]
fn scene_objects_become_expected_colliders() {
    use zn_scene::{ColliderShape, Scene, SceneObject};

    let mut scene = Scene::new("shapes");
    scene.add(SceneObject::new(
        glam::Vec3::ZERO,
        glam::Vec3::splat(2.0),
        glam::Vec4::ONE,
    ));
    scene.add(
        SceneObject::new(
            glam::Vec3::new(4.0, 0.0, 0.0),
            glam::Vec3::new(1.0, 3.0, 1.0),
            glam::Vec4::ONE,
        )
        .with_collider(ColliderShape::Cylinder),
    );
    scene.add(
        SceneObject::new(
            glam::Vec3::new(-4.0, 0.0, 0.0),
            glam::Vec3::splat(1.5),
            glam::Vec4::ONE,
        )
        .with_collider(ColliderShape::Sphere),
    );
    // Не-solid объект коллайдера не получает.
    scene.add(
        SceneObject::new(
            glam::Vec3::new(0.0, 5.0, 0.0),
            glam::Vec3::ONE,
            glam::Vec4::ONE,
        )
        .invulnerable(),
    );

    let mut world = PhysicsWorld::default();
    for object in &scene.objects {
        world.add_scene_object(object);
    }
    world.step_dynamic_bodies();

    assert_eq!(world.static_collider_count(), 3);

    let mut boxes = 0;
    let mut balls = 0;
    let mut cylinders = 0;
    for (_, collider) in world.colliders().iter() {
        let shape = collider.shape();
        if shape.as_cuboid().is_some() {
            boxes += 1;
        } else if shape.as_ball().is_some() {
            balls += 1;
        } else if shape.as_cylinder().is_some() {
            cylinders += 1;
        }
    }
    assert_eq!(boxes, 1);
    assert_eq!(balls, 1);
    assert_eq!(cylinders, 1);
}

#[test]
fn add_static_shape_uses_scale_as_full_size() {
    let mut world = PhysicsWorld::default();
    world.add_static_shape(
        zn_scene::ColliderShape::Box,
        Vector::new(0.0, 0.0, 0.0),
        Vector::new(4.0, 2.0, 6.0),
        0.8,
    );
    let (_, collider) = world.colliders().iter().next().expect("collider added");
    let cuboid = collider.shape().as_cuboid().expect("cuboid shape");
    assert!((cuboid.half_extents.x - 2.0).abs() < 1e-6);
    assert!((cuboid.half_extents.y - 1.0).abs() < 1e-6);
    assert!((cuboid.half_extents.z - 3.0).abs() < 1e-6);
}
