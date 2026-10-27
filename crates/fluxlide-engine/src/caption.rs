//! Captions: title, author and description over the picture.
//!
//! Everything is sized from the output's height, so a caption looks the
//! same on a preview, on a projector and in an exported video of any size.
//! The sizes below are for a 1080 pixel high output and scale from there.

use crate::overlay::{Color, Overlay, OverlayPlan, PlanPiece, Rect, check_size};
use crate::text::{Halo, TextBlock, TextStyle, render_text};
use crate::{EngineError, Renderer};

/// Text of a caption. Any of the three may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caption {
    pub title: String,
    pub author: String,
    pub description: String,
}

impl Caption {
    pub fn is_empty(&self) -> bool {
        [&self.title, &self.author, &self.description]
            .iter()
            .all(|text| text.trim().is_empty())
    }
}

/// How a caption is laid out and decorated.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, ts_rs::TS,
)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CaptionDesign {
    /// A soft dark gradient along the bottom edge, text at the bottom left.
    #[default]
    Band,
    /// A thin vertical line and text at the bottom left, no background.
    Simple,
    /// Only the text at the bottom left, with a soft shadow.
    Plain,
    /// A light card at the bottom right with dark text.
    Card,
    /// No caption.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionStyle {
    pub design: CaptionDesign,
    /// Multiplies every text size; 1 is the standard size.
    pub text_scale: f32,
    /// Width of a black outline around light text, in pixels at a height
    /// of 1080 (scaled with the text); 0 for none. The card has none.
    pub outline: f32,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            design: CaptionDesign::default(),
            text_scale: 1.0,
            outline: 0.0,
        }
    }
}

/// Sizes as fractions of the output's height (45, 18 and 20 pixels at 1080).
const TITLE_SIZE: f32 = 0.0417;
const AUTHOR_SIZE: f32 = 0.0167;
const DESCRIPTION_SIZE: f32 = 0.0185;
/// Bold for the title, Light for the smaller supporting text.
const TITLE_WEIGHT: u16 = 700;
const AUTHOR_WEIGHT: u16 = 300;
const DESCRIPTION_WEIGHT: u16 = 300;
/// Letter spacing in em.
const TITLE_TRACKING: f32 = 0.01;
const AUTHOR_TRACKING: f32 = 0.025;
const DESCRIPTION_TRACKING: f32 = 0.0;
/// Space between baselines in multiples of the text size.
const TITLE_LINE_HEIGHT: f32 = 1.2;
const AUTHOR_LINE_HEIGHT: f32 = 1.2;
const DESCRIPTION_LINE_HEIGHT: f32 = 1.3;
/// Longest each part may get before it is cut with "…".
const TITLE_MAX_LINES: usize = 2;
const AUTHOR_MAX_LINES: usize = 1;
const DESCRIPTION_MAX_LINES: usize = 3;
/// Space under the title and under the author, as fractions of the height.
const GAP_AFTER_TITLE: f32 = 0.0139;
const GAP_AFTER_AUTHOR: f32 = 0.0046;
/// Distance of the caption from the edges of the output.
const MARGIN: f32 = 0.05;

const WHITE: [u8; 3] = [255, 255, 255];
/// Navy of the Fluxlide brand, for text on the card.
const NAVY: [u8; 3] = [0x13, 0x21, 0x3B];
const PAPER: [u8; 3] = [0xF5, 0xF6, 0xF8];

/// Which part of the caption a line is.
#[derive(Clone, Copy)]
enum Part {
    Title,
    Author,
    Description,
}

/// A rendered part with its position inside the text block.
struct Line {
    block: TextBlock,
    /// Distance of its top from the top of the text block.
    top: f32,
    part: Part,
}

impl Overlay {
    /// Lays out `caption` for an output of `size` pixels. Returns `None`
    /// when there is nothing to draw (no text, or [`CaptionDesign::None`]).
    pub fn caption(
        renderer: &Renderer,
        caption: &Caption,
        style: &CaptionStyle,
        size: (u32, u32),
    ) -> Result<Option<Self>, EngineError> {
        Ok(plan_caption(caption, style, size)?.map(|plan| Self::from_plan(renderer, plan)))
    }
}

