//! Rendering into a native window surface for live playback.

use std::num::NonZeroU64;

use crate::media::{MediaTexture, PixelLayout};
use crate::{EngineError, Renderer};

/// Most layers one frame can draw (a crossfade needs two).
pub const MAX_LAYERS: usize = 4;
/// Uniform data of one layer: rect, opacity, mode, padding.
const LAYER_UNIFORM_SIZE: u64 = 32;
/// Offsets into a uniform buffer must be multiples of this (WebGPU default).
const LAYER_UNIFORM_STRIDE: u64 = 256;

/// One picture drawn over the whole output, fitted inside it
/// (letterboxed) and scaled by `opacity`.
pub struct Layer<'a> {
    pub texture: &'a MediaTexture,
    pub opacity: f32,
}

/// What one frame shows.
pub enum Scene<'a> {
    /// The moving test pattern used to check the window itself.
    TestPattern { time_secs: f32 },
    /// Pictures added together over black, back to front.
    Layers(&'a [Layer<'a>]),
}

/// Result of one [`Presenter::render`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A frame was rendered and queued for display.
    Presented,
    /// No frame could be acquired (window hidden, timeout or surface reconfigured).
    Skipped,
}

/// Draws frames into one window. Create it on the thread that owns the
/// window, then it can be moved to a dedicated render thread.
pub struct Presenter {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    layer_pipeline: wgpu::RenderPipeline,
    layer_layout: wgpu::BindGroupLayout,
    layer_uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

impl Presenter {
    pub fn new(
        renderer: &Renderer,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self, EngineError> {
        let device = renderer.device();
        let surface = renderer
            .instance()
            .create_surface(target)
            .map_err(|err| EngineError::Surface(err.to_string()))?;

        // The adapter is chosen before any window exists, so it may not be able
        // to present to this window (e.g. a different GPU drives the display).
        if !renderer.adapter().is_surface_supported(&surface) {
            return Err(EngineError::Surface(format!(
                "the GPU \"{}\" cannot present to this window",
                renderer.info().name
            )));
        }
        let caps = surface.get_capabilities(renderer.adapter());
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| {
                EngineError::Surface("the window surface has no usable format".into())
            })?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            // Fifo waits for vertical sync and is supported everywhere.
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fluxlide-test-pattern"),
            source: wgpu::ShaderSource::Wgsl(include_str!("test_pattern.wgsl").into()),
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fluxlide-test-pattern-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fluxlide-test-pattern-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fluxlide-test-pattern-bind-group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fluxlide-test-pattern-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fluxlide-test-pattern-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let (layer_pipeline, layer_layout) = create_layer_pipeline(device, format);
        let layer_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fluxlide-layer-uniforms"),
            size: LAYER_UNIFORM_STRIDE * MAX_LAYERS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fluxlide-picture-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Ok(Self {
            surface,
            config,
            pipeline,
            params,
            bind_group,
            layer_pipeline,
            layer_layout,
            layer_uniforms,
            sampler,
        })
    }

    /// Current surface size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, renderer: &Renderer, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(renderer.device(), &self.config);
    }

