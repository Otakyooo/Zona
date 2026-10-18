pub mod camera;
pub mod mesh;
pub mod renderer;

pub use camera::Camera;
pub use mesh::{cpu_mesh_vertices, cube_geometry, GpuMesh, Vertex};
pub use renderer::cull::{self, Frustum};
pub use renderer::{
    directional_shadow_matrix, Cache, DrawBatch, DrawList, InstanceRaw, MaterialId, MaterialParams,
    MeshId, Renderer, MESH_BUDGET_BYTES, SHADOW_MAP_SIZE, TEXTURE_BUDGET_BYTES,
};

use zn_scene::Scene;

pub const INITIAL_INSTANCE_CAPACITY: usize = 4096;

pub fn instances_from_scene(scene: &Scene) -> Vec<InstanceRaw> {
    scene
        .objects
        .iter()
        .map(|object| InstanceRaw {
            model: object.model_matrix().to_cols_array(),
            color: object.color.into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_from_scene_matches_objects() {
        let scene = Scene::demo_zone();
        let instances = instances_from_scene(&scene);
        assert_eq!(instances.len(), scene.len());
        assert_eq!(instances[0].color, scene.objects[0].color.to_array());
    }

    #[test]
    fn shadow_matrix_covers_scene_corners() {
        let min = glam::Vec3::new(-30.0, -1.0, -30.0);
        let max = glam::Vec3::new(30.0, 6.0, 30.0);
        for direction in [
            glam::Vec3::new(0.4, 0.8, 0.3),
            glam::Vec3::new(0.0, 1.0, 0.0),
            glam::Vec3::new(-0.9, 0.1, 0.2),
        ] {
            let matrix = directional_shadow_matrix(direction, min, max);
            for i in 0..8 {
                let corner = glam::Vec3::new(
                    if i & 1 == 0 { min.x } else { max.x },
                    if i & 2 == 0 { min.y } else { max.y },
                    if i & 4 == 0 { min.z } else { max.z },
                );
                let clip = matrix * corner.extend(1.0);
                assert!(
                    clip.x.abs() <= 1.0 + 1e-3 && clip.y.abs() <= 1.0 + 1e-3,
                    "угол вне карты: {clip:?} (направление {direction:?})"
                );
                assert!(
                    (0.0..=1.0).contains(&clip.z),
                    "глубина вне [0, 1]: {clip:?} (направление {direction:?})"
                );
            }
        }
    }

    #[test]
    fn shadow_matrix_survives_zero_direction() {
        let matrix = directional_shadow_matrix(glam::Vec3::ZERO, -glam::Vec3::ONE, glam::Vec3::ONE);
        assert!(matrix.is_finite(), "матрица должна остаться конечной");
    }
}
