//! Japanese (and Latin) text turned into coverage masks on the CPU.
//!
//! Shaping, line breaking and rasterizing are done by `cosmic-text` (with
//! `swash` for the glyphs) using only the bundled IBM Plex Sans JP, so a caption
//! has the same pixels on every OS and in live playback and export alike.
//! The GPU only blends the finished masks.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use cosmic_text::fontdb::{self, Source};
use cosmic_text::{
    Attrs, Buffer, CacheKeyFlags, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
    Wrap,
};

/// IBM Plex Sans JP in the three weights the app draws with: Light (300)
/// for caption text, Medium (500) for the title card and the watermark,
/// Bold (700) for caption titles. Other weights get the nearest of these.
/// Embedded so that the app never depends on installed fonts or on the
/// location of resource files.
const FONTS: [&[u8]; 3] = [
    include_bytes!("../../../assets/fonts/IBMPlexSansJP-Light.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSansJP-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSansJP-Bold.ttf"),
];
const FONT_FAMILY: &str = "IBM Plex Sans JP";

/// How much a blurred shadow is strengthened before it is clamped.
const SHADOW_GAIN: f32 = 4.0;

/// Appended when text is cut to fit its line limit.
const ELLIPSIS: char = '…';

struct TextEngine {
    fonts: FontSystem,
    swash: SwashCache,
}

