// Постобработка (M4g-G3): HDR-кадр сцены из offscreen-таргета проходит
// тонмаппинг ACES, виньетку и кодирование в sRGB поверхности. Bloom
// подмешивается сюда же (M4g-G3b).

struct Post {
    vignette: f32,
    output_srgb: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> post: Post;
@group(0) @binding(1) var hdr_tex: texture_2d<f32>;
@group(0) @binding(2) var hdr_sampler: sampler;

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Полноэкранный треугольник в NDC (без вершинных буферов).
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.clip_position = vec4<f32>(positions[index], 0.0, 1.0);
    return out;
}

fn aces_fitted(x: vec3<f32>) -> vec3<f32> {
    // ACES-аппроксимация (Krzysztof Narkowicz), шаг в шаг с прежним pbr.wgsl.
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
    let dims = vec2<f32>(textureDimensions(hdr_tex));
    // Позиция фрагмента уже в пикселях кадра: (0,0) — левый верх.
    let uv = in.clip_position.xy / dims;

    var color = textureSample(hdr_tex, hdr_sampler, uv).rgb;

    // Виньетка: мягкое затемнение к краям (0 — выключена). Дистанция
    // нормирована так, что в углах кадра даёт 1.0.
    let centered = uv - vec2<f32>(0.5);
    let d = length(centered) * 1.41421356;
    color = color * (1.0 - post.vignette * smoothstep(0.45, 1.0, d));

    color = aces_fitted(color);
    if (post.output_srgb > 0.5) {
        color = linear_to_srgb(color);
    }
    return vec4<f32>(color, 1.0);
}
