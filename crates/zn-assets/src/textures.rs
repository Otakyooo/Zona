//! Встроенные процедурные текстуры (M4g): генерируются на CPU, без внешних
//! файлов. Тайлятся бесшовно (периодический value-noise), детерминированы —
//! одни и те же байты на всех платформах.

use crate::types::CpuImage;

/// Сторона встроенной текстуры (квадрат).
pub const BUILTIN_TEXTURE_SIZE: u32 = 256;

/// Встроенные текстуры альбедо: ключ → имя.
pub const BUILTIN_TEXTURE_NAMES: &[&str] =
    &["concrete", "asphalt", "metal", "dirt", "checker", "noise"];

/// Парсит `builtin://` ключ текстуры; возвращает имя при её наличии.
pub fn parse_builtin_texture_key(key: &str) -> Option<&str> {
    key.strip_prefix("builtin://")
        .filter(|name| BUILTIN_TEXTURE_NAMES.contains(name))
}

/// Генерирует встроенную текстуру (RGBA8, sRGB) с мип-цепочкой.
/// `None` — неизвестное имя.
pub fn builtin_texture(name: &str) -> Option<CpuImage> {
    let size = BUILTIN_TEXTURE_SIZE;
    let image = match name {
        "concrete" => concrete(size),
        "asphalt" => asphalt(size),
        "metal" => metal(size),
        "dirt" => dirt(size),
        "checker" => checker(size),
        "noise" => noise(size),
        _ => return None,
    };
    Some(image.with_mips().with_name(format!("builtin://{name}")))
}

