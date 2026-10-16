use crate::types::{AssetVertex, CpuMesh};
use glam::Vec3;

/// Встроенные процедурные примитивы: ключ → имя.
pub const BUILTIN_MESH_NAMES: &[&str] = &["cube", "sphere", "cylinder", "capsule", "quad"];

/// Число сегментов у встроенных сферы и цилиндра.
pub const CYLINDER_SEGMENTS: u32 = 24;
pub const SPHERE_RINGS: u32 = 12;
pub const SPHERE_SEGMENTS: u32 = 16;
/// Число колец на каждую полусферу встроенной капсулы.
pub const CAPSULE_CAP_RINGS: u32 = 8;

/// Ключ builtin-ассета (`builtin://<name>`).
pub fn builtin_key(name: &str) -> String {
    format!("builtin://{name}")
}

/// Парсит `builtin://` ключ; возвращает имя примитива.
pub fn parse_builtin_key(key: &str) -> Option<&str> {
    key.strip_prefix("builtin://")
        .filter(|name| BUILTIN_MESH_NAMES.contains(name))
}

/// Меш встроенного примитива по имени или `None`.
pub fn builtin_mesh(name: &str) -> Option<CpuMesh> {
    let mesh = match name {
        "cube" => cube(),
        "sphere" => sphere(SPHERE_SEGMENTS, SPHERE_RINGS),
        "cylinder" => cylinder(CYLINDER_SEGMENTS),
        "capsule" => capsule(CYLINDER_SEGMENTS, CAPSULE_CAP_RINGS),
        "quad" => quad(),
        _ => return None,
    };
    Some(mesh.with_name(name))
}

/// Все встроенные меши (статичны для времени жизни приложения).
pub fn builtin_meshes() -> Vec<CpuMesh> {
    BUILTIN_MESH_NAMES
        .iter()
        .map(|name| builtin_mesh(name).expect("builtin mesh"))
        .collect()
}

/// Единичный куб [-0.5, 0.5]^3 с UV по граням.
pub fn cube() -> CpuMesh {
    let faces = [
        // normal, u, v
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
        let n = Vec3::from_array(normal) * 0.5;
        let u = Vec3::from_array(u) * 0.5;
        let v = Vec3::from_array(v) * 0.5;
        let corners = [(-u - v), (u - v), (u + v), (-u + v)];
        let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        for (corner, uv) in corners.into_iter().zip(uvs) {
            vertices.push(AssetVertex::new(n + corner, n * 2.0, uv));
        }
        let base = vertices.len() as u32 - 4;
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    CpuMesh::new(vertices, indices)
}

/// Сфера единичного диаметра (радиус 0.5, как у остальных примитивов);
/// UV-параметризация: u по долготе, v по широте.
pub fn sphere(segments: u32, rings: u32) -> CpuMesh {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let theta = v * std::f32::consts::PI;
        for segment in 0..=segments {
            let u = segment as f32 / segments as f32;
            let phi = u * std::f32::consts::TAU;
            let direction = Vec3::new(
                theta.sin() * phi.cos(),
                theta.cos(),
                theta.sin() * phi.sin(),
            );
            vertices.push(AssetVertex::new(direction * 0.5, direction, [u, v]));
        }
    }
    let stride = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let a = ring * stride + segment;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    CpuMesh::new(vertices, indices)
}

