// Небо (M4g): градиент от горизонта к зениту + пятно солнца.
// Рисуется первым в главном проходе как фон (глубина не пишется).

struct Camera {
    light_view_proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>, // rgb — цвет солнца, w — интенсивность
    ambient: vec4<f32>,
    fog_color: vec4<f32>, // rgb — цвет тумана, w — плотность
    sky_top: vec4<f32>,
    sky_horizon: vec4<f32>,
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

    // Солнце: узкое ядро и совсем слабый ореол.
    let sun_amount = max(dot(direction, normalize(camera.light_dir.xyz)), 0.0);
    let glow = pow(sun_amount, 12.0) * 0.05 + pow(sun_amount, 256.0) * 0.8;
    color += camera.light_color.rgb * camera.light_color.w * glow;

    // Тонмаппинг (ACES) и кодирование делает пост-пасс (M4g-G3): небо
    // отдаёт линейный HDR-цвет.
    return vec4<f32>(color, 1.0);
}
