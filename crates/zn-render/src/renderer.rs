pub mod cache;
pub mod cull;
pub mod draw;
pub mod material;
pub mod types;

pub use cache::{Cache, MaterialId, MeshId, MESH_BUDGET_BYTES, TEXTURE_BUDGET_BYTES};
pub use draw::{DrawBatch, DrawList};
pub use material::{GpuMaterial, MaterialParams, TEXTURE_SLOTS};
pub use types::InstanceRaw;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use glam::camera::rh::proj::directx::orthographic;
use glam::camera::rh::view::look_at_mat4;

use std::sync::Arc;

use crate::camera::Camera;
use crate::mesh::{cpu_mesh_vertices, GpuMesh, Vertex};
use crate::renderer::types::{BloomBlurUniform, BloomBrightUniform, CameraUniform, PostUniform};
use zn_assets::{CpuImage, CpuMaterial, CpuMesh};
use zn_scene::SceneEnvironment;

pub const MAX_INSTANCES: usize = 8192;

/// Сторона карты теней (M4g): одна ортопроекция на локацию, PCF 3×3.
pub const SHADOW_MAP_SIZE: u32 = 2048;

/// Формат offscreen-таргета сцены (M4g-G3): линейный HDR до тонмаппинга.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Делитель разрешения таргетов bloom (M4g-G3b): ¼ по каждой стороне.
const BLOOM_DIVISOR: u32 = 4;

/// Настройки постобработки (M4g-G3): виньетка и bloom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PostSettings {
    /// Сила виньетки 0..1 (0 — выключена).
    pub vignette: f32,
    /// Порог яркости bloom по максимальному каналу HDR.
    pub bloom_threshold: f32,
    /// Интенсивность подмешивания bloom к кадру (0 — выключен).
    pub bloom_intensity: f32,
}

impl Default for PostSettings {
    fn default() -> Self {
        Self {
            vignette: 0.35,
            bloom_threshold: 0.9,
            bloom_intensity: 0.45,
        }
    }
}

/// Мягкий порог (smoothstep), как в WGSL.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl PostSettings {
    /// Затемнение виньетки по дистанции от центра кадра: 0 — центр,
    /// 1 — углы (дистанция нормирована по полудиагонали). Зеркало формулы
    /// из `post.wgsl`, закрыто тестом.
    pub fn vignette_factor(&self, d: f32) -> f32 {
        1.0 - self.vignette * smoothstep(0.45, 1.0, d)
    }

    /// Вклад цвета в bloom (зеркало bright-pass из `bloom_bright.wgsl`):
    /// ниже порога — нули, выше — цвет переносится целиком.
    pub fn bloom_contribution(&self, color: [f32; 3]) -> [f32; 3] {
        let brightness = color[0].max(color[1]).max(color[2]);
        let scale = (brightness - self.bloom_threshold).max(0.0) / brightness.max(1e-4);
        [color[0] * scale, color[1] * scale, color[2] * scale]
    }
}

/// Матрица теневого прохода (M4g): ортопроекция вдоль направления солнца,
/// накрывающая AABB сцены (сфера радиуса `radius` вокруг центра). Нулевое
/// направление трактуется как строго вверх. Чистая функция — тестируется без GPU.
pub fn directional_shadow_matrix(
    direction: glam::Vec3,
    min: glam::Vec3,
    max: glam::Vec3,
) -> glam::Mat4 {
    let mut direction = direction.normalize_or_zero();
    if direction == glam::Vec3::ZERO {
        direction = glam::Vec3::Y;
    }
    let center = (min + max) * 0.5;
    let radius = ((max - min).length() * 0.5).max(1.0);
    let up = if direction.y.abs() > 0.99 {
        glam::Vec3::Z
    } else {
        glam::Vec3::Y
    };
    let eye = center + direction * (radius + 1.0);
    let view = look_at_mat4(eye, center, up);
    let projection = orthographic(-radius, radius, -radius, radius, 0.0, 2.0 * radius + 2.0);
    projection * view
}

fn create_depth(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("zn-depth"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth24Plus,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// HDR-таргет сцены (M4g-G3): сцена рисуется в него, пост-пасс читает.
fn create_hdr(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("zn-hdr-target"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Uniform пост-пасса (M4g-G3): настройки + признак не-sRGB поверхности.
fn post_uniform(settings: &PostSettings, output_srgb: f32) -> PostUniform {
    PostUniform {
        vignette: settings.vignette,
        output_srgb,
        bloom_intensity: settings.bloom_intensity,
        _padding: 0.0,
    }
}

/// Шаги размытия bloom (M4g-G3b) в UV: по одному текселю таргета за шаг
/// в горизонталь и вертикаль.
fn bloom_blur_uniforms(width: u32, height: u32) -> (BloomBlurUniform, BloomBlurUniform) {
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    (
        BloomBlurUniform {
            step: [1.0 / width, 0.0],
            _padding: [0.0; 2],
        },
        BloomBlurUniform {
            step: [0.0, 1.0 / height],
            _padding: [0.0; 2],
        },
    )
}

/// Биндинг пост-пасса: настройки + HDR-кадр + bloom + сэмплер.
fn make_post_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    uniform: &wgpu::Buffer,
    hdr: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    bloom: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(hdr),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(bloom),
            },
        ],
    })
}