/// Цилиндр вдоль Y: радиус 0.5, высота 1, крышки и боковая поверхность.
pub fn cylinder(segments: u32) -> CpuMesh {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let segments = segments.max(3);

    for segment in 0..segments {
        let angle = segment as f32 / segments as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let position = Vec3::new(cos * 0.5, -0.5, sin * 0.5);
        vertices.push(AssetVertex::new(
            position,
            Vec3::new(cos, 0.0, sin),
            [segment as f32 / segments as f32, 0.0],
        ));
        let top = Vec3::new(cos * 0.5, 0.5, sin * 0.5);
        vertices.push(AssetVertex::new(
            top,
            Vec3::new(cos, 0.0, sin),
            [segment as f32 / segments as f32, 1.0],
        ));
    }
    for segment in 0..segments {
        let a = segment * 2;
        let b = a + 1;
        let c = (a + 2) % (segments * 2);
        let d = (a + 3) % (segments * 2);
        indices.extend_from_slice(&[a, b, c, b, d, c]);
    }

    let top_center = vertices.len() as u32;
    vertices.push(AssetVertex::new(
        Vec3::new(0.0, 0.5, 0.0),
        Vec3::Y,
        [0.5, 0.5],
    ));
    for segment in 0..segments {
        let angle = segment as f32 / segments as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        vertices.push(AssetVertex::new(
            Vec3::new(cos * 0.5, 0.5, sin * 0.5),
            Vec3::Y,
            [cos * 0.5 + 0.5, sin * 0.5 + 0.5],
        ));
        indices.extend_from_slice(&[
            top_center,
            top_center + 1 + segment,
            top_center + 1 + (segment + 1) % segments,
        ]);
    }
    let bottom_center = vertices.len() as u32;
    vertices.push(AssetVertex::new(
        Vec3::new(0.0, -0.5, 0.0),
        -Vec3::Y,
        [0.5, 0.5],
    ));
    for segment in 0..segments {
        let angle = segment as f32 / segments as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        vertices.push(AssetVertex::new(
            Vec3::new(cos * 0.5, -0.5, sin * 0.5),
            -Vec3::Y,
            [cos * 0.5 + 0.5, sin * 0.5 + 0.5],
        ));
        indices.extend_from_slice(&[
            bottom_center,
            bottom_center + 1 + (segment + 1) % segments,
            bottom_center + 1 + segment,
        ]);
    }

    CpuMesh::new(vertices, indices)
}

/// Капсула вдоль Y: радиус 0.5, цилиндрическая часть высотой 1, полусферы
/// сверху и снизу (полная высота 2). Нормали гладкие, экватор без шва.
pub fn capsule(segments: u32, cap_rings: u32) -> CpuMesh {
    let segments = segments.max(3);
    let cap_rings = cap_rings.max(1);
    let radius = 0.5f32;
    let half_height = 0.5f32;
    let half_pi = std::f32::consts::FRAC_PI_2;

    // Профиль снизу вверх: (y, радиальный масштаб 0..1, y-компонента нормали).
    let mut profile: Vec<(f32, f32, f32)> = Vec::with_capacity(cap_rings as usize * 2 + 2);
    profile.push((-half_height - radius, 0.0, -1.0));
    for i in 1..=cap_rings {
        let psi = half_pi * (1.0 - i as f32 / cap_rings as f32);
        profile.push((-half_height - radius * psi.sin(), psi.cos(), -psi.sin()));
    }
    profile.push((half_height, 1.0, 0.0));
    for i in 1..=cap_rings {
        let psi = half_pi * (i as f32 / cap_rings as f32);
        profile.push((half_height + radius * psi.sin(), psi.cos(), psi.sin()));
    }

    let rows = profile.len() as u32;
    let stride = segments + 1;
    let mut vertices = Vec::with_capacity(rows as usize * stride as usize);
    let mut indices = Vec::new();
    for (row, &(y, radial, normal_y)) in profile.iter().enumerate() {
        let v = row as f32 / (rows - 1) as f32;
        for segment in 0..=segments {
            let u = segment as f32 / segments as f32;
            let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
            vertices.push(AssetVertex::new(
                Vec3::new(cos * radius * radial, y, sin * radius * radial),
                Vec3::new(cos * radial, normal_y, sin * radial),
                [u, v],
            ));
        }
    }
    // Ряды идут снизу вверх — по сравнению со sphere() обход перевёрнут,
    // поэтому индексы образуют те же внешние треугольники в порядке [c, a, d, d, a, b].
    for row in 0..rows - 1 {
        for segment in 0..segments {
            let a = row * stride + segment;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices.extend_from_slice(&[c, a, d, d, a, b]);
        }
    }

    CpuMesh::new(vertices, indices)
}

