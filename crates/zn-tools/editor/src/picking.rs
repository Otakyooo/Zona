//! Чистая герия редактора: луч из курсора, ray-vs-AABB, пикинг объектов,
//! placement (пересечение с землёй y=0 и боксами коллайдеров).
//! Всё без GPU — тестируемо на CPU.

use glam::{Vec2, Vec3};
use zn_assets::Bounds;

/// Луч из курсора: origin (near-плоскость) и направление (normalize).
pub fn cursor_ray(camera: &zn_render::Camera, cursor_px: Vec2, viewport_px: Vec2) -> (Vec3, Vec3) {
    let width = viewport_px.x.max(1.0);
    let height = viewport_px.y.max(1.0);
    let ndc_x = (cursor_px.x / width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (cursor_px.y / height) * 2.0;

    let inverse = camera.view_projection().inverse();
    let unproject = |z: f32| {
        let clip = inverse * glam::Vec4::new(ndc_x, ndc_y, z, 1.0);
        clip.truncate() / clip.w
    };
    let near = unproject(0.0);
    let far = unproject(1.0);
    let dir = (far - near).normalize_or_zero();
    (near, dir)
}

/// Ray vs AABB (slab). Возвращает (t, нормаль грани) или None.
pub fn ray_aabb(origin: Vec3, dir: Vec3, bounds: &Bounds) -> Option<(f32, Vec3)> {
    ray_aabb_raw(origin, dir, bounds.min, bounds.max)
}

/// Ray vs AABB (min/max), slab-метод: (t входа, нормаль).
pub fn ray_aabb_raw(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<(f32, Vec3)> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    let mut hit_axis = 0usize;
    let mut hit_sign = -1.0f32;

    for axis in 0..3 {
        if dir[axis].abs() < 1e-8 {
            // Луч параллелен slab-ам: промах, если origin вне slab.
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / dir[axis];
        let mut t1 = (min[axis] - origin[axis]) * inv;
        let mut t2 = (max[axis] - origin[axis]) * inv;
        let mut sign = -1.0;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
            sign = 1.0;
        }
        if t1 > t_min {
            t_min = t1;
            hit_axis = axis;
            hit_sign = sign;
        }
        t_max = t_max.min(t2);
        if t_min > t_max {
            return None;
        }
    }

    if t_max < 0.0 {
        return None; // бокс позади луча
    }
    let t = if t_min >= 0.0 { t_min } else { t_max };
    let mut normal = Vec3::ZERO;
    normal[hit_axis] = hit_sign;
    Some((t, normal))
}

/// Ближайший объект под лучом: (индекс, t). Индекс — позиция в slice.
pub fn pick_object<'a>(
    bounds: impl Iterator<Item = &'a Bounds> + 'a,
    origin: Vec3,
    dir: Vec3,
) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (index, bounds) in bounds.enumerate() {
        if let Some((t, _)) = ray_aabb_raw(origin, dir, bounds.min, bounds.max) {
            match best {
                Some((_, best_t)) if best_t <= t => {}
                _ => best = Some((index, t)),
            }
        }
    }
    best
}

/// Пересечение луча с горизонтальной землёй y = ground_y.
pub fn ground_intersection(origin: Vec3, dir: Vec3, ground_y: f32) -> Option<Vec3> {
    if dir.y.abs() < 1e-6 {
        return None;
    }
    let t = (ground_y - origin.y) / dir.y;
    if t < 0.0 {
        return None;
    }
    Some(origin + dir * t)
}

