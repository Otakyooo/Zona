use glam::{Mat4, Vec3};

/// Осевой bounding box в пространстве ассета/мира.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut any = false;
        for point in points {
            min = min.min(point);
            max = max.max(point);
            any = true;
        }
        if !any {
            return Self::EMPTY;
        }
        Self { min, max }
    }

    pub const EMPTY: Self = Self {
        min: Vec3::ZERO,
        max: Vec3::ZERO,
    };

    /// Трансформирует бокс матрицей: 8 углов → новый min/max.
    pub fn transformed(&self, matrix: Mat4) -> Self {
        let corners = [
            Vec3::new(self.min.x, self.min.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.max.z),
        ];
        Self::from_points(
            corners
                .into_iter()
                .map(|corner| matrix.project_point3(corner)),
        )
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn extents(&self) -> Vec3 {
        (self.max - self.min).max(Vec3::ZERO)
    }

    pub fn contains(&self, point: Vec3) -> bool {
        point.cmpge(self.min).all() && point.cmple(self.max).all()
    }
}

/// CPU-вершина: позиция, нормаль, UV (совместима с GPU-раскладкой zn-render).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct AssetVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

impl AssetVertex {
    pub fn new(position: Vec3, normal: Vec3, uv: [f32; 2]) -> Self {
        Self {
            position: position.to_array(),
            normal: normal.to_array(),
            uv,
        }
    }
}

/// CPU-меш: вершины, u32-индексы, bounds в пространстве ассета.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuMesh {
    pub name: Option<String>,
    pub vertices: Vec<AssetVertex>,
    pub indices: Vec<u32>,
    pub bounds: Bounds,
}

impl CpuMesh {
    pub fn new(vertices: Vec<AssetVertex>, indices: Vec<u32>) -> Self {
        let bounds = Bounds::from_points(vertices.iter().map(|v| Vec3::from_array(v.position)));
        Self {
            name: None,
            vertices,
            indices,
            bounds,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Пересчитывает flat-нормали по граням (для мешей без атрибута нормалей).
    pub fn recompute_flat_normals(&mut self) {
        let mut normals = vec![Vec3::ZERO; self.vertices.len()];
        for triangle in self.indices.chunks_exact(3) {
            let [i0, i1, i2] = [
                triangle[0] as usize,
                triangle[1] as usize,
                triangle[2] as usize,
            ];
            let p0 = Vec3::from_array(self.vertices[i0].position);
            let p1 = Vec3::from_array(self.vertices[i1].position);
            let p2 = Vec3::from_array(self.vertices[i2].position);
            let normal = (p1 - p0).cross(p2 - p0);
            if normal.length_squared() <= f32::EPSILON {
                continue; // вырожденный треугольник
            }
            for index in [i0, i1, i2] {
                normals[index] += normal;
            }
        }
        for (vertex, normal) in self.vertices.iter_mut().zip(normals) {
            if normal.length_squared() > f32::EPSILON {
                vertex.normal = normal.normalize().to_array();
            }
        }
    }

    pub fn vertex_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.vertices)
    }
}

/// Декодированное изображение: всегда RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuImage {
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// true — sRGB-кодировка (base-color/emissive), false — линейные данные.
    pub srgb: bool,
}

impl CpuImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>, srgb: bool) -> Self {
        Self {
            name: None,
            width,
            height,
            pixels,
            srgb,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Режим альфы материала (glTF alphaMode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

/// CPU-материал: metallic-roughness PBR + карты.
/// Индексы текстур указывают в `ImportedAsset::images`.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuMaterial {
    pub name: Option<String>,
    pub base_color_factor: [f32; 4],
    pub base_color_texture: Option<usize>,
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub metallic_roughness_texture: Option<usize>,
    pub normal_texture: Option<usize>,
    pub occlusion_texture: Option<usize>,
    pub emissive_factor: [f32; 3],
    pub emissive_texture: Option<usize>,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
}

impl Default for CpuMaterial {
    fn default() -> Self {
        Self {
            name: None,
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            base_color_texture: None,
            metallic_factor: 1.0,
            roughness_factor: 1.0,
            metallic_roughness_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            emissive_factor: [0.0, 0.0, 0.0],
            emissive_texture: None,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
        }
    }
}

/// Результат импорта glTF: меши, материалы, декодированные изображения.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedAsset {
    pub name: String,
    pub meshes: Vec<CpuMesh>,
    pub materials: Vec<CpuMaterial>,
    pub images: Vec<CpuImage>,
}

impl ImportedAsset {
    pub fn report(&self) -> crate::AssetImportReport {
        crate::AssetImportReport {
            meshes: self.meshes.len(),
            materials: self.materials.len(),
            textures: self.images.len(),
        }
    }

