//! Overlays: what is drawn over the picture and does not move with
//! transitions. Captions and watermarks are both overlays.
//!
//! An overlay is made for one output size. Its text is rasterized at that
//! size, so what is drawn does not depend on how the frame is shown.
//! Planning an overlay ([`OverlayPlan`]) is pure CPU work and needs no
//! GPU; [`Overlay::from_plan`] uploads the result.

use std::sync::Arc;

use crate::media::MediaTexture;
use crate::text::TextBlock;
use crate::{EngineError, Renderer};

/// Size of the uniform data of one overlay draw. Matches `Draw` in `overlay.wgsl`.
pub(crate) const OVERLAY_UNIFORM_SIZE: u64 = 80;

/// A color in sRGB with an alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Color {
    pub rgb: [u8; 3],
    pub alpha: f32,
}

impl Color {
    pub const fn new(rgb: [u8; 3], alpha: f32) -> Self {
        Self { rgb, alpha }
    }

    /// Linear-light RGB and alpha, not premultiplied, as the shader wants it.
    fn linear(self) -> [f32; 4] {
        let [r, g, b] = self.rgb.map(|c| srgb_to_linear(f32::from(c) / 255.0));
        [r, g, b, self.alpha]
    }

    /// Exponent applied to text coverage so that text of this color has
    /// the same weight as it would blended in sRGB: darker text gets
    /// thicker edges, lighter text thinner ones.
    fn text_power(self) -> f32 {
        let [r, g, b] = self.rgb.map(|c| f32::from(c) / 255.0);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        0.7 + 0.5 * luma
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A rectangle in output pixels, from the top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Left, top, right, bottom in normalized device coordinates.
    fn ndc(self, (width, height): (u32, u32)) -> [f32; 4] {
        let (w, h) = (width as f32, height as f32);
        [
            self.x / w * 2.0 - 1.0,
            1.0 - self.y / h * 2.0,
            (self.x + self.width) / w * 2.0 - 1.0,
            1.0 - (self.y + self.height) / h * 2.0,
        ]
    }
}

/// One piece of an overlay, before it is uploaded.
pub(crate) enum PlanPiece {
    /// A filled or outlined rounded rectangle.
    Shape {
        rect: Rect,
        color: Color,
        radius: f32,
        /// 0 fills the shape; above 0 draws only an outline this wide.
        border: f32,
        /// Alpha multipliers at the top and the bottom edge.
        gradient: [f32; 2],
    },
    /// Text, drawn pixel for pixel with its top left at `(x, y)`.
    Text {
        x: i32,
        y: i32,
        block: TextBlock,
        color: Color,
        /// Alpha of the shadow under the text; 0 for none.
        shadow: f32,
    },
    /// A picture scaled into `rect`.
    Image {
        rect: Rect,
        texture: Arc<MediaTexture>,
    },
}

/// Everything an overlay draws, laid out for one output size.
pub(crate) struct OverlayPlan {
    pub size: (u32, u32),
    pub opacity: f32,
    pub pieces: Vec<PlanPiece>,
}

/// One uploaded piece.
pub(crate) enum OverlayDraw {
    Shape {
        rect: Rect,
        color: [f32; 4],
        radius: f32,
        border: f32,
        gradient: [f32; 2],
    },
    Text {
        rect: Rect,
        mask: wgpu::TextureView,
        color: [f32; 4],
        /// See [`Color::text_power`].
        power: f32,
        shadow: f32,
    },
    Image {
        rect: Rect,
        texture: Arc<MediaTexture>,
    },
}

impl OverlayDraw {
    pub(crate) fn texture_view(&self) -> Option<&wgpu::TextureView> {
        match self {
            Self::Shape { .. } => None,
            Self::Text { mask, .. } => Some(mask),
            Self::Image { texture, .. } => Some(texture.plane_views()[0]),
        }
    }

