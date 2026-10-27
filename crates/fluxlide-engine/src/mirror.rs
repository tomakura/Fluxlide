//! A small copy of what a presentation window shows, for the operator's
//! screen.
//!
//! With a mirror, the [`Presenter`](crate::Presenter) draws each frame into
//! a texture of the window's size and copies that onto the window. The same
//! texture is also scaled down and read back, frame after frame. A read-back
//! is not waited for: it is picked up on a later frame, so the window keeps
//! its frame rate. A few buffers take turns, so a copy can be started on
//! every frame while earlier ones are still on their way back.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::compositor::{begin_pass, create_pipeline};
use crate::offscreen::{create_readback_buffer, padded_row_bytes};
use crate::{FRAME_FORMAT, Renderer};

/// Read-back buffers taking turns.
const BUFFERS: usize = 3;

/// States of a read-back buffer.
const IDLE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

/// One small copy of the window, 8-bit sRGB RGBA, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorFrame {
    /// Counts up with every copy, so a newer one can be told apart.
    pub number: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub(crate) struct Mirror {
    /// Width of the copies; the height follows the window's shape.
    width: u32,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    to_window: wgpu::RenderPipeline,
    to_copy: wgpu::RenderPipeline,
    frame: Option<Sized>,
    /// Number of the last copy started.
    started: u64,
    /// Number of the last copy taken.
    taken: u64,
    latest: Option<MirrorFrame>,
}

/// What depends on the window's size.
struct Sized {
    window: (u32, u32),
    full: wgpu::TextureView,
    small: wgpu::Texture,
    small_view: wgpu::TextureView,
    small_size: (u32, u32),
    slots: Vec<Slot>,
    /// Reads `full`.
    group: wgpu::BindGroup,
}

/// A read-back buffer and the copy in it.
struct Slot {
    buffer: wgpu::Buffer,
    /// Set by the map callback.
    state: Arc<AtomicU8>,
    number: u64,
}

