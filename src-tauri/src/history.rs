//! "元に戻す" and "やり直す" for the open slideshow.
//!
//! Each change keeps the whole definition from before it. Definitions are
//! small (no pixels), so copies are cheaper to get right than a list of
//! inverse operations. The history lives only while the project is open.

use std::time::{Duration, Instant};

use fluxlide_engine::show::ShowDefinition;

/// Changes kept for undo; older ones are forgotten.
const LIMIT: usize = 100;

/// Changes in the same group (typing in one text field, dragging one
/// slider) closer together than this are undone as one.
const GROUP_GAP: Duration = Duration::from_millis(1_500);

#[derive(Default)]
pub struct History {
    undo: Vec<ShowDefinition>,
    redo: Vec<ShowDefinition>,
    /// The group of the last change and when it was made.
    last: Option<(String, Instant)>,
}

impl History {
    /// Records that the definition was `before` until a change at `now`.
    /// A change in the same `group` as the one just before it, soon after,
    /// joins that one instead of adding another step.
    pub fn record(&mut self, before: ShowDefinition, group: Option<&str>, now: Instant) {
        let joins = match (&self.last, group) {
            (Some((last, at)), Some(group)) => last == group && now - *at < GROUP_GAP,
            _ => false,
        };
        if !joins {
            self.undo.push(before);
            if self.undo.len() > LIMIT {
                self.undo.remove(0);
            }
        }
        self.last = group.map(|group| (group.to_string(), now));
        self.redo.clear();
    }

    /// The definition before the last change, given the `current` one.
    pub fn undo(&mut self, current: ShowDefinition) -> Option<ShowDefinition> {
        let previous = self.undo.pop()?;
        self.redo.push(current);
        self.last = None;
        Some(previous)
    }

    pub fn redo(&mut self, current: ShowDefinition) -> Option<ShowDefinition> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.last = None;
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titled(title: &str) -> ShowDefinition {
        ShowDefinition {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn changes_are_undone_and_redone_in_order() {
        let mut history = History::default();
        let now = Instant::now();
        history.record(titled("a"), None, now);
        history.record(titled("b"), None, now);
        assert_eq!(history.undo(titled("c")).unwrap().title, "b");
        assert_eq!(history.undo(titled("b")).unwrap().title, "a");
        assert!(history.undo(titled("a")).is_none());
        assert_eq!(history.redo(titled("a")).unwrap().title, "b");
        assert!(history.can_undo() && history.can_redo());
    }

    #[test]
    fn a_new_change_forgets_what_could_be_redone() {
        let mut history = History::default();
        let now = Instant::now();
        history.record(titled("a"), None, now);
        history.undo(titled("b"));
        history.record(titled("a"), None, now);
        assert!(!history.can_redo());
    }

    #[test]
    fn typing_in_one_field_is_undone_as_one_step() {
        let mut history = History::default();
        let start = Instant::now();
        history.record(titled(""), Some("title"), start);
        history.record(
            titled("港"),
            Some("title"),
            start + Duration::from_millis(300),
        );
        history.record(
            titled("港の"),
            Some("title"),
            start + Duration::from_millis(600),
        );
        // Another field, or a pause, starts a new step.
        history.record(
            titled("港の朝"),
            Some("author"),
            start + Duration::from_millis(900),
        );
        history.record(
            titled("港の朝"),
            Some("author"),
            start + Duration::from_secs(5),
        );
        assert_eq!(history.undo(titled("x")).unwrap().title, "港の朝");
        assert_eq!(history.undo(titled("x")).unwrap().title, "港の朝");
        assert_eq!(history.undo(titled("x")).unwrap().title, "");
        assert!(!history.can_undo());
    }

    #[test]
    fn only_the_latest_changes_are_kept() {
        let mut history = History::default();
        let now = Instant::now();
        for n in 0..LIMIT + 10 {
            history.record(titled(&n.to_string()), None, now);
        }
        let mut oldest = None;
        while let Some(show) = history.undo(titled("now")) {
            oldest = Some(show.title);
        }
        assert_eq!(oldest.as_deref(), Some("10"));
    }
}
