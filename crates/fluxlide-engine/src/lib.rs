//! Fluxlide rendering engine.
//!
//! The same [`Renderer`] drives both live playback windows and offscreen
//! video export, so both paths share one GPU pipeline. This crate does not
//! depend on Tauri or any UI toolkit.

use std::fmt;
use std::sync::mpsc;

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
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter(err) => write!(f, "no compatible GPU adapter: {err}"),
            Self::Device(err) => write!(f, "failed to open GPU device: {err}"),
            Self::Readback(msg) => write!(f, "failed to read frame from GPU: {msg}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Owns the GPU device and queue shared by every output.
pub struct Renderer {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: GpuInfo,
}

impl Renderer {
    /// Opens the default high-performance GPU without a window surface.
    pub async fn new() -> Result<Self, EngineError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
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

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Renders a solid-color frame offscreen and returns tightly packed RGBA8 pixels.
    ///
    /// This is the minimal export path: render to a texture, copy it into a
    /// mappable buffer and read it back on the CPU.
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

        // Buffer rows must be aligned to COPY_BYTES_PER_ROW_ALIGNMENT.
        let unpadded_row = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = unpadded_row.div_ceil(align) * align;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fluxlide-readback"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

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
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        self.queue.submit([encoder.finish()]);

        let (tx, rx) = mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|err| EngineError::Readback(err.to_string()))?;
        rx.recv()
            .map_err(|err| EngineError::Readback(err.to_string()))?
            .map_err(|err| EngineError::Readback(err.to_string()))?;

        let mapped = buffer
            .get_mapped_range(..)
            .map_err(|err| EngineError::Readback(format!("{err:?}")))?;
        let mut pixels = Vec::with_capacity((unpadded_row * height) as usize);
        for row in mapped.chunks(padded_row as usize) {
            pixels.extend_from_slice(&row[..unpadded_row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_frame_matches_clear_color() {
        // CI runners may have no GPU; skip instead of failing there.
        let renderer = match Renderer::new_blocking() {
            Ok(renderer) => renderer,
            Err(err) => {
                eprintln!("skipping GPU test: {err}");
                return;
            }
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
