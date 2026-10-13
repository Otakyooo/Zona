pub mod camera;
pub mod mesh;
pub mod renderer;

pub use camera::Camera;
pub use mesh::{cube_geometry, Vertex};
pub use renderer::{InstanceRaw, Renderer};

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