    /// The `Draw` struct of `overlay.wgsl`.
    pub(crate) fn uniform_bytes(&self, output: (u32, u32), opacity: f32) -> Vec<u8> {
        let (rect, color, radius, border, gradient, shadow, kind, power) = match self {
            Self::Shape {
                rect,
                color,
                radius,
                border,
                gradient,
            } => (*rect, *color, *radius, *border, *gradient, 0.0, 0u32, 1.0),
            Self::Text {
                rect,
                color,
                power,
                shadow,
                ..
            } => (*rect, *color, 0.0, 0.0, [1.0, 1.0], *shadow, 1, *power),
            Self::Image { rect, .. } => (*rect, [1.0; 4], 0.0, 0.0, [1.0, 1.0], 0.0, 2, 1.0),
        };
        let mut bytes = Vec::with_capacity(OVERLAY_UNIFORM_SIZE as usize);
        let floats = rect
            .ndc(output)
            .into_iter()
            .chain(color)
            .chain([rect.width, rect.height, rect.x, rect.y])
            .chain(gradient)
            .chain([radius, border, opacity, shadow]);
        for value in floats {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        bytes.extend_from_slice(&kind.to_ne_bytes());
        bytes.extend_from_slice(&power.to_ne_bytes());
        debug_assert_eq!(bytes.len() as u64, OVERLAY_UNIFORM_SIZE);
        bytes
    }
}

/// A caption or a watermark, uploaded and ready to be put in a
/// [`Frame`](crate::Frame). Made for one output size.
pub struct Overlay {
    size: (u32, u32),
    opacity: f32,
    draws: Vec<OverlayDraw>,
}

impl Overlay {
    /// The output size this overlay was laid out for.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Opacity of the whole overlay; the opacity given in a frame is
    /// multiplied with it.
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub(crate) fn draws(&self) -> &[OverlayDraw] {
        &self.draws
    }

    /// Uploads the masks of `plan` to the GPU.
    pub(crate) fn from_plan(renderer: &Renderer, plan: OverlayPlan) -> Self {
        let draws = plan
            .pieces
            .into_iter()
            .map(|piece| match piece {
                PlanPiece::Shape {
                    rect,
                    color,
                    radius,
                    border,
                    gradient,
                } => OverlayDraw::Shape {
                    rect,
                    color: color.linear(),
                    radius,
                    border,
                    gradient,
                },
                PlanPiece::Text {
                    x,
                    y,
                    block,
                    color,
                    shadow,
                } => {
                    let (width, height) = block.canvas;
                    // Red: the text. Green: its shadow.
                    let mut texels = Vec::with_capacity(block.coverage.len() * 2);
                    for (i, &coverage) in block.coverage.iter().enumerate() {
                        texels.push(coverage);
                        texels.push(block.shadow.get(i).copied().unwrap_or(0));
                    }
                    OverlayDraw::Text {
                        rect: Rect {
                            x: (x - block.pad as i32) as f32,
                            y: (y - block.pad as i32) as f32,
                            width: width as f32,
                            height: height as f32,
                        },
                        mask: upload_mask(renderer, width, height, &texels),
                        color: color.linear(),
                        power: color.text_power(),
                        shadow,
                    }
                }
                PlanPiece::Image { rect, texture } => OverlayDraw::Image { rect, texture },
            })
            .collect();
        Self {
            size: plan.size,
            opacity: plan.opacity,
            draws,
        }
    }
}

fn upload_mask(renderer: &Renderer, width: u32, height: u32, texels: &[u8]) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = renderer.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("fluxlide-text-mask"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rg8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    renderer.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 2),
            rows_per_image: Some(height),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Checks that an overlay can be made for this output size.
pub(crate) fn check_size(size: (u32, u32)) -> Result<(), EngineError> {
    if size.0 == 0 || size.1 == 0 {
        return Err(EngineError::Scene("the output has no pixels".into()));
    }
    Ok(())
}