/// Вызывает `fill(x, y) -> [f32; 3]` (линейные значения 0..1) и кодирует в RGBA8.
fn generate(size: u32, mut fill: impl FnMut(u32, u32, f32, f32) -> [f32; 3]) -> CpuImage {
    let mut pixels = vec![255u8; (size * size * 4) as usize];
    let inv = 1.0 / size as f32;
    for y in 0..size {
        for x in 0..size {
            // u, v — центр текселя в [0, 1): периодический шум тайлится.
            let rgb = fill(x, y, (x as f32 + 0.5) * inv, (y as f32 + 0.5) * inv);
            let base = ((y * size + x) * 4) as usize;
            for (channel, value) in rgb.iter().enumerate() {
                pixels[base + channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    CpuImage::new(size, size, pixels, true)
}

/// Детерминированный целочисленный хэш (PCG-подобный) → [0, 1).
fn hash2(x: u32, y: u32, seed: u32) -> f32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Периодический value-noise: сетка `px × py` ячеек, зациклена на текстуре.
fn value_noise(u: f32, v: f32, px: u32, py: u32, seed: u32) -> f32 {
    let x = u * px as f32;
    let y = v * py as f32;
    let x0 = x.floor();
    let y0 = y.floor();
    let tx = smoothstep(x - x0);
    let ty = smoothstep(y - y0);
    let wrap = |cell: i64, period: i64| (cell.rem_euclid(period)) as u32;
    let (xi, yi) = (x0 as i64, y0 as i64);
    let v00 = hash2(wrap(xi, px as i64), wrap(yi, py as i64), seed);
    let v10 = hash2(wrap(xi + 1, px as i64), wrap(yi, py as i64), seed);
    let v01 = hash2(wrap(xi, px as i64), wrap(yi + 1, py as i64), seed);
    let v11 = hash2(wrap(xi + 1, px as i64), wrap(yi + 1, py as i64), seed);
    let a = v00 + (v10 - v00) * tx;
    let b = v01 + (v11 - v01) * tx;
    a + (b - a) * ty
}

/// Фрактальный шум: `octaves` октав с удвоением частоты и убыванием веса.
fn fbm(u: f32, v: f32, base_px: u32, base_py: u32, octaves: u32, seed: u32) -> f32 {
    let mut sum = 0.0;
    let mut weight = 0.0;
    let mut amplitude = 1.0;
    for octave in 0..octaves {
        let scale = 1u32 << octave;
        sum += value_noise(u, v, base_px * scale, base_py * scale, seed + octave) * amplitude;
        weight += amplitude;
        amplitude *= 0.5;
    }
    sum / weight
}

/// Мип-уровни встроенной текстуры (готовый CpuImage → GPU).
fn concrete(size: u32) -> CpuImage {
    generate(size, |_, _, u, v| {
        // Бетонные плиты: крупные пятна + мелкое зерно + тёмные швы сетки 4×4.
        let mottling = fbm(u, v, 4, 4, 4, 0x51);
        let grain = value_noise(u, v, 96, 96, 0x52);
        let speck = if value_noise(u, v, 128, 128, 0x53) > 0.972 {
            -0.10
        } else {
            0.0
        };
        let gx = (u * 4.0).fract();
        let gy = (v * 4.0).fract();
        let seam = if gx < 0.02 || gy < 0.02 { -0.13 } else { 0.0 };
        let value = 0.54 * (0.85 + 0.30 * mottling) + 0.07 * (grain - 0.5) + speck + seam;
        [value, value * 0.995, value * 0.97]
    })
}

/// Асфальт: тёмная основа, мелкое зерно, редкая светлая крошка.
fn asphalt(size: u32) -> CpuImage {
    generate(size, |_, _, u, v| {
        let patches = fbm(u, v, 3, 3, 3, 0x61);
        let grain = value_noise(u, v, 128, 128, 0x62);
        let pebble = if value_noise(u, v, 40, 40, 0x63) > 0.93 {
            0.10
        } else {
            0.0
        };
        let value = 0.16 * (0.8 + 0.5 * patches) + 0.05 * (grain - 0.5) + pebble;
        [value, value * 0.99, value * 0.96]
    })
}

/// Металл: горизонтальная шлифовка + пятна ржавчины.
fn metal(size: u32) -> CpuImage {
    generate(size, |_, _, u, v| {
        // Анизотропный шум: частая сетка по u, редкая по v — «шлифовка».
        let brushed = value_noise(u, v, 160, 4, 0x71);
        let mottling = fbm(u, v, 4, 4, 3, 0x72);
        let rust = fbm(u, v, 6, 6, 4, 0x73);
        let base = 0.42 * (0.9 + 0.2 * brushed) + 0.05 * (mottling - 0.5);
        // Ржавчина: участки с высоким fbm получают тёплый оттенок.
        let rust_amount = ((rust - 0.62) * 3.0).clamp(0.0, 1.0) * 0.8;
        let rust_color = [0.34, 0.18, 0.10];
        [
            base * (1.0 - rust_amount) + rust_color[0] * rust_amount,
            base * (1.0 - rust_amount) + rust_color[1] * rust_amount,
            base * (1.0 - rust_amount) + rust_color[2] * rust_amount,
        ]
    })
}

/// Земля: бурая с комками и тёмными впадинами.
fn dirt(size: u32) -> CpuImage {
    generate(size, |_, _, u, v| {
        let clumps = fbm(u, v, 5, 5, 4, 0x81);
        let pits = value_noise(u, v, 64, 64, 0x82);
        let stones = if value_noise(u, v, 48, 48, 0x83) > 0.95 {
            0.12
        } else {
            0.0
        };
        let dark = if pits < 0.30 { -0.08 } else { 0.0 };
        let value = 0.36 + 0.22 * (clumps - 0.5) + dark + stones;
        [value, value * 0.80, value * 0.58]
    })
}

/// Шахматка 8×8 — отладочная текстура проверки тайлинга и UV.
fn checker(size: u32) -> CpuImage {
    generate(size, |x, y, _, _| {
        let cell = (x * 8 / size + y * 8 / size) % 2;
        let value = if cell == 0 { 0.78 } else { 0.20 };
        [value, value, value]
    })
}

/// Чистый серый шум — база для тонирования цветом объекта.
fn noise(size: u32) -> CpuImage {
    generate(size, |_, _, u, v| {
        let value = 0.45 + 0.9 * (fbm(u, v, 6, 6, 4, 0x91) - 0.5);
        [value, value, value]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_and_validate() {
        assert_eq!(
            parse_builtin_texture_key("builtin://concrete"),
            Some("concrete")
        );
        assert_eq!(parse_builtin_texture_key("builtin://metal"), Some("metal"));
        assert_eq!(parse_builtin_texture_key("builtin://cube"), None);
        assert_eq!(parse_builtin_texture_key("concrete"), None);
        assert_eq!(parse_builtin_texture_key("builtin://nope"), None);
    }

    #[test]
    fn all_textures_generate_valid_images() {
        for name in BUILTIN_TEXTURE_NAMES {
            let image = builtin_texture(name).unwrap_or_else(|| panic!("нет текстуры {name}"));
            let size = BUILTIN_TEXTURE_SIZE;
            assert_eq!(image.width, size);
            assert_eq!(image.height, size);
            assert_eq!(image.pixels.len(), (size * size * 4) as usize);
            assert!(image.srgb, "{name}: альбедо должно быть sRGB");
            assert!(!image.mips.is_empty(), "{name}: ожидалась мип-цепочка");
            // Альфа непрозрачная у всех.
            for index in (3..image.pixels.len()).step_by(4) {
                assert_eq!(image.pixels[index], 255, "{name}: альфа должна быть 255");
            }
        }
    }

    #[test]
    fn textures_are_deterministic_and_distinct() {
        let first = builtin_texture("concrete").unwrap();
        let second = builtin_texture("concrete").unwrap();
        assert_eq!(first, second, "генерация должна быть детерминированной");
        assert_ne!(first, builtin_texture("asphalt").unwrap());
    }

    #[test]
    fn textures_are_not_uniform() {
        for name in BUILTIN_TEXTURE_NAMES {
            let image = builtin_texture(name).unwrap();
            let min = image.pixels.iter().step_by(4).copied().min().unwrap();
            let max = image.pixels.iter().step_by(4).copied().max().unwrap();
            assert!(
                max - min >= 20,
                "{name}: текстура слишком ровная ({min}..{max})"
            );
        }
    }

    #[test]
    fn checker_has_exact_cells() {
        let image = builtin_texture("checker").unwrap();
        let size = BUILTIN_TEXTURE_SIZE;
        let cell = size / 8;
        let pixel = |x: u32, y: u32| image.pixels[((y * size + x) * 4) as usize];
        // Ячейка (0,0) светлая, соседняя справа — тёмная, диагональная — светлая.
        assert!(pixel(cell / 2, cell / 2) > 180);
        assert!(pixel(cell + cell / 2, cell / 2) < 80);
        assert!(pixel(cell + cell / 2, cell + cell / 2) > 180);
    }

    #[test]
    fn noise_stays_in_range() {
        let image = builtin_texture("noise").unwrap();
        for value in image.pixels.iter().step_by(4).copied() {
            assert!(value > 0 && value < 255, "значение вне диапазона: {value}");
        }
    }
}