fn engine() -> MutexGuard<'static, TextEngine> {
    static ENGINE: OnceLock<Mutex<TextEngine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            for font in FONTS {
                db.load_font_source(Source::Binary(Arc::new(font)));
            }
            // The locale only steers font fallback, which has nothing to
            // choose from here. A fixed one keeps results identical everywhere.
            let fonts = FontSystem::new_with_locale_and_db("ja-JP".into(), db);
            Mutex::new(TextEngine {
                fonts,
                swash: SwashCache::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How one block of text looks. All sizes are in output pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextStyle {
    pub size: f32,
    /// 300 Light, 500 Medium or 700 Bold.
    pub weight: u16,
    /// Extra space after every character, in em.
    pub tracking: f32,
    /// Distance between baselines, in multiples of `size`.
    pub line_height: f32,
}

/// Text laid out and rasterized into a mask.
#[derive(Debug, Clone)]
pub(crate) struct TextBlock {
    /// The text that was laid out: the input, or its beginning ending with
    /// "…" when it was cut to fit the line limit. Read by tests.
    #[allow(dead_code)]
    pub text: String,
    /// Number of lines after wrapping. Read by tests.
    #[allow(dead_code)]
    pub lines: usize,
    /// Size of the laid out text. Trailing letter spacing is not counted.
    pub width: f32,
    pub height: f32,
    /// Empty pixels around the text in the masks, where a shadow can spread.
    pub pad: u32,
    /// Size of the masks: the text plus `pad` on every side.
    pub canvas: (u32, u32),
    /// One byte per pixel: how much of it the text covers.
    pub coverage: Vec<u8>,
    /// The text's soft shadow, same size as `coverage`.
    pub shadow: Vec<u8>,
}

/// Lays out `text` in at most `max_lines` lines no wider than `max_width`
/// pixels and rasterizes it. What does not fit is cut and ends with "…".
/// Returns `None` when there is nothing to draw.
///
/// `shadow` is `(blur radius, downward offset)` in pixels, or `None`; it
/// only decides how much padding the masks get and how `shadow` is filled.
pub(crate) fn render_text(
    text: &str,
    style: &TextStyle,
    max_width: f32,
    max_lines: usize,
    shadow: Option<(u32, u32)>,
) -> Option<TextBlock> {
    let text = text.trim();
    if text.is_empty() || max_lines == 0 || max_width < style.size {
        return None;
    }
    let mut engine = engine();
    let TextEngine { fonts, swash } = &mut *engine;

    let mut buffer = Buffer::new(
        fonts,
        Metrics::new(style.size, style.size * style.line_height),
    );
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_size(Some(max_width), None);
    let attrs = Attrs::new()
        .family(Family::Name(FONT_FAMILY))
        .weight(Weight(style.weight))
        .letter_spacing(style.tracking)
        // Hinting would change the shapes with the size. Unhinted glyphs
        // scale smoothly, so a preview looks like a smaller export.
        .cache_key_flags(CacheKeyFlags::DISABLE_HINTING);

    let mut shown = text.to_string();
    let mut truncated = false;
    loop {
        buffer.set_text(&shown, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        let runs: Vec<(usize, f32, usize)> = buffer
            .layout_runs()
            .map(|run| {
                let last_end = run.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
                (run.line_i, run.line_w, last_end)
            })
            .collect();
        if runs.len() <= max_lines {
            break;
        }
        if truncated {
            // "…" did not fit either: drop it and one more character.
            shown.pop();
            shown.pop();
        } else {
            // Keep what the last allowed line shows.
            let (line_i, _, last_end) = runs[max_lines - 1];
            let paragraph_start: usize = shown
                .split('\n')
                .take(line_i)
                .map(|paragraph| paragraph.len() + 1)
                .sum();
            shown.truncate((paragraph_start + last_end).min(shown.len()));
        }
        let kept = shown.trim_end().len();
        shown.truncate(kept);
        if shown.is_empty() {
            return None;
        }
        shown.push(ELLIPSIS);
        truncated = true;
    }

    let trailing = style.tracking * style.size;
    let mut width = 0.0f32;
    let mut height = 0.0f32;
    let mut lines = 0;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w - trailing);
        height = height.max(run.line_top + run.line_height);
        lines += 1;
    }
    if lines == 0 {
        return None;
    }

    let (blur, shift) = shadow.unwrap_or((0, 0));
    let pad = if shadow.is_some() {
        blur * 2 + shift
    } else {
        0
    }
    .max(2);
    let canvas = (
        width.ceil() as u32 + pad * 2,
        height.ceil() as u32 + pad * 2,
    );
    let mut coverage = vec![0u8; (canvas.0 * canvas.1) as usize];
    buffer.draw(
        fonts,
        swash,
        Color::rgb(255, 255, 255),
        |x, y, w, h, color| {
            for py in y..y + h as i32 {
                for px in x..x + w as i32 {
                    let (px, py) = (px + pad as i32, py + pad as i32);
                    if px < 0 || py < 0 || px >= canvas.0 as i32 || py >= canvas.1 as i32 {
                        continue;
                    }
                    let at = (py as u32 * canvas.0 + px as u32) as usize;
                    coverage[at] = coverage[at].saturating_add(color.a());
                }
            }
        },
    );
    let shadow = match shadow {
        Some((blur, shift)) => blurred(&coverage, canvas, blur, shift),
        None => Vec::new(),
    };
    Some(TextBlock {
        text: shown,
        lines,
        width,
        height,
        pad,
        canvas,
        coverage,
        shadow,
    })
}

/// A Gaussian blur of `mask` (sigma is half of `radius`), moved down by
/// `shift` and strengthened by `SHADOW_GAIN`: thin strokes blur to a faint
/// haze, which would not set light text off from a bright picture.
fn blurred(mask: &[u8], (width, height): (u32, u32), radius: u32, shift: u32) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let sigma = (radius as f32 / 2.0).max(0.5);
    let kernel: Vec<f32> = (-(radius as i32)..=radius as i32)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
    let radius = radius as i32;

    // Horizontal pass.
    let mut across = vec![0f32; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (k, weight) in kernel.iter().enumerate() {
                let sx = x as i32 + k as i32 - radius;
                if (0..width as i32).contains(&sx) {
                    sum += f32::from(mask[y * width + sx as usize]) * weight;
                }
            }
            across[y * width + x] = sum;
        }
    }
    // Vertical pass, reading `shift` rows higher so the result moves down.
    let mut out = vec![0u8; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (k, weight) in kernel.iter().enumerate() {
                let sy = y as i32 + k as i32 - radius - shift as i32;
                if (0..height as i32).contains(&sy) {
                    sum += across[sy as usize * width + x] * weight;
                }
            }
            out[y * width + x] = (sum * SHADOW_GAIN).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: TextStyle = TextStyle {
        size: 40.0,
        weight: 500,
        tracking: 0.06,
        line_height: 1.4,
    };

    #[test]
    fn empty_text_draws_nothing() {
        assert!(render_text("   ", &STYLE, 800.0, 3, None).is_none());
    }

    #[test]
    fn short_text_stays_on_one_line_and_leaves_ink() {
        let block = render_text("夕暮れの海", &STYLE, 800.0, 3, None).unwrap();
        assert_eq!(block.lines, 1);
        // Five characters of about one em plus tracking each.
        assert!((190.0..230.0).contains(&block.width), "{}", block.width);
        assert!(block.coverage.contains(&255));
        assert!(block.shadow.is_empty());
    }

    #[test]
    fn long_text_wraps_and_is_cut_at_the_line_limit_with_an_ellipsis() {
        let sentence =
            "朝の光が差し込む静かな港町で、漁を終えた船がゆっくりと戻ってくる様子を撮影しました。";
        let block = render_text(sentence, &STYLE, 400.0, 2, None).unwrap();
        assert_eq!(block.lines, 2);
        assert!(block.width <= 400.0, "{}", block.width);
        assert!(
            block.text.ends_with('…') && sentence.starts_with(&block.text[..block.text.len() - 3])
        );
        assert!(block.text.len() < sentence.len());
        // With more room, the whole sentence fits in fewer lines, uncut.
        let roomy = render_text(sentence, &STYLE, 2400.0, 2, None).unwrap();
        assert_eq!(roomy.lines, 1);
        assert_eq!(roomy.text, sentence);
    }

    #[test]
    fn closing_punctuation_never_starts_a_line() {
        // Every width from narrow to wide, so a break lands on 、 or 。 for some.
        let sentence = "春、夏、秋、冬。海、山、空。";
        for width in (80..400).step_by(7) {
            let mut engine = engine();
            let TextEngine { fonts, .. } = &mut *engine;
            let mut buffer = Buffer::new(fonts, Metrics::new(40.0, 56.0));
            buffer.set_wrap(Wrap::WordOrGlyph);
            buffer.set_size(Some(width as f32), None);
            buffer.set_text(
                sentence,
                &Attrs::new().family(Family::Name(FONT_FAMILY)),
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(fonts, false);
            for run in buffer.layout_runs() {
                let first = run.glyphs.first().map(|g| &run.text[g.start..g.end]);
                assert!(
                    !matches!(first, Some("、" | "。")),
                    "width {width}: a line starts with {first:?}"
                );
            }
        }
    }

    fn ink(text: &str, weight: u16) -> TextBlock {
        let style = TextStyle {
            weight,
            tracking: 0.0,
            ..STYLE
        };
        render_text(text, &style, 800.0, 1, None).unwrap()
    }

    #[test]
    fn each_weight_has_its_own_strokes() {
        let total = |block: &TextBlock| block.coverage.iter().map(|&c| u64::from(c)).sum::<u64>();
        let (light, medium, bold) = (
            total(&ink("田", 300)),
            total(&ink("田", 500)),
            total(&ink("田", 700)),
        );
        // Heavier weights put more ink on the page.
        assert!(light * 11 < medium * 10, "{light} {medium}");
        assert!(medium * 11 < bold * 10, "{medium} {bold}");
        // The same holds for Latin letters.
        assert!(total(&ink("Fluxlide", 300)) < total(&ink("Fluxlide", 500)));
    }

    #[test]
    fn characters_in_captions_are_not_drawn_as_tofu() {
        // Unassigned private-use characters are missing from the font, so
        // they show the font's "not defined" box. No real character may look like it.
        let tofu = ink("\u{E000}", 400).coverage;
        assert_eq!(
            tofu,
            ink("\u{E001}", 400).coverage,
            "the box is the same for any missing character"
        );
        assert!(
            tofu.iter().any(|&c| c > 0),
            "the missing glyph is drawn as a box"
        );
        let samples = "夕暮れの港撮影山田太郎朝光差込静町漁終船戻様子潮香声印象的一日でしたABCabc0123、。「」…・ー";
        for c in samples.chars() {
            assert_ne!(ink(&c.to_string(), 400).coverage, tofu, "{c} is tofu");
        }
    }

    #[test]
    fn shadow_spreads_beyond_the_ink() {
        let block = render_text("A", &STYLE, 800.0, 1, Some((6, 3))).unwrap();
        assert_eq!(block.shadow.len(), block.coverage.len());
        let ink = block.coverage.iter().filter(|&&c| c > 0).count();
        let dark = block.shadow.iter().filter(|&&c| c > 0).count();
        assert!(dark > ink);
    }
}
