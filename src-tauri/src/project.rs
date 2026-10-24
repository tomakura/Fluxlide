//! Where a slideshow is kept.
//!
//! Each project is one folder: `project.sqlite` holds the [`ShowDefinition`]
//! as JSON, and `media/` holds the photos and videos copied into it. New
//! projects are made under the app's data folder, but a project folder can
//! be moved or opened from anywhere, since the definition names its files
//! only relative to `media/`.
//!
//! The database is written in WAL mode after every change, so a crash loses
//! at most the change being written. The design and its reasons are in
//! `docs/project-storage.md`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxlide_engine::show::{DEFAULT_TRANSITION_MS, SHOW_VERSION, ShowDefinition};
use rusqlite::{Connection, OptionalExtension, params};

pub const DATABASE_FILE: &str = "project.sqlite";
pub const MEDIA_DIR: &str = "media";

/// Layout of the database. Raised (with a conversion in [`migrate`]) when
/// the tables change; the definition inside has its own version.
const SCHEMA_VERSION: i32 = 1;

#[derive(Debug)]
pub enum ProjectError {
    /// Not a project folder, or one that cannot be read.
    NotFound(PathBuf),
    /// Made by a newer Fluxlide than this one.
    TooNew,
    Damaged(String),
    Io(String),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(path) => {
                write!(f, "スライドショーが見つかりません（{}）。", path.display())
            }
            Self::TooNew => write!(
                f,
                "新しい版の Fluxlide で作られたスライドショーです。Fluxlide を更新してください。"
            ),
            Self::Damaged(reason) => write!(f, "スライドショーを読み込めません: {reason}"),
            Self::Io(reason) => write!(f, "スライドショーを保存できません: {reason}"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<rusqlite::Error> for ProjectError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<std::io::Error> for ProjectError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// What the home screen shows about a project without opening it.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub dir: PathBuf,
    pub title: String,
    pub slide_count: usize,
    /// Milliseconds since the Unix epoch.
    pub updated_at: i64,
    /// File name in `media/` of the first slide's asset, for a thumbnail.
    pub first_file: Option<String>,
}

/// An open project folder.
pub struct Project {
    dir: PathBuf,
    db: Connection,
}

impl Project {
    /// Makes a new project folder inside `parent` and stores `show` in it.
    pub fn create(parent: &Path, show: &ShowDefinition) -> Result<Self, ProjectError> {
        std::fs::create_dir_all(parent)?;
        let dir = unique_dir(parent);
        std::fs::create_dir_all(dir.join(MEDIA_DIR))?;
        let db = Connection::open(dir.join(DATABASE_FILE))?;
        prepare(&db)?;
        let project = Self { dir, db };
        project.save(show)?;
        Ok(project)
    }

    /// Opens the project in `dir` and reads its definition.
    pub fn open(dir: &Path) -> Result<(Self, ShowDefinition), ProjectError> {
        let path = dir.join(DATABASE_FILE);
        if !path.is_file() {
            return Err(ProjectError::NotFound(dir.to_path_buf()));
        }
        let db = Connection::open(&path)?;
        prepare(&db)?;
        let json: Option<String> = db
            .query_row("SELECT definition FROM project WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        let json = json.ok_or_else(|| ProjectError::Damaged("中身がありません".into()))?;
        let show = parse_definition(&json)?;
        std::fs::create_dir_all(dir.join(MEDIA_DIR))?;
        Ok((
            Self {
                dir: dir.to_path_buf(),
                db,
            },
            show,
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn media_dir(&self) -> PathBuf {
        self.dir.join(MEDIA_DIR)
    }

    /// Replaces the stored definition with `show`.
    pub fn save(&self, show: &ShowDefinition) -> Result<(), ProjectError> {
        let json = serde_json::to_string(show).map_err(|err| ProjectError::Io(err.to_string()))?;
        let first_file = show
            .slides
            .first()
            .and_then(|slide| show.asset(slide.asset))
            .map(|asset| asset.file.clone());
        self.db.execute(
            "INSERT INTO project (id, definition, title, slide_count, first_file, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET definition = ?1, title = ?2, slide_count = ?3,
                 first_file = ?4, updated_at = ?5",
            params![
                json,
                show.title,
                show.slides.len() as i64,
                first_file,
                now_ms()
            ],
        )?;
        Ok(())
    }
}

/// Reads the summary of the project in `dir` without parsing its definition.
pub fn summary(dir: &Path) -> Result<Summary, ProjectError> {
    let path = dir.join(DATABASE_FILE);
    if !path.is_file() {
        return Err(ProjectError::NotFound(dir.to_path_buf()));
    }
    let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.query_row(
        "SELECT title, slide_count, updated_at, first_file FROM project WHERE id = 1",
        [],
        |row| {
            Ok(Summary {
                dir: dir.to_path_buf(),
                title: row.get(0)?,
                slide_count: row.get::<_, i64>(1)? as usize,
                updated_at: row.get(2)?,
                first_file: row.get(3)?,
            })
        },
    )
    .map_err(|err| ProjectError::Damaged(err.to_string()))
}

/// The projects in the folders inside `parent`, most recently changed first.
/// Folders that are not projects are left out.
pub fn list(parent: &Path) -> Vec<Summary> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .filter_map(|entry| summary(&entry.ok()?.path()).ok())
        .collect();
    found.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
    found
}

/// Checks the version of a stored definition before reading it. A
/// definition without one is not accepted as the current version, so a
/// future conversion cannot miss it.
fn parse_definition(json: &str) -> Result<ShowDefinition, ProjectError> {
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|err| ProjectError::Damaged(err.to_string()))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ProjectError::Damaged("版の情報がありません".into()))?;
    if version > u64::from(SHOW_VERSION) {
        return Err(ProjectError::TooNew);
    }
    if version < 2 {
        // Version 1 always stored the look's transition length, 1 s unless
        // changed. Every transition then had that length by default, so
        // leaving it unset keeps them as they were and lets the paw, added
        // in version 2, take its own.
        if let Some(look) = value
            .get_mut("look")
            .and_then(serde_json::Value::as_object_mut)
            && look.get("transitionMs").and_then(serde_json::Value::as_u64)
                == Some(u64::from(DEFAULT_TRANSITION_MS))
        {
            look.insert("transitionMs".into(), serde_json::Value::Null);
        }
        value["version"] = SHOW_VERSION.into();
    }
    serde_json::from_value(value).map_err(|err| ProjectError::Damaged(err.to_string()))
}

fn prepare(db: &Connection) -> Result<(), ProjectError> {
    // Only this app writes the file; waiting long for a lock helps no one.
    db.busy_timeout(std::time::Duration::from_millis(500))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    let version: i32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(ProjectError::TooNew);
    }
    migrate(db, version)
}

fn migrate(db: &Connection, from: i32) -> Result<(), ProjectError> {
    if from < 1 {
        db.execute_batch(
            "CREATE TABLE project (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 definition TEXT NOT NULL,
                 title TEXT NOT NULL,
                 slide_count INTEGER NOT NULL,
                 first_file TEXT,
                 updated_at INTEGER NOT NULL
             );
             PRAGMA user_version = 1;",
        )?;
    }
    Ok(())
}

