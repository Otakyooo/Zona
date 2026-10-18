//! ZONA asset pipeline: CPU-сторона импорта (glTF, PNG/JPEG/KTX2) и встроенные
//! процедурные примитивы. GPU-кэши живут в `zn-render`.

pub mod builtin;
pub mod error;
pub mod images;
pub mod import;
pub mod textures;
pub mod types;

pub use builtin::{
    builtin_key, builtin_mesh, builtin_meshes, parse_builtin_key, BUILTIN_MESH_NAMES,
};
pub use error::AssetError;
pub use images::{decode_image, decode_image_bytes, scan_asset_directory, IMAGE_EXTENSIONS};
pub use import::{apply_base_color_srgb, import_gltf};
pub use textures::{
    builtin_texture, parse_builtin_texture_key, BUILTIN_TEXTURE_NAMES, BUILTIN_TEXTURE_SIZE,
};
pub use types::{
    generate_mip_chain, AlphaMode, AssetVertex, Bounds, CpuImage, CpuMaterial, CpuMesh,
    ImportedAsset,
};

pub const SUPPORTED_SOURCE_FORMATS: &[&str] = &["gltf", "glb"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetImportReport {
    pub meshes: usize,
    pub materials: usize,
    pub textures: usize,
}

impl AssetImportReport {
    pub fn total_assets(&self) -> usize {
        self.meshes + self.materials + self.textures
    }
}

pub fn is_supported_source(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| SUPPORTED_SOURCE_FORMATS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use std::path::Path;

    #[test]
    fn supported_formats_are_detected() {
        assert!(is_supported_source(Path::new("scene.gltf")));
        assert!(is_supported_source(Path::new("scene.GLB")));
        assert!(!is_supported_source(Path::new("scene.fbx")));
        assert!(!is_supported_source(Path::new("scene")));
    }

    #[test]
    fn report_sums_assets() {
        let report = AssetImportReport {
            meshes: 3,
            materials: 2,
            textures: 1,
        };
        assert_eq!(report.total_assets(), 6);
    }

    #[test]
    fn gltf_triangle_with_node_matrix_imports() {
        // Треугольник: 3 позиции + 3 нормали + 3 индекса u16 (data-URI).
        let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let normals: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]];
        let mut buffer = Vec::new();
        for vertex in &positions {
            for component in vertex {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        for normal in &normals {
            for component in normal {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        for index in [0u16, 1, 2] {
            buffer.extend_from_slice(&index.to_le_bytes());
        }
        assert_eq!(buffer.len(), 78);
        let encoded = base64_encode(&buffer);

        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [ {{ "nodes": [0] }} ],
  "nodes": [
    {{ "mesh": 0, "matrix": [2,0,0,0, 0,2,0,0, 0,0,2,0, 10,20,30,1] }}
  ],
  "meshes": [
    {{
      "primitives": [
        {{ "attributes": {{ "POSITION": 0, "NORMAL": 1 }}, "indices": 2, "mode": 4 }}
      ]
    }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0,0,0], "max": [1,1,0] }},
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "byteOffset": 36 }},
    {{ "bufferView": 0, "componentType": 5123, "count": 3, "type": "SCALAR", "byteOffset": 72 }}
  ],
  "bufferViews": [ {{ "buffer": 0, "byteOffset": 0, "byteLength": 78 }} ],
  "buffers": [ {{ "byteLength": 78, "uri": "data:application/octet-stream;base64,{encoded}" }} ]
}}"#
        );

        let dir = std::env::temp_dir().join(format!("zn-assets-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("triangle.gltf");
        std::fs::write(&path, json).unwrap();

        let asset = import_gltf(&path).unwrap();
        assert_eq!(asset.meshes.len(), 1);
        let mesh = &asset.meshes[0];
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.indices, vec![0, 1, 2]);

        // Матрица узла: scale 2 + translate (10,20,30) применена к позициям.
        let first = Vec3::from_array(mesh.vertices[0].position);
        assert!((first - Vec3::new(10.0, 20.0, 30.0)).length() < 1e-4);
        let second = Vec3::from_array(mesh.vertices[1].position);
        assert!((second - Vec3::new(12.0, 20.0, 30.0)).length() < 1e-4);
        let third = Vec3::from_array(mesh.vertices[2].position);
        assert!((third - Vec3::new(10.0, 22.0, 30.0)).length() < 1e-4);

        // Нормаль +Z проходит через узловую матрицу без изменений направления.
        for vertex in &mesh.vertices {
            let normal = Vec3::from_array(vertex.normal);
            assert!((normal - Vec3::Z).length() < 1e-4, "normal = {normal}");
        }

        // Материалов/текстур в фикстуре нет.
        assert!(asset.materials.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gltf_missing_normals_get_flat_generated() {
        // Тот же треугольник, но без NORMAL-аксессора: UV/Normal отсутствуют.
        let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let mut buffer = Vec::new();
        for vertex in &positions {
            for component in vertex {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        for index in [0u16, 1, 2] {
            buffer.extend_from_slice(&index.to_le_bytes());
        }
        let encoded = base64_encode(&buffer);
        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [ {{ "nodes": [0] }} ],
  "nodes": [ {{ "mesh": 0 }} ],
  "meshes": [
    {{ "primitives": [ {{ "attributes": {{ "POSITION": 0 }}, "indices": 1, "mode": 4 }} ] }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0,0,0], "max": [1,0,1] }},
    {{ "bufferView": 0, "componentType": 5123, "count": 3, "type": "SCALAR", "byteOffset": 36 }}
  ],
  "bufferViews": [ {{ "buffer": 0, "byteOffset": 0, "byteLength": 42 }} ],
  "buffers": [ {{ "byteLength": 42, "uri": "data:application/octet-stream;base64,{encoded}" }} ]
}}"#
        );

        let dir = std::env::temp_dir().join(format!("zn-assets-nonormal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("flat.gltf");
        std::fs::write(&path, json).unwrap();

        let asset = import_gltf(&path).unwrap();
        let mesh = &asset.meshes[0];
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        for vertex in &mesh.vertices {
            let normal = Vec3::from_array(vertex.normal);
            assert!(
                (normal.length() - 1.0).abs() < 1e-5,
                "flat normal = {normal}"
            );
            assert!(
                normal.y.abs() > 0.5,
                "expected +Y face normal, got {normal}"
            );
            assert_eq!(vertex.uv, [0.0, 0.0]);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nested_node_matrices_accumulate() {
        let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut buffer = Vec::new();
        for vertex in &positions {
            for component in vertex {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        let encoded = base64_encode(&buffer);
        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [ {{ "nodes": [0] }} ],
  "nodes": [
    {{ "children": [1], "translation": [1, 0, 0] }},
    {{ "mesh": 0, "translation": [0, 2, 0] }}
  ],
  "meshes": [
    {{ "primitives": [ {{ "attributes": {{ "POSITION": 0 }}, "mode": 4 }} ] }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0,0,0], "max": [1,1,0] }}
  ],
  "bufferViews": [ {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }} ],
  "buffers": [ {{ "byteLength": 36, "uri": "data:application/octet-stream;base64,{encoded}" }} ]
}}"#
        );

        let dir = std::env::temp_dir().join(format!("zn-assets-nested-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nested.gltf");
        std::fs::write(&path, json).unwrap();

        let asset = import_gltf(&path).unwrap();
        let mesh = &asset.meshes[0];
        assert_eq!(mesh.indices.len(), 3); // индексы сгенерированы 0..3
        let first = Vec3::from_array(mesh.vertices[0].position);
        assert!(
            (first - Vec3::new(1.0, 2.0, 0.0)).length() < 1e-4,
            "{first}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gltf_primitives_keep_material_index() {
        let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut buffer = Vec::new();
        for vertex in &positions {
            for component in vertex {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        let encoded = base64_encode(&buffer);
        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [ {{ "nodes": [0] }} ],
  "nodes": [ {{ "mesh": 0 }} ],
  "materials": [
    {{ "name": "Первый", "pbrMetallicRoughness": {{ "baseColorFactor": [1, 0, 0, 1] }} }},
    {{ "name": "Второй", "pbrMetallicRoughness": {{ "baseColorFactor": [0, 0, 1, 1] }} }}
  ],
  "meshes": [
    {{ "primitives": [
      {{ "attributes": {{ "POSITION": 0 }}, "material": 0 }},
      {{ "attributes": {{ "POSITION": 0 }}, "material": 1 }}
    ] }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0,0,0], "max": [1,1,0] }}
  ],
  "bufferViews": [ {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }} ],
  "buffers": [ {{ "byteLength": 36, "uri": "data:application/octet-stream;base64,{encoded}" }} ]
}}"#
        );

        let dir = std::env::temp_dir().join(format!("zn-assets-materials-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("materials.gltf");
        std::fs::write(&path, json).unwrap();

        let asset = import_gltf(&path).unwrap();
        assert_eq!(asset.meshes.len(), 2);
        assert_eq!(asset.meshes[0].material, Some(0));
        assert_eq!(asset.meshes[1].material, Some(1));
        assert_eq!(asset.materials.len(), 2);
        assert_eq!(asset.materials[0].name.as_deref(), Some("Первый"));
        assert_eq!(asset.materials[1].name.as_deref(), Some("Второй"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn imported_images_get_mip_chain_and_srgb() {
        // 2×2 PNG (data-uri) как base-color карта: 1 мип + флаг sRGB.
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png_base64 = base64_encode(&png.into_inner());

        let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut buffer = Vec::new();
        for vertex in &positions {
            for component in vertex {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        let positions_base64 = base64_encode(&buffer);
        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [ {{ "nodes": [0] }} ],
  "nodes": [ {{ "mesh": 0 }} ],
  "images": [ {{ "uri": "data:image/png;base64,{png_base64}" }} ],
  "textures": [ {{ "source": 0 }} ],
  "materials": [ {{ "pbrMetallicRoughness": {{ "baseColorTexture": {{ "index": 0 }} }} }} ],
  "meshes": [
    {{ "primitives": [ {{ "attributes": {{ "POSITION": 0 }}, "material": 0 }} ] }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0,0,0], "max": [1,1,0] }}
  ],
  "bufferViews": [ {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }} ],
  "buffers": [ {{ "byteLength": 36, "uri": "data:application/octet-stream;base64,{positions_base64}" }} ]
}}"#
        );

        let dir = std::env::temp_dir().join(format!("zn-assets-mips-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("textured.gltf");
        std::fs::write(&path, json).unwrap();

        let asset = import_gltf(&path).unwrap();
        assert_eq!(asset.images.len(), 1);
        let image = &asset.images[0];
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.mips.len(), 1);
        assert_eq!(image.mips[0], vec![10, 20, 30, 255]);
        assert!(image.srgb);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glb_magic_is_detected() {
        let dir = std::env::temp_dir().join(format!("zn-assets-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.glb");
        std::fs::write(&path, b"not a glb").unwrap();
        assert!(import_gltf(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Минимальный base64-кодировщик (RFC 4648), чтобы не тянуть зависимость.
    fn base64_encode(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
            out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(triple >> 6) as usize & 63] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[triple as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}
