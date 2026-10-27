//! Rendering into a texture that is not on screen, and reading the pixels
//! back. Video export and the preview in the management window use this.
//!
//! Pixels come back as 8-bit RGBA in the sRGB color space (the texture
//! format is [`FRAME_FORMAT`], and the GPU encodes linear light to sRGB on
//! write), the same values a window shows. Alpha is always 255. Rows are
//! tightly packed: `width * 4` bytes each, no padding.
//!
//! The GPU can only copy a texture into a buffer whose rows are padded to a
//! multiple of [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] (256) bytes. That
//! padding exists only in the temporary readback buffer and is removed
//! while copying out, so callers never see it.

use std::sync::mpsc;

use crate::{Compositor, EngineError, FRAME_FORMAT, Renderer, Scene};

/// An offscreen texture of a fixed size that scenes can be drawn into.
pub struct OffscreenTarget {
    compositor: Compositor,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    readback: wgpu::Buffer,
}

impl OffscreenTarget {
    pub fn new(renderer: &Renderer, width: u32, height: u32) -> Result<Self, EngineError> {
        if width == 0 || height == 0 {
            return Err(EngineError::Scene("the output has no pixels".into()));
        }
        let limit = renderer.device().limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            return Err(EngineError::Scene(format!(
                "{width}x{height} is larger than the {limit} pixels the GPU allows"
            )));
        }
        let texture = renderer.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("fluxlide-offscreen-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = create_readback_buffer(renderer, width, height);
        Ok(Self {
            compositor: Compositor::new(renderer, FRAME_FORMAT)?,
            texture,
            view,
            size: (width, height),
            readback,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The texture drawn into, e.g. to sample it in another pass.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Draws `scene`. Nothing is read back yet.
    pub fn render(&mut self, renderer: &Renderer, scene: &Scene<'_>) -> Result<(), EngineError> {
        self.compositor.draw(renderer, &self.view, self.size, scene)
    }

    /// Reads the last drawn frame into `out` (replacing its contents) as
    /// `width * height * 4` bytes of sRGB RGBA, rows top to bottom.
    pub fn read_rgba_into(
        &self,
        renderer: &Renderer,
        out: &mut Vec<u8>,
    ) -> Result<(), EngineError> {
        read_texture_rgba(renderer, &self.texture, self.size, &self.readback, out)
    }

    /// Draws `scene` and returns its pixels, as [`OffscreenTarget::read_rgba_into`] does.
    pub fn render_rgba(
        &mut self,
        renderer: &Renderer,
        scene: &Scene<'_>,
    ) -> Result<Vec<u8>, EngineError> {
        self.render(renderer, scene)?;
        let mut pixels = Vec::new();
        self.read_rgba_into(renderer, &mut pixels)?;
        Ok(pixels)
    }
}

/// Bytes in one row of the readback buffer: `width * 4` rounded up to
/// the alignment the GPU requires for texture-to-buffer copies.
pub(crate) fn padded_row_bytes(width: u32) -> u64 {
    let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    (u64::from(width) * 4).div_ceil(align) * align
}

/// Copies an RGBA8 texture into `buffer` (at least `padded_row_bytes *
/// height` large), waits for the copy and writes the tightly packed rows to `out`.
pub(crate) fn read_texture_rgba(
    renderer: &Renderer,
    texture: &wgpu::Texture,
    (width, height): (u32, u32),
    buffer: &wgpu::Buffer,
    out: &mut Vec<u8>,
) -> Result<(), EngineError> {
    let padded_row = padded_row_bytes(width);
    let unpadded_row = width as usize * 4;
    let mut encoder = renderer
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fluxlide-readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row as u32),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    renderer.queue().submit([encoder.finish()]);

    let (tx, rx) = mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = tx.send(result);
    });
    renderer
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|err| EngineError::Readback(err.to_string()))?;
    rx.recv()
        .map_err(|err| EngineError::Readback(err.to_string()))?
        .map_err(|err| EngineError::Readback(err.to_string()))?;

    let mapped = buffer
        .get_mapped_range(..)
        .map_err(|err| EngineError::Readback(format!("{err:?}")))?;
    out.clear();
    out.reserve(unpadded_row * height as usize);
    for row in mapped.chunks(padded_row as usize).take(height as usize) {
        out.extend_from_slice(&row[..unpadded_row]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(())
}

/// A readback buffer for a texture `width` pixels wide and `height` high.
pub(crate) fn create_readback_buffer(renderer: &Renderer, width: u32, height: u32) -> wgpu::Buffer {
    renderer.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("fluxlide-readback"),
        size: padded_row_bytes(width) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}
