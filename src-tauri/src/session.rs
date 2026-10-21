//! The slideshow being edited: its definition, its undo history, and its
//! project folder, which every change is written to at once.
//!
//! Two parties change the definition. The user edits it (through the
//! management window), and those changes can be undone. Imports report on
//! the files (copied, failed, how big), and those reports are facts, not
//! edits: undoing must not bring back "still copying" for a file that has
//! since been copied. So the file facts of each asset are kept apart and
//! laid over every definition that comes in, whether from the window, from
//! undo or from redo.
//!
//! The window sends whole definitions, so an edit made from an older one
//! would quietly undo whatever changed since (slides added by an import, a
//! file that finished copying). Every change raises [`Session::revision`],
//! and an edit is applied only if it was made from the current revision.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use fluxlide_engine::show::{Asset, AssetId, AssetState, ShowDefinition, SlideId};

use crate::history::History;
use crate::project::{Project, ProjectError};

/// Shown for a file whose copy was cut short when the app last closed.
pub const INTERRUPTED: &str = "取り込みが途中で止まりました。もう一度選んでください。";

pub struct Session {
    project: Project,
    show: ShowDefinition,
    history: History,
    /// The latest file facts of every asset seen while open, including
    /// assets that an undo has taken out (a redo may bring them back).
    files: HashMap<AssetId, Asset>,
    /// The highest slide number used while open, so that a slide brought
    /// back by redo never shares its number with a newer one.
    last_slide_id: u64,
    save_error: Option<String>,
    revision: u64,
    epoch: u64,
}

/// Counts the sessions opened since the app started.
static EPOCHS: AtomicU64 = AtomicU64::new(0);

impl Session {
    /// Starts editing `show` from `project`. Copies that were still running
    /// when the project was last open have stopped, so they are marked as
    /// failed. Files in the media folder that no asset uses (partial copies,
    /// and files of assets removed before the project was closed) are
    /// deleted.
    pub fn new(project: Project, mut show: ShowDefinition) -> Self {
        let mut interrupted = false;
        for asset in &mut show.assets {
            if asset.state == AssetState::Importing {
                asset.state = AssetState::Failed {
                    reason: INTERRUPTED.into(),
                };
                interrupted = true;
            }
        }
        clean_media(&project.media_dir(), &show);
        let files = show
            .assets
            .iter()
            .map(|asset| (asset.id, asset.clone()))
            .collect();
        let mut session = Self {
            project,
            show,
            history: History::default(),
            files,
            last_slide_id: 0,
            save_error: None,
            revision: 0,
            epoch: EPOCHS.fetch_add(1, Ordering::Relaxed) + 1,
        };
        session.note_slide_ids();
        if interrupted {
            session.save();
        }
        session
    }

    pub fn show(&self) -> &ShowDefinition {
        &self.show
    }

    pub fn dir(&self) -> &Path {
        self.project.dir()
    }

    pub fn media_dir(&self) -> PathBuf {
        self.project.media_dir()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Raised by every change to the definition.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Different for every session, even of the same project, so work
    /// queued for one is not applied to another.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Why the last save failed, until one succeeds.
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Replaces the definition with the user's `next`. See
    /// [`History::record`] for `group`. Returns whether anything changed.
    pub fn edit(&mut self, mut next: ShowDefinition, group: Option<&str>) -> bool {
        self.lay_files_over(&mut next);
        if next == self.show {
            return false;
        }
        let before = std::mem::replace(&mut self.show, next);
        self.history.record(before, group, Instant::now());
        self.note_slide_ids();
        self.save();
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(mut previous) = self.history.undo(self.show.clone()) else {
            return false;
        };
        self.lay_files_over(&mut previous);
        self.show = previous;
        self.save();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut next) = self.history.redo(self.show.clone()) else {
            return false;
        };
        self.lay_files_over(&mut next);
        self.show = next;
        self.save();
        true
    }

    /// Records new facts about an asset's file. They apply to the
    /// definition at once, without a step in the undo history.
    pub fn update_file(&mut self, asset: Asset) {
        if let Some(current) = self.show.assets.iter_mut().find(|a| a.id == asset.id) {
            take_file_facts(current, &asset);
            self.save();
        }
        self.files.insert(asset.id, asset);
    }

