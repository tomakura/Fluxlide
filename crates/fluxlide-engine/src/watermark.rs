//! Watermarks: a logo or a short text kept in one corner of the output.
//!
//! A watermark is an [`Overlay`], and overlays are drawn after the
//! transition, so it keeps its position and opacity while pictures slide,
//! zoom or fade.

use std::sync::Arc;

use crate::media::MediaTexture;
use crate::overlay::{Color, Overlay, OverlayPlan, PlanPiece, Rect, check_size};
use crate::text::{TextStyle, render_text};
use crate::{EngineError, Renderer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

pub enum WatermarkContent {
    /// A short text, drawn wide-spaced inside a thin frame.
    Text(String),
    /// A picture with transparency (a logo).
    Image(Arc<MediaTexture>),
}

pub struct Watermark {
    pub content: WatermarkContent,
    pub corner: Corner,
    /// 0 is invisible, 1 opaque.
    pub opacity: f32,
    /// Height of the watermark as a fraction of the output's height. For
    /// text, this is the height of its frame.
    pub size: f32,
}

impl Watermark {
    pub fn text(text: impl Into<String>, corner: Corner, opacity: f32) -> Self {
        Self {
            content: WatermarkContent::Text(text.into()),
            corner,
            opacity,
            size: 0.045,
        }
    }

    pub fn image(texture: Arc<MediaTexture>, corner: Corner, opacity: f32) -> Self {
        Self {
            content: WatermarkContent::Image(texture),
            corner,
            opacity,
            size: 0.08,
        }
    }
}

/// Distance from the edges of the output, as a fraction of its height.
const MARGIN: f32 = 0.035;
/// A logo is at most this wide, as a fraction of the output's width.
const MAX_IMAGE_WIDTH: f32 = 0.3;
const TEXT_WEIGHT: u16 = 500;
/// Wide letter spacing, in em.
const TEXT_TRACKING: f32 = 0.3;
/// Text size as a fraction of the frame's height.
const TEXT_TO_FRAME: f32 = 0.42;

impl Overlay {
    /// Lays out `watermark` for an output of `size` pixels.
    pub fn watermark(
        renderer: &Renderer,
        watermark: &Watermark,
        size: (u32, u32),
    ) -> Result<Self, EngineError> {
        Ok(Self::from_plan(renderer, plan_watermark(watermark, size)?))
    }
}

/// Everything [`Overlay::watermark`] draws, without touching the GPU.
pub(crate) fn plan_watermark(
    watermark: &Watermark,
    size: (u32, u32),
) -> Result<OverlayPlan, EngineError> {
    check_size(size)?;
    let (width, height) = (size.0 as f32, size.1 as f32);
    let margin = (height * MARGIN).round();
    let mut pieces = Vec::new();

    // Width and height of the watermark, then where it goes.
    // The text is laid out once, so the frame fits what is drawn in it.
    let mut text_block = None;
    let (content_width, content_height) = match &watermark.content {
        WatermarkContent::Image(texture) => {
            let (tw, th) = texture.size();
            let mut h = height * watermark.size;
            let mut w = h * tw.max(1) as f32 / th.max(1) as f32;
            if w > width * MAX_IMAGE_WIDTH {
                h *= width * MAX_IMAGE_WIDTH / w;
                w = width * MAX_IMAGE_WIDTH;
            }
            (w.round(), h.round())
        }
        WatermarkContent::Text(text) => {
            let frame_height = (height * watermark.size).round();
            let style = TextStyle {
                size: frame_height * TEXT_TO_FRAME,
                weight: TEXT_WEIGHT,
                tracking: TEXT_TRACKING,
                line_height: 1.2,
            };
            let max_width = width - 2.0 * margin - frame_height;
            let shadow = ((frame_height * 0.08).round().max(2.0) as u32, 1);
            let Some(block) = render_text(text, &style, max_width, 1, Some(shadow)) else {
                // Nothing to show for an empty text.
                return Ok(OverlayPlan {
                    size,
                    opacity: watermark.opacity,
                    pieces,
                });
            };
            let padding = frame_height * 0.5;
            let content = (block.width.ceil() + 2.0 * padding, frame_height);
            text_block = Some(block);
            content
        }
    };
    let x = match watermark.corner {
        Corner::TopLeft | Corner::BottomLeft => margin,
        Corner::TopRight | Corner::BottomRight => width - margin - content_width,
    };
    let y = match watermark.corner {
        Corner::TopLeft | Corner::TopRight => margin,
        Corner::BottomLeft | Corner::BottomRight => height - margin - content_height,
    };
    let rect = Rect {
        x,
        y,
        width: content_width,
        height: content_height,
    };

    match &watermark.content {
        WatermarkContent::Image(texture) => pieces.push(PlanPiece::Image {
            rect,
            texture: texture.clone(),
        }),
        WatermarkContent::Text(_) => {
            let block = text_block.expect("the text was laid out above");
            let white = Color::new([255, 255, 255], 1.0);
            pieces.push(PlanPiece::Shape {
                rect,
                color: white,
                radius: (content_height * 0.12).round(),
                border: (height * 0.0015).round().max(1.0),
                gradient: [1.0, 1.0],
            });
            pieces.push(PlanPiece::Text {
                x: (rect.x + (rect.width - block.width) / 2.0).round() as i32,
                y: (rect.y + (rect.height - block.height) / 2.0).round() as i32,
                block,
                color: white,
                shadow: 0.5,
            });
        }
    }
    Ok(OverlayPlan {
        size,
        opacity: watermark.opacity,
        pieces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_rect(corner: Corner, size: (u32, u32)) -> Rect {
        let watermark = Watermark::text("FLUXLIDE", corner, 0.6);
        let plan = plan_watermark(&watermark, size).unwrap();
        let PlanPiece::Shape { rect, .. } = plan.pieces[0] else {
            panic!("the frame comes first");
        };
        rect
    }

    #[test]
    fn each_corner_keeps_the_same_margin() {
        let size = (1920, 1080);
        let margin = (1080.0f32 * MARGIN).round();
        let top_left = text_rect(Corner::TopLeft, size);
        assert_eq!((top_left.x, top_left.y), (margin, margin));
        let top_right = text_rect(Corner::TopRight, size);
        assert_eq!(top_right.x + top_right.width, 1920.0 - margin);
        assert_eq!(top_right.y, margin);
        let bottom_left = text_rect(Corner::BottomLeft, size);
        assert_eq!(bottom_left.x, margin);
        assert_eq!(bottom_left.y + bottom_left.height, 1080.0 - margin);
        let bottom_right = text_rect(Corner::BottomRight, size);
        assert_eq!(bottom_right.x + bottom_right.width, 1920.0 - margin);
        assert_eq!(bottom_right.y + bottom_right.height, 1080.0 - margin);
    }

    #[test]
    fn a_long_text_is_cut_to_fit_inside_its_frame_and_the_output() {
        let text = "FLUXLIDE ".repeat(40);
        let watermark = Watermark::text(text, Corner::TopRight, 0.6);
        let plan = plan_watermark(&watermark, (640, 360)).unwrap();
        let PlanPiece::Shape { rect, .. } = plan.pieces[0] else {
            panic!("the frame comes first");
        };
        let PlanPiece::Text { x, ref block, .. } = plan.pieces[1] else {
            panic!("the text comes second");
        };
        assert!(rect.x >= 0.0, "{rect:?}");
        assert!(x as f32 >= rect.x, "{x} {rect:?}");
        assert!(
            x as f32 + block.width <= rect.x + rect.width + 1.0,
            "{rect:?}"
        );
    }

    #[test]
    fn the_overlay_keeps_the_watermarks_opacity() {
        let watermark = Watermark::text("FLUXLIDE", Corner::TopRight, 0.35);
        assert_eq!(
            plan_watermark(&watermark, (1920, 1080)).unwrap().opacity,
            0.35
        );
    }
}
