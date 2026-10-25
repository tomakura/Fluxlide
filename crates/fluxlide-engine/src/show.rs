//! The definition of a slideshow, and the timeline worked out from it.
//!
//! A [`ShowDefinition`] is what the management window edits, what a project
//! saves and what the engine is given: the media files ([`Asset`]), the
//! slides that show them in order ([`Slide`]), and the settings shared by
//! all slides ([`Look`], [`Music`], [`Playback`]). It holds no GPU objects
//! and no paths outside the managed folder, so it can be stored, sent to
//! the webview as JSON and compared for undo.
//!
//! A [`Timeline`] places one pass of the slides on playback time. Live
//! playback, the preview and video export all read their timing from it, so
//! the three agree on when each slide starts and how it is joined to the
//! next. The design and its reasons are in `docs/scene-model.md`.
//!
//! # Timing
//!
//! All times are whole milliseconds. A slide's transition is the one used to
//! enter it, from the slide before (or from black for the first). A photo's
//! duration is the time it is fully visible: the transitions in and out come
//! on top of it. A video plays its chosen range once, while it is fully
//! visible: it waits on its first frame through the transition in, and
//! stays on its last frame through the transition out.
//!
//! ```text
//!            enter     shown (photo: duration,     leave = next's enter
//!                             video: its range)
//! slide 1  |=======|---------------------------|=======|
//! slide 2                                      |=======|-----------|====
//!          ^start                              ^start of slide 2
//! ```

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{CaptionDesign, Corner, Direction, Fit};

/// Version of the [`ShowDefinition`] layout. Raised when a stored project
/// needs converting to be read.
pub const SHOW_VERSION: u32 = 2;

/// Defaults from the screen design (`docs/ui-draft.md`).
pub const DEFAULT_PHOTO_MS: u32 = 10_000;
pub const DEFAULT_TRANSITION_MS: u32 = 1_000;
/// The paw closes and opens slowly, as in the PowerPoint it comes from.
pub const PAW_TRANSITION_MS: u32 = 5_000;

/// Identifies an [`Asset`] within its project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AssetId(pub u64);

/// Identifies a [`Slide`] within its project. Stays the same when slides
/// are reordered, so the selection and undo can refer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SlideId(pub u64);

/// Identifies a [`CustomTransition`] within its project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TransitionId(pub u64);

/// Everything needed to show, preview or export one slideshow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct ShowDefinition {
    pub version: u32,
    pub title: String,
    pub assets: Vec<Asset>,
    /// In playing order.
    pub slides: Vec<Slide>,
    pub look: Look,
    pub music: Music,
    pub playback: Playback,
}

impl Default for ShowDefinition {
    fn default() -> Self {
        Self {
            version: SHOW_VERSION,
            title: String::new(),
            assets: Vec::new(),
            slides: Vec::new(),
            look: Look::default(),
            music: Music::default(),
            playback: Playback::default(),
        }
    }
}

impl ShowDefinition {
    pub fn asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    pub fn custom_transition(&self, id: TransitionId) -> Option<&CustomTransition> {
        self.look
            .custom_transitions
            .iter()
            .find(|transition| transition.id == id)
    }
}

/// A photo or video copied into the project's managed folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: AssetId,
    pub kind: AssetKind,
    /// File name inside the managed folder. The folder itself is not part of
    /// the definition, so a project can be moved.
    pub file: String,
    /// Name of the file it was copied from, shown to the user.
    pub original_name: String,
    pub state: AssetState,
    /// Title, author and description, used by captions.
    #[serde(default)]
    pub meta: AssetMeta,
    /// Size in pixels after the orientation is applied, once known.
    #[serde(default)]
    pub size: Option<[u32; 2]>,
    /// Length of a video, once known.
    #[serde(default)]
    pub duration_ms: Option<u32>,
    /// Whether a video has sound.
    #[serde(default)]
    pub has_audio: bool,
}

