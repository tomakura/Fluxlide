//! Draws a [`Scene`] into any texture view.
//!
//! The compositor does not know where the pixels go. A window surface
//! ([`Presenter`](crate::Presenter)) and an offscreen texture
//! ([`OffscreenTarget`](crate::OffscreenTarget)) both use it, so live
//! playback, previews and video export draw with the same pipelines.
//!
//! One frame is drawn in these steps:
//!
//! 1. Each picture is placed on a black slide of the output's size
//!    ([`Fit`], [`Transform`]). `Fit::Blur` first draws a darkened, blurred
//!    copy of the picture behind it.
//! 2. With a transition running, the two slides are drawn into offscreen
//!    textures and the transition shader writes the target. Otherwise the
//!    one slide is drawn straight into the target.
//! 3. Overlays are drawn over the target, so a transition never moves them.
//!
//! All blending happens in linear light: the targets are sRGB textures, and
//! the hardware decodes on read and encodes on write.

use std::collections::HashMap;
use std::num::NonZeroU64;

use crate::caption;
use crate::media::PixelLayout;
use crate::overlay::OVERLAY_UNIFORM_SIZE;
use crate::scene::{Fit, Frame, Layer, OverlayRef, Scene, TransitionRun, fit_rect, placed_rect};
use crate::transition::{TRANSITION_UNIFORM_SIZE, Transition, TransitionLayouts};
use crate::{EngineError, Renderer};

/// Format of the offscreen slides and the blur textures. sRGB, so that
/// they hold the same 8-bit values the output does.
const SLIDE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Offsets into a uniform buffer must be multiples of this (WebGPU default).
const UNIFORM_STRIDE: u64 = 256;
/// Most draw calls with their own uniforms in one frame.
const UNIFORM_SLOTS: u64 = 128;

const LAYER_UNIFORM_SIZE: u64 = 32;
const BLUR_UNIFORM_SIZE: u64 = 16;

/// The blurred background is drawn at one eighth of the output's size.
const BLUR_DIVISOR: u32 = 8;
/// Distance between blur taps, in texels of the small texture.
const BLUR_TAP_STEP: f32 = 2.0;
/// How much of its brightness the blurred background keeps.
const BLUR_DIM: f32 = 0.55;
/// Samples per axis when the picture is shrunk into the small texture.
const BLUR_SUPERSAMPLE: u32 = 4;

/// Pipelines that write one color format.
struct FormatPipelines {
    picture: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
}

/// Textures the frame is built in. Sized to the output.
struct Targets {
    size: (u32, u32),
    from: wgpu::TextureView,
    to: wgpu::TextureView,
    blur_size: (u32, u32),
    /// Picture shrunk, then blurred horizontally, then vertically.
    blur: [wgpu::TextureView; 3],
}

/// Uniform data for each draw call, in slots of [`UNIFORM_STRIDE`] bytes.
/// Every slot is written once per frame; the queue applies all writes
/// before the frame's commands run, so draws cannot share a slot.
struct UniformArena {
    buffer: wgpu::Buffer,
    next: u64,
}

impl UniformArena {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fluxlide-uniforms"),
                size: UNIFORM_STRIDE * UNIFORM_SLOTS,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            next: 0,
        }
    }

    fn reset(&mut self) {
        self.next = 0;
    }

    /// Stores `bytes` in the next free slot and returns its dynamic offset.
    fn push(&mut self, queue: &wgpu::Queue, bytes: &[u8]) -> Result<u32, EngineError> {
        debug_assert!(bytes.len() as u64 <= UNIFORM_STRIDE);
        if self.next >= UNIFORM_SLOTS {
            return Err(EngineError::Scene(format!(
                "a frame needs more than {UNIFORM_SLOTS} draw calls"
            )));
        }
        let offset = self.next * UNIFORM_STRIDE;
        self.next += 1;
        queue.write_buffer(&self.buffer, offset, bytes);
        Ok(offset as u32)
    }

    /// Binds the first `size` bytes of the slot picked by the dynamic offset.
    fn binding(&self, size: u64) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &self.buffer,
            offset: 0,
            size: NonZeroU64::new(size),
        })
    }
}

/// Collects little-endian numbers for a uniform buffer.
#[derive(Default)]
struct UniformBytes(Vec<u8>);

impl UniformBytes {
    fn f32s(mut self, values: &[f32]) -> Self {
        for value in values {
            self.0.extend_from_slice(&value.to_ne_bytes());
        }
        self
    }

    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_ne_bytes());
        self
    }
}

