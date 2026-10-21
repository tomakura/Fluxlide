//! Description of what one frame shows. A [`Scene`] is drawn by the
//! [`Compositor`](crate::Compositor) into any texture view: a window
//! surface for live playback or an [`OffscreenTarget`](crate::OffscreenTarget)
//! for export and previews.

use crate::media::MediaTexture;
use crate::overlay::Overlay;
use crate::transition::Transition;

/// How a picture whose aspect ratio differs from the output is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    /// The whole picture is visible; the rest of the output stays black
    /// (letterbox or pillarbox).
    #[default]
    Contain,
    /// The picture covers the whole output; what sticks out is cropped.
    Cover,
    /// Like [`Fit::Contain`], over a darkened, blurred copy of the same
    /// picture that covers the whole output.
    Blur,
}

/// Moves and enlarges a picture after it has been fitted. This is what a
/// slow zoom (Ken Burns effect) animates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// 1 keeps the fitted size; 2 doubles it around the picture's center.
    pub scale: f32,
    /// Shift of the picture's center from the center of the output, in
    /// fractions of the output's width and height. Positive is right and down.
    pub offset: [f32; 2],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset: [0.0, 0.0],
        }
    }
}

/// One picture placed over the output.
pub struct Layer<'a> {
    pub texture: &'a MediaTexture,
    /// 0 is invisible, 1 opaque. Less than 1 shows what is behind the picture.
    pub opacity: f32,
    pub fit: Fit,
    pub transform: Transform,
}

impl<'a> Layer<'a> {
    /// An opaque picture, fitted with [`Fit::Contain`] and not transformed.
    pub fn new(texture: &'a MediaTexture) -> Self {
        Self {
            texture,
            opacity: 1.0,
            fit: Fit::default(),
            transform: Transform::default(),
        }
    }
}

/// A transition and how far it has gone.
#[derive(Clone, Copy)]
pub struct TransitionRun<'a> {
    pub transition: &'a Transition,
    /// 0 shows only the outgoing picture, 1 only the incoming one.
    pub progress: f32,
}

/// An overlay (caption or watermark) and its opacity.
#[derive(Clone, Copy)]
pub struct OverlayRef<'a> {
    pub overlay: &'a Overlay,
    pub opacity: f32,
}

/// One slideshow frame: a picture, or two pictures joined by a transition,
/// with overlays drawn on top. Overlays are never part of the transition.
#[derive(Default)]
pub struct Frame<'a> {
    /// The picture that is leaving. `None` is black.
    pub from: Option<Layer<'a>>,
    /// The picture that is arriving, or the one on screen.
    pub to: Option<Layer<'a>>,
    /// Joins `from` and `to`. Without it, or once its progress reaches 1,
    /// only `to` is drawn.
    pub transition: Option<TransitionRun<'a>>,
    /// Drawn in order, back to front, over the picture.
    pub overlays: Vec<OverlayRef<'a>>,
}

/// What one frame shows.
pub enum Scene<'a> {
    /// The moving test pattern used to check the window itself.
    TestPattern {
        time_secs: f32,
    },
    Frame(Frame<'a>),
}

/// Largest rectangle with the picture's aspect ratio that fits the output
/// (or, for `cover`, the smallest one that covers it), centered, as left,
/// top, right, bottom in normalized device coordinates.
pub(crate) fn fit_rect(picture: (u32, u32), output: (u32, u32), cover: bool) -> [f32; 4] {
    placed_rect(picture, output, cover, Transform::default())
}

/// [`fit_rect`] followed by `transform`.
pub(crate) fn placed_rect(
    picture: (u32, u32),
    output: (u32, u32),
    cover: bool,
    transform: Transform,
) -> [f32; 4] {
    let (pw, ph) = (picture.0.max(1) as f32, picture.1.max(1) as f32);
    let (ow, oh) = (output.0.max(1) as f32, output.1.max(1) as f32);
    let (sw, sh) = (ow / pw, oh / ph);
    let scale = if cover { sw.max(sh) } else { sw.min(sh) } * transform.scale;
    let half_w = pw * scale / ow;
    let half_h = ph * scale / oh;
    // The output spans 2 units in normalized coordinates, and y points up.
    let center_x = transform.offset[0] * 2.0;
    let center_y = -transform.offset[1] * 2.0;
    [
        center_x - half_w,
        center_y + half_h,
        center_x + half_w,
        center_y - half_h,
    ]
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
        assert_rect(
            fit_rect((1920, 1080), (1920, 1080), false),
            [-1.0, 1.0, 1.0, -1.0],
        );
        // A 4:3 photo on a 16:9 screen keeps the full height.
        assert_rect(
            fit_rect((4000, 3000), (1600, 900), false),
            [-0.75, 1.0, 0.75, -1.0],
        );
        // A 16:9 video on a 16:10 screen keeps the full width.
        assert_rect(
            fit_rect((1920, 1080), (1920, 1200), false),
            [-1.0, 0.9, 1.0, -0.9],
        );
    }

    #[test]
    fn cover_fills_the_output_and_crops_the_long_side() {
        // A 4:3 photo on a 16:9 screen fills the width and overflows the height.
        assert_rect(
            fit_rect((4000, 3000), (1600, 900), true),
            [-1.0, 4.0 / 3.0, 1.0, -4.0 / 3.0],
        );
        // A 16:9 video on a 16:10 screen fills the height and overflows the width.
        assert_rect(
            fit_rect((1920, 1080), (1920, 1200), true),
            [-1200.0 / 1080.0, 1.0, 1200.0 / 1080.0, -1.0],
        );
    }

    #[test]
    fn transform_scales_around_the_center_and_moves_in_output_fractions() {
        let transform = Transform {
            scale: 2.0,
            offset: [0.25, -0.1],
        };
        // The fitted rectangle is [-1, 1, 1, -1]; doubled, then 0.25 of the
        // width (0.5 units) right and 0.1 of the height (0.2 units) up.
        assert_rect(
            placed_rect((1920, 1080), (1920, 1080), false, transform),
            [-1.5, 2.2, 2.5, -1.8],
        );
    }
}
