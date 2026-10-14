//! Собственный painter UI по epaint: wgpu-конвейер для тесселированных
//! egui-мешей + атлас шрифта. Фолбэк из-за конфликта версий
//! (egui-wgpu 0.36 тянет wgpu 30, проект на wgpu 24).

use egui::epaint::{ClippedPrimitive, ImageDelta, Primitive, TextureId};

const INITIAL_VERTEX_CAPACITY: usize = 4096;
const INITIAL_INDEX_CAPACITY: usize = 8192;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UiVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UiLocals {
    screen_size: [f32; 2],
    dithering: u32,
    predictable_texture_filtering: u32,
}

pub struct EguiPainter {
    pipeline: wgpu::RenderPipeline,
    locals_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    texture_bind_group: Option<wgpu::BindGroup>,
    locals_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    font_texture: Option<wgpu::Texture>,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertex_capacity: usize,
    index_capacity: usize,
    warned_foreign_texture: bool,
    warned_callback: bool,
}

impl EguiPainter {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zn-editor-ui-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "ui_painter.wgsl"
            ))),
        });

        let locals_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("zn-ui-locals-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                }],
            });
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("zn-ui-texture-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("zn-ui-pipeline-layout"),
            bind_group_layouts: &[&locals_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zn-ui-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: 16,
                            shader_location: 2,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState {
                        // epaint-цвета — premultiplied alpha.
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                // Проход сцены несёт Depth24Plus: пайплайн должен объявить тот же
                // формат, но не писать и не тестировать глубину (UI поверх 3D).
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let locals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zn-ui-locals"),
            size: std::mem::size_of::<UiLocals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let locals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zn-ui-locals-bind-group"),
            layout: &locals_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: locals_buffer.as_entire_binding(),
            }],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("zn-ui-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let vertex_buffer = create_vertex_buffer(device, INITIAL_VERTEX_CAPACITY);
        let index_buffer = create_index_buffer(device, INITIAL_INDEX_CAPACITY);

        Self {
            pipeline,
            locals_bind_group,
            texture_bind_group_layout,
            texture_bind_group: None,
            locals_buffer,
            sampler,
            font_texture: None,
            vertex_buffer,
            index_buffer,
            vertex_capacity: INITIAL_VERTEX_CAPACITY,
            index_capacity: INITIAL_INDEX_CAPACITY,
            warned_foreign_texture: false,
            warned_callback: false,
        }
    }

    /// Загружает дельту атласа/текстур от egui (шрифт — Managed(0)).
    pub fn update_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        delta: &ImageDelta,
    ) {
        // ImageData в epaint 0.36 — единственный вариант Color.
        let egui::epaint::ImageData::Color(image) = &delta.image;
        let (width, height) = (image.size[0] as u32, image.size[1] as u32);
        let pixels: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(|color| [color.r(), color.g(), color.b(), color.a()])
            .collect();

        match &delta.pos {
            None => {
                // Полная замена: пересоздаём текстуру при смене размера.
                let recreate = self
                    .font_texture
                    .as_ref()
                    .map(|texture| texture.width() != width || texture.height() != height)
                    .unwrap_or(true);
                if recreate {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("zn-ui-font-atlas"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    self.font_texture = Some(texture);
                    self.texture_bind_group = None;
                }
                let texture = self.font_texture.as_ref().expect("font texture");
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &pixels,
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
            Some(pos) => {
                let Some(texture) = self.font_texture.as_ref() else {
                    return;
                };
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: pos[0] as u32,
                            y: pos[1] as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &pixels,
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
        }
    }

    /// Загружает тесселированные примитивы и рисует их в переданный pass.
    /// `screen_size_logical` — размер в точках, `pixels_per_point` — для scissor.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        clipped: &[ClippedPrimitive],
        screen_size_logical: [f32; 2],
        pixels_per_point: f32,
    ) {
        let locals = UiLocals {
            screen_size: screen_size_logical,
            dithering: 0,
            predictable_texture_filtering: 0,
        };
        queue.write_buffer(&self.locals_buffer, 0, bytemuck::cast_slice(&[locals]));

        let mut vertices: Vec<UiVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut draws: Vec<(egui::Rect, u32, u32)> = Vec::new();

        for ClippedPrimitive {
            clip_rect,
            primitive,
        } in clipped
        {
            let Primitive::Mesh(mesh) = primitive else {
                if !self.warned_callback {
                    self.warned_callback = true;
                    log::warn!("paint-callback примитивы в UI не поддерживаются");
                }
                continue;
            };
            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                continue;
            }
            if mesh.texture_id != TextureId::default() && !self.warned_foreign_texture {
                self.warned_foreign_texture = true;
                log::warn!("user-текстуры UI не поддерживаются — используется атлас шрифта");
            }

            let first_index = indices.len() as u32;
            let base_vertex = vertices.len() as u32;
            for vertex in &mesh.vertices {
                let [r, g, b, a] = vertex.color.to_array();
                vertices.push(UiVertex {
                    pos: [vertex.pos.x, vertex.pos.y],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24),
                });
            }
            for index in &mesh.indices {
                indices.push(*index + base_vertex);
            }
            draws.push((*clip_rect, first_index, mesh.indices.len() as u32));
        }

        if vertices.is_empty() || indices.is_empty() {
            return;
        }

        if vertices.len() > self.vertex_capacity {
            self.vertex_capacity = vertices.len().next_power_of_two();
            self.vertex_buffer = create_vertex_buffer(device, self.vertex_capacity);
        }
        if indices.len() > self.index_capacity {
            self.index_capacity = indices.len().next_power_of_two();
            self.index_buffer = create_index_buffer(device, self.index_capacity);
        }
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        queue.write_buffer(&self.index_buffer, 0, bytemuck::cast_slice(&indices));

        if self.texture_bind_group.is_none() {
            let Some(font_texture) = self.font_texture.as_ref() else {
                return; // атлас шрифта ещё не загружен
            };
            let view = font_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("zn-ui-texture-bind-group"),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.texture_bind_group = Some(bind_group);
        }

        let Some(texture_bind_group) = self.texture_bind_group.as_ref() else {
            return;
        };

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.locals_bind_group, &[]);
        pass.set_bind_group(1, texture_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);

        for (clip_rect, first_index, index_count) in draws {
            let min_x = (clip_rect.min.x * pixels_per_point).max(0.0) as u32;
            let min_y = (clip_rect.min.y * pixels_per_point).max(0.0) as u32;
            let max_x = (clip_rect.max.x * pixels_per_point)
                .max(min_x as f32 + 1.0)
                .ceil() as u32;
            let max_y = (clip_rect.max.y * pixels_per_point)
                .max(min_y as f32 + 1.0)
                .ceil() as u32;
            pass.set_scissor_rect(min_x, min_y, max_x - min_x, max_y - min_y);
            pass.draw_indexed(first_index..first_index + index_count, 0, 0..1);
        }
    }
}

fn create_vertex_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("zn-ui-vertices"),
        size: (capacity * std::mem::size_of::<UiVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_index_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("zn-ui-indices"),
        size: (capacity * std::mem::size_of::<u32>()) as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