    /// A slide number not used while open. Each call gives a new one.
    pub fn next_slide_id(&mut self) -> SlideId {
        self.note_slide_ids();
        self.last_slide_id += 1;
        SlideId(self.last_slide_id)
    }

    fn note_slide_ids(&mut self) {
        let highest = self.show.slides.iter().map(|slide| slide.id.0).max();
        self.last_slide_id = self.last_slide_id.max(highest.unwrap_or(0));
    }

    /// A number not used by any asset seen while open.
    pub fn next_asset_id(&self) -> AssetId {
        let used = self
            .files
            .keys()
            .chain(self.show.assets.iter().map(|asset| &asset.id))
            .map(|id| id.0)
            .max()
            .unwrap_or(0);
        AssetId(used + 1)
    }

    fn lay_files_over(&self, show: &mut ShowDefinition) {
        for asset in &mut show.assets {
            if let Some(file) = self.files.get(&asset.id) {
                take_file_facts(asset, file);
            }
        }
    }

    fn save(&mut self) {
        self.revision += 1;
        self.save_error = self
            .project
            .save(&self.show)
            .err()
            .map(|err: ProjectError| err.to_string());
    }
}

/// Everything about `asset` that comes from its file rather than the user.
fn take_file_facts(asset: &mut Asset, file: &Asset) {
    asset.kind = file.kind;
    asset.file.clone_from(&file.file);
    asset.original_name.clone_from(&file.original_name);
    asset.state = file.state.clone();
    asset.size = file.size;
    asset.duration_ms = file.duration_ms;
    asset.has_audio = file.has_audio;
}

/// Deletes what no asset of `show` uses from `media_dir` and its thumbnails.
fn clean_media(media_dir: &Path, show: &ShowDefinition) {
    let used: HashSet<&str> = show
        .assets
        .iter()
        .map(|asset| asset.file.as_str())
        .collect();
    let remove_unused = |dir: &Path, name_of: &dyn Fn(&str) -> Option<String>| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name_of(&name).is_some_and(|file| used.contains(file.as_str())) {
                let _ = std::fs::remove_file(path);
            }
        }
    };
    remove_unused(media_dir, &|name| Some(name.to_string()));
    remove_unused(&media_dir.join("thumbs"), &|name| {
        name.strip_suffix(".jpg").map(str::to_string)
    });
}

