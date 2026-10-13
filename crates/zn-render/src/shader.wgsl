struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_main(
    in: VsIn,
    @location(2) model_c0: vec4<f32>,
    @location(3) model_c1: vec4<f32>,
    @location(4) model_c2: vec4<f32>,
    @location(5) model_c3: vec4<f32>,
    @location(6) color: vec4<f32>,
) -> VsOut {
    let model = mat4x4<f32>(model_c0, model_c1, model_c2, model_c3);
    let world_position = model * vec4<f32>(in.position, 1.0);

    var out: VsOut;
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    out.normal = normalize((model * vec4<f32>(in.normal, 0.0)).xyz);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let normal = normalize(in.normal);
    let light_direction = normalize(camera.light_dir.xyz);
    let view_direction = normalize(camera.camera_pos.xyz - in.world_position);

    let diffuse = max(dot(normal, light_direction), 0.0);
    let halfway = normalize(light_direction + view_direction);
    let specular = pow(max(dot(normal, halfway), 0.0), 32.0);
    let rim = pow(1.0 - max(dot(normal, view_direction), 0.0), 3.0) * 0.15;

    let base = in.color.rgb;
    let lit = base * (camera.ambient.rgb + camera.light_color.rgb * diffuse)
        + specular * 0.2 * camera.light_color.rgb
        + rim * camera.ambient.rgb;

    return vec4<f32>(lit, in.color.a);
}