impl Asset {
    pub fn is_ready(&self) -> bool {
        self.state == AssetState::Ready
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    Photo,
    Video,
}

/// Whether an asset's copy can be used. How far a copy has got is not
/// stored: it lives only while the import runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum AssetState {
    /// Being copied; its slide is shown in the list but skipped in playback.
    Importing,
    Ready,
    /// The copy or the first read failed.
    Failed {
        reason: String,
    },
}

/// What the user typed about a photo or video. Any of the three may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct AssetMeta {
    pub title: String,
    pub author: String,
    pub description: String,
}

/// One entry of the slide list. Settings left as `None` follow [`Look`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Slide {
    pub id: SlideId,
    pub asset: AssetId,
    /// How long a photo stays fully visible. Ignored for videos.
    #[serde(default)]
    pub duration_ms: Option<u32>,
    /// How this slide is entered from the one before.
    #[serde(default)]
    pub transition: Option<TransitionChoice>,
    #[serde(default)]
    pub transition_ms: Option<u32>,
    #[serde(default = "default_true")]
    pub show_caption: bool,
    /// Slow zoom while the slide is on screen.
    #[serde(default)]
    pub motion: Option<Motion>,
    /// Where the picture sits, before any slow zoom.
    #[serde(default)]
    pub framing: Framing,
    /// Only for videos.
    #[serde(default)]
    pub video: VideoOptions,
}

impl Slide {
    /// A slide with every setting following the look.
    pub fn new(id: SlideId, asset: AssetId) -> Self {
        Self {
            id,
            asset,
            duration_ms: None,
            transition: None,
            transition_ms: None,
            show_caption: true,
            motion: None,
            framing: Framing::default(),
            video: VideoOptions::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// A transition chosen by name, turned into a shader by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TransitionChoice {
    /// No transition: the next picture replaces the last at once.
    Cut,
    Fade,
    Slide {
        direction: Direction,
    },
    Wipe {
        direction: Direction,
    },
    Zoom,
    /// A paw-shaped hole closes and opens again.
    Paw,
    /// One of [`Look::custom_transitions`].
    Custom {
        id: TransitionId,
    },
}

impl TransitionChoice {
    /// Length used when neither the slide nor the look sets one.
    pub fn default_ms(self) -> u32 {
        match self {
            Self::Cut => 0,
            Self::Paw => PAW_TRANSITION_MS,
            _ => DEFAULT_TRANSITION_MS,
        }
    }
}

/// A transition written by the user in WGSL (see `transition.rs` for the
/// contract the source must follow).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CustomTransition {
    pub id: TransitionId,
    pub name: String,
    pub wgsl: String,
    #[serde(default)]
    pub params: [f32; 4],
}

/// Slow zoom (Ken Burns effect) across the time a slide is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Motion {
    /// Scale at the start and at the end, relative to the framing (1 = as framed).
    pub from_scale: f32,
    pub to_scale: f32,
    /// Shift at the start and at the end, in fractions of the output.
    #[serde(default)]
    pub from_offset: [f32; 2],
    #[serde(default)]
    pub to_offset: [f32; 2],
}

/// Placement of the picture on its slide.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct Framing {
    /// `None` follows [`Look::fit`].
    pub fit: Option<Fit>,
    pub scale: f32,
    /// Shift of the picture's centre, in fractions of the output.
    pub offset: [f32; 2],
}

impl Default for Framing {
    fn default() -> Self {
        Self {
            fit: None,
            scale: 1.0,
            offset: [0.0, 0.0],
        }
    }
}

/// Which part of a video plays, and how loud.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct VideoOptions {
    /// Where playback starts in the file.
    pub start_ms: u32,
    /// Where it ends; `None` plays to the end of the file.
    pub end_ms: Option<u32>,
    /// Volume of the video's own sound, 0 to 1.
    pub volume: f32,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            start_ms: 0,
            end_ms: None,
            volume: 1.0,
        }
    }
}

