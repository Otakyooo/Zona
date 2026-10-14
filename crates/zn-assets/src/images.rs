use crate::error::AssetError;
use crate::types::CpuImage;
use std::path::Path;

/// Декодирует изображение с диска в RGBA8. PNG/JPEG — через `image`, KTX2 —
/// через `ktx2` (uncompressed; BasisU не поддерживается — см. README).
///
/// `srgb` помечает base-color/emissive карты (glTF-конвенция); вызывающий код
/// решает, какой смысл несёт текстура.
pub fn decode_image(path: &Path, srgb: bool) -> Result<CpuImage, AssetError> {
    let bytes = std::fs::read(path)?;
    decode_image_bytes(&bytes, srgb).map_err(|error| match error {
        AssetError::UnsupportedFormat(detail) => {
            AssetError::UnsupportedFormat(format!("{}: {detail}", path.display()))
        }
        other => other,
    })
}

/// Декодирует изображение из байтов (PNG/JPEG/KTX2). Формат определяется
/// магическими байтами, не расширением.
pub fn decode_image_bytes(bytes: &[u8], srgb: bool) -> Result<CpuImage, AssetError> {
    if bytes.starts_with(&ktx2::MAGIC[..8]) {
        return decode_ktx2(bytes, srgb);
    }
    let image = image::load_from_memory(bytes)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(CpuImage::new(width, height, rgba.into_raw(), srgb))
}

fn decode_ktx2(bytes: &[u8], srgb: bool) -> Result<CpuImage, AssetError> {
    let reader = ktx2::Reader::new(bytes)
        .map_err(|error| AssetError::UnsupportedFormat(format!("некорректный KTX2: {error}")))?;
    let header = reader.header();

    if header.supercompression_scheme.is_some() {
        return Err(AssetError::UnsupportedFormat(
            "KTX2 с суперкомпрессией (BasisU/Zstandard/ZLIB) не поддерживается".into(),
        ));
    }

    let channels = match header.format {
        Some(ktx2::Format::R8_UNORM) => 1,
        Some(ktx2::Format::R8G8_UNORM) => 2,
        Some(ktx2::Format::R8G8B8_UNORM) => 3,
        Some(ktx2::Format::R8G8B8A8_UNORM) | Some(ktx2::Format::R8G8B8A8_SRGB) => 4,
        Some(ktx2::Format::R8_SRGB) => 1,
        Some(ktx2::Format::R8G8_SRGB) => 2,
        Some(ktx2::Format::R8G8B8_SRGB) => 3,
        Some(format) => {
            return Err(AssetError::UnsupportedFormat(format!(
                "KTX2-формат {value} не поддерживается",
                value = format.value()
            )));
        }
        None => {
            return Err(AssetError::UnsupportedFormat(
                "KTX2 без формата (BasisU) не поддерживается".into(),
            ));
        }
    };

    let width = header.pixel_width;
    let height = header.pixel_height.max(1);
    let expected = width as usize * height as usize * channels;
    let level = reader
        .levels()
        .next()
        .ok_or_else(|| AssetError::UnsupportedFormat("KTX2 без уровней".into()))?;
    let data = level.data;
    if data.len() < expected {
        return Err(AssetError::UnsupportedFormat(format!(
            "KTX2: данных {actual} < {expected}",
            actual = data.len()
        )));
    }

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for texel in data[..expected].chunks_exact(channels) {
        let (r, g, b) = match channels {
            1 => (texel[0], texel[0], texel[0]),
            2 => (texel[0], texel[0], texel[0]),
            3 => (texel[0], texel[1], texel[2]),
            _ => (texel[0], texel[1], texel[2]),
        };
        pixels.extend_from_slice(&[r, g, b, if channels == 4 { texel[3] } else { 255 }]);
    }

    Ok(CpuImage::new(width, height, pixels, srgb))
}

