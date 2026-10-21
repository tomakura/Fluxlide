//! Showing frames in a native window for live playback.

use crate::{Compositor, EngineError, Renderer, Scene};

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
///
/// The drawing itself is done by a [`Compositor`], which also draws into
/// offscreen textures; the presenter only owns the surface and hands the
/// compositor each frame's texture.
pub struct Presenter {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The sRGB format the compositor draws in: the surface's own, or an
    /// sRGB view of it.
    view_format: wgpu::TextureFormat,
    compositor: Compositor,
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
        // The compositor writes linear light and relies on an sRGB format
        // to encode it. A surface that offers only plain formats is drawn
        // through an sRGB view of one; without either it cannot be drawn
        // correctly.
        let (format, view_format) = match caps.formats.iter().copied().find(|f| f.is_srgb()) {
            Some(format) => (format, format),
            None => caps
                .formats
                .iter()
                .copied()
                .map(|format| (format, format.add_srgb_suffix()))
                .find(|(_, view)| view.is_srgb())
                .ok_or_else(|| {
                    EngineError::Surface("the window surface has no sRGB format".into())
                })?,
        };
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
            view_formats: if view_format == format {
                vec![]
            } else {
                vec![view_format]
            },
        };
        surface.configure(device, &config);

        Ok(Self {
            surface,
            config,
            view_format,
            compositor: Compositor::new(renderer, view_format)?,
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

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        self.compositor.draw(renderer, &view, self.size(), scene)?;
        renderer.queue().present(frame);
        Ok(FrameOutcome::Presented)
    }
}
