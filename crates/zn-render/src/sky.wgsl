// Небо (M4g): градиент от горизонта к зениту + пятно солнца.
// Рисуется первым в главном проходе как фон (глубина не пишется).

struct Camera {
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>, // rgb — цвет солнца, w — интенсивность
    ambient: vec4<f32>,
    fog_color: vec4<f32>, // rgb — цвет тумана, w — плотность
    sky_top: vec4<f32>,
    sky_horizon: vec4<f32>,
    output_srgb: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Полноэкранный треугольник в NDC (без вершинных буферов).
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[index];
    var out: VsOut;
    out.clip_position = vec4<f32>(position, 1.0, 1.0);
    out.ndc = position;
    return out;
}

fn aces_fitted(x: vec3<f32>) -> vec3<f32> {
    // ACES-аппроксимация (Krzysztof Narkowicz), шаг в шаг с pbr.wgsl.
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    let clamped = clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
    return clamped * clamped;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = pow(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.4)) * 1.055 - vec3<f32>(0.055);
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Луч камеры для пикселя: восстанавливаем из NDC обратной VP.
    let near_point = camera.inverse_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far_point = camera.inverse_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(far_point.xyz / far_point.w - near_point.xyz / near_point.w);

    // Градиент по высоте луча; ниже горизонта — цвет тумана,
    // чтобы стык с дальним планом сцены не выдавал границу мира.
    let height = clamp(direction.y, 0.0, 1.0);
    var color = mix(camera.sky_horizon.rgb, camera.sky_top.rgb, pow(height, 0.65));
    let below = clamp(-direction.y * 3.0, 0.0, 1.0);
    color = mix(color, camera.fog_color.rgb, below);

    // Солнце: мягкий блик и ядро.
    let sun_amount = max(dot(direction, normalize(camera.light_dir.xyz)), 0.0);
    let glow = pow(sun_amount, 8.0) * 0.10 + pow(sun_amount, 128.0) * 0.60;
    color += camera.light_color.rgb * camera.light_color.w * glow;

    color = aces_fitted(color);
    if (camera.output_srgb > 0.5) {
        color = linear_to_srgb(color);
    }
    return vec4<f32>(color, 1.0);
}
