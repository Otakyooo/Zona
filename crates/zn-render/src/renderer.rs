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

use std::sync::Arc;

use crate::camera::Camera;
use crate::mesh::{cpu_mesh_vertices, GpuMesh, Vertex};
use crate::renderer::types::CameraUniform;
use zn_assets::{CpuImage, CpuMaterial, CpuMesh};

pub const MAX_INSTANCES: usize = 8192;

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

/// Диапазон инстансов батча в общем буфере: смещение и количество.
#[derive(Clone, Copy, Debug)]
struct DrawRange {
    mesh: MeshId,
    material: MaterialId,
    instance_offset: u32,
    instance_count: u32,
}

pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    pipeline: wgpu::RenderPipeline,
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

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        let display_handle = window.display_handle().expect("display handle");
        let window_handle = window.window_handle().expect("window handle");
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: display_handle.as_raw(),
            raw_window_handle: window_handle.as_raw(),
        };
        let surface: wgpu::Surface<'static> =
            unsafe { instance.create_surface_unsafe(target) }.expect("failed to create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no compatible gpu adapter found");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("zn-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
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
        };
        surface.configure(&device, &config);

        let (depth_texture, depth_view) = create_depth(&device, &config);

        // Не-sRGB поверхность (Bgra8Unorm): шейдер кодирует линейный цвет сам.
        let output_srgb = if config.format.is_srgb() { 0.0 } else { 1.0 };

        let light_direction = glam::Vec3::new(0.4, 0.8, 0.3).normalize();
        let camera_uniform = CameraUniform {
            view_projection: glam::Mat4::IDENTITY.to_cols_array(),
            camera_position: [0.0, 0.0, 0.0, 1.0],
            light_direction: [light_direction.x, light_direction.y, light_direction.z, 0.0],
            light_color: [1.0, 0.96, 0.88, 1.0],
            ambient_color: [0.18, 0.20, 0.26, 1.0],
            output_srgb,
            _padding: [0.0; 3],
        };
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
                entries: &[wgpu::BindGroupLayoutEntry {
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
                }],
            });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zn-camera-bind-group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
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
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-pbr-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("pbr.wgsl"))),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("zn-pipeline-layout"),
            bind_group_layouts: &[&camera_bind_group_layout, &material_bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-pbr-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc(), InstanceRaw::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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
                cull_mode: Some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

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
            pipeline,
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
        renderer.default_material = renderer.register_material(&CpuMaterial::default(), &[]);

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
        debug_assert_eq!(
            image.pixels.len(),
            image.width as usize * image.height as usize * 4,
            "CpuImage должен быть RGBA8"
        );
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("zn-material-texture"),
            size: wgpu::Extent3d {
                width: image.width.max(1),
                height: image.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
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
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width.max(1),
                height: image.height.max(1),
                depth_or_array_layers: 1,
            },
        );
        let _ = material_name;
        let bytes = image.width as u64 * image.height as u64 * 4;
        (texture, bytes)
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
        self.camera_uniform.view_projection = camera.view_projection().to_cols_array();
        self.camera_uniform.camera_position = camera.position.extend(1.0).to_array();
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[self.camera_uniform]),
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
            let offset = flattened.len() as u32;
            flattened.extend_from_slice(&batch.instances);
            self.draw_ranges.push(DrawRange {
                mesh: batch.mesh,
                material: batch.material,
                instance_offset: offset,
                instance_count: batch.instances.len() as u32,
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

    /// Рендер + хук для оверлея (egui редактора): команды оверлея
    /// добавляются в тот же render pass после отрисовки сцены.
    /// Pass передаётся во владение — оверлей может вызвать
    /// `pass.forget_lifetime()` (egui-wgpu принимает `RenderPass<'static>`).
    pub fn render_with(&mut self, camera: &Camera, overlay: impl FnOnce(wgpu::RenderPass<'_>)) {
        self.update_camera(camera);

        let texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(error) => {
                log::warn!("surface error: {error}");
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

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("zn-main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
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
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);

            for index in 0..self.draw_ranges.len() {
                let range = self.draw_ranges[index];
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

            overlay(pass);
        }

        self.queue.submit(Some(encoder.finish()));
        texture.present();
    }
}