/// Draws scenes into texture views of one color format.
pub struct Compositor {
    format: wgpu::TextureFormat,
    sampler: wgpu::Sampler,
    arena: UniformArena,
    picture_layout: wgpu::BindGroupLayout,
    blur_layout: wgpu::BindGroupLayout,
    overlay_layout: wgpu::BindGroupLayout,
    transition_layouts: TransitionLayouts,
    /// Pipelines for the offscreen slides and the blur textures.
    slide_pipes: FormatPipelines,
    /// Pipelines for the target.
    target_pipes: FormatPipelines,
    overlay_pipeline: wgpu::RenderPipeline,
    /// Draws overlays onto the offscreen slides.
    slide_overlay_pipeline: wgpu::RenderPipeline,
    transitions: HashMap<u64, wgpu::RenderPipeline>,
    test_pattern: TestPattern,
    /// Bound where an overlay piece has no texture of its own.
    blank: wgpu::TextureView,
    targets: Option<Targets>,
}

struct TestPattern {
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Compositor {
    /// Creates a compositor that draws into textures of `format`, which
    /// must be an sRGB format (the shaders output linear light).
    pub fn new(renderer: &Renderer, format: wgpu::TextureFormat) -> Result<Self, EngineError> {
        if !format.is_srgb() {
            return Err(EngineError::Scene(format!(
                "the output format {format:?} is not sRGB"
            )));
        }
        let device = renderer.device();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fluxlide-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_entry = |size: u64, visibility| wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        };
        let sampler_entry = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
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
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let picture_layout = layout(
            "fluxlide-picture-layout",
            &[
                uniform_entry(LAYER_UNIFORM_SIZE, wgpu::ShaderStages::VERTEX_FRAGMENT),
                sampler_entry,
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
            ],
        );
        let blur_layout = layout(
            "fluxlide-blur-layout",
            &[
                uniform_entry(BLUR_UNIFORM_SIZE, wgpu::ShaderStages::FRAGMENT),
                sampler_entry,
                texture_entry(2),
            ],
        );
        let overlay_layout = layout(
            "fluxlide-overlay-layout",
            &[
                uniform_entry(OVERLAY_UNIFORM_SIZE, wgpu::ShaderStages::VERTEX_FRAGMENT),
                sampler_entry,
                texture_entry(2),
            ],
        );

        let picture_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fluxlide-picture"),
            source: wgpu::ShaderSource::Wgsl(include_str!("layer.wgsl").into()),
        });
        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fluxlide-blur"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blur.wgsl").into()),
        });
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fluxlide-overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });

        let pipes = |format| FormatPipelines {
            picture: create_pipeline(
                device,
                "fluxlide-picture",
                &picture_layout,
                &picture_shader,
                wgpu::PrimitiveTopology::TriangleStrip,
                format,
                Some(PREMULTIPLIED_OVER),
            ),
            blur: create_pipeline(
                device,
                "fluxlide-blur",
                &blur_layout,
                &blur_shader,
                wgpu::PrimitiveTopology::TriangleList,
                format,
                None,
            ),
        };
        let overlay_pipeline = create_pipeline(
            device,
            "fluxlide-overlay",
            &overlay_layout,
            &overlay_shader,
            wgpu::PrimitiveTopology::TriangleStrip,
            format,
            Some(PREMULTIPLIED_OVER),
        );
        let slide_overlay_pipeline = create_pipeline(
            device,
            "fluxlide-slide-overlay",
            &overlay_layout,
            &overlay_shader,
            wgpu::PrimitiveTopology::TriangleStrip,
            SLIDE_FORMAT,
            Some(PREMULTIPLIED_OVER),
        );

        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("fluxlide-blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());

        Ok(Self {
            format,
            sampler,
            arena: UniformArena::new(device),
            slide_pipes: pipes(SLIDE_FORMAT),
            target_pipes: pipes(format),
            picture_layout,
            blur_layout,
            overlay_layout,
            transition_layouts: TransitionLayouts::new(device),
            overlay_pipeline,
            slide_overlay_pipeline,
            transitions: HashMap::new(),
            test_pattern: TestPattern::new(device, format),
            blank,
            targets: None,
        })
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Draws `scene` into `target`, a view of a `size` texture of this
    /// compositor's format, and submits the commands.
    pub fn draw(
        &mut self,
        renderer: &Renderer,
        target: &wgpu::TextureView,
        size: (u32, u32),
        scene: &Scene<'_>,
    ) -> Result<(), EngineError> {
        if size.0 == 0 || size.1 == 0 {
            return Err(EngineError::Scene("the output has no pixels".into()));
        }
        self.arena.reset();
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fluxlide-frame"),
                });
        match scene {
            Scene::TestPattern { time_secs } => {
                self.encode_test_pattern(renderer, &mut encoder, target, size, *time_secs);
            }
            Scene::Frame(frame) => {
                self.encode_frame(renderer, &mut encoder, target, size, frame)?
            }
        }
        renderer.queue().submit([encoder.finish()]);
        Ok(())
    }

    fn encode_test_pattern(
        &self,
        renderer: &Renderer,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        time_secs: f32,
    ) {
        let params = UniformBytes::default().f32s(&[time_secs, size.0 as f32, size.1 as f32, 0.0]);
        renderer
            .queue()
            .write_buffer(&self.test_pattern.params, 0, &params.0);
        let mut pass = begin_pass(encoder, target, Some(wgpu::Color::BLACK));
        pass.set_pipeline(&self.test_pattern.pipeline);
        pass.set_bind_group(0, &self.test_pattern.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn encode_frame(
        &mut self,
        renderer: &Renderer,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        frame: &Frame<'_>,
    ) -> Result<(), EngineError> {
        let running = frame.transition.filter(|run| run.progress < 1.0);
        let needs_blur = [&frame.from, &frame.to]
            .into_iter()
            .flatten()
            .any(|layer| layer.fit == Fit::Blur);
        if running.is_some() || needs_blur {
            self.ensure_targets(renderer.device(), size);
        }

        match running {
            Some(run) => {
                self.encode_slide(
                    renderer,
                    encoder,
                    frame.from.as_ref(),
                    SlideDest::From,
                    size,
                )?;
                self.encode_overlays(
                    renderer,
                    encoder,
                    SlideDest::From,
                    size,
                    &frame.from_overlays,
                )?;
                self.encode_slide(renderer, encoder, frame.to.as_ref(), SlideDest::To, size)?;
                self.encode_transition(renderer, encoder, target, size, run)?;
            }
            None => {
                self.encode_slide(
                    renderer,
                    encoder,
                    frame.to.as_ref(),
                    SlideDest::Target(target),
                    size,
                )?;
            }
        }
        self.encode_overlays(
            renderer,
            encoder,
            SlideDest::Target(target),
            size,
            &frame.overlays,
        )
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let texture = |label: &str, (width, height): (u32, u32)| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: SLIDE_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let blur_size = (
            size.0.div_ceil(BLUR_DIVISOR).max(1),
            size.1.div_ceil(BLUR_DIVISOR).max(1),
        );
        self.targets = Some(Targets {
            size,
            from: texture("fluxlide-slide-from", size),
            to: texture("fluxlide-slide-to", size),
            blur_size,
            blur: [
                texture("fluxlide-blur-0", blur_size),
                texture("fluxlide-blur-1", blur_size),
                texture("fluxlide-blur-2", blur_size),
            ],
        });
    }

    /// Draws one picture on a black slide (or just black for `None`).
    fn encode_slide(
        &mut self,
        renderer: &Renderer,
        encoder: &mut wgpu::CommandEncoder,
        layer: Option<&Layer<'_>>,
        dest: SlideDest<'_>,
        size: (u32, u32),
    ) -> Result<(), EngineError> {
        let device = renderer.device();
        let queue = renderer.queue();
        let (dest_view, dest_pipes) = match dest {
            SlideDest::Target(view) => (view, &self.target_pipes),
            SlideDest::From => (
                &self.targets.as_ref().expect("targets exist").from,
                &self.slide_pipes,
            ),
            SlideDest::To => (
                &self.targets.as_ref().expect("targets exist").to,
                &self.slide_pipes,
            ),
        };
        let Some(layer) = layer else {
            drop(begin_pass(encoder, dest_view, Some(wgpu::Color::BLACK)));
            return Ok(());
        };

        let picture_size = layer.texture.size();
        let rect = placed_rect(picture_size, size, layer.fit == Fit::Cover, layer.transform);
        let covers_output = rect[0] <= -1.0 && rect[2] >= 1.0 && rect[1] >= 1.0 && rect[3] <= -1.0;
        let blurred = layer.fit == Fit::Blur && !covers_output;

        if blurred {
            let targets = self.targets.as_ref().expect("targets exist");
            let [shrunk, across, down] = &targets.blur;
            let (small_w, small_h) = targets.blur_size;
            // The picture, covering the small texture.
            let cover = fit_rect(picture_size, size, true);
            let group = picture_bind_group(
                &mut self.arena,
                device,
                queue,
                &self.picture_layout,
                &self.sampler,
                layer,
                cover,
                1.0,
                BLUR_SUPERSAMPLE,
            )?;
            {
                let mut pass = begin_pass(encoder, shrunk, Some(wgpu::Color::BLACK));
                pass.set_pipeline(&self.slide_pipes.picture);
                pass.set_bind_group(0, &group.0, &[group.1]);
                pass.draw(0..4, 0..1);
            }
            for (source, dest, step) in [
                (shrunk, across, [BLUR_TAP_STEP / small_w as f32, 0.0]),
                (across, down, [0.0, BLUR_TAP_STEP / small_h as f32]),
            ] {
                let group = blur_bind_group(
                    &mut self.arena,
                    device,
                    queue,
                    &self.blur_layout,
                    &self.sampler,
                    source,
                    step,
                    1.0,
                    0,
                )?;
                let mut pass = begin_pass(encoder, dest, None);
                pass.set_pipeline(&self.slide_pipes.blur);
                pass.set_bind_group(0, &group.0, &[group.1]);
                pass.draw(0..3, 0..1);
            }
        }

        let background = if blurred {
            let down = &self.targets.as_ref().expect("targets exist").blur[2];
            Some(blur_bind_group(
                &mut self.arena,
                device,
                queue,
                &self.blur_layout,
                &self.sampler,
                down,
                [0.0, 0.0],
                BLUR_DIM,
                1,
            )?)
        } else {
            None
        };
        let group = picture_bind_group(
            &mut self.arena,
            device,
            queue,
            &self.picture_layout,
            &self.sampler,
            layer,
            rect,
            layer.opacity.clamp(0.0, 1.0),
            1,
        )?;
        let mut pass = begin_pass(encoder, dest_view, Some(wgpu::Color::BLACK));
        if let Some(background) = &background {
            pass.set_pipeline(&dest_pipes.blur);
            pass.set_bind_group(0, &background.0, &[background.1]);
            pass.draw(0..3, 0..1);
        }
        pass.set_pipeline(&dest_pipes.picture);
        pass.set_bind_group(0, &group.0, &[group.1]);
        pass.draw(0..4, 0..1);
        Ok(())
    }

    fn encode_transition(
        &mut self,
        renderer: &Renderer,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        run: TransitionRun<'_>,
    ) -> Result<(), EngineError> {
        let device = renderer.device();
        let transition: &Transition = run.transition;
        if !self.transitions.contains_key(&transition.key()) {
            let pipeline =
                transition.build_pipeline(device, &self.transition_layouts, self.format)?;
            self.transitions.insert(transition.key(), pipeline);
        }
        let pipeline = &self.transitions[&transition.key()];

        let [p0, p1, p2, p3] = transition.params();
        let uniforms = UniformBytes::default().f32s(&[
            size.0 as f32,
            size.1 as f32,
            run.progress.clamp(0.0, 1.0),
            size.0 as f32 / size.1 as f32,
            p0,
            p1,
            p2,
            p3,
        ]);
        let offset = self.arena.push(renderer.queue(), &uniforms.0)?;
        let targets = self.targets.as_ref().expect("targets exist");
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fluxlide-transition-bind-group"),
            layout: &self.transition_layouts.bind_group,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.arena.binding(TRANSITION_UNIFORM_SIZE),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&targets.from),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&targets.to),
                },
            ],
        });
        let mut pass = begin_pass(encoder, target, Some(wgpu::Color::BLACK));
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[offset]);
        pass.draw(0..3, 0..1);
        Ok(())
    }

    fn encode_overlays(
        &mut self,
        renderer: &Renderer,
        encoder: &mut wgpu::CommandEncoder,
        dest: SlideDest<'_>,
        size: (u32, u32),
        overlays: &[OverlayRef<'_>],
    ) -> Result<(), EngineError> {
        let device = renderer.device();
        // Bind groups must outlive the render pass that uses them.
        let mut groups: Vec<(wgpu::BindGroup, u32)> = Vec::new();
        for item in overlays {
            let opacity = (item.opacity * item.overlay.opacity()).clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            if item.overlay.size() != size {
                return Err(EngineError::Scene(format!(
                    "an overlay made for {}x{} was drawn on a {}x{} output",
                    item.overlay.size().0,
                    item.overlay.size().1,
                    size.0,
                    size.1
                )));
            }
            for (index, piece) in item.overlay.draws().iter().enumerate() {
                let (fade, shift) = item.reveal.map_or((1.0, 0.0), |secs| {
                    caption::reveal(item.overlay.part(index), secs, size.1 as f32)
                });
                if fade <= 0.0 {
                    continue;
                }
                let uniforms = piece.uniform_bytes(size, opacity * fade, shift);
                let offset = self.arena.push(renderer.queue(), &uniforms)?;
                let view = piece.texture_view().unwrap_or(&self.blank);
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fluxlide-overlay-bind-group"),
                    layout: &self.overlay_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.arena.binding(OVERLAY_UNIFORM_SIZE),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                    ],
                });
                groups.push((group, offset));
            }
        }
        if groups.is_empty() {
            return Ok(());
        }
        let (view, pipeline) = match dest {
            SlideDest::Target(view) => (view, &self.overlay_pipeline),
            SlideDest::From => (
                &self.targets.as_ref().expect("targets exist").from,
                &self.slide_overlay_pipeline,
            ),
            SlideDest::To => (
                &self.targets.as_ref().expect("targets exist").to,
                &self.slide_overlay_pipeline,
            ),
        };
        let mut pass = begin_pass(encoder, view, None);
        pass.set_pipeline(pipeline);
        for (group, offset) in &groups {
            pass.set_bind_group(0, group, &[*offset]);
            pass.draw(0..4, 0..1);
        }
        Ok(())
    }
}