/// Everything [`Overlay::caption`] draws, without touching the GPU.
pub(crate) fn plan_caption(
    caption: &Caption,
    style: &CaptionStyle,
    size: (u32, u32),
) -> Result<Option<OverlayPlan>, EngineError> {
    check_size(size)?;
    if style.design == CaptionDesign::None || caption.is_empty() {
        return Ok(None);
    }
    let (width, height) = (size.0 as f32, size.1 as f32);
    let margin = (height * MARGIN).round();
    let card_padding = (height * 0.022).round();
    let max_text_width = match style.design {
        CaptionDesign::Band => (width * 0.58).min(width - 2.0 * margin),
        CaptionDesign::Simple => (width * 0.6).min(width - 2.0 * margin - height * 0.03),
        CaptionDesign::Plain => (width * 0.6).min(width - 2.0 * margin),
        // Narrow outputs (portrait) get a wider card than 0.36 of the width.
        CaptionDesign::Card => (width * 0.36)
            .max(height * 0.3)
            .min(width - 2.0 * margin - 2.0 * card_padding),
        CaptionDesign::None => unreachable!("handled above"),
    };
    // Light text gets the outline it asks for, and a soft shadow when it
    // stands on the picture alone or has an outline.
    let outline = match style.design {
        CaptionDesign::Card => 0.0,
        _ => height / 1080.0 * style.outline * style.text_scale,
    };
    let shadowed =
        outline > 0.0 || matches!(style.design, CaptionDesign::Simple | CaptionDesign::Plain);
    let halo = Halo {
        outline,
        shadow: shadowed.then(|| ((height * 0.004).round().max(1.0) as u32, 0)),
    };

    let parts = [
        (Part::Title, &caption.title, TITLE_MAX_LINES),
        (Part::Author, &caption.author, AUTHOR_MAX_LINES),
        (
            Part::Description,
            &caption.description,
            DESCRIPTION_MAX_LINES,
        ),
    ];
    let mut lines: Vec<Line> = Vec::new();
    let mut top = 0.0f32;
    for (part, text, max_lines) in parts {
        let text_style = part_style(part, height, style.text_scale);
        let Some(block) = render_text(text, &text_style, max_text_width, max_lines, halo) else {
            continue;
        };
        let gap = match part {
            Part::Title => height * GAP_AFTER_TITLE,
            Part::Author => height * GAP_AFTER_AUTHOR,
            Part::Description => 0.0,
        };
        let block_height = block.height;
        lines.push(Line { block, top, part });
        top += block_height + gap;
    }
    // The last gap is not part of the block.
    let Some(last) = lines.last() else {
        return Ok(None);
    };
    let text_height = (last.top + last.block.height).round();
    let text_width = lines
        .iter()
        .map(|line| line.block.width)
        .fold(0.0f32, f32::max)
        .ceil();

    let mut pieces = Vec::new();
    // Top left of the text block, in output pixels.
    let (text_x, text_y) = match style.design {
        CaptionDesign::Band => {
            let bottom = height - (height * 0.045).round();
            let band_height = text_height + (height * 0.045).round() + (height * 0.12).round();
            pieces.push(PlanPiece::Shape {
                rect: Rect {
                    x: 0.0,
                    y: height - band_height,
                    width,
                    height: band_height,
                },
                color: Color::new([0, 0, 0], 1.0),
                radius: 0.0,
                border: 0.0,
                gradient: [0.0, 0.72],
            });
            (margin, bottom - text_height)
        }
        CaptionDesign::Simple => {
            let line_width = (height * 0.003).round().max(2.0);
            let gap = (height * 0.016).round();
            let top = height - margin - text_height;
            pieces.push(PlanPiece::Shape {
                rect: Rect {
                    x: margin,
                    y: top,
                    width: line_width,
                    height: text_height,
                },
                color: Color::new(WHITE, 0.9),
                radius: 0.0,
                border: 0.0,
                gradient: [1.0, 1.0],
            });
            (margin + line_width + gap, top)
        }
        CaptionDesign::Plain => (margin, height - margin - text_height),
        CaptionDesign::Card => {
            let card_width = text_width + 2.0 * card_padding;
            let card_height = text_height + 2.0 * card_padding;
            let card_x = width - margin - card_width;
            let card_y = height - margin - card_height;
            pieces.push(PlanPiece::Shape {
                rect: Rect {
                    x: card_x,
                    y: card_y,
                    width: card_width,
                    height: card_height,
                },
                color: Color::new(PAPER, 0.93),
                radius: (height * 0.008).round(),
                border: 0.0,
                gradient: [1.0, 1.0],
            });
            (card_x + card_padding, card_y + card_padding)
        }
        CaptionDesign::None => unreachable!("handled above"),
    };

    let (text_color, author_alpha, description_alpha) = match style.design {
        CaptionDesign::Card => (NAVY, 0.85, 0.8),
        _ => (WHITE, 0.88, 0.8),
    };
    for line in lines {
        let alpha = match line.part {
            Part::Title => 1.0,
            Part::Author => author_alpha,
            Part::Description => description_alpha,
        };
        pieces.push(PlanPiece::Text {
            x: text_x.round() as i32,
            y: (text_y + line.top).round() as i32,
            block: line.block,
            color: Color::new(text_color, alpha),
            shadow: if halo.shadow.is_some() { 0.55 } else { 0.0 },
        });
    }
    // The band, line or card is part 0, the lines 1, 2, 3 from the top.
    let mut line = 0;
    let parts = pieces
        .iter()
        .map(|piece| match piece {
            PlanPiece::Text { .. } => {
                line += 1;
                line
            }
            _ => 0,
        })
        .collect();
    Ok(Some(OverlayPlan {
        size,
        opacity: 1.0,
        pieces,
        parts,
    }))
}

