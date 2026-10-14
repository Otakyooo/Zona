pub mod camera;
pub mod mesh;
pub mod renderer;

pub use camera::Camera;
pub use mesh::{cpu_mesh_vertices, cube_geometry, GpuMesh, Vertex};
pub use renderer::cull::{self, Frustum};
pub use renderer::{
    Cache, DrawBatch, DrawList, InstanceRaw, MaterialId, MaterialParams, MeshId, Renderer,
    MESH_BUDGET_BYTES, TEXTURE_BUDGET_BYTES,
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
}
