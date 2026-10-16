use std::path::Path;

use glam::{Mat4, Vec3};

use crate::error::AssetError;
use crate::types::{AlphaMode, AssetVertex, CpuImage, CpuMaterial, CpuMesh, ImportedAsset};

/// Импортирует glTF/GLB: меши (мировые матрицы узлов), материалы, изображения.
///
/// Внешние `.bin`, data-URI и сцен-граф разрешает крейт `gltf`. Отсутствующие
/// нормали генерируются flat-методом, отсутствующие UV обнуляются.
pub fn import_gltf(path: &Path) -> Result<ImportedAsset, AssetError> {
    let (document, buffers, images) = gltf::import(path)?;

    let mut imported = ImportedAsset {
        name: file_stem(path),
        meshes: Vec::new(),
        materials: Vec::new(),
        images: Vec::new(),
    };

    for (index, image) in images.iter().enumerate() {
        imported
            .images
            .push(convert_image(image).with_name(format!("image{index}")));
    }
    for material in document.materials() {
        imported.materials.push(convert_material(&material));
    }

    let has_scenes = document.scenes().next().is_some();
    if has_scenes {
        for scene in document.scenes() {
            for node in scene.nodes() {
                walk_node(&node, Mat4::IDENTITY, &buffers, &mut imported);
            }
        }
    } else {
        for node in document.nodes() {
            walk_node(&node, Mat4::IDENTITY, &buffers, &mut imported);
        }
    }

    if imported.meshes.is_empty() {
        return Err(AssetError::Validation(format!(
            "в {} нет треугольных мешей",
            path.display()
        )));
    }

    apply_base_color_srgb(&mut imported);
    Ok(imported)
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "asset".into())
}

fn walk_node(
    node: &gltf::Node<'_>,
    parent_matrix: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut ImportedAsset,
) {
    // [[f32;4];4] в column-major порядке → glam::Mat4
    let array = node.transform().matrix();
    let local = Mat4::from_cols_array(&[
        array[0][0],
        array[0][1],
        array[0][2],
        array[0][3],
        array[1][0],
        array[1][1],
        array[1][2],
        array[1][3],
        array[2][0],
        array[2][1],
        array[2][2],
        array[2][3],
        array[3][0],
        array[3][1],
        array[3][2],
        array[3][3],
    ]);
    let matrix = parent_matrix * local;

    if let Some(mesh) = node.mesh() {
        let mesh_name = mesh.name().map(|name| name.to_string());
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                log::warn!(
                    "пропуск примитива с режимом {:?} (ожидается Triangles)",
                    primitive.mode()
                );
                continue;
            }
            if let Some(cpu_mesh) = convert_primitive(
                &primitive,
                matrix,
                buffers,
                mesh_name.as_deref(),
                node.name(),
            ) {
                out.meshes.push(cpu_mesh);
            }
        }
    }

    for child in node.children() {
        walk_node(&child, matrix, buffers, out);
    }
}

fn convert_primitive(
    primitive: &gltf::Primitive<'_>,
    matrix: Mat4,
    buffers: &[gltf::buffer::Data],
    mesh_name: Option<&str>,
    node_name: Option<&str>,
) -> Option<CpuMesh> {
    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|v| &**v));

    let positions: Vec<[f32; 3]> = reader.read_positions()?.collect();
    if positions.is_empty() {
        return None;
    }

    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|iterator| iterator.collect());
    let tex_coords: Option<Vec<[f32; 2]>> = reader
        .read_tex_coords(0)
        .map(|iterator| iterator.into_f32().collect());
    let indices: Vec<u32> = match reader.read_indices() {
        Some(iterator) => iterator.into_u32().collect(),
        None => (0..positions.len() as u32).collect(),
    };

    let normal_matrix = matrix.inverse().transpose();
    let mut vertices = Vec::with_capacity(positions.len());
    for index in 0..positions.len() {
        let position = matrix.transform_point3(Vec3::from_array(positions[index]));
        let normal = match &normals {
            Some(values) if index < values.len() => {
                let transformed = normal_matrix.transform_vector3(Vec3::from_array(values[index]));
                if transformed.length_squared() > f32::EPSILON {
                    transformed.normalize()
                } else {
                    Vec3::ZERO
                }
            }
            _ => Vec3::ZERO, // заполнится flat-проходом ниже
        };
        let uv = tex_coords
            .as_ref()
            .and_then(|values| values.get(index).copied())
            .unwrap_or([0.0, 0.0]);
        vertices.push(AssetVertex::new(position, normal, uv));
    }

    let mut mesh = CpuMesh::new(vertices, indices);
    mesh.material = primitive.material().index();
    if normals.is_none() {
        mesh.recompute_flat_normals();
    }
    let name = node_name.or(mesh_name).unwrap_or("mesh");
    mesh.name = Some(name.to_string());
    Some(mesh)
}

