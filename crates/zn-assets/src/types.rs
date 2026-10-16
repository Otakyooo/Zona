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
    /// Индекс материала примитива в `ImportedAsset::materials` (None — дефолтный).
    pub material: Option<usize>,
}

impl CpuMesh {
    pub fn new(vertices: Vec<AssetVertex>, indices: Vec<u32>) -> Self {
        let bounds = Bounds::from_points(vertices.iter().map(|v| Vec3::from_array(v.position)));
        Self {
            name: None,
            vertices,
            indices,
            bounds,
            material: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Пересчитывает flat-нормали по граням (для мешей без атрибута нормалей).
    pub fn recompute_flat_normals(&mut self) {
        let mut normals = vec![Vec3::ZERO; self.vertices.len()];
        for triangle in self.indices.as_chunks::<3>().0 {
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
    /// Мип-уровни 1..N (RGBA8, каждый вдвое меньше предыдущего, последний — 1×1).
    pub mips: Vec<Vec<u8>>,
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
            mips: Vec::new(),
            srgb,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Досчитывает мип-цепочку box-фильтром (если она ещё не построена).
    pub fn with_mips(mut self) -> Self {
        if self.mips.is_empty() {
            self.mips = generate_mip_chain(self.width, self.height, &self.pixels);
        }
        self
    }
}

/// Строит мип-цепочку box-фильтром 2×2 для RGBA8-изображения.
///
/// Каждый следующий уровень — не менее 1×1; нечётные размеры округляются
/// вниз (последняя строка/столбец не участвуют). Возвращает уровни 1..N.
pub fn generate_mip_chain(width: u32, height: u32, pixels: &[u8]) -> Vec<Vec<u8>> {
    let mut mips = Vec::new();
    let mut current = pixels.to_vec();
    let (mut w, mut h) = (width.max(1), height.max(1));
    while w > 1 || h > 1 {
        let nw = (w / 2).max(1);
        let nh = (h / 2).max(1);
        let mut next = vec![0u8; nw as usize * nh as usize * 4];
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = [0u32; 4];
                for (dy, dx) in [(0u32, 0u32), (0, 1), (1, 0), (1, 1)] {
                    // Сторона 1: 2×2-блок вырождается, тексель дублируется.
                    let sx = (x * 2 + dx).min(w - 1);
                    let sy = (y * 2 + dy).min(h - 1);
                    let base = ((sy * w + sx) * 4) as usize;
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += current[base + channel] as u32;
                    }
                }
                let out = ((y * nw + x) * 4) as usize;
                for (channel, value) in next[out..out + 4].iter_mut().enumerate() {
                    *value = (sum[channel] / 4) as u8;
                }
            }
        }
        mips.push(next.clone());
        current = next;
        w = nw;
        h = nh;
    }
    mips
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
        assert_eq!(mesh.material, None);
        assert_eq!(
            mesh.vertex_bytes().len(),
            2 * std::mem::size_of::<AssetVertex>()
        );
    }

    #[test]
    fn mip_chain_halves_until_one_texel() {
        let pixels = vec![128u8; 4 * 4 * 4];
        let mips = generate_mip_chain(4, 4, &pixels);
        assert_eq!(mips.len(), 2);
        assert_eq!(mips[0].len(), 2 * 2 * 4);
        assert_eq!(mips[1].len(), 4);
        assert!(mips.iter().flatten().all(|&value| value == 128));
    }

    #[test]
    fn mip_chain_averages_2x2_blocks() {
        // Красный, зелёный, синий, белый → средние (127, 127, 127, 255).
        #[rustfmt::skip]
        let pixels = vec![
            255, 0, 0, 255,      // (0,0)
            0, 255, 0, 255,      // (1,0)
            0, 0, 255, 255,      // (0,1)
            255, 255, 255, 255,  // (1,1)
        ];
        let mips = generate_mip_chain(2, 2, &pixels);
        assert_eq!(mips.len(), 1);
        assert_eq!(mips[0], vec![127, 127, 127, 255]);
    }

    #[test]
    fn mip_chain_supports_non_pow2() {
        // 3×3 → 1×1: усредняются первые 2×2 текселя, край отбрасывается.
        let mut pixels = vec![0u8; 3 * 3 * 4];
        for (x, y) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
            pixels[(y * 3 + x) * 4] = 255;
        }
        let mips = generate_mip_chain(3, 3, &pixels);
        assert_eq!(mips, vec![vec![255, 0, 0, 0]]);
    }

    #[test]
    fn mip_chain_of_one_texel_is_empty() {
        assert!(generate_mip_chain(1, 1, &[1, 2, 3, 4]).is_empty());
    }

    #[test]
    fn mip_chain_handles_single_row_and_column() {
        // 2×1 → 1×1 и 1×2 → 1×1: тексели усредняются, вырожденная сторона дублируется.
        let pixels = vec![0u8, 0, 0, 0, 200, 0, 0, 0];
        assert_eq!(generate_mip_chain(2, 1, &pixels), vec![vec![100, 0, 0, 0]]);
        let pixels = vec![40u8, 0, 0, 0, 80, 0, 0, 0];
        assert_eq!(generate_mip_chain(1, 2, &pixels), vec![vec![60, 0, 0, 0]]);
    }

    #[test]
    fn image_with_mips_builds_chain_once() {
        let image = CpuImage::new(2, 2, vec![0; 16], true).with_mips();
        assert_eq!(image.mips.len(), 1);
        assert_eq!(image.mips[0].len(), 4);
        // Уже построенная цепочка не пересчитывается.
        let mut manual = CpuImage::new(2, 2, vec![0; 16], true);
        manual.mips = vec![vec![9, 9, 9, 9]];
        assert_eq!(manual.with_mips().mips, vec![vec![9, 9, 9, 9]]);
    }
}