    /// Renders `scene` and presents it.
    /// Blocks until the display can accept the frame (vertical sync).
    pub fn render(
        &mut self,
        renderer: &Renderer,
        scene: &Scene<'_>,
    ) -> Result<FrameOutcome, EngineError> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // Present this frame, then reconfigure for the next one.
                self.surface.configure(renderer.device(), &self.config);
                frame
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(FrameOutcome::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(renderer.device(), &self.config);
                return Ok(FrameOutcome::Skipped);
            }
            other => {
                return Err(EngineError::Surface(format!(
                    "could not acquire a frame: {other:?}"
                )));
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fluxlide-present"),
                });
        // Bind groups must outlive the render pass that uses them.
        let layer_groups = match scene {
            Scene::TestPattern { time_secs } => {
                let (width, height) = self.size();
                let params = [*time_secs, width as f32, height as f32, 0.0];
                let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_ne_bytes()).collect();
                renderer.queue().write_buffer(&self.params, 0, &bytes);
                Vec::new()
            }
            Scene::Layers(layers) => self.prepare_layers(renderer, layers),
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fluxlide-scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            match scene {
                Scene::TestPattern { .. } => {
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &self.bind_group, &[]);
                    pass.draw(0..3, 0..1);
                }
                Scene::Layers(_) => {
                    pass.set_pipeline(&self.layer_pipeline);
                    for group in &layer_groups {
                        pass.set_bind_group(0, group, &[]);
                        pass.draw(0..4, 0..1);
                    }
                }
            }
        }
        renderer.queue().submit([encoder.finish()]);
        renderer.queue().present(frame);
        Ok(FrameOutcome::Presented)
    }

    /// Writes each layer's placement and opacity and binds its textures.
    fn prepare_layers(&self, renderer: &Renderer, layers: &[Layer<'_>]) -> Vec<wgpu::BindGroup> {
        let surface_size = self.size();
        layers
            .iter()
            .filter(|layer| layer.opacity > 0.0)
            .take(MAX_LAYERS)
            .enumerate()
            .map(|(slot, layer)| {
                let offset = slot as u64 * LAYER_UNIFORM_STRIDE;
                let [left, top, right, bottom] = fit_rect(layer.texture.size(), surface_size);
                let mode: u32 = match layer.texture.layout() {
                    PixelLayout::Rgba => 0,
                    PixelLayout::Yuv420 => 1,
                };
                let mut bytes = Vec::with_capacity(LAYER_UNIFORM_SIZE as usize);
                for value in [left, top, right, bottom, layer.opacity.min(1.0)] {
                    bytes.extend_from_slice(&value.to_ne_bytes());
                }
                bytes.extend_from_slice(&mode.to_ne_bytes());
                bytes.extend_from_slice(&[0; 8]);
                renderer
                    .queue()
                    .write_buffer(&self.layer_uniforms, offset, &bytes);

                let [view0, view1, view2] = layer.texture.plane_views();
                renderer
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fluxlide-layer-bind-group"),
                        layout: &self.layer_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &self.layer_uniforms,
                                    offset,
                                    size: NonZeroU64::new(LAYER_UNIFORM_SIZE),
                                }),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(view0),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::TextureView(view1),
                            },
                            wgpu::BindGroupEntry {
                                binding: 4,
                                resource: wgpu::BindingResource::TextureView(view2),
                            },
                        ],
                    })
            })
            .collect()
    }
}

/// Largest rectangle with the picture's aspect ratio that fits the output,
/// centered, as left, top, right, bottom in normalized device coordinates.
fn fit_rect(picture: (u32, u32), output: (u32, u32)) -> [f32; 4] {
    let (pw, ph) = (picture.0.max(1) as f32, picture.1.max(1) as f32);
    let (ow, oh) = (output.0.max(1) as f32, output.1.max(1) as f32);
    let scale = (ow / pw).min(oh / ph);
    let half_w = pw * scale / ow;
    let half_h = ph * scale / oh;
    [-half_w, half_h, half_w, -half_h]
}

fn create_layer_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("fluxlide-layer-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(LAYER_UNIFORM_SIZE),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture_entry(2),
            texture_entry(3),
            texture_entry(4),
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fluxlide-layer"),
        source: wgpu::ShaderSource::Wgsl(include_str!("layer.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("fluxlide-layer-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    // Additive: each layer is already scaled by its opacity in the shader.
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fluxlide-layer-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rect(actual: [f32; 4], expected: [f32; 4]) {
        let close = actual
            .iter()
            .zip(expected)
            .all(|(a, e)| (a - e).abs() < 1e-5);
        assert!(close, "{actual:?} != {expected:?}");
    }

    #[test]
    fn pictures_are_letterboxed_or_pillarboxed() {
        assert_rect(fit_rect((1920, 1080), (1920, 1080)), [-1.0, 1.0, 1.0, -1.0]);
        // A 4:3 photo on a 16:9 screen keeps the full height.
        assert_rect(
            fit_rect((4000, 3000), (1600, 900)),
            [-0.75, 1.0, 0.75, -1.0],
        );
        // A 16:9 video on a 16:10 screen keeps the full width.
        assert_rect(fit_rect((1920, 1080), (1920, 1200)), [-1.0, 0.9, 1.0, -0.9]);
    }
}
