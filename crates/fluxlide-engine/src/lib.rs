//! Fluxlide rendering engine.
//!
//! The same [`Renderer`] drives both live playback windows and offscreen
//! video export, so both paths share one GPU pipeline. This crate does not
//! depend on Tauri or any UI toolkit.
//!
//! A [`Scene`] says what one frame shows. The [`Compositor`] draws it into
//! any texture view; a [`Presenter`] (window) and an [`OffscreenTarget`]
//! (export, previews) are the two places it is drawn into.

use std::fmt;

pub mod audio;
pub mod bgm;
pub mod calibration;
mod caption;
mod compositor;
pub mod export;
pub mod media;
mod offscreen;
mod overlay;
mod presenter;
#[cfg(test)]
mod render_tests;
mod scene;
pub mod show;
mod slideshow;
mod text;
mod title_card;
mod transition;
mod watermark;

pub use caption::{Caption, CaptionDesign, CaptionStyle};
pub use compositor::Compositor;
pub use media::{MediaKind, MediaTexture};
pub use offscreen::OffscreenTarget;
pub use overlay::Overlay;
pub use presenter::{FrameOutcome, Presenter};
pub use scene::{Fit, Frame, Layer, OverlayRef, Scene, Transform, TransitionRun};
pub use slideshow::{Position, Slideshow, SlideshowStats};
pub use transition::{Direction, Transition};
pub use watermark::{Corner, Watermark, WatermarkContent};

/// Pixel format used for offscreen frames handed to the encoder.
pub const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Summary of the GPU adapter the renderer runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
}

impl From<wgpu::AdapterInfo> for GpuInfo {
    fn from(info: wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name,
            backend: info.backend.to_string(),
            device_type: format!("{:?}", info.device_type),
        }
    }
}

#[derive(Debug)]
pub enum EngineError {
    NoAdapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
    Readback(String),
    Surface(String),
    /// A scene that cannot be drawn (an output with no pixels, an overlay
    /// made for another size, too many draw calls).
    Scene(String),
    /// A transition shader the GPU rejected. `message` is the compiler's.
    Shader {
        name: String,
        message: String,
    },
    Media(String),
    Audio(String),
    Export(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter(err) => write!(f, "no compatible GPU adapter: {err}"),
            Self::Device(err) => write!(f, "failed to open GPU device: {err}"),
            Self::Readback(msg) => write!(f, "failed to read frame from GPU: {msg}"),
            Self::Surface(msg) => write!(f, "window surface error: {msg}"),
            Self::Scene(msg) => write!(f, "cannot draw the scene: {msg}"),
            Self::Shader { name, message } => {
                write!(f, "transition shader \"{name}\" is invalid: {message}")
            }
            Self::Media(msg) => write!(f, "media error: {msg}"),
            Self::Audio(msg) => write!(f, "audio error: {msg}"),
            Self::Export(msg) => write!(f, "export error: {msg}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Owns the GPU device and queue shared by every output.
pub struct Renderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: GpuInfo,
}

impl Renderer {
    /// Opens the default high-performance GPU without a window surface.
    pub async fn new() -> Result<Self, EngineError> {
        // `WGPU_BACKEND` (vulkan, dx12, metal, gl) chooses the graphics API, for
        // telling a driver problem apart from ours.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .map_err(EngineError::NoAdapter)?;
        let info = GpuInfo::from(adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("fluxlide-device"),
                ..Default::default()
            })
            .await
            .map_err(EngineError::Device)?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            info,
        })
    }

    /// Blocking variant of [`Renderer::new`] for non-async callers.
    pub fn new_blocking() -> Result<Self, EngineError> {
        pollster::block_on(Self::new())
    }

    pub fn info(&self) -> &GpuInfo {
        &self.info
    }

    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Renders a solid-color frame offscreen and returns tightly packed RGBA8 pixels.
    ///
    /// The minimal offscreen path, kept as a smoke test of readback. Real
    /// scenes go through [`OffscreenTarget`].
    pub fn render_solid_frame(
        &self,
        width: u32,
        height: u32,
        color: wgpu::Color,
    ) -> Result<Vec<u8>, EngineError> {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fluxlide-offscreen-frame"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let buffer = offscreen::create_readback_buffer(self, width, height);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fluxlide-frame"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fluxlide-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit([encoder.finish()]);

        let mut pixels = Vec::new();
        offscreen::read_texture_rgba(self, &texture, (width, height), &buffer, &mut pixels)?;
        Ok(pixels)
    }
}

/// One renderer shared by every test and never dropped. Creating and
/// tearing down several devices crashed the Linux CI runner's software
/// driver when the test process exited.
#[cfg(test)]
pub(crate) fn test_renderer() -> Option<std::sync::Arc<Renderer>> {
    static RENDERER: std::sync::OnceLock<Option<std::sync::Arc<Renderer>>> =
        std::sync::OnceLock::new();
    RENDERER
        .get_or_init(|| match Renderer::new_blocking() {
            Ok(renderer) => Some(std::sync::Arc::new(renderer)),
            Err(err) => {
                // CI runners may have no GPU; skip instead of failing there.
                eprintln!("skipping GPU tests: {err}");
                None
            }
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_frame_matches_clear_color() {
        let Some(renderer) = test_renderer() else {
            return;
        };
        let (width, height) = (70, 3); // 70 * 4 bytes forces row padding
        let pixels = renderer
            .render_solid_frame(
                width,
                height,
                wgpu::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            )
            .expect("render frame");
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        assert!(pixels.chunks(4).all(|px| px == [255, 0, 0, 255]));
    }
}