/// How long each line of a caption takes to float in, how long it waits
/// after the line above it, and how far it rises (a fraction of the height).
const FLOAT_SECS: f32 = 0.6;
const FLOAT_STAGGER_SECS: f32 = 0.15;
const FLOAT_RISE: f32 = 0.02;

/// Opacity and downward shift in pixels of `part` of a caption, `secs`
/// after it started to float in on an output `height` pixels high. Part 0
/// (the band, line or card behind the text) fades in; the lines (1, 2 and
/// 3 from the top) each rise into place a little after the one above.
pub(crate) fn reveal(part: u8, secs: f32, height: f32) -> (f32, f32) {
    let start = f32::from(part.saturating_sub(1)) * FLOAT_STAGGER_SECS;
    let t = ((secs - start) / FLOAT_SECS).clamp(0.0, 1.0);
    if part == 0 {
        return (t, 0.0);
    }
    let eased = 1.0 - (1.0 - t).powi(3);
    // Whole pixels, so the text stays sharp as it moves.
    (eased, ((1.0 - eased) * FLOAT_RISE * height).round())
}

fn part_style(part: Part, output_height: f32, text_scale: f32) -> TextStyle {
    let (size, weight, tracking, line_height) = match part {
        Part::Title => (TITLE_SIZE, TITLE_WEIGHT, TITLE_TRACKING, TITLE_LINE_HEIGHT),
        Part::Author => (
            AUTHOR_SIZE,
            AUTHOR_WEIGHT,
            AUTHOR_TRACKING,
            AUTHOR_LINE_HEIGHT,
        ),
        Part::Description => (
            DESCRIPTION_SIZE,
            DESCRIPTION_WEIGHT,
            DESCRIPTION_TRACKING,
            DESCRIPTION_LINE_HEIGHT,
        ),
    };
    TextStyle {
        size: output_height * size * text_scale,
        weight,
        tracking,
        line_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caption() -> Caption {
        Caption {
            title: "夕暮れの港".into(),
            author: "撮影 山田 太郎".into(),
            description: "朝の光が差し込む静かな港町で、漁を終えた船がゆっくりと戻ってくる様子を撮影しました。".into(),
        }
    }

    fn text_boxes(plan: &OverlayPlan) -> Vec<(i32, i32, u32, u32)> {
        plan.pieces
            .iter()
            .filter_map(|piece| match piece {
                PlanPiece::Text { x, y, block, .. } => Some((
                    *x,
                    *y,
                    block.width.ceil() as u32,
                    block.height.ceil() as u32,
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn nothing_is_drawn_without_text_or_with_design_none() {
        let size = (1920, 1080);
        let empty = Caption::default();
        assert!(
            plan_caption(&empty, &CaptionStyle::default(), size)
                .unwrap()
                .is_none()
        );
        let none = CaptionStyle {
            design: CaptionDesign::None,
            ..Default::default()
        };
        assert!(plan_caption(&caption(), &none, size).unwrap().is_none());
    }

    #[test]
    fn lines_are_stacked_in_order_inside_the_bottom_left_corner() {
        let size = (1920, 1080);
        for design in [
            CaptionDesign::Band,
            CaptionDesign::Simple,
            CaptionDesign::Plain,
        ] {
            let style = CaptionStyle {
                design,
                ..Default::default()
            };
            let plan = plan_caption(&caption(), &style, size).unwrap().unwrap();
            let boxes = text_boxes(&plan);
            assert_eq!(boxes.len(), 3, "{design:?}");
            // Title, author, description from top to bottom, not overlapping.
            for pair in boxes.windows(2) {
                let (_, y, _, h) = pair[0];
                assert!(pair[1].1 >= y + h as i32, "{design:?}: {boxes:?}");
            }
            // All of it in the lower left, no wider than 70% of the width.
            for &(x, y, w, h) in &boxes {
                assert!(
                    x >= 54 && x as u32 + w <= 54 + 1344,
                    "{design:?}: {boxes:?}"
                );
                assert!(
                    y > 540 && y as u32 + h <= 1080 - 40,
                    "{design:?}: {boxes:?}"
                );
            }
        }
    }

    #[test]
    fn the_card_hugs_the_bottom_right_corner() {
        let style = CaptionStyle {
            design: CaptionDesign::Card,
            ..Default::default()
        };
        let plan = plan_caption(&caption(), &style, (1920, 1080))
            .unwrap()
            .unwrap();
        let PlanPiece::Shape { rect, .. } = &plan.pieces[0] else {
            panic!("the card comes first");
        };
        assert_eq!(rect.x + rect.width, 1920.0 - 54.0);
        assert_eq!(rect.y + rect.height, 1080.0 - 54.0);
        assert!(rect.width <= 0.36 * 1920.0 + 2.0 * 24.0);
        // The text is inside the card.
        for (x, y, w, h) in text_boxes(&plan) {
            assert!(x as f32 >= rect.x && (x as u32 + w) as f32 <= rect.x + rect.width);
            assert!(y as f32 >= rect.y && (y as u32 + h) as f32 <= rect.y + rect.height);
        }
    }

    #[test]
    fn an_outline_comes_with_a_shadow_and_scales_with_the_output() {
        // Outline width (from the padding) and shadow alpha of the title.
        let outline_and_shadow = |design, outline, size: (u32, u32)| {
            let style = CaptionStyle {
                design,
                outline,
                ..Default::default()
            };
            let plan = plan_caption(&caption(), &style, size).unwrap().unwrap();
            plan.pieces
                .iter()
                .find_map(|piece| match piece {
                    PlanPiece::Text { block, shadow, .. } => {
                        let blur = (size.1 as f32 * 0.004).round() as u32;
                        let width = match block.outline.is_empty() {
                            true => 0,
                            false => block.pad - 2 * blur - 1,
                        };
                        Some((width, *shadow))
                    }
                    _ => None,
                })
                .unwrap()
        };
        let hd = (1920, 1080);
        assert_eq!(outline_and_shadow(CaptionDesign::Plain, 0.0, hd), (0, 0.55));
        assert_eq!(outline_and_shadow(CaptionDesign::Band, 0.0, hd), (0, 0.0));
        // An outline as wide as asked for at 1080 and twice as wide at
        // 2160, with a shadow under it.
        assert_eq!(outline_and_shadow(CaptionDesign::Band, 3.0, hd), (3, 0.55));
        assert_eq!(
            outline_and_shadow(CaptionDesign::Plain, 3.0, (3840, 2160)),
            (6, 0.55)
        );
        // Dark text on the light card gets neither.
        assert_eq!(outline_and_shadow(CaptionDesign::Card, 3.0, hd), (0, 0.0));
    }

    #[test]
    fn lines_float_in_one_after_another() {
        let plan = plan_caption(&caption(), &CaptionStyle::default(), (1920, 1080))
            .unwrap()
            .unwrap();
        // The band, then the title, the author and the description.
        assert_eq!(plan.parts, [0, 1, 2, 3]);
        // At the start nothing shows and the lines sit lower.
        for part in 0..=3 {
            assert_eq!(reveal(part, 0.0, 1080.0).0, 0.0);
        }
        assert!(reveal(1, 0.0, 1080.0).1 > 20.0);
        // The title is on its way before the author starts, and so on.
        let (title, author, description) = (
            reveal(1, 0.2, 1080.0),
            reveal(2, 0.2, 1080.0),
            reveal(3, 0.2, 1080.0),
        );
        assert!(title.0 > author.0 && author.0 > description.0 && description.0 == 0.0);
        assert!(title.1 < author.1);
        // In the end every part is in place.
        for part in 0..=3 {
            assert_eq!(reveal(part, 2.0, 1080.0), (1.0, 0.0));
        }
    }

    #[test]
    fn sizes_follow_the_output_height() {
        let style = CaptionStyle::default();
        let small = plan_caption(&caption(), &style, (960, 540))
            .unwrap()
            .unwrap();
        let large = plan_caption(&caption(), &style, (3840, 2160))
            .unwrap()
            .unwrap();
        let (small, large) = (text_boxes(&small), text_boxes(&large));
        // Four times the height gives four times the line spacing (the
        // positions are rounded to pixels, so allow a pixel per level).
        for (s, l) in small.iter().zip(&large) {
            assert!((l.1 - 4 * s.1).abs() <= 4, "{small:?} {large:?}");
            assert!(
                (l.2 as i32 - 4 * s.2 as i32).abs() <= 8,
                "{small:?} {large:?}"
            );
        }
    }

    #[test]
    fn a_long_description_is_cut_to_three_lines() {
        let long = Caption {
            description: "長い説明です。".repeat(60),
            ..Default::default()
        };
        let plan = plan_caption(&long, &CaptionStyle::default(), (1920, 1080))
            .unwrap()
            .unwrap();
        let PlanPiece::Text { block, .. } = plan
            .pieces
            .iter()
            .find(|piece| matches!(piece, PlanPiece::Text { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(block.lines, 3);
    }
}