/// Writes the picture's uniforms and binds its textures.
#[allow(clippy::too_many_arguments)]
fn picture_bind_group(
    arena: &mut UniformArena,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    layer: &Layer<'_>,
    rect: [f32; 4],
    opacity: f32,
    supersample: u32,
) -> Result<(wgpu::BindGroup, u32), EngineError> {
    let mode = match layer.texture.layout() {
        PixelLayout::Rgba => 0,
        PixelLayout::Yuv420 => 1,
    };
    let uniforms = UniformBytes::default()
        .f32s(&rect)
        .f32s(&[opacity])
        .u32(mode)
        .u32(supersample)
        .u32(0);
    let offset = arena.push(queue, &uniforms.0)?;
    let [view0, view1, view2] = layer.texture.plane_views();
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fluxlide-picture-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: arena.binding(LAYER_UNIFORM_SIZE),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
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
    });
    Ok((group, offset))
}

#[allow(clippy::too_many_arguments)]
fn blur_bind_group(
    arena: &mut UniformArena,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    source: &wgpu::TextureView,
    step: [f32; 2],
    dim: f32,
    mode: u32,
) -> Result<(wgpu::BindGroup, u32), EngineError> {
    let uniforms = UniformBytes::default().f32s(&step).f32s(&[dim]).u32(mode);
    let offset = arena.push(queue, &uniforms.0)?;
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fluxlide-blur-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: arena.binding(BLUR_UNIFORM_SIZE),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(source),
            },
        ],
    });
    Ok((group, offset))
}

