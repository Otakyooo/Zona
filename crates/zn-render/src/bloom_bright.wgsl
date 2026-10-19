// Bright-pass bloom (M4g-G3b): из HDR-кадра остаются только пиксели ярче
// порога — в ¼ разрешения, дальше размытие и подмешивание в пост-пассе.

struct Bright {
    threshold: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> bright: Bright;
@group(0) @binding(1) var hdr_tex: texture_2d<f32>;
@group(0) @binding(2) var hdr_sampler: sampler;

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.clip_position = vec4<f32>(positions[index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(hdr_tex));
    let uv = in.clip_position.xy / dims;
    let color = textureSample(hdr_tex, hdr_sampler, uv).rgb;

    // Порог по максимальному каналу: ниже порога вклад нулевой, выше —
    // переносится весь цвет (мягкой границы хватает за счёт размытия).
    let brightness = max(color.r, max(color.g, color.b));
    let scale = max(brightness - bright.threshold, 0.0) / max(brightness, 1e-4);
    return vec4<f32>(color * scale, 1.0);
}
