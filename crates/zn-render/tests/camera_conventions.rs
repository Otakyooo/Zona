//! Тесты конвенций камеры: куда смотрит «право экрана», куда поворачивают
//! положительные дельты yaw/pitch и как это связано с векторами right/up.
//! Эти конвенции общие для рендера, ввода и симуляции движения (zn-sim).

use glam::Vec3;
use zn_render::Camera;

fn ndc(camera: &Camera, point: Vec3) -> Vec3 {
    let clip = camera.view_projection() * point.extend(1.0);
    clip.truncate() / clip.w
}

#[test]
fn screen_right_is_positive_z_at_yaw_zero() {
    let camera = Camera::new(Vec3::ZERO, 0.0, 1.0);

    let right_point = ndc(&camera, Vec3::new(1.0, 0.0, 1.0));
    let left_point = ndc(&camera, Vec3::new(1.0, 0.0, -1.0));
    let up_point = ndc(&camera, Vec3::new(1.0, 1.0, 0.0));

    assert!(right_point.x > 0.0, "ожидали +Z справа: {right_point:?}");
    assert!(left_point.x < 0.0, "ожидали -Z слева: {left_point:?}");
    assert!(up_point.y > 0.0, "ожидали +Y сверху: {up_point:?}");
}

#[test]
fn positive_yaw_delta_turns_toward_screen_right() {
    let mut camera = Camera::new(Vec3::ZERO, 0.0, 1.0);
    assert!(camera.forward().x > 0.9);

    camera.look(0.2, 0.0);

    // Положительная дельта yaw уводит взгляд вправо (+Z при yaw = 0).
    assert!(camera.forward().z > 0.0, "forward = {:?}", camera.forward());
    // Симуляция движения использует тот же знак: yaw_delta > 0 — поворот вправо.
    let mut simulated = Camera::new(Vec3::ZERO, 0.0, 1.0);
    simulated.yaw += 0.2;
    assert!((simulated.forward() - camera.forward()).length() < 1e-6);
}

#[test]
fn positive_pitch_delta_looks_up() {
    let mut camera = Camera::new(Vec3::ZERO, 0.0, 1.0);
    camera.look(0.0, 0.2);
    assert!(camera.forward().y > 0.0, "forward = {:?}", camera.forward());
}

#[test]
fn right_and_up_vectors_map_to_positive_ndc() {
    let camera = Camera::new(Vec3::ZERO, 0.0, 1.0);
    let forward = camera.forward();

    let right_point = ndc(&camera, forward + camera.right());
    let up_point = ndc(&camera, forward + camera.up());

    assert!(
        right_point.x > 0.0,
        "right() не указывает вправо: {right_point:?}"
    );
    assert!(up_point.y > 0.0, "up() не указывает вверх: {up_point:?}");

    // Ортонормальный базис.
    let basis = glam::Mat3::from_cols(camera.right(), camera.up(), -forward);
    let identity = basis * basis.transpose();
    assert!((identity - glam::Mat3::IDENTITY)
        .to_cols_array()
        .iter()
        .all(|v| v.abs() < 1e-4));
}