impl Mirror {
    /// A mirror whose copies are `width` pixels wide, for a window drawn
    /// in `format`.
    pub(crate) fn new(renderer: &Renderer, format: wgpu::TextureFormat, width: u32) -> Self {
        let device = renderer.device();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fluxlide-mirror-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fluxlide-mirror"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mirror.wgsl").into()),
        });
        let pipeline = |label, format| {
            create_pipeline(
                device,
                label,
                &layout,
                &shader,
                wgpu::PrimitiveTopology::TriangleList,
                format,
                None,
            )
        };
        let to_window = pipeline("fluxlide-mirror-window", format);
        let to_copy = pipeline("fluxlide-mirror-copy", FRAME_FORMAT);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fluxlide-mirror-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            width: width.max(2),
            layout,
            sampler,
            to_window,
            to_copy,
            frame: None,
            started: 0,
            taken: 0,
            latest: None,
        }
    }

    /// The texture to draw a frame of the window's `size` into, in the
    /// window's `format`, before [`Mirror::finish`].
    pub(crate) fn target(
        &mut self,
        renderer: &Renderer,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> &wgpu::TextureView {
        if self.frame.as_ref().is_none_or(|frame| frame.window != size) {
            // Copies still being read back go with the old buffers.
            self.frame = Some(self.sized(renderer, format, size));
        }
        &self.frame.as_ref().expect("made above").full
    }

    fn sized(&self, renderer: &Renderer, format: wgpu::TextureFormat, window: (u32, u32)) -> Sized {
        let device = renderer.device();
        let texture = |label, size: (u32, u32), format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let full = texture(
            "fluxlide-mirror-full",
            window,
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        let width = self.width.min(window.0);
        let height = ((u64::from(width) * u64::from(window.1)) / u64::from(window.0)).max(1) as u32;
        let small_size = (width, height);
        let small = texture(
            "fluxlide-mirror-small",
            small_size,
            FRAME_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let small_view = small.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fluxlide-mirror-group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&full),
                },
            ],
        });
        let slots = (0..BUFFERS)
            .map(|_| Slot {
                buffer: create_readback_buffer(renderer, width, height),
                state: Arc::new(AtomicU8::new(IDLE)),
                number: 0,
            })
            .collect();
        Sized {
            window,
            full,
            small,
            small_view,
            small_size,
            slots,
            group,
        }
    }

    /// Copies the frame drawn into [`Mirror::target`] onto `window`, and
    /// starts a small copy of it when a buffer is free.
    pub(crate) fn finish(&mut self, renderer: &Renderer, window: &wgpu::TextureView) {
        self.collect(renderer);
        let Some(frame) = &mut self.frame else {
            return;
        };
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fluxlide-mirror"),
                });
        {
            let mut pass = begin_pass(&mut encoder, window, None);
            pass.set_pipeline(&self.to_window);
            pass.set_bind_group(0, &frame.group, &[]);
            pass.draw(0..3, 0..1);
        }
        let free = frame
            .slots
            .iter_mut()
            .find(|slot| slot.state.load(Ordering::Acquire) == IDLE);
        if let Some(slot) = &free {
            {
                let mut pass = begin_pass(&mut encoder, &frame.small_view, None);
                pass.set_pipeline(&self.to_copy);
                pass.set_bind_group(0, &frame.group, &[]);
                pass.draw(0..3, 0..1);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &frame.small,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &slot.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_row_bytes(frame.small_size.0) as u32),
                        rows_per_image: Some(frame.small_size.1),
                    },
                },
                wgpu::Extent3d {
                    width: frame.small_size.0,
                    height: frame.small_size.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        renderer.queue().submit([encoder.finish()]);
        if let Some(slot) = free {
            self.started += 1;
            slot.number = self.started;
            slot.state.store(MAPPING, Ordering::Release);
            let state = slot.state.clone();
            slot.buffer
                .map_async(wgpu::MapMode::Read, .., move |result| {
                    let next = if result.is_ok() { MAPPED } else { FAILED };
                    state.store(next, Ordering::Release);
                });
        }
    }

    /// Picks up finished read-backs, keeping the newest.
    fn collect(&mut self, renderer: &Renderer) {
        let _ = renderer.device().poll(wgpu::PollType::Poll);
        let Some(frame) = &self.frame else {
            return;
        };
        let (width, height) = frame.small_size;
        let padded = padded_row_bytes(width) as usize;
        let row = width as usize * 4;
        for slot in &frame.slots {
            match slot.state.load(Ordering::Acquire) {
                MAPPED => {
                    let newest = self
                        .latest
                        .as_ref()
                        .map_or(self.taken, |latest| latest.number);
                    let newer = slot.number > newest;
                    if newer && let Ok(mapped) = slot.buffer.get_mapped_range(..) {
                        let mut rgba = Vec::with_capacity(row * height as usize);
                        for line in mapped.chunks(padded).take(height as usize) {
                            rgba.extend_from_slice(&line[..row]);
                        }
                        self.latest = Some(MirrorFrame {
                            number: slot.number,
                            width,
                            height,
                            rgba,
                        });
                    }
                    slot.buffer.unmap();
                    slot.state.store(IDLE, Ordering::Release);
                }
                FAILED => slot.state.store(IDLE, Ordering::Release),
                _ => {}
            }
        }
    }

    /// The newest small copy not taken yet.
    pub(crate) fn take(&mut self) -> Option<MirrorFrame> {
        let frame = self.latest.take()?;
        self.taken = frame.number;
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::read_texture_rgba;
    use std::time::Duration;

    use crate::{Compositor, Frame, Layer, MediaTexture, Scene};

    #[test]
    fn the_window_gets_the_frame_and_a_small_copy_comes_back() {
        let Some(renderer) = crate::test_renderer() else {
            return;
        };
        let size = (320, 180);
        let pixels: Vec<u8> = std::iter::repeat_n([200, 40, 90, 255], 16)
            .flatten()
            .collect();
        let picture = MediaTexture::from_rgba(&renderer, 4, 4, &pixels);
        // An offscreen texture stands in for the window.
        let window = renderer.device().create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let window_view = window.create_view(&wgpu::TextureViewDescriptor::default());
        let mut compositor = Compositor::new(&renderer, FRAME_FORMAT).unwrap();
        let mut mirror = Mirror::new(&renderer, FRAME_FORMAT, 64);
        let scene = Scene::Frame(Frame {
            to: Some(Layer::new(&picture)),
            ..Default::default()
        });

        let mut copy = None;
        for _ in 0..200 {
            let target = mirror.target(&renderer, FRAME_FORMAT, size);
            compositor.draw(&renderer, target, size, &scene).unwrap();
            mirror.finish(&renderer, &window_view);
            copy = mirror.take();
            if copy.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let copy = copy.expect("a copy comes back");
        assert_eq!((copy.width, copy.height), (64, 36));
        assert_eq!(copy.rgba.len(), 64 * 36 * 4);
        // The square picture sits in the middle, black on either side.
        let at = |rgba: &[u8], width: usize, x: usize, y: usize| {
            let i = (y * width + x) * 4;
            rgba[i..i + 4].to_vec()
        };
        assert_eq!(at(&copy.rgba, 64, 32, 18), [200, 40, 90, 255]);
        assert_eq!(at(&copy.rgba, 64, 2, 18), [0, 0, 0, 255]);

        let buffer = create_readback_buffer(&renderer, size.0, size.1);
        let mut shown = Vec::new();
        read_texture_rgba(&renderer, &window, size, &buffer, &mut shown).unwrap();
        assert_eq!(at(&shown, 320, 160, 90), [200, 40, 90, 255]);
        // Taken once; the next frames bring newer copies, one per frame.
        assert!(mirror.take().is_none());
        let mut numbers = vec![copy.number];
        for _ in 0..200 {
            let target = mirror.target(&renderer, FRAME_FORMAT, size);
            compositor.draw(&renderer, target, size, &scene).unwrap();
            mirror.finish(&renderer, &window_view);
            if let Some(copy) = mirror.take() {
                numbers.push(copy.number);
            }
            if numbers.len() == 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            numbers.windows(2).all(|pair| pair[0] < pair[1]),
            "{numbers:?}"
        );
    }
}
