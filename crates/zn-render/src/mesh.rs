use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

fn quad(normal: [f32; 3], u: [f32; 3], v: [f32; 3]) -> ([Vertex; 4], [u16; 6]) {
    let n = glam::Vec3::from_array(normal) * 0.5;
    let u = glam::Vec3::from_array(u) * 0.5;
    let v = glam::Vec3::from_array(v) * 0.5;
    let corners = [(-u - v), (u - v), (u + v), (-u + v)];
    let mut vertices = [Vertex { position: [0.0; 3], normal: [0.0; 3] }; 4];
    for (vertex, corner) in vertices.iter_mut().zip(corners) {
        vertex.position = (n + corner).to_array();
        vertex.normal = normal;
    }
    (vertices, [0, 1, 2, 0, 2, 3])
}

pub fn cube_geometry() -> (Vec<Vertex>, Vec<u16>) {
    let faces = [
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ];

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, u, v) in faces {
        let (face_vertices, face_indices) = quad(normal, u, v);
        let base = vertices.len() as u16;
        vertices.extend_from_slice(&face_vertices);
        indices.extend(face_indices.iter().map(|index| base + index));
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::{cube_geometry, Vertex};

    #[test]
    fn cube_has_24_vertices_and_36_indices() {
        let (vertices, indices) = cube_geometry();
        assert_eq!(vertices.len(), 24);
        assert_eq!(indices.len(), 36);
        assert!(indices.iter().all(|i| (*i as usize) < vertices.len()));
    }

    #[test]
    fn cube_is_centered_and_unit_sized() {
        let (vertices, _) = cube_geometry();
        for vertex in &vertices {
            for component in vertex.position {
                assert!(component.abs() <= 0.5 + 1e-6, "component {component}");
            }
        }
        let min_y = vertices.iter().map(|v| v.position[1]).fold(f32::INFINITY, f32::min);
        let max_y = vertices.iter().map(|v| v.position[1]).fold(f32::NEG_INFINITY, f32::max);
        assert!((max_y - min_y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn vertex_layout_size() {
        assert_eq!(std::mem::size_of::<Vertex>(), 24);
    }
}