    /// Собирает общие bounds по всем мешам.
    pub fn bounds(&self) -> Bounds {
        self.meshes.iter().fold(Bounds::EMPTY, |acc, mesh| Bounds {
            min: acc.min.min(mesh.bounds.min),
            max: acc.max.max(mesh.bounds.max),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    fn unit_bounds() -> Bounds {
        Bounds {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        }
    }

    #[test]
    fn bounds_from_points_finds_min_max() {
        let bounds = Bounds::from_points([
            Vec3::new(-1.0, 0.0, 2.0),
            Vec3::new(3.0, -5.0, 1.0),
            Vec3::new(0.0, 1.0, 0.0),
        ]);
        assert_eq!(bounds.min, Vec3::new(-1.0, -5.0, 0.0));
        assert_eq!(bounds.max, Vec3::new(3.0, 1.0, 2.0));
    }

    #[test]
    fn bounds_empty_on_no_points() {
        let bounds = Bounds::from_points(std::iter::empty());
        assert_eq!(bounds, Bounds::EMPTY);
    }

    #[test]
    fn bounds_transformed_by_translation_and_scale() {
        let transformed = unit_bounds().transformed(Mat4::from_scale_rotation_translation(
            Vec3::splat(2.0),
            Quat::IDENTITY,
            Vec3::new(10.0, 0.0, -5.0),
        ));
        assert_eq!(transformed.min, Vec3::new(8.0, -2.0, -7.0));
        assert_eq!(transformed.max, Vec3::new(12.0, 2.0, -3.0));
    }

    #[test]
    fn bounds_transformed_by_rotation_grows_axis_aligned() {
        // Поворот на 90° вокруг Y: X ↔ Z
        let transformed = unit_bounds().transformed(Mat4::from_quat(Quat::from_rotation_y(
            std::f32::consts::FRAC_PI_2,
        )));
        assert!((transformed.min - Vec3::new(-1.0, -1.0, -1.0)).length() < 1e-5);
        assert!((transformed.max - Vec3::new(1.0, 1.0, 1.0)).length() < 1e-5);

        // 45° вокруг Y увеличивает AABB по X/Z в √2 раз
        let rotated = unit_bounds().transformed(Mat4::from_quat(Quat::from_rotation_y(
            std::f32::consts::FRAC_PI_4,
        )));
        let expected = std::f32::consts::SQRT_2;
        assert!(
            (rotated.max.x - expected).abs() < 1e-5,
            "x = {}",
            rotated.max.x
        );
        assert!(
            (rotated.max.z - expected).abs() < 1e-5,
            "z = {}",
            rotated.max.z
        );
        assert!((rotated.max.y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn bounds_helpers() {
        let bounds = Bounds {
            min: Vec3::new(0.0, 0.0, 0.0),
            max: Vec3::new(2.0, 4.0, 6.0),
        };
        assert_eq!(bounds.center(), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(bounds.extents(), Vec3::new(2.0, 4.0, 6.0));
        assert!(bounds.contains(Vec3::new(1.0, 2.0, 3.0)));
        assert!(!bounds.contains(Vec3::new(3.0, 0.0, 0.0)));
    }

    #[test]
    fn cpu_mesh_computes_bounds() {
        let mesh = CpuMesh::new(
            vec![
                AssetVertex::new(Vec3::new(-1.0, -1.0, -1.0), Vec3::Z, [0.0, 0.0]),
                AssetVertex::new(Vec3::new(1.0, 1.0, 1.0), Vec3::Z, [1.0, 1.0]),
            ],
            vec![],
        );
        assert_eq!(mesh.bounds.min, Vec3::new(-1.0, -1.0, -1.0));
        assert_eq!(mesh.bounds.max, Vec3::new(1.0, 1.0, 1.0));
        assert_eq!(
            mesh.vertex_bytes().len(),
            2 * std::mem::size_of::<AssetVertex>()
        );
    }
}