/// Сканирует директорию ассетов: glTF-источники и изображения.
pub fn scan_asset_directory(dir: &Path) -> Result<Vec<std::path::PathBuf>, AssetError> {
    let mut paths = Vec::new();
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .unwrap_or_default();
        if crate::is_supported_source(&path) || IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "ktx2"];

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn png_bytes(width: u32, height: u32, fill: Rgba<u8>) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        RgbaImage::from_pixel(width, height, fill)
            .write_to(&mut buffer, image::ImageFormat::Png)
            .unwrap();
        buffer.into_inner()
    }

    #[test]
    fn decodes_png_bytes_to_rgba8() {
        let bytes = png_bytes(2, 3, Rgba([10, 20, 30, 255]));
        let image = decode_image_bytes(&bytes, true).unwrap();
        assert_eq!((image.width, image.height), (2, 3));
        assert_eq!(image.pixels.len(), 2 * 3 * 4);
        assert!(image.srgb);
        assert_eq!(image.pixels[0..4], [10, 20, 30, 255]);
    }

    #[test]
    fn decodes_jpeg_bytes_to_rgba8() {
        // JPEG не хранит альфу: кодируем RGB-источник (Rgba-источник кодировщик
        // image 0.25 для JPEG не принимает).
        let mut buffer = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(4, 4, image::Rgb([200, 100, 50]))
            .write_to(&mut buffer, image::ImageFormat::Jpeg)
            .unwrap();
        let image = decode_image_bytes(&buffer.into_inner(), false).unwrap();
        assert_eq!((image.width, image.height), (4, 4));
        assert!(!image.srgb);
        let pixel = &image.pixels[0..3];
        for (got, expected) in pixel.iter().zip([200u8, 100, 50]) {
            assert!(
                (*got as i32 - expected as i32).abs() <= 24,
                "jpeg lossy: {got} vs {expected}"
            );
        }
    }

    #[test]
    fn ktx2_uncompressed_rgba8_decodes() {
        let file = ktx2_rgba8_fixture(2, 2);
        let image = decode_image_bytes(&file, true).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert!(image.srgb);
        assert_eq!(image.pixels.len(), 16);
        assert_eq!(&image.pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&image.pixels[4..8], &[0, 255, 0, 255]);
        assert_eq!(&image.pixels[8..12], &[0, 0, 255, 255]);
        assert_eq!(&image.pixels[12..16], &[255, 255, 0, 255]);
    }

    #[test]
    fn ktx2_srgb_format_decodes() {
        let bytes = ktx2_fixture(1, 1, ktx2::Format::R8G8B8A8_SRGB, &[1, 2, 3, 4]);
        let image = decode_image_bytes(&bytes, true).unwrap();
        assert_eq!(image.pixels, vec![1, 2, 3, 4]);
    }

    #[test]
    fn ktx2_r8_expands_to_rgba() {
        let bytes = ktx2_fixture(2, 1, ktx2::Format::R8_UNORM, &[16, 240]);
        let image = decode_image_bytes(&bytes, false).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, vec![16, 16, 16, 255, 240, 240, 240, 255]);
    }

    #[test]
    fn ktx2_supercompressed_rejected() {
        let mut bytes = ktx2_fixture(1, 1, ktx2::Format::R8G8B8A8_UNORM, &[1, 2, 3, 4]);
        // supercompression_scheme = BasisLZ (1) в байтах [44..48]
        bytes[44..48].copy_from_slice(&1u32.to_le_bytes());
        let error = decode_image_bytes(&bytes, false).unwrap_err();
        assert!(matches!(error, AssetError::UnsupportedFormat(_)));
    }

    #[test]
    fn ktx2_undefined_format_rejected() {
        let bytes = ktx2_fixture(1, 1, ktx2::Format::R8G8B8A8_UNORM, &[1, 2, 3, 4]);
        let mut bytes = bytes;
        bytes[12..16].copy_from_slice(&0u32.to_le_bytes()); // VK_FORMAT_UNDEFINED
        let error = decode_image_bytes(&bytes, false).unwrap_err();
        assert!(matches!(error, AssetError::UnsupportedFormat(_)));
    }

    #[test]
    fn rejects_unknown_bytes() {
        let error = decode_image_bytes(b"not an image", false).unwrap_err();
        assert!(matches!(error, AssetError::Image(_)));
    }

    /// Ручной байтовый фикстур KTX2: header + 1 уровень + DFD + данные.
    fn ktx2_fixture(width: u32, height: u32, format: ktx2::Format, pixels: &[u8]) -> Vec<u8> {
        let channels = match format {
            ktx2::Format::R8_UNORM | ktx2::Format::R8_SRGB => 1,
            ktx2::Format::R8G8_UNORM | ktx2::Format::R8G8_SRGB => 2,
            ktx2::Format::R8G8B8_UNORM | ktx2::Format::R8G8B8_SRGB => 3,
            _ => 4,
        };
        assert_eq!(pixels.len(), width as usize * height as usize * channels);

        let (basic, type_size) = ktx2::dfd::Basic::from_format(format).unwrap();
        let block = ktx2::dfd::Block::Basic(basic);
        let mut dfd_block = vec![0u8; block.serialized_length()];
        block.to_bytes(&mut dfd_block);
        let dfd_length = 4 + dfd_block.len() as u32;

        let dfd_offset = 80 + 24; // header + 1 level index entry
        let kvd_offset = dfd_offset + dfd_length;
        let level_offset = kvd_offset;

        let mut bytes = vec![0u8; level_offset as usize + pixels.len()];
        bytes[0..12].copy_from_slice(&ktx2::MAGIC);
        bytes[12..16].copy_from_slice(&format.value().to_le_bytes());
        bytes[16..20].copy_from_slice(&type_size.to_le_bytes());
        bytes[20..24].copy_from_slice(&width.to_le_bytes());
        bytes[24..28].copy_from_slice(&height.to_le_bytes());
        bytes[36..40].copy_from_slice(&1u32.to_le_bytes()); // face_count
        bytes[40..44].copy_from_slice(&1u32.to_le_bytes()); // level_count
        bytes[48..52].copy_from_slice(&dfd_offset.to_le_bytes());
        bytes[52..56].copy_from_slice(&dfd_length.to_le_bytes());
        bytes[56..60].copy_from_slice(&kvd_offset.to_le_bytes());
        bytes[64..72].copy_from_slice(&(level_offset as u64).to_le_bytes());
        // level index entry
        bytes[80..88].copy_from_slice(&(level_offset as u64).to_le_bytes());
        bytes[88..96].copy_from_slice(&(pixels.len() as u64).to_le_bytes());
        bytes[96..104].copy_from_slice(&(pixels.len() as u64).to_le_bytes());
        // DFD: total length + block
        bytes[dfd_offset as usize..dfd_offset as usize + 4]
            .copy_from_slice(&dfd_length.to_le_bytes());
        bytes[dfd_offset as usize + 4..level_offset as usize].copy_from_slice(&dfd_block);
        // level 0 data
        bytes[level_offset as usize..].copy_from_slice(pixels);
        bytes
    }

    fn ktx2_rgba8_fixture(width: u32, height: u32) -> Vec<u8> {
        let colors = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        let mut pixels = Vec::new();
        for index in 0..(width * height) {
            pixels.extend_from_slice(&colors[(index as usize) % colors.len()]);
        }
        ktx2_fixture(width, height, ktx2::Format::R8G8B8A8_UNORM, &pixels)
    }
}
