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
        let report = AssetImportReport { meshes: 3, materials: 2, textures: 1 };
        assert_eq!(report.total_assets(), 6);
    }
}