/// Биндинг полноэкранного прохода bloom: uniform + текстура + сэмплер.
fn make_bloom_pass_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    uniform: &wgpu::Buffer,
    texture: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(texture),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Полноэкранный проход постобработки/bloom: очистка чёрным и треугольник
/// без вершинного буфера (одна группа биндингов).
fn fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

/// Таргет bloom (M4g-G3b): ¼ разрешения, HDR — яркие пиксели и размытие.
fn create_bloom(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("zn-bloom-target"),
        size: wgpu::Extent3d {
            width: (config.width / BLOOM_DIVISOR).max(1),
            height: (config.height / BLOOM_DIVISOR).max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Диапазон инстансов батча в общем буфере: смещение и количество.
#[derive(Clone, Copy, Debug)]
struct DrawRange {
    mesh: MeshId,
    material: MaterialId,
    instance_offset: u32,
    instance_count: u32,
    /// Blend-материал: рисуется вторым проходом back-to-front.
    blend: bool,
    /// Позиция первого инстанса — ключ сортировки blend-прохода.
    first_position: [f32; 3],
}

pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    /// Offscreen HDR-таргет сцены (M4g-G3) — вход пост-пасса.
    hdr_texture: wgpu::Texture,
    hdr_view: wgpu::TextureView,
    /// Пост-пасс (M4g-G3): ACES, виньетка, кодирование в sRGB поверхности.
    post_pipeline: wgpu::RenderPipeline,
    post_bind_group: wgpu::BindGroup,
    post_uniform_buffer: wgpu::Buffer,
    post_sampler: wgpu::Sampler,
    post_settings: PostSettings,
    /// Bloom (M4g-G3b): ¼-разрешение, bright-pass → blur → пост-пасс.
    bloom_a_texture: wgpu::Texture,
    bloom_a_view: wgpu::TextureView,
    bloom_b_texture: wgpu::Texture,
    bloom_b_view: wgpu::TextureView,
    bloom_bright_pipeline: wgpu::RenderPipeline,
    bloom_blur_pipeline: wgpu::RenderPipeline,
    bloom_bright_bind_group: wgpu::BindGroup,
    bloom_blur_h_bind_group: wgpu::BindGroup,
    bloom_blur_v_bind_group: wgpu::BindGroup,
    bloom_bright_buffer: wgpu::Buffer,
    bloom_blur_h_buffer: wgpu::Buffer,
    bloom_blur_v_buffer: wgpu::Buffer,
    pipeline: wgpu::RenderPipeline,
    blend_pipeline: wgpu::RenderPipeline,
    /// Фон-небо (M4g): полноэкранный пасс с градиентом и солнцем.
    sky_pipeline: wgpu::RenderPipeline,
    /// Теневой проход (M4g): глубина сцены вдоль солнца.
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_camera_bind_group: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
    /// Границы сцены для теневой карты (обновляет клиент/редактор).
    shadow_bounds: (glam::Vec3, glam::Vec3),
    camera_uniform: CameraUniform,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    material_bind_group_layout: wgpu::BindGroupLayout,
    material_sampler: wgpu::Sampler,
    mesh_cache: Cache<GpuMesh>,
    material_cache: Cache<GpuMaterial>,
    builtin_cube_mesh: MeshId,
    default_material: MaterialId,
    instance_buffer: wgpu::Buffer,
    draw_ranges: Vec<DrawRange>,
    instance_count: u32,
    clear_color: wgpu::Color,
}

impl Renderer {
    pub fn new(event_loop: &ActiveEventLoop) -> Self {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("ZONA — M2 content pipeline")
                        .with_inner_size(winit::dpi::LogicalSize::new(1600u32, 900u32)),
                )
                .expect("failed to create window"),
        );
        Self::init(window)
    }

    fn init(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        // Backend'ы в wgpu 25+ за cargo-фичами: vulkan+wgsl включены в Cargo.toml.
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(instance_descriptor);

        let display_handle = window.display_handle().expect("display handle");
        let window_handle = window.window_handle().expect("window handle");
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(display_handle.as_raw()),
            raw_window_handle: window_handle.as_raw(),
        };
        let surface: wgpu::Surface<'static> =
            unsafe { instance.create_surface_unsafe(target) }.expect("failed to create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("no compatible gpu adapter found");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("zn-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .expect("failed to request device");

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .first()
            .copied()
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        log::info!("surface format: {format:?}");

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: capabilities
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Opaque),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let (depth_texture, depth_view) = create_depth(&device, &config);
        // HDR-таргет сцены (M4g-G3) — размер окна, пересоздаётся в resize.
        let (hdr_texture, hdr_view) = create_hdr(&device, &config);

        // Карта теней (M4g): depth-only цель солнца, сравнивается sampler'ом.
        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("zn-shadow-map"),
            size: wgpu::Extent3d {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("zn-shadow-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        // Не-sRGB поверхность (Bgra8Unorm): пост-пасс кодирует линейный цвет сам.
        let output_srgb = if config.format.is_srgb() { 0.0 } else { 1.0 };

        // Стартовая атмосфера — нейтраль контента (M4g): клиент перезапишет
        // её атмосферой загруженной локации.
        let mut camera_uniform = CameraUniform {
            light_view_projection: glam::Mat4::IDENTITY.to_cols_array(),
            view_projection: glam::Mat4::IDENTITY.to_cols_array(),
            inverse_view_projection: glam::Mat4::IDENTITY.to_cols_array(),
            camera_position: [0.0, 0.0, 0.0, 1.0],
            light_direction: [0.0, 1.0, 0.0, 0.0],
            light_color: [1.0, 1.0, 1.0, 1.0],
            ambient_color: [0.0, 0.0, 0.0, 1.0],
            fog_color: [0.0, 0.0, 0.0, 0.0],
            sky_top: [0.0, 0.0, 0.0, 0.0],
            sky_horizon: [0.0, 0.0, 0.0, 0.0],
        };
        camera_uniform.apply_environment(&SceneEnvironment::default());
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&camera_buffer, 0, bytemuck::cast_slice(&[camera_uniform]));

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("zn-camera-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(
                                std::mem::size_of::<CameraUniform>() as u64,
                            ),
                        },
                        count: None,
                    },
                    // Карта теней (M4g): глубина солнца + сравнивающий сэмплер.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                ],
            });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zn-camera-bind-group"),
            layout: &camera_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
            ],
        });

        // Layout материала: uniform + 5 текстур + сэмплер.
        let material_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("zn-material-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                MaterialParams,
                            >()
                                as u64),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("zn-material-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-pbr-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("pbr.wgsl"))),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("zn-pipeline-layout"),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&material_bind_group_layout),
            ],
            immediate_size: 0,
        });

        // Сцена рисуется в HDR-таргет (M4g-G3); в поверхность окна пишет
        // пост-пасс. Формат целей сцены — HDR_FORMAT, у пост-пайплайна — поверхность.
        let opaque_targets = [Some(wgpu::ColorTargetState {
            format: HDR_FORMAT,
            blend: Some(wgpu::BlendState::REPLACE),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let blend_targets = [Some(wgpu::ColorTargetState {
            format: HDR_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let opaque_descriptor = wgpu::RenderPipelineDescriptor {
            label: Some("zn-pbr-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc()), Some(InstanceRaw::desc())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &opaque_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        };
        let pipeline = device.create_render_pipeline(&opaque_descriptor);

        // Blend-конвейер: без записи глубины, альфа-блендинг вторым проходом.
        let mut blend_descriptor = opaque_descriptor.clone();
        blend_descriptor.label = Some("zn-pbr-pipeline-blend");
        blend_descriptor.fragment = Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &blend_targets,
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });
        blend_descriptor.depth_stencil = Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth24Plus,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });
        let blend_pipeline = device.create_render_pipeline(&blend_descriptor);

        // Небо (M4g): полноэкранный треугольник без вершинных буферов; глубина
        // не пишется и не проверяется — пасс идёт первым и служит фоном.
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-sky-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("sky.wgsl"))),
        });
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("zn-sky-layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-sky-pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("fs_main"),
                targets: &opaque_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Теневой проход (M4g): глубина вдоль солнца, без цвета и материала.
        // Отсекаются лицевые грани (пишем дальние) — меньше acne на кубах;
        // slope-scale bias добавляет pipeline.
        // Отдельный bind group только с матрицей: карта теней не должна быть
        // одновременно целью записи и ресурсом в одном проходе.
        let shadow_camera_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("zn-shadow-camera-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<CameraUniform>() as u64,
                        ),
                    },
                    count: None,
                }],
            });
        let shadow_camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zn-shadow-camera-bind-group"),
            layout: &shadow_camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "shadow.wgsl"
            ))),
        });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("zn-shadow-layout"),
                bind_group_layouts: &[Some(&shadow_camera_layout)],
                immediate_size: 0,
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-shadow-pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc()), Some(InstanceRaw::desc())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Front),
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Постобработка (M4g-G3): полноэкранный треугольник поверх HDR-кадра.
        // Layout: uniform настроек + HDR-текстура + bloom-текстура + сэмплер.
        let post_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-post-uniform"),
            size: std::mem::size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_settings = PostSettings::default();
        queue.write_buffer(
            &post_uniform_buffer,
            0,
            bytemuck::cast_slice(&[post_uniform(&post_settings, output_srgb)]),
        );
        let post_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("zn-post-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let post_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("zn-post-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(
                                std::mem::size_of::<PostUniform>() as u64,
                            ),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Bloom-текстура (M4g-G3b) — размытые яркие пиксели.
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });
        let post_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-post-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("post.wgsl"))),
        });
        let post_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("zn-post-pipeline-layout"),
            bind_group_layouts: &[Some(&post_bind_group_layout)],
            immediate_size: 0,
        });
        let post_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-post-pipeline"),
            layout: Some(&post_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &post_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &post_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Bloom (M4g-G3b): bright-pass и разделимое размытие в ¼ разрешения.
        // У трёх проходов общий layout: uniform + текстура + сэмплер.
        let bloom_uniform_size = std::mem::size_of::<BloomBrightUniform>() as u64;
        debug_assert_eq!(
            std::mem::size_of::<BloomBlurUniform>(),
            bloom_uniform_size as usize,
            "униформы проходов bloom должны совпадать по размеру"
        );
        let bloom_pass_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("zn-bloom-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(bloom_uniform_size),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bloom_bright_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-bloom-bright-uniform"),
            size: bloom_uniform_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bloom_blur_h_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-bloom-blur-h-uniform"),
            size: bloom_uniform_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bloom_blur_v_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-bloom-blur-v-uniform"),
            size: bloom_uniform_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let (blur_h, blur_v) =
            bloom_blur_uniforms(config.width / BLOOM_DIVISOR, config.height / BLOOM_DIVISOR);
        queue.write_buffer(
            &bloom_bright_buffer,
            0,
            bytemuck::cast_slice(&[BloomBrightUniform {
                threshold: post_settings.bloom_threshold,
                _padding: [0.0; 3],
            }]),
        );
        queue.write_buffer(&bloom_blur_h_buffer, 0, bytemuck::cast_slice(&[blur_h]));
        queue.write_buffer(&bloom_blur_v_buffer, 0, bytemuck::cast_slice(&[blur_v]));
        let bloom_bright_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-bloom-bright-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "bloom_bright.wgsl"
            ))),
        });
        let bloom_blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-bloom-blur-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "bloom_blur.wgsl"
            ))),
        });
        let bloom_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("zn-bloom-pipeline-layout"),
                bind_group_layouts: &[Some(&bloom_pass_layout)],
                immediate_size: 0,
            });
        let bloom_targets = [Some(wgpu::ColorTargetState {
            format: HDR_FORMAT,
            blend: Some(wgpu::BlendState::REPLACE),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let bloom_primitive = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        };
        let bloom_bright_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("zn-bloom-bright-pipeline"),
                layout: Some(&bloom_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &bloom_bright_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &bloom_bright_shader,
                    entry_point: Some("fs_main"),
                    targets: &bloom_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: bloom_primitive,
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        let bloom_blur_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-bloom-blur-pipeline"),
            layout: Some(&bloom_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &bloom_blur_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &bloom_blur_shader,
                entry_point: Some("fs_main"),
                targets: &bloom_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: bloom_primitive,
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let (bloom_a_texture, bloom_a_view) = create_bloom(&device, &config);
        let (bloom_b_texture, bloom_b_view) = create_bloom(&device, &config);
        let post_bind_group = make_post_bind_group(
            &device,
            &post_pipeline.get_bind_group_layout(0),
            "zn-post-bind-group",
            &post_uniform_buffer,
            &hdr_view,
            &post_sampler,
            &bloom_a_view,
        );
        let bloom_bright_bind_group = make_bloom_pass_bind_group(
            &device,
            &bloom_bright_pipeline.get_bind_group_layout(0),
            "zn-bloom-bright-bind-group",
            &bloom_bright_buffer,
            &hdr_view,
            &post_sampler,
        );
        let bloom_blur_h_bind_group = make_bloom_pass_bind_group(
            &device,
            &bloom_blur_pipeline.get_bind_group_layout(0),
            "zn-bloom-blur-h-bind-group",
            &bloom_blur_h_buffer,
            &bloom_a_view,
            &post_sampler,
        );
        let bloom_blur_v_bind_group = make_bloom_pass_bind_group(
            &device,
            &bloom_blur_pipeline.get_bind_group_layout(0),
            "zn-bloom-blur-v-bind-group",
            &bloom_blur_v_buffer,
            &bloom_b_view,
            &post_sampler,
        );

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-instances"),
            size: (MAX_INSTANCES * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            window,
            surface,
            device,
            queue,
            config,
            depth_texture,
            depth_view,
            hdr_texture,
            hdr_view,
            post_pipeline,
            post_bind_group,
            post_uniform_buffer,
            post_sampler,
            post_settings,
            bloom_a_texture,
            bloom_a_view,
            bloom_b_texture,
            bloom_b_view,
            bloom_bright_pipeline,
            bloom_blur_pipeline,
            bloom_bright_bind_group,
            bloom_blur_h_bind_group,
            bloom_blur_v_bind_group,
            bloom_bright_buffer,
            bloom_blur_h_buffer,
            bloom_blur_v_buffer,
            pipeline,
            blend_pipeline,
            sky_pipeline,
            shadow_pipeline,
            shadow_camera_bind_group,
            shadow_view,
            shadow_bounds: (glam::Vec3::splat(-1.0), glam::Vec3::splat(1.0)),
            camera_uniform,
            camera_buffer,
            camera_bind_group,
            material_bind_group_layout,
            material_sampler: sampler,
            mesh_cache: Cache::with_budget(MESH_BUDGET_BYTES),
            material_cache: Cache::with_budget(TEXTURE_BUDGET_BYTES),
            builtin_cube_mesh: 0,
            default_material: 0,
            instance_buffer,
            draw_ranges: Vec::new(),
            instance_count: 0,
            clear_color: wgpu::Color {
                r: 0.02,
                g: 0.025,
                b: 0.035,
                a: 1.0,
            },
        };

        // Встроенный куб + нейтральный материал — базовые ресурсы сцены.
        let builtin_cube = zn_assets::builtin_mesh("cube").expect("builtin cube");
        renderer.builtin_cube_mesh = renderer.register_mesh(&builtin_cube);
        // Материал по умолчанию (M4g): диэлектрик — поверхности реагируют на
        // солнце диффузно, а не как металл (у glTF-дефолта metallic = 1).
        let default_material = CpuMaterial {
            metallic_factor: 0.0,
            roughness_factor: 0.85,
            ..CpuMaterial::default()
        };
        renderer.default_material = renderer.register_material(&default_material, &[]);

        renderer
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn surface_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(self.config.width, self.config.height)
    }

    /// Заголовок окна (редактор ставит свой поверх дефолтного).
    pub fn set_window_title(&self, title: &str) {
        self.window.set_title(title);
    }

    pub fn instance_count(&self) -> u32 {
        self.instance_count
    }

    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32) {
        self.clear_color = wgpu::Color {
            r: r as f64,
            g: g as f64,
            b: b as f64,
            a: 1.0,
        };
    }

    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        let (texture, view) = create_depth(&self.device, &self.config);
        self.depth_texture = texture;
        self.depth_view = view;
        // HDR- и bloom-таргеты пересоздаются под новый размер: текстуры
        // зафиксированы в биндингах, поэтому после — пересборка биндингов.
        let (hdr_texture, hdr_view) = create_hdr(&self.device, &self.config);
        self.hdr_texture = hdr_texture;
        self.hdr_view = hdr_view;
        let (bloom_a_texture, bloom_a_view) = create_bloom(&self.device, &self.config);
        self.bloom_a_texture = bloom_a_texture;
        self.bloom_a_view = bloom_a_view;
        let (bloom_b_texture, bloom_b_view) = create_bloom(&self.device, &self.config);
        self.bloom_b_texture = bloom_b_texture;
        self.bloom_b_view = bloom_b_view;
        self.write_bloom_steps();
        self.rebuild_target_bind_groups();
    }

    /// Пересобирает биндинги пассов после пересоздания таргетов (resize).
    fn rebuild_target_bind_groups(&mut self) {
        self.post_bind_group = make_post_bind_group(
            &self.device,
            &self.post_pipeline.get_bind_group_layout(0),
            "zn-post-bind-group",
            &self.post_uniform_buffer,
            &self.hdr_view,
            &self.post_sampler,
            &self.bloom_a_view,
        );
        self.bloom_bright_bind_group = make_bloom_pass_bind_group(
            &self.device,
            &self.bloom_bright_pipeline.get_bind_group_layout(0),
            "zn-bloom-bright-bind-group",
            &self.bloom_bright_buffer,
            &self.hdr_view,
            &self.post_sampler,
        );
        self.bloom_blur_h_bind_group = make_bloom_pass_bind_group(
            &self.device,
            &self.bloom_blur_pipeline.get_bind_group_layout(0),
            "zn-bloom-blur-h-bind-group",
            &self.bloom_blur_h_buffer,
            &self.bloom_a_view,
            &self.post_sampler,
        );
        self.bloom_blur_v_bind_group = make_bloom_pass_bind_group(
            &self.device,
            &self.bloom_blur_pipeline.get_bind_group_layout(0),
            "zn-bloom-blur-v-bind-group",
            &self.bloom_blur_v_buffer,
            &self.bloom_b_view,
            &self.post_sampler,
        );
    }

    /// Пишет шаги размытия bloom от размера ¼-таргета (init и resize).
    fn write_bloom_steps(&self) {
        let (blur_h, blur_v) = bloom_blur_uniforms(
            self.config.width / BLOOM_DIVISOR,
            self.config.height / BLOOM_DIVISOR,
        );
        self.queue.write_buffer(
            &self.bloom_blur_h_buffer,
            0,
            bytemuck::cast_slice(&[blur_h]),
        );
        self.queue.write_buffer(
            &self.bloom_blur_v_buffer,
            0,
            bytemuck::cast_slice(&[blur_v]),
        );
    }

    // --- кэши -------------------------------------------------------------

    /// ID меша `builtin://cube`.
    pub fn builtin_cube_mesh(&self) -> MeshId {
        self.builtin_cube_mesh
    }

    /// ID нейтрального материала без текстур.
    pub fn default_material(&self) -> MaterialId {
        self.default_material
    }

    /// Бюджеты кэшей (байты).
    pub fn mesh_budget(&self) -> u64 {
        self.mesh_cache.budget()
    }

    pub fn texture_budget(&self) -> u64 {
        self.material_cache.budget()
    }

    pub fn mesh_count(&self) -> usize {
        self.mesh_cache.len()
    }

    pub fn material_count(&self) -> usize {
        self.material_cache.len()
    }

    /// Bounds меша из кэша — для фрустум-culling'а на стороне клиента.
    pub fn mesh_bounds(&self, mesh: MeshId) -> Option<zn_assets::Bounds> {
        self.mesh_cache.peek(mesh).map(|entry| entry.bounds)
    }

    /// Помечает ресурсы всех батчей списка использованными до куллинга:
    /// отсечённые инстансы не должны выселять видимые LRU-записи.
    pub fn touch_scene(&mut self, list: &DrawList) {
        for batch in &list.batches {
            if batch.is_empty() {
                continue;
            }
            self.mesh_cache.touch(batch.mesh);
            self.material_cache.touch(batch.material);
        }
    }

    /// Регистрирует CPU-меш в GPU-кэше, возвращает ID.
    pub fn register_mesh(&mut self, mesh: &CpuMesh) -> MeshId {
        debug_assert_eq!(
            std::mem::size_of::<zn_assets::AssetVertex>(),
            std::mem::size_of::<Vertex>(),
            "раскладка AssetVertex и Vertex разошлась"
        );
        let vertices = cpu_mesh_vertices(mesh);
        let indices: Vec<u32> = mesh.indices.clone();

        let vertex_bytes: &[u8] = bytemuck::cast_slice(&vertices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&indices);
        let size_bytes = vertex_bytes.len() as u64 + index_bytes.len() as u64;

        let mesh_label = format!("zn-mesh:{}", mesh.name.as_deref().unwrap_or("unnamed"));
        let vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&mesh_label),
            size: vertex_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&vertex_buffer, 0, vertex_bytes);

        let index_label = format!("{mesh_label}-indices");
        let index_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&index_label),
            size: index_bytes.len() as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&index_buffer, 0, index_bytes);

        let gpu_mesh = GpuMesh {
            id: 0, // проставится после insert
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            bounds: mesh.bounds,
            size_bytes,
            name: mesh.name.clone(),
        };
        let id = self.mesh_cache.insert(gpu_mesh, size_bytes);
        if let Some(entry) = self.mesh_cache.entries_mut().get_mut(&id) {
            entry.resource.id = id;
        }
        id
    }

    /// Регистрирует материал с изображениями слотов
    /// (albedo, metallic-roughness, normal, occlusion, emissive).
    pub fn register_material(
        &mut self,
        material: &CpuMaterial,
        images: &[Option<&CpuImage>],
    ) -> MaterialId {
        let params = MaterialParams::from(material);

        let mut owned_textures = Vec::new();
        let mut texture_views = Vec::with_capacity(TEXTURE_SLOTS);
        let mut texture_bytes = 0u64;

        for slot in 0..TEXTURE_SLOTS {
            let image = images.get(slot).copied().flatten();
            let (texture, bytes) = match image {
                Some(image) => self.create_texture(image, material.name.as_deref()),
                None => {
                    let pixels = crate::renderer::material::default_texture_pixels(slot);
                    self.create_default_texture(&pixels)
                }
            };
            texture_bytes += bytes;
            texture_views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
            owned_textures.push(texture);
        }

        let params_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-material-params"),
            size: std::mem::size_of::<MaterialParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&params_buffer, 0, bytemuck::cast_slice(&[params]));

        let layout = &self.material_bind_group_layout;
        let sampler = &self.material_sampler;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zn-material-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&texture_views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&texture_views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&texture_views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&texture_views[3]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&texture_views[4]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        let gpu_material = GpuMaterial::new(0, params, texture_bytes, bind_group, owned_textures);
        let id = self.material_cache.insert(gpu_material, texture_bytes);
        if let Some(entry) = self.material_cache.entries_mut().get_mut(&id) {
            entry.resource.id = id;
        }
        id
    }

    fn create_texture(
        &self,
        image: &CpuImage,
        material_name: Option<&str>,
    ) -> (wgpu::Texture, u64) {
        // sRGB-декодирование оборудованием: albedo/emissive в srgb-форматах,
        // данные (metallic/roughness/normal/occlusion) — линейные.
        let format = if image.srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let width = image.width.max(1);
        let height = image.height.max(1);
        debug_assert_eq!(
            image.pixels.len(),
            width as usize * height as usize * 4,
            "CpuImage должен быть RGBA8"
        );
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("zn-material-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1 + image.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.write_texture_level(&texture, 0, &image.pixels, width, height);
        let mut level_width = width;
        let mut level_height = height;
        for (index, mip) in image.mips.iter().enumerate() {
            level_width = (level_width / 2).max(1);
            level_height = (level_height / 2).max(1);
            debug_assert_eq!(
                mip.len(),
                level_width as usize * level_height as usize * 4,
                "размер мип-уровня {index} не совпал"
            );
            self.write_texture_level(&texture, index as u32 + 1, mip, level_width, level_height);
        }
        let _ = material_name;
        // Бюджет LRU честный: base level + все мипы (≈×4/3).
        let bytes =
            (image.pixels.len() + image.mips.iter().map(|mip| mip.len()).sum::<usize>()) as u64;
        (texture, bytes)
    }

    /// Грузит один мип-уровень RGBA8-текстуры.
    fn write_texture_level(
        &self,
        texture: &wgpu::Texture,
        level: u32,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn create_default_texture(&self, pixels: &[u8; 4]) -> (wgpu::Texture, u64) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("zn-material-default-texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        (texture, 4)
    }

    // --- кадровый цикл -----------------------------------------------------

    pub fn update_camera(&mut self, camera: &Camera) {
        let view_projection = camera.view_projection();
        self.camera_uniform.view_projection = view_projection.to_cols_array();
        self.camera_uniform.inverse_view_projection = view_projection.inverse().to_cols_array();
        self.camera_uniform.camera_position = camera.position.extend(1.0).to_array();
        // Теневая матрица (M4g) — от текущего солнца и границ локации.
        let light = &self.camera_uniform.light_direction;
        self.camera_uniform.light_view_projection = directional_shadow_matrix(
            glam::Vec3::new(light[0], light[1], light[2]),
            self.shadow_bounds.0,
            self.shadow_bounds.1,
        )
        .to_cols_array();
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[self.camera_uniform]),
        );
    }

    /// Применяет атмосферу локации (M4g): солнце, ambient, туман и цвета неба.
    /// Проводится каждый кадр — смена локации подхватывается сразу.
    pub fn set_atmosphere(&mut self, environment: &SceneEnvironment) {
        self.camera_uniform.apply_environment(environment);
    }

    /// Границы сцены (AABB объектов) для охвата теневой карты (M4g).
    pub fn set_shadow_bounds(&mut self, min: glam::Vec3, max: glam::Vec3) {
        self.shadow_bounds = (min, max);
    }

    /// Текущие настройки постобработки (M4g-G3).
    pub fn post_settings(&self) -> PostSettings {
        self.post_settings
    }

    /// Обновляет настройки постобработки и пересылает их в пост-пасс.
    pub fn set_post_settings(&mut self, settings: PostSettings) {
        self.post_settings = settings;
        self.write_post_uniform();
    }

    fn write_post_uniform(&self) {
        let output_srgb = if self.config.format.is_srgb() {
            0.0
        } else {
            1.0
        };
        self.queue.write_buffer(
            &self.post_uniform_buffer,
            0,
            bytemuck::cast_slice(&[post_uniform(&self.post_settings, output_srgb)]),
        );
        // Bright-pass (M4g-G3b) отсекает всё ниже порога.
        self.queue.write_buffer(
            &self.bloom_bright_buffer,
            0,
            bytemuck::cast_slice(&[BloomBrightUniform {
                threshold: self.post_settings.bloom_threshold,
                _padding: [0.0; 3],
            }]),
        );
    }

    /// Загружает кадровый DrawList: плоский буфер инстансов + диапазоны
    /// батчей. Триггерит LRU-обновление использованных ресурсов.
    pub fn update_draw_list(&mut self, list: &DrawList) {
        let total: usize = list.instance_count();
        assert!(
            total <= MAX_INSTANCES,
            "draw list overflow: {total} > {MAX_INSTANCES}"
        );

        let mut flattened = Vec::with_capacity(total);
        self.draw_ranges.clear();
        for batch in &list.batches {
            if batch.is_empty() {
                continue;
            }
            let blend = self
                .material_cache
                .peek(batch.material)
                .is_some_and(|material| {
                    material.params.flags & crate::renderer::material::FLAG_BLEND != 0
                });
            let first_position = batch
                .instances
                .first()
                .map(|instance| [instance.model[12], instance.model[13], instance.model[14]])
                .unwrap_or([0.0; 3]);
            let offset = flattened.len() as u32;
            flattened.extend_from_slice(&batch.instances);
            self.draw_ranges.push(DrawRange {
                mesh: batch.mesh,
                material: batch.material,
                instance_offset: offset,
                instance_count: batch.instances.len() as u32,
                blend,
                first_position,
            });
            self.mesh_cache.touch(batch.mesh);
            self.material_cache.touch(batch.material);
        }

        self.instance_count = flattened.len() as u32;
        if !flattened.is_empty() {
            self.queue
                .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&flattened));
        }
    }

    pub fn draw_range_count(&self) -> usize {
        self.draw_ranges.len()
    }

    pub fn render(&mut self, camera: &Camera) {
        self.render_with(camera, |_pass| {});
    }

    /// Рендер + хук для оверлея (UI редактора): после прохода сцены
    /// открывается отдельный проход поверх результата (`LoadOp::Load`,
    /// без depth attach). Pass передаётся во владение — оверлей может вызвать
    /// `pass.forget_lifetime()` (egui-wgpu принимает `RenderPass<'static>`).
    pub fn render_with(&mut self, camera: &Camera, overlay: impl FnOnce(wgpu::RenderPass<'_>)) {
        self.update_camera(camera);

        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                log::debug!("surface suboptimal — переконфигурация");
                self.surface.configure(&self.device, &self.config);
                texture
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => {
                return;
            }
        };

        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("zn-frame"),
            });

        // Теневой проход (M4g): только непрозрачные кастеры, глубину пишем
        // в карту; главный проход затем сэмплирует её в pbr.wgsl.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("zn-shadow-pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.shadow_camera_bind_group, &[]);
            let stride = std::mem::size_of::<InstanceRaw>() as u64;
            for range in &self.draw_ranges {
                if range.blend {
                    continue; // полупрозрачные (ауры, пузыри) тени не бросают
                }
                let Some(mesh) = self.mesh_cache.peek(range.mesh) else {
                    continue;
                };
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                let start = range.instance_offset as u64 * stride;
                let end = start + range.instance_count as u64 * stride;
                pass.set_vertex_buffer(1, self.instance_buffer.slice(start..end));
                pass.draw_indexed(0..mesh.index_count, 0, 0..range.instance_count);
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("zn-main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.hdr_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Порядок двух проходов: opaque — как есть, blend — back-to-front
            // по дистанции до камеры (ключ — позиция первого инстанса).
            let mut order: Vec<usize> = Vec::with_capacity(self.draw_ranges.len());
            let mut blend_indices: Vec<usize> = Vec::new();
            for (index, range) in self.draw_ranges.iter().enumerate() {
                if range.blend {
                    blend_indices.push(index);
                } else {
                    order.push(index);
                }
            }
            let camera_position = camera.position;
            blend_indices.sort_by(|&a, &b| {
                let distance_a = (glam::Vec3::from_array(self.draw_ranges[a].first_position)
                    - camera_position)
                    .length_squared();
                let distance_b = (glam::Vec3::from_array(self.draw_ranges[b].first_position)
                    - camera_position)
                    .length_squared();
                distance_b.total_cmp(&distance_a)
            });
            order.extend(blend_indices);

            let mut blend_pass_active = false;

            // Фон-небо (M4g) — первым; сцена с depth-тестом перекрывает его.
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.draw(0..3, 0..1);

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);

            for &index in &order {
                let range = self.draw_ranges[index];
                if range.blend != blend_pass_active {
                    blend_pass_active = range.blend;
                    if blend_pass_active {
                        pass.set_pipeline(&self.blend_pipeline);
                    } else {
                        pass.set_pipeline(&self.pipeline);
                    }
                    pass.set_bind_group(0, &self.camera_bind_group, &[]);
                }
                let mesh = self.mesh_cache.peek(range.mesh);
                let material = self.material_cache.peek(range.material);
                let (Some(mesh), Some(material)) = (mesh, material) else {
                    log::warn!(
                        "ресурс батча (меш {}, материал {}) выселен — пропуск",
                        range.mesh,
                        range.material
                    );
                    continue;
                };

                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                let stride = std::mem::size_of::<InstanceRaw>() as u64;
                let start = range.instance_offset as u64 * stride;
                let end = start + range.instance_count as u64 * stride;
                pass.set_vertex_buffer(1, self.instance_buffer.slice(start..end));
                pass.draw_indexed(0..mesh.index_count, 0, 0..range.instance_count);
            }
        }

        // Bloom (M4g-G3b): bright-pass из HDR в ¼ разрешения, затем
        // разделимое размытие — горизонталь и вертикаль.
        fullscreen_pass(
            &mut encoder,
            "zn-bloom-bright-pass",
            &self.bloom_a_view,
            &self.bloom_bright_pipeline,
            &self.bloom_bright_bind_group,
        );
        fullscreen_pass(
            &mut encoder,
            "zn-bloom-blur-h-pass",
            &self.bloom_b_view,
            &self.bloom_blur_pipeline,
            &self.bloom_blur_h_bind_group,
        );
        fullscreen_pass(
            &mut encoder,
            "zn-bloom-blur-v-pass",
            &self.bloom_a_view,
            &self.bloom_blur_pipeline,
            &self.bloom_blur_v_bind_group,
        );

        // Пост-пасс (M4g-G3): HDR-кадр + bloom → ACES, виньетка, sRGB —
        // в поверхность.
        fullscreen_pass(
            &mut encoder,
            "zn-post-pass",
            &view,
            &self.post_pipeline,
            &self.post_bind_group,
        );

        // Оверлей (UI): отдельный проход без depth attach — конвейер
        // egui-wgpu создаётся без depth-состояния.
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("zn-overlay-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            overlay(pass);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(texture);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_defaults_are_mild() {
        let settings = PostSettings::default();
        assert!((0.0..=1.0).contains(&settings.vignette));
        // Центр кадра не затемняется, углы — на силу виньетки.
        assert_eq!(settings.vignette_factor(0.0), 1.0);
        assert!(
            (settings.vignette_factor(1.0) - (1.0 - settings.vignette)).abs() < 1e-6,
            "в углу ожидалось 1 - сила виньетки"
        );
        // До порога затемнения нет (0.2 и 0.4 ниже 0.45), дальше — темнее.
        assert_eq!(settings.vignette_factor(0.2), settings.vignette_factor(0.4));
        assert!(settings.vignette_factor(0.9) < settings.vignette_factor(0.6));
        // Bloom: порог не ниже середины диапазона, вклад умеренный.
        assert!(settings.bloom_threshold > 0.5);
        assert!((0.0..=1.0).contains(&settings.bloom_intensity));
    }

    #[test]
    fn vignette_disabled_leaves_frame_untouched() {
        let settings = PostSettings {
            vignette: 0.0,
            ..PostSettings::default()
        };
        for d in [0.0, 0.3, 0.7, 0.9, 1.0] {
            assert_eq!(settings.vignette_factor(d), 1.0);
        }
    }

    #[test]
    fn bloom_contribution_starts_above_threshold() {
        let settings = PostSettings {
            bloom_threshold: 1.0,
            ..PostSettings::default()
        };
        // Тусклые пиксели и пиксели ровно на пороге — не светятся.
        assert_eq!(settings.bloom_contribution([0.2, 0.4, 0.1]), [0.0; 3]);
        assert_eq!(settings.bloom_contribution([1.0, 0.5, 0.0]), [0.0; 3]);
        // Выше порога: у серого вклад равен превышению порога.
        let dim = settings.bloom_contribution([1.2, 1.2, 1.2]);
        let bright = settings.bloom_contribution([2.4, 2.4, 2.4]);
        assert!((dim[0] - 0.2).abs() < 1e-5, "вклад 1.2 при пороге 1.0");
        assert!((bright[0] - 1.4).abs() < 1e-5, "вклад 2.4 при пороге 1.0");
        assert!(bright[0] > dim[0]);
    }
}
