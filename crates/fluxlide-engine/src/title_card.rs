//! The screen shown before a presentation starts and after it ends: the
//! show's title under the Fluxlide mark, on black, so the audience never
//! waits in front of a blank screen (`docs/ui-draft.md`, E-5).
//!
//! The mark is drawn from its three plates (`assets/brand/fluxlide-mark.svg`)
//! rather than from a picture, so it is sharp at any size. On black, the two
//! back plates are white instead of navy.

use crate::overlay::{Color, Overlay, OverlayPlan, PlanPiece, Rect, check_size};
use crate::text::{TextStyle, render_text};
use crate::{EngineError, Renderer};

/// Height of the mark's 64-unit box, as a fraction of the output's height.
const LOGO_HEIGHT: f32 = 0.16;
/// The plates in the mark's 64-unit box, back to front: x, y and alpha.
const PLATES: [(f32, f32, f32); 3] = [(6.0, 9.0, 0.24), (15.0, 18.0, 0.5), (24.0, 27.0, 1.0)];
const PLATE_SIZE: (f32, f32) = (34.0, 24.0);
const PLATE_RADIUS: f32 = 4.0;
/// The part of the box the plates cover, from the top.
const PLATES_TOP: f32 = 9.0;
const PLATES_HEIGHT: f32 = 42.0;
const CORAL: [u8; 3] = [0xE8, 0x55, 0x3A];
/// Title size, as a fraction of the output's height.
const TITLE_SIZE: f32 = 0.055;
const TITLE_WEIGHT: u16 = 500;
const TITLE_TRACKING: f32 = 0.06;
/// Space between the logo and the title, as a fraction of the height.
const GAP: f32 = 0.05;
/// The title may use this much of the output's width.
const MAX_TITLE_WIDTH: f32 = 0.8;

impl Overlay {
    /// Lays out the title screen of a show called `title` (which may be
    /// empty) for an output of `size` pixels.
    pub fn title_card(
        renderer: &Renderer,
        title: &str,
        size: (u32, u32),
    ) -> Result<Self, EngineError> {
        Ok(Self::from_plan(renderer, plan_title_card(title, size)?))
    }
}

pub(crate) fn plan_title_card(title: &str, size: (u32, u32)) -> Result<OverlayPlan, EngineError> {
    check_size(size)?;
    let (width, height) = (size.0 as f32, size.1 as f32);
    let style = TextStyle {
        size: (height * TITLE_SIZE).round(),
        weight: TITLE_WEIGHT,
        tracking: TITLE_TRACKING,
        line_height: 1.3,
    };
    let shadow = ((height * 0.004).round().max(1.0) as u32, 1);
    let block = render_text(title, &style, width * MAX_TITLE_WIDTH, 2, Some(shadow));
    let unit = height * LOGO_HEIGHT / 64.0;

    // The plates and the title are centred together as one group.
    let gap = if block.is_some() {
        (height * GAP).round()
    } else {
        0.0
    };
    let group = PLATES_HEIGHT * unit + gap + block.as_ref().map_or(0.0, |block| block.height);
    let top = ((height - group) / 2.0).round();
    let left = (width - 64.0 * unit) / 2.0;
    let mut pieces: Vec<PlanPiece> = PLATES
        .iter()
        .map(|&(x, y, alpha)| PlanPiece::Shape {
            rect: Rect {
                x: (left + x * unit).round(),
                y: (top + (y - PLATES_TOP) * unit).round(),
                width: (PLATE_SIZE.0 * unit).round(),
                height: (PLATE_SIZE.1 * unit).round(),
            },
            color: match alpha {
                1.0 => Color::new(CORAL, 1.0),
                _ => Color::new([255, 255, 255], alpha),
            },
            radius: (PLATE_RADIUS * unit).round(),
            border: 0.0,
            gradient: [1.0, 1.0],
        })
        .collect();
    let y = (top + PLATES_HEIGHT * unit + gap).round();
    if let Some(block) = block {
        pieces.push(PlanPiece::Text {
            x: ((width - block.width) / 2.0).round() as i32,
            y: y as i32,
            block,
            color: Color::new([255, 255, 255], 1.0),
            shadow: 0.4,
        });
    }
    Ok(OverlayPlan {
        size,
        opacity: 1.0,
        pieces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_and_the_title_are_centred_together() {
        let plan = plan_title_card("港の写真展 2026", (1920, 1080)).unwrap();
        let [
            PlanPiece::Shape { rect: back, .. },
            _,
            PlanPiece::Shape { rect: front, .. },
            PlanPiece::Text {
                x, y, ref block, ..
            },
        ] = plan.pieces[..]
        else {
            panic!("three plates and the title");
        };
        let title_centre = x as f32 + block.width / 2.0;
        assert!((title_centre - 960.0).abs() <= 1.0, "{title_centre}");
        // The plates span 6 to 58 of the 64-unit box, around the centre.
        let plates_centre = (back.x + front.x + front.width) / 2.0;
        assert!((plates_centre - 960.0).abs() <= 2.0, "{plates_centre}");
        let group = (back.y, y as f32 + block.height);
        assert!(
            ((group.0 + group.1) / 2.0 - 540.0).abs() <= 1.0,
            "{group:?}"
        );

        // Without a title, only the mark.
        let plan = plan_title_card("  ", (1920, 1080)).unwrap();
        assert_eq!(plan.pieces.len(), 3);
    }
}