impl VideoOptions {
    /// Length of the chosen range of a video `file_ms` long.
    pub fn range_ms(&self, file_ms: u32) -> u32 {
        let end = self.end_ms.unwrap_or(file_ms).min(file_ms);
        end.saturating_sub(self.start_ms)
    }
}

/// Settings shared by all slides (the "見た目" tab).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct Look {
    pub photo_ms: u32,
    pub transition: TransitionChoice,
    /// Length of every transition whose slide sets none. `None`: each
    /// transition's own length ([`TransitionChoice::default_ms`]).
    pub transition_ms: Option<u32>,
    pub fit: Fit,
    pub caption: CaptionLook,
    pub watermark: Option<WatermarkSpec>,
    pub custom_transitions: Vec<CustomTransition>,
    /// Slow zoom for every photo whose slide has none of its own. A slide
    /// that should stay still while this is set has a motion of 1 to 1.
    pub motion: Option<Motion>,
}

impl Look {
    /// Length of the fade to black after the last slide.
    pub fn to_black_ms(&self) -> u32 {
        self.transition_ms
            .unwrap_or(TransitionChoice::Fade.default_ms())
    }
}

impl Default for Look {
    fn default() -> Self {
        Self {
            photo_ms: DEFAULT_PHOTO_MS,
            transition: TransitionChoice::Fade,
            transition_ms: None,
            fit: Fit::Blur,
            caption: CaptionLook::default(),
            watermark: None,
            custom_transitions: Vec::new(),
            motion: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptionLook {
    pub design: CaptionDesign,
    /// Multiplies every text size; 1 is the standard size.
    pub text_scale: f32,
}

impl Default for CaptionLook {
    fn default() -> Self {
        Self {
            design: CaptionDesign::Band,
            text_scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatermarkSpec {
    pub content: WatermarkSource,
    pub corner: Corner,
    pub opacity: f32,
    /// Height as a fraction of the output's height.
    pub size: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum WatermarkSource {
    Text {
        text: String,
    },
    /// A logo, copied into the managed folder like the photos.
    Image {
        asset: AssetId,
    },
}

/// Background music (the "音楽" tab).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct Music {
    pub source: Option<MusicSource>,
    pub shuffle: bool,
    pub volume: f32,
    /// Fade the music out while a video with sound plays.
    pub duck_for_videos: bool,
    pub fade_ms: u32,
    pub gap_ms: u32,
    pub duck_fade_out_ms: u32,
    pub duck_fade_in_ms: u32,
}

impl Default for Music {
    fn default() -> Self {
        Self {
            source: None,
            shuffle: true,
            volume: 0.5,
            duck_for_videos: true,
            fade_ms: 2_000,
            gap_ms: 1_000,
            duck_fade_out_ms: 1_000,
            duck_fade_in_ms: 2_000,
        }
    }
}

/// Where the music comes from. Unlike photos, music is not copied: a folder
/// is read again each time playback starts, so songs added to it are
/// picked up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MusicSource {
    Folder { path: String },
    Files { paths: Vec<String> },
}

/// How playback proceeds (chosen on the output screen, kept with the show).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct Playback {
    pub advance: Advance,
    pub at_end: AtEnd,
    pub order: Order,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Advance {
    /// Each slide moves on after its time.
    #[default]
    Auto,
    /// Slides move on only when the operator says so.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum AtEnd {
    #[default]
    Loop,
    /// Stay on the last slide.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Order {
    #[default]
    AsListed,
    /// A new random order each pass.
    Shuffle,
}

// ---------------------------------------------------------------------------
// Timeline

/// One pass of the slides placed on playback time.
#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub entries: Vec<TimelineEntry>,
    /// Slides left out, and why.
    pub skipped: Vec<(SlideId, SkipReason)>,
    /// When the last slide has fully left (its fade to black included).
    pub end_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    MissingAsset,
    /// Still being copied, or the copy failed.
    AssetNotReady,
    /// A video whose length is not known yet, or whose range is empty.
    NoLength,
}

/// When one slide is on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineEntry {
    pub slide: SlideId,
    pub asset: AssetId,
    /// The transition that brings this slide in (from black for the first).
    pub enter: TransitionChoice,
    /// When it starts to come in.
    pub start_ms: u64,
    /// Length of the transition in; it is fully visible from
    /// `start_ms + enter_ms`.
    pub enter_ms: u32,
    /// When the next slide (or black) starts to replace it.
    pub leave_ms: u64,
    /// When it is gone: the end of the transition out.
    pub end_ms: u64,
    /// For a video, where in the file playback starts, at
    /// [`play_start_ms`](Self::play_start_ms). It plays until `leave_ms`.
    pub video_start_ms: Option<u32>,
}

impl TimelineEntry {
    /// When the slide is fully visible: the end of its transition in, and
    /// when a video starts to play.
    pub fn play_start_ms(&self) -> u64 {
        self.start_ms + u64::from(self.enter_ms)
    }
}

impl Timeline {
    /// Places the slides of `show` in the order of `order` (indices into
    /// `show.slides`; pass `0..n` for the listed order). After the last slide
    /// the picture fades to black, as at the end of an export or of a show
    /// that does not loop. A looping show starts the next pass at the last
    /// entry's `leave_ms` instead.
    pub fn build(show: &ShowDefinition, order: &[usize]) -> Self {
        let look = &show.look;
        let mut skipped = Vec::new();
        // Each usable slide with the time it is fully visible: the photo's
        // duration or the video's range.
        let mut slides = Vec::new();
        for &index in order {
            let Some(slide) = show.slides.get(index) else {
                continue;
            };
            let Some(asset) = show.asset(slide.asset) else {
                skipped.push((slide.id, SkipReason::MissingAsset));
                continue;
            };
            if !asset.is_ready() {
                skipped.push((slide.id, SkipReason::AssetNotReady));
                continue;
            }
            let shown = match asset.kind {
                AssetKind::Photo => slide.duration_ms.unwrap_or(look.photo_ms),
                AssetKind::Video => match asset.duration_ms.map(|ms| slide.video.range_ms(ms)) {
                    Some(range) if range > 0 => range,
                    _ => {
                        skipped.push((slide.id, SkipReason::NoLength));
                        continue;
                    }
                },
            };
            slides.push((slide, asset, shown));
        }

        // The transition into each slide, and the final one to black.
        let mut joins: Vec<(TransitionChoice, u32)> = slides
            .iter()
            .map(|(slide, _, _)| {
                let choice = slide.transition.unwrap_or(look.transition);
                let ms = match choice {
                    TransitionChoice::Cut => 0,
                    _ => slide
                        .transition_ms
                        .or(look.transition_ms)
                        .unwrap_or(choice.default_ms()),
                };
                (choice, ms)
            })
            .collect();
        joins.push((TransitionChoice::Fade, look.to_black_ms()));

        let mut entries = Vec::with_capacity(slides.len());
        let mut start = 0u64;
        for (index, (slide, asset, shown)) in slides.iter().enumerate() {
            let (enter, enter_ms) = joins[index];
            let leave_ms = joins[index + 1].1;
            // Where the next slide starts to come in.
            let leave = start + u64::from(enter_ms) + u64::from(*shown);
            entries.push(TimelineEntry {
                slide: slide.id,
                asset: asset.id,
                enter,
                start_ms: start,
                enter_ms,
                leave_ms: leave,
                end_ms: leave + u64::from(leave_ms),
                video_start_ms: (asset.kind == AssetKind::Video).then_some(slide.video.start_ms),
            });
            start = leave;
        }
        let end_ms = entries.last().map_or(0, |entry| entry.end_ms);
        Self {
            entries,
            skipped,
            end_ms,
        }
    }

    /// The entries on screen at `time_ms`: the one coming in last.
    pub fn at(&self, time_ms: u64) -> impl Iterator<Item = &TimelineEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.start_ms <= time_ms && time_ms < entry.end_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(id: u64) -> Asset {
        Asset {
            id: AssetId(id),
            kind: AssetKind::Photo,
            file: format!("{id}.jpg"),
            original_name: format!("photo{id}.jpg"),
            state: AssetState::Ready,
            meta: AssetMeta::default(),
            size: Some([4032, 3024]),
            duration_ms: None,
            has_audio: false,
        }
    }

    fn video(id: u64, duration_ms: u32) -> Asset {
        Asset {
            kind: AssetKind::Video,
            file: format!("{id}.mp4"),
            duration_ms: Some(duration_ms),
            has_audio: true,
            ..photo(id)
        }
    }

    fn show(assets: Vec<Asset>) -> ShowDefinition {
        let slides = assets
            .iter()
            .enumerate()
            .map(|(index, asset)| Slide::new(SlideId(index as u64 + 1), asset.id))
            .collect();
        ShowDefinition {
            assets,
            slides,
            ..Default::default()
        }
    }

    fn listed(show: &ShowDefinition) -> Vec<usize> {
        (0..show.slides.len()).collect()
    }

    #[test]
    fn photos_are_fully_visible_for_their_duration_between_transitions() {
        let show = show(vec![photo(1), photo(2)]);
        let timeline = Timeline::build(&show, &listed(&show));
        let [first, second] = timeline.entries[..] else {
            panic!("{timeline:?}");
        };
        // From black in 1 s, 10 s fully visible, then the next comes in over 1 s.
        assert_eq!((first.start_ms, first.enter_ms), (0, 1_000));
        assert_eq!((first.leave_ms, first.end_ms), (11_000, 12_000));
        assert_eq!((second.start_ms, second.leave_ms), (11_000, 22_000));
        // The last one fades to black.
        assert_eq!(timeline.end_ms, 23_000);
        assert_eq!(first.enter, TransitionChoice::Fade);
    }

    #[test]
    fn a_slides_transition_is_the_one_that_brings_it_in() {
        let mut show = show(vec![photo(1), photo(2), photo(3)]);
        show.slides[1].transition = Some(TransitionChoice::Slide {
            direction: Direction::Left,
        });
        show.slides[1].transition_ms = Some(500);
        show.slides[2].transition = Some(TransitionChoice::Cut);
        show.slides[0].duration_ms = Some(3_000);
        let timeline = Timeline::build(&show, &listed(&show));
        let entries = &timeline.entries;
        assert_eq!(entries[0].leave_ms, 4_000);
        assert_eq!(entries[0].end_ms, 4_500);
        assert_eq!(entries[1].enter_ms, 500);
        assert!(matches!(entries[1].enter, TransitionChoice::Slide { .. }));
        // A cut takes no time.
        assert_eq!(entries[2].enter_ms, 0);
        assert_eq!(entries[1].end_ms, entries[2].start_ms);
    }

    #[test]
    fn a_transition_without_a_length_takes_its_own() {
        let mut show = show(vec![photo(1), photo(2), photo(3)]);
        show.look.transition = TransitionChoice::Paw;
        show.slides[2].transition = Some(TransitionChoice::Wipe {
            direction: Direction::Right,
        });
        let timeline = Timeline::build(&show, &listed(&show));
        let entries = &timeline.entries;
        assert_eq!(entries[0].enter_ms, 5_000);
        assert_eq!(entries[1].enter_ms, 5_000);
        assert_eq!(entries[2].enter_ms, 1_000);
        // The fade to black at the end is not a paw.
        assert_eq!(timeline.end_ms - entries[2].leave_ms, 1_000);

        // A length set in the look is used for every transition.
        show.look.transition_ms = Some(2_000);
        let timeline = Timeline::build(&show, &listed(&show));
        assert_eq!(timeline.entries[1].enter_ms, 2_000);
        assert_eq!(timeline.entries[2].enter_ms, 2_000);
    }

    #[test]
    fn a_video_plays_its_range_once_between_its_transitions() {
        let mut show = show(vec![photo(1), video(2, 8_000), photo(3)]);
        show.slides[1].video = VideoOptions {
            start_ms: 2_000,
            end_ms: Some(6_000),
            volume: 0.5,
        };
        let timeline = Timeline::build(&show, &listed(&show));
        let video = timeline.entries[1];
        // In over 1 s, then the 4 s range, then the next comes in.
        assert_eq!(video.play_start_ms(), video.start_ms + 1_000);
        assert_eq!(video.leave_ms - video.play_start_ms(), 4_000);
        assert_eq!(video.end_ms - video.start_ms, 6_000);
        assert_eq!(video.video_start_ms, Some(2_000));
        assert_eq!(timeline.entries[2].start_ms, video.leave_ms);
    }

    #[test]
    fn transitions_next_to_a_short_video_keep_their_length() {
        let mut show = show(vec![photo(1), video(2, 1_200), photo(3)]);
        show.look.transition_ms = Some(2_000);
        let timeline = Timeline::build(&show, &listed(&show));
        let video = timeline.entries[1];
        assert_eq!(video.enter_ms, 2_000);
        assert_eq!(video.leave_ms - video.play_start_ms(), 1_200);
        assert_eq!(timeline.entries[2].enter_ms, 2_000);
    }

    #[test]
    fn slides_that_cannot_play_are_skipped_with_a_reason() {
        let mut importing = photo(2);
        importing.state = AssetState::Importing;
        let mut unknown = video(3, 0);
        unknown.duration_ms = None;
        let mut show = show(vec![photo(1), importing, unknown]);
        show.slides.push(Slide::new(SlideId(9), AssetId(99)));
        let timeline = Timeline::build(&show, &listed(&show));
        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(
            timeline.skipped,
            [
                (SlideId(2), SkipReason::AssetNotReady),
                (SlideId(3), SkipReason::NoLength),
                (SlideId(9), SkipReason::MissingAsset),
            ]
        );
    }

    #[test]
    fn an_order_other_than_the_list_is_followed() {
        let show = show(vec![photo(1), photo(2), photo(3)]);
        let timeline = Timeline::build(&show, &[2, 0, 1]);
        let ids: Vec<_> = timeline.entries.iter().map(|entry| entry.slide).collect();
        assert_eq!(ids, [SlideId(3), SlideId(1), SlideId(2)]);
        assert_eq!(timeline.at(11_500).count(), 2);
    }

    #[test]
    fn an_empty_show_has_an_empty_timeline() {
        let show = ShowDefinition::default();
        let timeline = Timeline::build(&show, &[]);
        assert!(timeline.entries.is_empty());
        assert_eq!(timeline.end_ms, 0);
    }

    #[test]
    fn the_definition_round_trips_through_json_and_fills_in_defaults() {
        let mut show = show(vec![photo(1), video(2, 5_000)]);
        show.title = "夕暮れの港".into();
        show.look.watermark = Some(WatermarkSpec {
            content: WatermarkSource::Text {
                text: "FLUXLIDE".into(),
            },
            corner: Corner::TopRight,
            opacity: 0.6,
            size: 0.045,
        });
        show.music.source = Some(MusicSource::Folder {
            path: "/music".into(),
        });
        let json = serde_json::to_string(&show).unwrap();
        assert!(json.contains("\"originalName\""), "{json}");
        assert!(json.contains("\"kind\":\"text\""), "{json}");
        let back: ShowDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, show);

        // A project saved with fewer fields still opens, with defaults.
        let minimal = r#"{
            "assets": [{"id": 1, "kind": "photo", "file": "1.jpg",
                        "originalName": "a.jpg", "state": {"state": "ready"}}],
            "slides": [{"id": 1, "asset": 1}]
        }"#;
        let show: ShowDefinition = serde_json::from_str(minimal).unwrap();
        assert_eq!(show.version, SHOW_VERSION);
        assert!(show.slides[0].show_caption);
        assert_eq!(show.slides[0].video.volume, 1.0);
        assert_eq!(show.look.photo_ms, DEFAULT_PHOTO_MS);
        assert_eq!(show.look.fit, Fit::Blur);
    }
}