/// Квад в плоскости XZ, нормаль вверх (+Y), размер 1x1.
pub fn quad() -> CpuMesh {
    let vertices = vec![
        AssetVertex::new(Vec3::new(-0.5, 0.0, -0.5), Vec3::Y, [0.0, 1.0]),
        AssetVertex::new(Vec3::new(0.5, 0.0, -0.5), Vec3::Y, [1.0, 1.0]),
        AssetVertex::new(Vec3::new(0.5, 0.0, 0.5), Vec3::Y, [1.0, 0.0]),
        AssetVertex::new(Vec3::new(-0.5, 0.0, 0.5), Vec3::Y, [0.0, 0.0]),
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];
    CpuMesh::new(vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_unit_bounds(mesh: &CpuMesh) {
        assert!(mesh.bounds.min.cmpge(Vec3::splat(-0.501)).all());
        assert!(mesh.bounds.max.cmple(Vec3::splat(0.501)).all());
    }

    #[test]
    fn cube_geometry_and_bounds() {
        let mesh = builtin_mesh("cube").unwrap();
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_unit_bounds(&mesh);
        assert!(mesh
            .indices
            .iter()
            .all(|i| (*i as usize) < mesh.vertices.len()));
        for vertex in &mesh.vertices {
            let normal = Vec3::from_array(vertex.normal);
            assert!((normal.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn sphere_is_closed_and_normalized() {
        let mesh = builtin_mesh("sphere").unwrap();
        assert!(mesh.indices.len().is_multiple_of(3));
        for vertex in &mesh.vertices {
            let p = Vec3::from_array(vertex.position);
            let n = Vec3::from_array(vertex.normal);
            assert!((p.length() - 0.5).abs() < 1e-4, "radius = {}", p.length());
            assert!((n.length() - 1.0).abs() < 1e-4);
        }
        assert_unit_bounds(&mesh);
    }

    #[test]
    fn cylinder_has_caps_and_side() {
        let segments = CYLINDER_SEGMENTS;
        let mesh = builtin_mesh("cylinder").unwrap();
        assert_eq!(
            mesh.vertices.len(),
            segments as usize * 2 + 2 + segments as usize * 2
        );
        assert_eq!(
            mesh.indices.len(),
            (segments * 6 + segments * 3 + segments * 3) as usize
        );
        assert_unit_bounds(&mesh);
    }

    #[test]
    fn quad_faces_up() {
        let mesh = builtin_mesh("quad").unwrap();
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
        for vertex in &mesh.vertices {
            assert_eq!(vertex.normal, [0.0, 1.0, 0.0]);
        }
        assert_unit_bounds(&mesh);
    }

    #[test]
    fn capsule_spans_unit_radius_and_full_height() {
        let mesh = builtin_mesh("capsule").unwrap();
        assert_eq!(mesh.indices.len() % 3, 0);
        assert!((mesh.bounds.min.y + 1.0).abs() < 1e-3, "{:?}", mesh.bounds);
        assert!((mesh.bounds.max.y - 1.0).abs() < 1e-3, "{:?}", mesh.bounds);
        assert!(mesh.bounds.min.x >= -0.501 && mesh.bounds.max.x <= 0.501);
        assert!(mesh.bounds.min.z >= -0.501 && mesh.bounds.max.z <= 0.501);
        for vertex in &mesh.vertices {
            let position = Vec3::from_array(vertex.position);
            let normal = Vec3::from_array(vertex.normal);
            assert!((normal.length() - 1.0).abs() < 1e-4, "normal {normal:?}");
            // Цилиндрическая часть: точка лежит на радиусе 0.5.
            if position.y.abs() <= 0.5 + 1e-4 {
                let radial = (position.x * position.x + position.z * position.z).sqrt();
                assert!((radial - 0.5).abs() < 1e-3, "radial {radial}");
            }
        }
    }

    #[test]
    fn builtin_keys_roundtrip() {
        assert_eq!(parse_builtin_key("builtin://cube"), Some("cube"));
        assert_eq!(parse_builtin_key("builtin://sphere"), Some("sphere"));
        assert_eq!(parse_builtin_key("builtin://cylinder"), Some("cylinder"));
        assert_eq!(parse_builtin_key("builtin://capsule"), Some("capsule"));
        assert_eq!(parse_builtin_key("builtin://quad"), Some("quad"));
        assert_eq!(parse_builtin_key("builtin://teapot"), None);
        assert_eq!(parse_builtin_key("assets/teapot.gltf"), None);
        assert_eq!(builtin_key("cube"), "builtin://cube");
        assert_eq!(builtin_meshes().len(), 5);
    }

    #[test]
    fn flat_normals_regenerated_for_imported_geometry() {
        let mut mesh = builtin_mesh("cube").unwrap();
        for vertex in &mut mesh.vertices {
            vertex.normal = [0.0, 0.0, 0.0];
        }
        mesh.recompute_flat_normals();
        for vertex in &mesh.vertices {
            let n = Vec3::from_array(vertex.normal);
            assert!(n.length() > 0.5, "normal not recomputed");
        }
    }
}