/// Where a copy into the media folder is written until it is complete.
pub fn partial_path(media_dir: &Path, file: &str) -> PathBuf {
    media_dir.join(format!("{file}.part"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::tests::{TempDir, photo_show};
    use fluxlide_engine::show::Slide;

    fn session(root: &TempDir, show: ShowDefinition) -> Session {
        let project = Project::create(&root.0, &show).unwrap();
        Session::new(project, show)
    }

    fn stored(session: &Session) -> ShowDefinition {
        Project::open(session.dir()).unwrap().1
    }

    #[test]
    fn every_edit_is_saved_and_can_be_undone() {
        let root = TempDir::new("session-edit");
        let mut session = session(&root, photo_show(2));
        let mut next = session.show().clone();
        next.slides.reverse();
        assert!(session.edit(next.clone(), None));
        assert!(!session.edit(next.clone(), None), "no change, no step");
        assert_eq!(stored(&session), next);

        assert!(session.undo());
        assert_eq!(stored(&session), photo_show(2));
        assert!(session.redo());
        assert_eq!(stored(&session), next);
        assert!(session.save_error().is_none());
    }

    #[test]
    fn undo_keeps_what_imports_have_found_out_since() {
        let root = TempDir::new("session-files");
        let mut show = photo_show(1);
        show.assets[0].state = AssetState::Importing;
        show.assets[0].size = None;
        let mut session = session(&root, ShowDefinition::default());
        session.edit(show.clone(), None);

        // The user types a title, then the copy finishes.
        let mut titled = session.show().clone();
        titled.assets[0].meta.title = "朝の港".into();
        session.edit(titled, Some("title"));
        let mut copied = show.assets[0].clone();
        copied.state = AssetState::Ready;
        copied.size = Some([400, 300]);
        session.update_file(copied);

        // Undoing the title must not undo the copy.
        session.undo();
        let asset = &session.show().assets[0];
        assert_eq!(asset.meta.title, "");
        assert_eq!(asset.state, AssetState::Ready);
        assert_eq!(asset.size, Some([400, 300]));

        // Undoing the import takes the asset out, and redo brings it back
        // as it is now.
        session.undo();
        assert!(session.show().assets.is_empty());
        session.redo();
        assert_eq!(session.show().assets[0].state, AssetState::Ready);
        assert_eq!(session.next_asset_id(), AssetId(2));
    }

    #[test]
    fn every_change_raises_the_revision_and_sessions_differ() {
        let root = TempDir::new("session-revision");
        let mut session = session(&root, photo_show(1));
        let first = session.revision();
        let mut next = session.show().clone();
        next.title = "別の題名".into();
        session.edit(next.clone(), None);
        assert_eq!(session.revision(), first + 1);
        session.edit(next, None);
        assert_eq!(session.revision(), first + 1, "no change, same revision");
        let mut copied = session.show().assets[0].clone();
        copied.size = Some([1, 1]);
        session.update_file(copied);
        session.undo();
        assert_eq!(session.revision(), first + 3);

        let other = Session::new(
            Project::create(&root.0, &photo_show(1)).unwrap(),
            photo_show(1),
        );
        assert_ne!(other.epoch(), session.epoch());
    }

    #[test]
    fn slide_numbers_are_not_reused_after_an_undo() {
        let root = TempDir::new("session-slide-ids");
        let mut session = session(&root, photo_show(1));
        let mut next = session.show().clone();
        let id = session.next_slide_id();
        assert_eq!(id, SlideId(2));
        next.slides.push(Slide::new(id, AssetId(1)));
        session.edit(next, None);
        session.undo();
        assert_eq!(session.next_slide_id(), SlideId(3));
        assert_eq!(session.next_slide_id(), SlideId(4));
    }

    #[test]
    fn copies_cut_short_are_marked_failed_when_opened_again() {
        let root = TempDir::new("session-interrupted");
        let mut show = photo_show(2);
        show.assets[1].state = AssetState::Importing;
        let project = Project::create(&root.0, &show).unwrap();
        let partial = partial_path(&project.media_dir(), "2.jpg");
        std::fs::write(&partial, b"half").unwrap();
        let kept = project.media_dir().join("1.jpg");
        std::fs::write(&kept, b"photo").unwrap();
        let orphan = project.media_dir().join("8.png");
        std::fs::write(&orphan, b"removed earlier").unwrap();
        std::fs::create_dir(project.media_dir().join("thumbs")).unwrap();
        let thumbnail = project.media_dir().join("thumbs").join("1.jpg.jpg");
        std::fs::write(&thumbnail, b"thumb").unwrap();
        let dir = project.dir().to_path_buf();
        drop(project);

        let (project, show) = Project::open(&dir).unwrap();
        let session = Session::new(project, show);
        assert_eq!(
            session.show().assets[1].state,
            AssetState::Failed {
                reason: INTERRUPTED.into()
            }
        );
        assert!(!partial.exists() && !orphan.exists());
        assert!(kept.exists() && thumbnail.exists());
        assert_eq!(
            stored(&session).assets[1].state,
            session.show().assets[1].state
        );
        assert!(!session.can_undo());
    }

    #[test]
    fn a_failed_save_is_reported_until_one_succeeds() {
        let root = TempDir::new("session-save-error");
        let mut session = session(&root, photo_show(1));
        let db = session.dir().join(crate::project::DATABASE_FILE);
        // Make the table unwritable from another connection.
        let lock = rusqlite::Connection::open(&db).unwrap();
        lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let mut next = session.show().clone();
        next.slides.push(Slide::new(SlideId(9), AssetId(1)));
        session.edit(next, None);
        assert!(session.save_error().is_some());
        lock.execute_batch("ROLLBACK").unwrap();
        session.undo();
        assert!(session.save_error().is_none());
    }
}