/// A new folder name in `parent`, from the time it is made.
fn unique_dir(parent: &Path) -> PathBuf {
    let stamp = now_ms();
    (0..)
        .map(|n| match n {
            0 => parent.join(format!("show-{stamp}")),
            n => parent.join(format!("show-{stamp}-{n}")),
        })
        .find(|dir| !dir.exists())
        .expect("an unused folder name")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as i64)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fluxlide_engine::show::{Asset, AssetId, AssetKind, AssetMeta, AssetState, Slide, SlideId};

    /// An empty folder for one test, removed when dropped.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fluxlide-{name}-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn photo_show(count: u64) -> ShowDefinition {
        let assets: Vec<_> = (1..=count)
            .map(|id| Asset {
                id: AssetId(id),
                kind: AssetKind::Photo,
                file: format!("{id}.jpg"),
                original_name: format!("photo{id}.jpg"),
                state: AssetState::Ready,
                meta: AssetMeta::default(),
                size: Some([400, 300]),
                duration_ms: None,
                has_audio: false,
            })
            .collect();
        ShowDefinition {
            title: "港の写真".into(),
            slides: assets
                .iter()
                .map(|asset| Slide::new(SlideId(asset.id.0), asset.id))
                .collect(),
            assets,
            ..Default::default()
        }
    }

    #[test]
    fn a_saved_project_opens_with_the_same_definition() {
        let root = TempDir::new("project-roundtrip");
        let mut show = photo_show(2);
        let project = Project::create(&root.0, &show).unwrap();
        assert!(project.media_dir().is_dir());
        show.slides.swap(0, 1);
        project.save(&show).unwrap();
        let dir = project.dir().to_path_buf();
        drop(project);

        let (_, opened) = Project::open(&dir).unwrap();
        assert_eq!(opened, show);
        let summary = summary(&dir).unwrap();
        assert_eq!(summary.title, "港の写真");
        assert_eq!(summary.slide_count, 2);
        assert_eq!(summary.first_file.as_deref(), Some("2.jpg"));
    }

    #[test]
    fn projects_are_listed_newest_first_and_other_folders_are_left_out() {
        let root = TempDir::new("project-list");
        let older = Project::create(&root.0, &photo_show(1)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let newer = Project::create(&root.0, &photo_show(3)).unwrap();
        std::fs::create_dir(root.0.join("not-a-project")).unwrap();
        let listed: Vec<_> = list(&root.0).into_iter().map(|s| s.dir).collect();
        assert_eq!(
            listed,
            [newer.dir().to_path_buf(), older.dir().to_path_buf()]
        );
        assert!(list(&root.0.join("missing")).is_empty());
    }

    #[test]
    fn definitions_without_a_version_or_from_a_newer_version_are_refused() {
        let show = serde_json::to_value(photo_show(1)).unwrap();
        let mut missing = show.clone();
        missing.as_object_mut().unwrap().remove("version");
        assert!(matches!(
            parse_definition(&missing.to_string()),
            Err(ProjectError::Damaged(_))
        ));
        let mut newer = show.clone();
        newer["version"] = (SHOW_VERSION + 1).into();
        assert!(matches!(
            parse_definition(&newer.to_string()),
            Err(ProjectError::TooNew)
        ));
        assert_eq!(parse_definition(&show.to_string()).unwrap(), photo_show(1));
    }

    #[test]
    fn a_version_1_definition_keeps_a_changed_transition_length_only() {
        let mut show = serde_json::to_value(photo_show(1)).unwrap();
        show["version"] = 1.into();
        show["look"]["transitionMs"] = 1_000.into();
        let read = parse_definition(&show.to_string()).unwrap();
        assert_eq!(read.version, SHOW_VERSION);
        assert_eq!(read.look.transition_ms, None);
        show["look"]["transitionMs"] = 2_500.into();
        let read = parse_definition(&show.to_string()).unwrap();
        assert_eq!(read.look.transition_ms, Some(2_500));
    }

    #[test]
    fn a_folder_without_a_project_is_not_found() {
        let root = TempDir::new("project-missing");
        assert!(matches!(
            Project::open(&root.0),
            Err(ProjectError::NotFound(_))
        ));
    }
}
