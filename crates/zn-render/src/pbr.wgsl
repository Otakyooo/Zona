struct Camera {
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>, // rgb — цвет солнца, w — интенсивность
    ambient: vec4<f32>, // rgb — цвет ambient, w — интенсивность
    fog_color: vec4<f32>, // rgb — цвет тумана, w — плотность
    sky_top: vec4<f32>,
    sky_horizon: vec4<f32>,
    output_srgb: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

struct Material {
    base_color_factor: vec4<f32>,
    metallic_factor: f32,
    roughness_factor: f32,
    _pad0: f32,
    _pad1: f32,
    emissive_factor: vec4<f32>,
    alpha_cutoff: f32,
    flags: u32, // 1 = alpha mask, 2 = double sided
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var base_tex: texture_2d<f32>;
@group(1) @binding(2) var metal_rough_tex: texture_2d<f32>;
@group(1) @binding(3) var normal_tex: texture_2d<f32>;
@group(1) @binding(4) var occlusion_tex: texture_2d<f32>;
@group(1) @binding(5) var emissive_tex: texture_2d<f32>;
@group(1) @binding(6) var tex_sampler: sampler;

const PI: f32 = 3.141592653589793;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tint: vec4<f32>,
};

@vertex
fn vs_main(
    in: VsIn,
    @location(3) model_c0: vec4<f32>,
    @location(4) model_c1: vec4<f32>,
    @location(5) model_c2: vec4<f32>,
    @location(6) model_c3: vec4<f32>,
    @location(7) color: vec4<f32>,
) -> VsOut {
    let model = mat4x4<f32>(model_c0, model_c1, model_c2, model_c3);
    let world_position = model * vec4<f32>(in.position, 1.0);

    var out: VsOut;
    out.clip_position = camera.view_proj * world_position;
    out.world_position = world_position.xyz;
    out.normal = (model * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    out.tint = color;
    return out;
}

// --- PBR helpers ---------------------------------------------------------

fn distribution_ggx(n: vec3<f32>, h: vec3<f32>, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let ndoth = max(dot(n, h), 0.0);
    let denom = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / (PI * denom * denom + 1e-6);
}

fn geometry_schitz_ggx(n: vec3<f32>, v: vec3<f32>, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let nv = max(dot(n, v), 0.0);
    return nv / (nv * (1.0 - k) + k + 1e-6);
}

fn geometry_smith(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    return geometry_schitz_ggx(n, v, roughness) * geometry_schitz_ggx(n, l, roughness);
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// Tangent frame из производных экранных координат (атрибутов тангентса нет).
fn cotangent_frame(n: vec3<f32>, p: vec3<f32>, uv: vec2<f32>) -> mat3x3<f32> {
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;
    let invmax = inverseSqrt(max(dot(t, t), dot(b, b)));
    return mat3x3<f32>(t * invmax, b * invmax, n);
}

fn perturb_normal(n: vec3<f32>, view_dir: vec3<f32>, uv: vec2<f32>, map_n: vec3<f32>) -> vec3<f32> {
    let tbn = cotangent_frame(n, -view_dir, uv);
    let perturbed = tbn * map_n;
    if (dot(perturbed, perturbed) < 1e-8) {
        return n;
    }
    return normalize(perturbed);
}

fn aces_fitted(x: vec3<f32>) -> vec3<f32> {
    // ACES-аппроксимация (Krzysztof Narkowicz), тени чуть сняты.
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
    let view_dir = normalize(camera.camera_pos.xyz - in.world_position);
    let light_dir = normalize(camera.light_dir.xyz);

    var normal = normalize(in.normal);
    let double_sided = (material.flags & 2u) != 0u;
    var double_sided_flip = 1.0;
    if (double_sided && dot(normal, view_dir) < 0.0) {
        normal = -normal;
        double_sided_flip = -1.0;
    }

    let albedo_sample = textureSample(base_tex, tex_sampler, in.uv);
    let mr_sample = textureSample(metal_rough_tex, tex_sampler, in.uv);
    let ao_sample = textureSample(occlusion_tex, tex_sampler, in.uv).r;
    let emissive_sample = textureSample(emissive_tex, tex_sampler, in.uv);
    let normal_sample = textureSample(normal_tex, tex_sampler, in.uv).xyz * 2.0 - vec3<f32>(1.0);
    normal = perturb_normal(normal * double_sided_flip, view_dir, in.uv, normal_sample);

    let base_color = material.base_color_factor * albedo_sample * in.tint;
    let alpha = base_color.a;
    let alpha_mask = (material.flags & 1u) != 0u;
    if (alpha_mask && alpha < material.alpha_cutoff) {
        discard;
    }

    let metallic = clamp(material.metallic_factor * mr_sample.b, 0.0, 1.0);
    let roughness = clamp(material.roughness_factor * mr_sample.g, 0.045, 1.0);

    let n = normalize(normal);
    let v = view_dir;
    let l = light_dir;
    let h = normalize(v + l);
    let ndotl = max(dot(n, l), 0.0);
    let ndotv = max(dot(n, v), 1e-4);

    let f0 = mix(vec3<f32>(0.04), base_color.rgb, metallic);
    let radiance = camera.light_color.rgb * camera.light_color.w * ndotl;

    let ndf = distribution_ggx(n, h, roughness);
    let g = geometry_smith(n, v, l, roughness);
    let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
    let specular = (ndf * g * f) / (4.0 * ndotv * ndotl + 1e-5);
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    let diffuse = kd * base_color.rgb / PI;

    let direct = (diffuse + specular) * radiance;
    let ambient = camera.ambient.rgb * camera.ambient.w * base_color.rgb * ao_sample;
    let emissive = material.emissive_factor.rgb * emissive_sample.rgb;

    var color = direct + ambient + emissive;

    // Туман (M4g): экспоненциально по дистанции до камеры, в линейном цвете.
    let fog_distance = length(camera.camera_pos.xyz - in.world_position);
    let fog_factor = clamp(1.0 - exp(-camera.fog_color.w * fog_distance), 0.0, 1.0);
    color = mix(color, camera.fog_color.rgb, fog_factor);

    color = aces_fitted(color);
    if (camera.output_srgb > 0.5) {
        color = linear_to_srgb(color);
    }
    return vec4<f32>(color, alpha);
}