/// Точка размещения: ближайшее из пересечений с землёй и боксами
/// коллайдеров; результат сдвинут по нормали грани на `offset`.
pub fn placement_point(
    origin: Vec3,
    dir: Vec3,
    ground_y: f32,
    colliders: &[Bounds],
    offset: f32,
) -> Option<Vec3> {
    let mut best: Option<(f32, Vec3)> = None;

    if let Some(point) = ground_intersection(origin, dir, ground_y) {
        let t = point.distance(origin);
        best = Some((t, Vec3::Y));
    }

    for bounds in colliders {
        if let Some((t, normal)) = ray_aabb(origin, dir, bounds) {
            match best {
                Some((best_t, _)) if best_t <= t => {}
                _ => best = Some((t, normal)),
            }
        }
    }

    best.map(|(t, normal)| origin + dir * t + normal * offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(min: Vec3, max: Vec3) -> Bounds {
        Bounds { min, max }
    }

    fn camera_looking_minus_z() -> zn_render::Camera {
        zn_render::Camera::new(Vec3::new(0.0, 0.0, 10.0), -std::f32::consts::FRAC_PI_2, 1.0)
    }

    #[test]
    fn cursor_ray_center_hits_scene_ahead() {
        let camera = camera_looking_minus_z();
        let (origin, dir) = cursor_ray(&camera, Vec2::new(800.0, 450.0), Vec2::new(1600.0, 900.0));
        assert!(origin.distance(camera.position) < 0.2);
        assert!((dir + Vec3::Z).length() < 1e-3, "dir должен быть -Z: {dir}");
        let hit = ray_aabb_raw(origin, dir, Vec3::splat(-1.0), Vec3::splat(1.0));
        assert!(hit.is_some(), "луч центра должен попадать в куб сцены");
    }

    #[test]
    fn cursor_ray_offscreen_direction_changes() {
        let camera = camera_looking_minus_z();
        let (_, center_dir) =
            cursor_ray(&camera, Vec2::new(800.0, 450.0), Vec2::new(1600.0, 900.0));
        let (_, right_dir) =
            cursor_ray(&camera, Vec2::new(1600.0, 450.0), Vec2::new(1600.0, 900.0));
        assert!((center_dir + Vec3::Z).length() < 1e-3);
        assert!(
            right_dir.x > 0.5,
            "луч справа должен смотреть вправо: {right_dir}"
        );
        assert!((right_dir + Vec3::Z).length() > 0.5, "и вперёд");
    }

    #[test]
    fn ray_aabb_hits_top_with_up_normal() {
        let hit = ray_aabb_raw(
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        );
        let (t, normal) = hit.expect("луч должен попасть в бокс");
        assert!((t - 4.0).abs() < 1e-5, "t = {t}");
        assert_eq!(normal, Vec3::Y);
    }

    #[test]
    fn ray_aabb_misses_side() {
        let hit = ray_aabb_raw(
            Vec3::new(5.0, 5.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        );
        assert!(hit.is_none());
    }

    #[test]
    fn ray_aabb_behind_is_none() {
        let hit = ray_aabb_raw(
            Vec3::ZERO,
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(-1.0, 5.0, -1.0),
            Vec3::new(1.0, 6.0, 1.0),
        );
        assert!(hit.is_none(), "бокс позади луча");
    }

    #[test]
    fn pick_object_returns_nearest() {
        let boxes = [
            bounds(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0)), // t≈4
            bounds(Vec3::new(-0.5, -0.5, 2.0), Vec3::new(0.5, 0.5, 3.0)),  // t≈2
        ];
        let (index, t) =
            pick_object(boxes.iter(), Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z).expect("попадание");
        assert_eq!(index, 1, "ближайший объект — второй");
        assert!((t - 2.0).abs() < 1e-5);
    }

    #[test]
    fn pick_object_miss_returns_none() {
        let boxes = [bounds(Vec3::ZERO, Vec3::ONE)];
        assert!(pick_object(boxes.iter(), Vec3::new(10.0, 0.0, 0.0), Vec3::NEG_Z).is_none());
    }

    #[test]
    fn ground_intersection_works_looking_down() {
        let origin = Vec3::new(3.0, 4.0, 5.0);
        let dir = Vec3::new(0.0, -1.0, 0.0);
        let point = ground_intersection(origin, dir, 0.0).expect("пересечение с землёй");
        assert!((point - Vec3::new(3.0, 0.0, 5.0)).length() < 1e-5);
    }

    #[test]
    fn ground_intersection_none_for_parallel_ray() {
        let origin = Vec3::new(0.0, 1.0, 0.0);
        let dir = Vec3::X;
        assert!(ground_intersection(origin, dir, 0.0).is_none());
    }

    #[test]
    fn ground_intersection_none_for_diverging_ray() {
        let origin = Vec3::new(0.0, 1.0, 0.0);
        let dir = Vec3::Y;
        assert!(ground_intersection(origin, dir, 0.0).is_none());
    }

    #[test]
    fn placement_prefers_nearer_collider_over_ground() {
        // Земля y=0 на t=5 (луч вниз из (0,5,0)); бокс поверхнее.
        let collider = bounds(Vec3::new(-1.0, 3.0, -1.0), Vec3::new(1.0, 4.0, 1.0));
        let point = placement_point(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 0.0, &[collider], 0.5)
            .expect("точка найдена");
        assert!((point.y - 4.5).abs() < 1e-5, "y = {}", point.y);
    }

    #[test]
    fn placement_falls_back_to_ground() {
        let point = placement_point(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 0.0, &[], 0.5)
            .expect("точка найдена");
        assert!((point.y - 0.5).abs() < 1e-5, "y = {}", point.y);
        assert!((point.x).abs() < 1e-5);
    }

    #[test]
    fn placement_none_without_intersection() {
        let point = placement_point(Vec3::new(0.0, 5.0, 0.0), Vec3::X, 0.0, &[], 0.5);
        assert!(point.is_none());
    }
}
