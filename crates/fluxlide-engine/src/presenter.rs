//! Rendering into a native window surface for live playback.

use crate::{EngineError, Renderer};

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

        Ok(Self {
            surface,
            config,
            pipeline,
            params,
            bind_group,
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

    /// Renders the test pattern at `time_secs` and presents it.
    /// Blocks until the display can accept the frame (vertical sync).
    pub fn render(
        &mut self,
        renderer: &Renderer,
        time_secs: f32,
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

        let (width, height) = self.size();
        let params = [time_secs, width as f32, height as f32, 0.0];
        let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_ne_bytes()).collect();
        renderer.queue().write_buffer(&self.params, 0, &bytes);

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fluxlide-present"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fluxlide-test-pattern"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        renderer.queue().submit([encoder.finish()]);
        renderer.queue().present(frame);
        Ok(FrameOutcome::Presented)
    }
}
