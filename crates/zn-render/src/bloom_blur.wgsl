// Разделимое гауссово размытие bloom (M4g-G3b): за проход одно направление,
// его задаёт uniform (горизонталь или вертикаль). 9 отсчётов.

struct Blur {
    step: vec2<f32>,
    _pad0: vec2<f32>,
};

@group(0) @binding(0) var<uniform> blur: Blur;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;

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
    let dims = vec2<f32>(textureDimensions(src_tex));
    let uv = in.clip_position.xy / dims;

    // Веса гауссианы (sigma ≈ 2.5), симметричные пары.
    var weights = array<f32, 5>(0.227027, 0.194595, 0.121622, 0.054054, 0.016216);
    var sum = textureSample(src_tex, src_sampler, uv).rgb * weights[0];
    for (var i = 1; i < 5; i = i + 1) {
        let offset = blur.step * f32(i);
        sum = sum + textureSample(src_tex, src_sampler, uv + offset).rgb * weights[i];
        sum = sum + textureSample(src_tex, src_sampler, uv - offset).rgb * weights[i];
    }
    return vec4<f32>(sum, 1.0);
}
