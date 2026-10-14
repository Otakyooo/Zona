//! Чистые функции отсечения: фрустум из view-projection + дистанционный клип.
//! Без GPU — тестируемо на CPU.

use crate::camera::Camera;
use crate::renderer::draw::DrawBatch;
use crate::renderer::types::InstanceRaw;
use glam::{Mat4, Vec3, Vec4};

/// Фрустум из шести плоскостей (Gribb-Hartmann), нормали смотрят внутрь.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    pub planes: [Vec4; 6],
}

impl Frustum {
    pub fn from_view_projection(matrix: Mat4) -> Self {
        // Строки matrix.transpose(): plane_i = row_i(|M|) + row_i(-|M|)...
        let m = matrix.transpose();
        let rows = [m.x_axis, m.y_axis, m.z_axis, m.w_axis];
        let planes = [
            // left: w + x, right: w - x
            rows[3] + rows[0],
            rows[3] - rows[0],
            // bottom: w + y, top: w - y
            rows[3] + rows[1],
            rows[3] - rows[1],
            // near: w + z, far: w - z
            rows[3] + rows[2],
            rows[3] - rows[2],
        ];
        Self { planes }
    }

    /// Сфера (центр, радиус) полностью/частично внутри фрустума?
    pub fn intersects_sphere(&self, center: Vec3, radius: f32) -> bool {
        self.planes.iter().all(|plane| {
            let normal = plane.truncate();
            let length = normal.length();
            if length <= f32::EPSILON {
                return true;
            }
            // distance = (n·c + d) / |n|
            let distance = (normal.dot(center) + plane.w) / length;
            distance >= -radius
        })
    }

    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        // Тест «positive vertex» AABB vs плоскости.
        let corners = [
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(min.x, max.y, max.z),
            Vec3::new(max.x, max.y, max.z),
        ];
        self.planes.iter().all(|plane| {
            let normal = plane.truncate();
            let length = normal.length();
            if length <= f32::EPSILON {
                return true;
            }
            // Максимальная оценка расстояния по углам.
            let max_distance = corners
                .iter()
                .map(|corner| (normal.dot(*corner) + plane.w) / length)
                .fold(f32::NEG_INFINITY, f32::max);
            max_distance >= 0.0
        })
    }
}

/// Отсекает инстансы батчей по фрустуму камеры (с запасом `fov_pad`) и
/// дистанции до `camera.far`. `bounds_of` даёт bounds меша в пространстве
/// ассета (единичные для примитивов).
///
/// Возвращает копии батчей с видимыми инстансами; полностью отсечённые
/// батчи не возвращаются.
pub fn visible_instances(
    batches: &[DrawBatch],
    camera: &Camera,
    mesh_bounds: impl Fn(crate::renderer::cache::MeshId) -> zn_assets::Bounds,
) -> Vec<DrawBatch> {
    visible_instances_with_pad(batches, camera, 1.15, mesh_bounds)
}

