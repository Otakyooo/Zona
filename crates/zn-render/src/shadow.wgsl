// Теневой проход (M4g): глубина сцены вдоль направления солнца.
// Пишет только depth; читает первые 16 флоатов camera-uniform
// (light_view_projection) — раскладка совпадает с pbr.wgsl.

struct ShadowCamera {
    light_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: ShadowCamera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(
    in: VsIn,
    @location(3) model_c0: vec4<f32>,
    @location(4) model_c1: vec4<f32>,
    @location(5) model_c2: vec4<f32>,
    @location(6) model_c3: vec4<f32>,
    @location(7) color: vec4<f32>,
) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(model_c0, model_c1, model_c2, model_c3);
    return camera.light_view_proj * (model * vec4<f32>(in.position, 1.0));
}
