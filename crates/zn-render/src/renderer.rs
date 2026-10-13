pub mod types;

pub use types::InstanceRaw;

use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::camera::Camera;
use crate::mesh::{cube_geometry, Vertex};
use crate::renderer::types::CameraUniform;

pub const MAX_INSTANCES: usize = 8192;

fn create_depth(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> (wgpu::Texture, wgpu::TextureView) {
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
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    instance_buffer: wgpu::Buffer,
    instance_count: u32,
    clear_color: wgpu::Color,
}

impl Renderer {
    pub fn new(event_loop: &ActiveEventLoop) -> Self {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("ZONA — M1 vertical slice")
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

        let (vertices, indices) = cube_geometry();
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-cube-vertices"),
            size: (vertices.len() * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vertex_buffer, 0, bytemuck::cast_slice(&vertices));

        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-cube-indices"),
            size: (indices.len() * std::mem::size_of::<u16>()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&index_buffer, 0, bytemuck::cast_slice(&indices));

        let light_direction = glam::Vec3::new(0.4, 0.8, 0.3).normalize();
        let camera_uniform = CameraUniform {
            view_projection: glam::Mat4::IDENTITY.to_cols_array(),
            camera_position: [0.0, 0.0, 0.0, 1.0],
            light_direction: [light_direction.x, light_direction.y, light_direction.z, 0.0],
            light_color: [1.0, 0.96, 0.88, 1.0],
            ambient_color: [0.18, 0.20, 0.26, 1.0],
        };
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&camera_buffer, 0, bytemuck::cast_slice(&[camera_uniform]));

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("zn-camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<CameraUniform>() as u64),
                },
                count: None,
            }],
        });        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zn-camera-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("shader.wgsl"))),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("zn-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-cube-pipeline"),
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

        Self {
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
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            instance_buffer,
            instance_count: 0,
            clear_color: wgpu::Color { r: 0.02, g: 0.025, b: 0.035, a: 1.0 },
        }
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

    pub fn surface_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(self.config.width, self.config.height)
    }

    pub fn instance_count(&self) -> u32 {
        self.instance_count
    }

    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32) {
        self.clear_color = wgpu::Color { r: r as f64, g: g as f64, b: b as f64, a: 1.0 };
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

    pub fn update_camera(&mut self, camera: &Camera) {
        self.camera_uniform.view_projection = camera.view_projection().to_cols_array();
        self.camera_uniform.camera_position = camera.position.extend(1.0).to_array();
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[self.camera_uniform]));
    }

    pub fn update_instances(&mut self, instances: &[InstanceRaw]) {
        assert!(instances.len() <= MAX_INSTANCES, "instance overflow");
        self.instance_count = instances.len() as u32;
        if !instances.is_empty() {
            self.queue
                .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }
    }

    pub fn render(&mut self, camera: &Camera) {
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
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("zn-frame") });

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
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

            if self.instance_count > 0 {
                pass.draw_indexed(0..self.index_count, 0, 0..self.instance_count);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        texture.present();
    }
}