/// Вариант с явным коэффициентом запаса фрустума (`fov_pad`).
pub fn visible_instances_with_pad(
    batches: &[DrawBatch],
    camera: &Camera,
    fov_pad: f32,
    mesh_bounds: impl Fn(crate::renderer::cache::MeshId) -> zn_assets::Bounds,
) -> Vec<DrawBatch> {
    let frustum = Frustum::from_view_projection(camera.view_projection());
    let far = camera.far;
    let mut visible = Vec::new();

    for batch in batches {
        let bounds = mesh_bounds(batch.mesh);

        let mut kept: Vec<InstanceRaw> = Vec::new();
        for instance in &batch.instances {
            let model = Mat4::from_cols_array(&instance.model);
            let center = model.transform_point3(bounds.center());
            // Радиус ограничивающей сферы меша в мировых координатах.
            let world_radius = model.transform_vector3(bounds.extents() * 0.5).length();
            let radius = (world_radius * fov_pad).max(0.001);

            // Дистанционный клип по радиусу.
            let distance = center.distance(camera.position);
            if distance - radius > far {
                continue;
            }
            if frustum.intersects_sphere(center, radius) {
                kept.push(*instance);
            }
        }

        if !kept.is_empty() {
            let mut culled = DrawBatch::new(batch.mesh, batch.material);
            culled.instances = kept;
            visible.push(culled);
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::cache::MeshId;
    use zn_assets::Bounds;

    fn unit_bounds(_mesh: MeshId) -> Bounds {
        Bounds {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        }
    }

    /// Камера в начале координат, смотрит вдоль +X (yaw = 0).
    fn camera_at_origin() -> Camera {
        let mut camera = Camera::new(Vec3::ZERO, 0.0, 1.0);
        camera.far = 100.0;
        camera
    }

    fn instance_at(position: Vec3) -> InstanceRaw {
        InstanceRaw {
            model: Mat4::from_translation(position).to_cols_array(),
            color: [1.0; 4],
        }
    }

    fn batch(instances: Vec<InstanceRaw>) -> DrawBatch {
        DrawBatch {
            mesh: 1,
            material: 1,
            instances,
        }
    }

    #[test]
    fn frustum_separates_visible_and_hidden() {
        let camera = camera_at_origin();
        let batches = vec![batch(vec![
            instance_at(Vec3::new(5.0, 0.0, 0.0)), // перед камерой — виден
            instance_at(Vec3::new(10.0, 60.0, 0.0)), // на 80° вверх — вне фрустума
        ])];
        let frustum = Frustum::from_view_projection(camera.view_projection());
        assert!(frustum.intersects_sphere(Vec3::new(5.0, 0.0, 0.0), 0.5));
        assert!(!frustum.intersects_sphere(Vec3::new(10.0, 60.0, 0.0), 0.5));

        let visible = visible_instances(&batches, &camera, unit_bounds);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].instance_count(), 1);
    }

    #[test]
    fn distance_clip_drops_far_instances() {
        let mut camera = camera_at_origin();
        camera.far = 20.0;
        let batches = vec![batch(vec![
            instance_at(Vec3::new(10.0, 0.0, 0.0)), // ближе far
            instance_at(Vec3::new(50.0, 0.0, 0.0)), // дальше far
        ])];
        let visible = visible_instances(&batches, &camera, unit_bounds);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].instance_count(), 1);
    }

    #[test]
    fn objects_behind_camera_culled() {
        let camera = camera_at_origin();
        let batches = vec![batch(vec![instance_at(Vec3::new(-5.0, 0.0, 0.0))])];
        let visible = visible_instances(&batches, &camera, unit_bounds);
        assert!(visible.is_empty());
    }

    #[test]
    fn all_instances_kept_when_near_frustum_axis() {
        let camera = camera_at_origin();
        let instances: Vec<_> = (5..=11)
            .map(|x| instance_at(Vec3::new(x as f32, 0.5, 0.0)))
            .collect();
        let batches = vec![batch(instances)];
        let visible = visible_instances(&batches, &camera, unit_bounds);
        assert_eq!(visible[0].instance_count(), 7);
    }

    #[test]
    fn fov_pad_expands_culling_boundary() {
        let camera = camera_at_origin();
        // 56° от оси камеры — сильно вне фрустума (35°), но попадает
        // в «раздутую» сферу с запасом.
        let batches = vec![batch(vec![instance_at(Vec3::new(2.0, 3.0, 0.0))])];
        let strict = visible_instances_with_pad(&batches, &camera, 1.0, unit_bounds);
        let padded = visible_instances_with_pad(&batches, &camera, 3.0, unit_bounds);
        assert!(strict.is_empty(), "без запаса объект должен отсекаться");
        assert_eq!(padded[0].instance_count(), 1, "с запасом — виден");
    }

    #[test]
    fn empty_batches_stay_empty() {
        let camera = camera_at_origin();
        let visible = visible_instances(&[], &camera, unit_bounds);
        assert!(visible.is_empty());
    }
}
