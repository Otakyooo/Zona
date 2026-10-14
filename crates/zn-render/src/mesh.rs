use bytemuck::{Pod, Zeroable};

use crate::renderer::cache::MeshId;
use zn_assets::{Bounds, CpuMesh};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 12,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 24,
            shader_location: 2,
        },
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

impl From<zn_assets::AssetVertex> for Vertex {
    fn from(vertex: zn_assets::AssetVertex) -> Self {
        Self {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
        }
    }
}

impl From<Vertex> for zn_assets::AssetVertex {
    fn from(vertex: Vertex) -> Self {
        zn_assets::AssetVertex {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
        }
    }
}

/// GPU-меш: vertex/index буферы + bounds (+ размер для бюджетов кэша).
pub struct GpuMesh {
    pub id: MeshId,
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
    pub bounds: Bounds,
    pub size_bytes: u64,
    pub name: Option<String>,
}

impl std::fmt::Debug for GpuMesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuMesh")
            .field("id", &self.id)
            .field("index_count", &self.index_count)
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

fn quad(normal: [f32; 3], u: [f32; 3], v: [f32; 3], uvs: [[f32; 2]; 4]) -> ([Vertex; 4], [u16; 6]) {
    let n = glam::Vec3::from_array(normal) * 0.5;
    let u = glam::Vec3::from_array(u) * 0.5;
    let v = glam::Vec3::from_array(v) * 0.5;
    let corners = [(-u - v), (u - v), (u + v), (-u + v)];
    let mut vertices = [Vertex {
        position: [0.0; 3],
        normal: [0.0; 3],
        uv: [0.0; 2],
    }; 4];
    for (vertex, (corner, uv)) in vertices.iter_mut().zip(corners.into_iter().zip(uvs)) {
        vertex.position = (n + corner).to_array();
        vertex.normal = normal;
        vertex.uv = uv;
    }
    (vertices, [0, 1, 2, 0, 2, 3])
}

/// Процедурный единичный куб [-0.5, 0.5]^3 (совместим с `builtin://cube`).
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
        let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        let (face_vertices, face_indices) = quad(normal, u, v, uvs);
        let base = vertices.len() as u16;
        vertices.extend_from_slice(&face_vertices);
        indices.extend(face_indices.iter().map(|index| base + index));
    }
    (vertices, indices)
}

/// CPU-меш (zn-assets) в вершинный формат рендера (раскладка совпадает).
pub fn cpu_mesh_vertices(mesh: &CpuMesh) -> Vec<Vertex> {
    mesh.vertices.iter().copied().map(Vertex::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

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
        let min_y = vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::INFINITY, f32::min);
        let max_y = vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((max_y - min_y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cube_uvs_span_unit_square() {
        let (vertices, _) = cube_geometry();
        for vertex in &vertices {
            assert!((0.0..=1.0).contains(&vertex.uv[0]));
            assert!((0.0..=1.0).contains(&vertex.uv[1]));
        }
    }

    #[test]
    fn vertex_layout_matches_asset_vertex() {
        assert_eq!(std::mem::size_of::<Vertex>(), 32);
        assert_eq!(
            std::mem::size_of::<zn_assets::AssetVertex>(),
            std::mem::size_of::<Vertex>()
        );
    }

    #[test]
    fn cpu_mesh_converts_to_render_vertices() {
        let cpu = CpuMesh::new(
            vec![zn_assets::AssetVertex::new(
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::Y,
                [0.5, 0.5],
            )],
            vec![],
        );
        let vertices = cpu_mesh_vertices(&cpu);
        assert_eq!(vertices.len(), 1);
        assert_eq!(vertices[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(vertices[0].normal, [0.0, 1.0, 0.0]);
        assert_eq!(vertices[0].uv, [0.5, 0.5]);
    }

    #[test]
    fn render_vertex_converts_back_to_asset_vertex() {
        let vertex = Vertex {
            position: [0.0, 0.0, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        };
        let asset: zn_assets::AssetVertex = vertex.into();
        assert_eq!(
            asset,
            zn_assets::AssetVertex::new(Vec3::ZERO, Vec3::Y, [0.0, 0.0])
        );
    }
}