/// Where a slide is drawn.
enum SlideDest<'a> {
    /// The frame's target itself (no transition running).
    Target(&'a wgpu::TextureView),
    /// The offscreen slide for the outgoing picture.
    From,
    /// The offscreen slide for the incoming picture.
    To,
}

/// Premultiplied "over": the shaders output color already multiplied by alpha.
const PREMULTIPLIED_OVER: wgpu::BlendState = wgpu::BlendState {
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
};

/// Starts a pass on `view`: cleared to `clear`, or kept as it is.
pub(crate) fn begin_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    clear: Option<wgpu::Color>,
) -> wgpu::RenderPass<'e> {
    encoder
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fluxlide-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: match clear {
                        Some(color) => wgpu::LoadOp::Clear(color),
                        None => wgpu::LoadOp::Load,
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        })
        .forget_lifetime()
}

pub(crate) fn create_pipeline(
    device: &wgpu::Device,
    label: &str,
    bind_group_layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    topology: wgpu::PrimitiveTopology,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl TestPattern {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
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
        let pipeline = create_pipeline(
            device,
            "fluxlide-test-pattern",
            &layout,
            &shader,
            wgpu::PrimitiveTopology::TriangleList,
            format,
            None,
        );
        Self {
            pipeline,
            params,
            bind_group,
        }
    }
}
