//! Queue persistence.
//!
//! Two properties matter more than speed here:
//!
//! 1. **A corrupt queue must never prevent the app from starting.** A download
//!    manager that refuses to launch because its state file is malformed is
//!    worse than one that loses a task, so the loader degrades: it retries with
//!    line-delimited scanning for the last valid task object and, failing that,
//!    returns an empty queue.
//! 2. **Writes are atomic.** Temp file, `fsync`, then rename. A crash
//!    mid-write leaves the previous good file intact rather than a truncated one.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::download::task::Task;
use crate::error::{Error, Result};

/// Bumped when the on-disk shape changes incompatibly.
pub const STORE_VERSION: u32 = 1;

/// The persisted queue.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueFile {
    /// Format version.
    #[serde(default = "current_version")]
    pub version: u32,

    /// Tasks, keyed by id so the file is stable under reordering.
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
}

fn current_version() -> u32 {
    STORE_VERSION
}

impl QueueFile {
    /// An empty queue at the current version.
    pub fn new() -> Self {
        Self {
            version: STORE_VERSION,
            tasks: BTreeMap::new(),
        }
    }

    /// Number of tasks held.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether the queue holds no tasks.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Insert or replace a task.
    pub fn upsert(&mut self, task: Task) {
        self.tasks.insert(task.id.clone(), task);
    }

    /// Remove a task, returning it if it was present.
    pub fn remove(&mut self, id: &str) -> Option<Task> {
        self.tasks.remove(id)
    }

    /// Look up a task.
    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.get(id)
    }

    /// Iterate tasks in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }
}

/// Loads and saves a [`QueueFile`] at a fixed path.
#[derive(Debug, Default)]
pub struct Store {
    path: PathBuf,

    /// Set when the last [`Store::load`] had to recover from damage. Surfaced to
    /// the user so tasks lost to a bad write are visible rather than silently
    /// swallowed.
    ///
    /// Behind a mutex because `load` takes `&self` — a store is shared by the
    /// queue and every worker, and making `load` take `&mut self` to fit one
    /// note would be a worse trade than six bytes of synchronisation.
    recovery_note: Mutex<Option<String>>,
}

impl Store {
    /// A store rooted at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recovery_note: Mutex::new(None),
        }
    }

    /// The path this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the queue.
    ///
    /// Returns an empty queue when the file does not exist. When the file exists
    /// but cannot be parsed, it attempts a structural recovery before giving up
    /// and returning an empty queue; the failure is reported through
    /// [`Store::last_recovery_note`] so the UI can tell the user that tasks were
    /// lost rather than silently pretending everything is fine.
    pub fn load(&self) -> Result<QueueFile> {
        *self.note() = None;

        if !self.path.exists() {
            return Ok(QueueFile::new());
        }

        let text = std::fs::read_to_string(&self.path).map_err(|e| Error::io(&self.path, e))?;

        if text.trim().is_empty() {
            return Ok(QueueFile::new());
        }

        match serde_json::from_str::<QueueFile>(&text) {
            Ok(queue) => Ok(self.check_version(queue)),
            Err(parse_error) => {
                let tasks = salvage(&text);
                *self.note() = Some(format!(
                    "{} was not valid JSON ({parse_error}); recovered {} of the task(s) in it. \
                     Anything newer than the last complete task was lost.",
                    self.path.display(),
                    tasks.len()
                ));
                Ok(QueueFile {
                    version: STORE_VERSION,
                    tasks: tasks.into_iter().map(|t| (t.id.clone(), t)).collect(),
                })
            }
        }
    }

    /// Save the queue atomically.
    pub fn save(&self, queue: &QueueFile) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
        }

        let json = serde_json::to_string_pretty(queue)
            .map_err(|e| Error::Store(format!("could not serialise queue: {e}")))?;

        let tmp = temp_sibling(&self.path);

        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
            f.write_all(json.as_bytes())
                .map_err(|e| Error::io(&tmp, e))?;
            f.flush().map_err(|e| Error::io(&tmp, e))?;
            // Durability matters: without the sync, a power loss can leave the
            // renamed file pointing at unflushed content.
            f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        }

        std::fs::rename(&tmp, &self.path).map_err(|e| {
            // Clean up so a failed save does not leave litter behind.
            let _ = std::fs::remove_file(&tmp);
            Error::io(&self.path, e)
        })?;

        Ok(())
    }

    /// Note describing a lossy load, if the last load had to recover.
    ///
    /// Returns an owned `String`: the note lives behind a mutex, and handing out
    /// a borrow would mean holding the guard for as long as the caller cared
    /// about the text.
    pub fn last_recovery_note(&self) -> Option<String> {
        self.note().clone()
    }

    fn note(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.recovery_note
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Refuse to load a newer format rather than misinterpreting it.
    fn check_version(&self, queue: QueueFile) -> QueueFile {
        if queue.version > STORE_VERSION {
            // Downgrading would silently drop fields the newer writer wrote.
            // Returning an empty queue loses tasks but does not corrupt them.
            *self.note() = Some(format!(
                "{} was written by a newer ifami (v{} > v{}); starting with an empty queue. \
                 Nothing was overwritten, so downgrading and re-running will restore it.",
                self.path.display(),
                queue.version,
                STORE_VERSION
            ));
            return QueueFile::new();
        }
        queue
    }
}