fn convert_image(image: &gltf::image::Data) -> CpuImage {
    let pixels = to_rgba8(image);
    // srgb проставляется только base-color картам — см. apply_base_color_srgb.
    CpuImage::new(image.width, image.height, pixels, false).with_mips()
}

fn to_rgba8(image: &gltf::image::Data) -> Vec<u8> {
    match image.format {
        gltf::image::Format::R8 => image
            .pixels
            .iter()
            .flat_map(|r| [*r, *r, *r, 255])
            .collect(),
        gltf::image::Format::R8G8 => image
            .pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|texel| [texel[0], texel[0], texel[0], 255])
            .collect(),
        gltf::image::Format::R8G8B8 => image
            .pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|texel| [texel[0], texel[1], texel[2], 255])
            .collect(),
        gltf::image::Format::R8G8B8A8 => image.pixels.clone(),
        // 16/32-битные форматы: оставляем старший байт канала.
        gltf::image::Format::R16 => image
            .pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|texel| [texel[1], texel[1], texel[1], 255])
            .collect(),
        gltf::image::Format::R16G16 => image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|texel| [texel[1], texel[1], texel[1], 255])
            .collect(),
        gltf::image::Format::R16G16B16 => image
            .pixels
            .as_chunks::<6>()
            .0
            .iter()
            .flat_map(|texel| [texel[1], texel[3], texel[5], 255])
            .collect(),
        gltf::image::Format::R16G16B16A16 => image
            .pixels
            .as_chunks::<8>()
            .0
            .iter()
            .flat_map(|texel| [texel[1], texel[3], texel[5], texel[7]])
            .collect(),
        gltf::image::Format::R32G32B32FLOAT => image
            .pixels
            .as_chunks::<12>()
            .0
            .iter()
            .flat_map(|texel| [texel[3], texel[7], texel[11], 255])
            .collect(),
        gltf::image::Format::R32G32B32A32FLOAT => image
            .pixels
            .as_chunks::<16>()
            .0
            .iter()
            .flat_map(|texel| [texel[3], texel[7], texel[11], texel[15]])
            .collect(),
    }
}

fn convert_material(material: &gltf::Material<'_>) -> CpuMaterial {
    let pbr = material.pbr_metallic_roughness();
    let mut cpu = CpuMaterial {
        name: material.name().map(|name| name.to_string()),
        base_color_factor: pbr.base_color_factor(),
        base_color_texture: pbr
            .base_color_texture()
            .map(|info| texture_source_index(info.texture())),
        metallic_factor: pbr.metallic_factor(),
        roughness_factor: pbr.roughness_factor(),
        metallic_roughness_texture: pbr
            .metallic_roughness_texture()
            .map(|info| texture_source_index(info.texture())),
        normal_texture: material
            .normal_texture()
            .map(|texture| texture_source_index(texture.texture())),
        occlusion_texture: material
            .occlusion_texture()
            .map(|texture| texture_source_index(texture.texture())),
        emissive_factor: material.emissive_factor(),
        emissive_texture: material
            .emissive_texture()
            .map(|info| texture_source_index(info.texture())),
        alpha_mode: match material.alpha_mode() {
            gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
            gltf::material::AlphaMode::Mask => AlphaMode::Mask,
            gltf::material::AlphaMode::Blend => AlphaMode::Blend,
        },
        double_sided: material.double_sided(),
    };
    cpu.normalize_metallic_roughness();
    cpu
}

impl CpuMaterial {
    /// Клампит факторы в допустимые диапазоны (валидность после импорта).
    pub fn normalize_metallic_roughness(&mut self) {
        self.metallic_factor = self.metallic_factor.clamp(0.0, 1.0);
        self.roughness_factor = self.roughness_factor.clamp(0.0, 1.0);
        self.base_color_factor[0] = self.base_color_factor[0].clamp(0.0, 1.0);
        self.base_color_factor[1] = self.base_color_factor[1].clamp(0.0, 1.0);
        self.base_color_factor[2] = self.base_color_factor[2].clamp(0.0, 1.0);
        self.base_color_factor[3] = self.base_color_factor[3].clamp(0.0, 1.0);
    }
}

fn texture_source_index(texture: gltf::texture::Texture<'_>) -> usize {
    texture.source().index()
}

/// Помечает base-color изображения как sRGB в импортированном ассете.
pub fn apply_base_color_srgb(asset: &mut ImportedAsset) {
    let base_color_images: Vec<usize> = asset
        .materials
        .iter()
        .filter_map(|material| material.base_color_texture)
        .collect();
    for index in base_color_images {
        if let Some(image) = asset.images.get_mut(index) {
            image.srgb = true;
        }
    }
}