/// Recover tasks from a malformed file.
///
/// The scanner is string-aware and brace-balanced, then tries every balanced
/// object in the document as a [`Task`]. That is deliberately more thorough than
/// assuming the pretty-printed layout: the file we are recovering from was
/// written by an older or newer build, or truncated mid-write, and the shape we
/// remember is not a guarantee about the shape on disk.
///
/// Anything unrecoverable is dropped. Losing a task is recoverable; refusing to
/// launch is not.
fn salvage(text: &str) -> Vec<Task> {
    let mut tasks = Vec::new();
    let mut seen = BTreeMap::new();

    for object in balanced_objects(text) {
        if let Ok(task) = serde_json::from_str::<Task>(&object) {
            seen.insert(task.id.clone(), task);
        }
    }

    tasks.extend(seen.into_values());
    tasks
}

/// Every balanced `{...}` span in `text`, innermost included.
///
/// Brace counting is only safe when it ignores braces inside strings, which a
/// title or a URL can absolutely contain.
fn balanced_objects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            match b {
                b'\\' => escaped = !escaped,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'{' => stack.push(i),
            b'}' => {
                if let Some(start) = stack.pop() {
                    out.push(text[start..=i].to_string());
                }
            }
            _ => {}
        }
    }

    out
}

/// A temp path beside `path`, so the rename is atomic (same filesystem).
fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &Path) -> Store {
        Store::new(dir.join("queue.json"))
    }

    #[test]
    fn a_missing_file_is_an_empty_queue_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());
        let q = s.load().unwrap();
        assert!(q.is_empty());
        assert_eq!(q.version, STORE_VERSION);
        assert!(s.last_recovery_note().is_none());
    }

    #[test]
    fn round_trips_a_queue() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());

        let mut q = QueueFile::new();
        q.upsert(Task::new("a", "https://x.invalid/1", "1.mp4", "mp4"));
        q.upsert(Task::new("b", "https://x.invalid/2", "2.mp4", "mp4"));
        s.save(&q).unwrap();

        let loaded = s.load().unwrap();
        assert_eq!(loaded, q);
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn saving_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());
        let q = QueueFile::new();
        s.save(&q).unwrap();

        assert!(s.path().exists());
        assert!(
            !temp_sibling(s.path()).exists(),
            "temp file was left behind"
        );
    }

    #[test]
    fn saving_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path().join("nested/deeper/queue.json"));
        s.save(&QueueFile::new()).unwrap();
        assert!(s.path().exists());
    }

    #[test]
    fn a_truncated_file_is_recovered_rather_than_failing_to_start() {
        // The whole point: a download manager that will not launch because its
        // state file is truncated is worse than one that lost a task.
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());

        let mut q = QueueFile::new();
        q.upsert(Task::new("keep-me", "https://x.invalid/1", "1.mp4", "mp4"));
        q.upsert(Task::new("lose-me", "https://x.invalid/2", "2.mp4", "mp4"));
        s.save(&q).unwrap();

        // Simulate a crash mid-write: cut the file immediately after the first task's
        // closing brace. Chopping at `len / 2` would test where the midpoint of a
        // pretty-printed file happens to fall rather than what recovery does —
        // and that lands inside the first task about as often as not.
        let text = std::fs::read_to_string(s.path()).unwrap();
        let first = balanced_objects(&text)
            .into_iter()
            .find(|o| o.contains("\"keep-me\""))
            .expect("the saved queue contains the first task");
        let end = text
            .find(first.as_str())
            .expect("the object came from this text")
            + first.len();
        std::fs::write(s.path(), &text[..end]).unwrap();

        let loaded = s.load().unwrap();
        assert!(
            s.last_recovery_note().is_some(),
            "a lossy load must be reported to the user"
        );
        assert!(
            loaded.get("keep-me").is_some(),
            "at least the first task should survive"
        );
    }

    #[test]
    fn a_completely_garbage_file_yields_an_empty_queue_without_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());
        std::fs::write(s.path(), b"\x00\x01\x02 not json at all").unwrap();

        let q = s.load().unwrap();
        assert!(q.is_empty());
        assert!(s.last_recovery_note().is_some());
    }

    #[test]
    fn an_empty_file_is_an_empty_queue_without_a_recovery_note() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());
        std::fs::write(s.path(), b"   \n  ").unwrap();
        let q = s.load().unwrap();
        assert!(q.is_empty());
        assert!(s.last_recovery_note().is_none(), "blank is not corruption");
    }

    #[test]
    fn a_newer_file_version_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());
        std::fs::write(
            s.path(),
            r#"{"version":9999,"tasks":{"a":{"id":"a","url":"u","format_id":"0","output":"o","state":"queued","bytes_done":0,"container_ext":"mp4"}}}"#,
        )
        .unwrap();

        let q = s.load().unwrap();
        assert!(q.is_empty());
        assert!(s.last_recovery_note().unwrap().contains("newer ifami"));
    }

    #[test]
    fn an_older_file_version_loads_with_missing_fields_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_in(dir.path());
        // A v0 file that predates the segment fields entirely.
        std::fs::write(
            s.path(),
            r#"{"version":0,"tasks":{"a":{"id":"a","url":"u","format_id":"0","output":"o","state":"paused","bytes_done":42,"container_ext":"mp4"}}}"#,
        )
        .unwrap();

        let q = s.load().unwrap();
        let t = q.get("a").expect("task survived");
        assert_eq!(t.bytes_done, 42);
        assert!(t.segments.is_none());
        assert!(t.completed_segments.is_empty());
    }

    #[test]
    fn salvage_recovers_only_well_formed_task_objects() {
        let text = r#"{
  "version": 1,
  "tasks": {
    "a": {
      "id": "a",
      "url": "https://x.invalid/1",
      "format_id": "0",
      "output": "1.mp4",
      "state": "paused",
      "bytes_done": 10,
      "container_ext": "mp4"
    },
    "b": {
      "id": "b",
      "url": "https://x.invalid/2",
      "format_id": "0",
      "output": "2.mp4",
      "state": "que"#;
        let tasks = salvage(text);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "a");
    }

    #[test]
    fn nested_braces_in_a_task_do_not_confuse_salvage() {
        // A metadata blob containing braces must not end the scan early.
        let text = r#"{
  "version": 1,
  "tasks": {
    "a": {
      "id": "a",
      "url": "https://x.invalid/1",
      "format_id": "0",
      "output": "1.mp4",
      "state": "paused",
      "bytes_done": 10,
      "container_ext": "mp4",
      "nested": {"a": {"b": 1}}
    }
  }
}"#;
        let tasks = salvage(text);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "a");
    }
}
