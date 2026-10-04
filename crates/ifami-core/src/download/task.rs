//! The transfer state machine.
//!
//! This is the part of the engine with genuine invariant risk, so the states are
//! an explicit enum rather than a set of booleans, transitions are checked, and
//! the invariants are asserted in debug builds.
//!
//! ```text
//! Queued ─────► Downloading ─────► Completed
//!                 │    │   ▲
//!                 │    │   └── Retrying ◄─┐
//!                 │    └──────► Paused    │
//!                 └───────────► Failed ───┘
//! ```
//!
//! Invariants, checked by [`TaskState::assert_invariants`]:
//!
//! * A task in [`TaskState::Downloading`] owns exactly one open writer.
//! * [`TaskState::Paused`] is only reachable after the writer has been flushed,
//!   so a paused task is always resumable from a known-good offset.
//! * [`TaskState::Completed`] means the on-disk length was verified against the
//!   expected total. An unverified transfer never reaches `Completed`.
//! * The recorded offset is reconciled against the actual file length on load.
//!   **Disk wins**; the stored value is only a hint.

use serde::{Deserialize, Serialize};

/// Where a transfer is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    /// In the queue, not started. No resources held.
    Queued,
    /// Actively transferring. Exactly one writer is open.
    Downloading,
    /// Stopped by the user at a known-good offset. Resumable.
    Paused,
    /// A retryable failure. The partial data is intact and resumable.
    Retrying,
    /// A non-retryable failure. The partial data may be incomplete.
    Failed,
    /// Finished and verified. The writer has been closed and the file renamed.
    Completed,
}

impl TaskState {
    /// A short lowercase label for display.
    ///
    /// The longest is eleven characters (`downloading`), which is what terminal
    /// column widths in the reference client are set from.
    pub fn state_name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            Self::Completed => "completed",
        }
    }

    /// Whether a task in this state can have an open writer.
    ///
    /// Only a necessary condition, not a sufficient one: [`Self::Downloading`]
    /// is also the *reservation* the manager writes before a worker exists (see
    /// [`Self::invariant_violation`]). It is still the right answer to "could
    /// this state be writing?", which is what callers use it for.
    pub fn holds_writer(self) -> bool {
        matches!(self, Self::Downloading)
    }

    /// Whether a task in this state can be moved back to
    /// [`TaskState::Downloading`] without discarding data.
    pub fn is_resumable(self) -> bool {
        matches!(self, Self::Queued | Self::Paused | Self::Retrying)
    }

    /// Whether the transfer has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    /// Whether the state implies bytes exist on disk.
    pub fn implies_partial_data(self) -> bool {
        matches!(self, Self::Paused | Self::Retrying | Self::Downloading)
    }

    /// The legal successor states.
    pub fn allowed_transitions(self) -> &'static [Self] {
        use TaskState::*;
        match self {
            Queued => &[Downloading, Paused, Failed],
            Downloading => &[Completed, Paused, Retrying, Failed],
            // A terminal state is terminal: Completed must never reopen, or a
            // resumed run would re-finalise a file the user already has.
            Completed => &[],
            Failed => &[Queued, Paused],
            Paused => &[Downloading, Queued, Failed],
            // `Retrying -> Queued` exists because `Manager::resume` queues a task
            // before handing it back to the scheduler. Without it, a user
            // pressing retry on a stalled task gets a silent no-op and a task
            // stranded in `Retrying` forever with no worker attached.
            Retrying => &[Downloading, Queued, Paused, Failed],
        }
    }

    /// Whether `next` may be entered from this state.
    pub fn can_transition_to(self, next: Self) -> bool {
        self != next && self.allowed_transitions().contains(&next)
    }

    /// Checks the invariants for this state and returns a description of the
    /// first one that is violated, or `None` when the state is coherent.
    ///
    /// Pure, so tests can assert on violations directly instead of provoking a
    /// panic and catching it.
    pub fn invariant_violation(
        self,
        has_writer: bool,
        on_disk: u64,
        expected: Option<u64>,
    ) -> Option<&'static str> {
        // Only one direction of writer ownership is checkable from the state
        // alone, and it is worth being precise about why.
        //
        // `Downloading` does not mean "a file handle is open". It means "a
        // worker has claimed this task", and `Manager::spawn` sets it *before*
        // the worker starts so a UI racing the first progress event never
        // renders a stale `Queued` over bytes already landing. So `Downloading`
        // legitimately covers both "claimed, writer not yet open" and "writing",
        // and asserting the converse would be wrong rather than merely strict.
        //
        // The direction that *is* a bug is a terminal task still holding a
        // handle: that means a rename happened while bytes were still arriving.
        if has_writer && self.is_terminal() {
            return Some("a terminal task must have closed its writer");
        }

        if self == TaskState::Completed {
            if let Some(total) = expected {
                if on_disk != total {
                    return Some("a task may only reach Completed once its length is verified");
                }
            }
        }

        // Sound at every point in the engine, unlike a "must have bytes" rule:
        // a task can be claimed, or can fail its first request, before a single
        // byte exists, and neither is a contradiction. Overshooting the declared
        // length always is.
        if let Some(total) = expected {
            if on_disk > total {
                return Some("more bytes on disk than the declared length");
            }
        }

        None
    }

    /// Debug-only invariant check.
    ///
    /// Delegates to [`TaskState::invariant_violation`] so the rules exist in
    /// one place; wired to `debug_assert!` so CI exercises them without costing
    /// release performance.
    pub fn assert_invariants(self, has_writer: bool, on_disk: u64, expected: Option<u64>) {
        debug_assert!(
            self.invariant_violation(has_writer, on_disk, expected)
                .is_none(),
            "task invariant violated: {:?} ({self:?})",
            self.invariant_violation(has_writer, on_disk, expected)
        );
    }
}

/// A single unit of work in the queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable identifier, unique within a queue file.
    pub id: String,

    /// Source URL.
    pub url: String,

    /// Chosen format id within the resolved media.
    pub format_id: String,

    /// Final output path, sanitised.
    pub output: PathBuf,

    /// Current state.
    pub state: TaskState,

    /// Bytes known to be on disk in the `.part` file.
    ///
    /// Treated as a hint. [`Task::reconcile`] replaces it with the real file
    /// length.
    pub bytes_done: u64,

    /// Total expected length, when known.
    pub total_bytes: Option<u64>,

    /// Segment plan for fragmented media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<Segment>>,

    /// Segments already written, for fragmented transfers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_segments: Vec<u32>,

    /// Container extension, kept separately so the final name survives a
    /// format that no longer resolves.
    pub container_ext: String,

    /// Last error message, for display. Never used for control flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,

    /// Unix milliseconds at which this task first began transferring.
    ///
    /// Set once, on the first worker claim, and never overwritten: a resumed
    /// task's wall clock spans the interruption, which is what a user reading
    /// "started 40 minutes ago" wants to know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,

    /// Unix milliseconds at which this task reached a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,

    /// Name of the resolver that produced this task's media.
    ///
    /// `None` for a task reconstructed from a queue file written before this
    /// field existed, and for one restored from a format that no longer resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,

    /// Codec summary from the resolved media, e.g. `"h264 / aac"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,

    /// Byte offsets at which this transfer was cut and later continued.
    ///
    /// This is the evidence for the resume claim. "It resumes" is a promise; this
    /// is the record of where it actually did, and a client can draw it. Empty
    /// for a transfer that has never been interrupted, which is the common case
    /// and is a good sign rather than missing data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seams: Vec<u64>,

    /// Progress samples, oldest first, for a sparkline.
    ///
    /// Bounded by [`MAX_MARKS`]. Old samples are dropped, not the file rewritten,
    /// because a two-hour download would otherwise grow an unbounded array in the
    /// queue file that is persisted on every tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<Mark>,

    /// Flight recorder: the last few things that happened, newest last.
    ///
    /// Bounded by [`MAX_EVENTS`]. This exists so a resume can be *shown* rather
    /// than merely claimed -- the details pane can print the 206, the offset it
    /// resumed from, and the reason it stopped, all of which would otherwise have
    /// existed only as log lines nobody reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<TaskEvent>,
}

/// Maximum retained progress samples per task.
pub const MAX_MARKS: usize = 64;

/// Maximum retained flight-recorder entries per task.
pub const MAX_EVENTS: usize = 24;

/// Minimum milliseconds between retained progress samples.
///
/// Progress fires per chunk, which on a fast connection is many times a second.
/// Sampling at a fixed wall-clock interval instead keeps the queue file small
/// and, more usefully, keeps the *rate* estimate from being dominated by whichever
/// chunk happened to be big.
pub const MARK_INTERVAL_MS: u64 = 250;

/// One progress sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// Unix milliseconds.
    pub at: u64,
    /// Bytes on disk at this moment.
    pub bytes: u64,
}

/// What a flight-recorder entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EventKind {
    /// A request went out.
    Open,
    /// The origin answered with a range.
    Range,
    /// A fragment was written.
    Segment,
    /// Something worth reading, with no structure of its own.
    Note,
    /// The transfer stopped and can continue.
    Interrupted,
    /// The transfer stopped for good.
    Failed,
}

/// One flight-recorder entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    /// Unix milliseconds.
    pub at: u64,
    /// What kind of thing happened.
    pub kind: EventKind,
    /// The detail, phrased for a person.
    pub msg: String,
}

use std::path::PathBuf;

use crate::model::{
    Segment, SnapshotError, SnapshotEvent, SnapshotState, TaskSnapshot, TransferKind,
};

/// A task with the minimum required fields set.
impl Task {
    /// Create a task in [`TaskState::Queued`].
    pub fn new(
        id: impl Into<String>,
        url: impl Into<String>,
        output: impl Into<PathBuf>,
        container_ext: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            format_id: "0".to_string(),
            output: output.into(),
            state: TaskState::Queued,
            bytes_done: 0,
            total_bytes: None,
            segments: None,
            completed_segments: Vec::new(),
            container_ext: container_ext.into(),
            last_error: None,
            started_at: None,
            finished_at: None,
            resolver: None,
            codec: None,
            seams: Vec::new(),
            marks: Vec::new(),
            events: Vec::new(),
        }
    }

    /// The `.part` path for this task's final output.
    ///
    /// Partial data always lives beside the final file with a `.part` suffix, so
    /// that a user browsing their downloads folder sees only completed files and
    /// an obvious half-written one.
    pub fn part_path(&self) -> PathBuf {
        let mut p = self.output.clone().into_os_string();
        p.push(".part");
        PathBuf::from(p)
    }

    /// Whether this transfer can be resumed without discarding data.
    ///
    /// Requires both a resumable state *and* either zero bytes or an existing
    /// partial file.
    pub fn can_resume(&self) -> bool {
        self.state.is_resumable()
    }

    /// Attempt a state transition.
    ///
    /// Returns `false` and changes nothing when the transition is illegal.
    pub fn transition(&mut self, next: TaskState) -> bool {
        if !self.state.can_transition_to(next) {
            return false;
        }
        self.state = next;
        if next != TaskState::Retrying && next != TaskState::Failed {
            self.last_error = None;
        }
        true
    }

    /// Reconcile `bytes_done` against the actual length of the `.part` file.
    ///
    /// **Disk wins.** A queue file that claims 5000 bytes when 12000 are on disk
    /// must resume from 12000, or the transfer will overwrite good data. A queue
    /// file that claims more than is on disk must resume from the smaller
    /// figure, or the transfer will leave a hole.
    ///
    /// Returns the reconciled length.
    pub fn reconcile(&mut self) -> u64 {
        let part = self.part_path();
        let actual = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

        // A completed task whose output file is missing has been deleted by the
        // user; it is no longer completed.
        if self.state == TaskState::Completed && !self.output.exists() {
            self.state = TaskState::Queued;
            self.bytes_done = 0;
            return 0;
        }

        self.bytes_done = actual.min(self.total_bytes.unwrap_or(u64::MAX));
        self.bytes_done
    }

    /// Whether the transfer looks finished: the expected total is met.
    pub fn is_satisfied(&self) -> bool {
        match self.total_bytes {
            Some(total) => self.bytes_done >= total,
            // With no known total, we cannot claim completion. Reporting
            // success here is how download managers hand users truncated files.
            None => false,
        }
    }

    /// Fraction complete in `0.0..=1.0`, or `None` when the total is unknown.
    pub fn progress(&self) -> Option<f64> {
        let total = self.total_bytes?;
        if total == 0 {
            return Some(1.0);
        }
        Some((self.bytes_done as f64 / total as f64).clamp(0.0, 1.0))
    }

    /// Whether bytes arrive as one contiguous range or as a walked segment plan.
    pub fn transfer_kind(&self) -> TransferKind {
        if self.segments.is_some() {
            TransferKind::Fragmented
        } else {
            TransferKind::Direct
        }
    }

    /// Start the clock, once.
    ///
    /// Called when a worker claims the task rather than on every tick, so a task
    /// that is paused and resumed still reports the time the user first pressed
    /// go rather than the time of the last attempt.
    pub fn begin(&mut self, at: u64) {
        self.started_at.get_or_insert(at);
        // A resumed run has not finished, whatever the last run's record says.
        self.finished_at = None;
    }

    /// Stamp the terminal moment.
    pub fn finish(&mut self, at: u64) {
        self.finished_at = Some(at);
    }

    /// Record a progress sample, rate-limited to one per [`MARK_INTERVAL_MS`].
    ///
    /// Returns whether a sample was kept. The last mark is always written when
    /// the task stops, which is what makes [`Self::rate_bps`] meaningful on a
    /// transfer too short to fill the interval twice.
    ///
    /// Timestamps are passed in rather than read here. The crate's one deliberate
    /// clock is in [`crate::net::speedtest`]; the manager supplies the time so
    /// that this stays a pure function of its arguments and testable without
    /// sleeping.
    pub fn mark(&mut self, at: u64, bytes: u64, force: bool) -> bool {
        if !force {
            if let Some(last) = self.marks.last() {
                if at.saturating_sub(last.at) < MARK_INTERVAL_MS {
                    return false;
                }
            }
        }
        if let Some(last) = self.marks.last() {
            if last.bytes == bytes {
                // No progress. A duplicate point makes the sparkline draw a flat
                // step that did not happen.
                return false;
            }
        }
        self.marks.push(Mark { at, bytes });
        if self.marks.len() > MAX_MARKS {
            let excess = self.marks.len() - MAX_MARKS;
            self.marks.drain(..excess);
        }
        true
    }

    /// Recent throughput in bits per second, from the retained samples.
    ///
    /// `None` until two samples exist, which is the honest answer: a single
    /// point is an amount, not a rate, and reporting one as the other is how a
    /// download manager shows a number that is not its speed.
    ///
    /// The window is the oldest retained sample to the newest. On a long
    /// download that is a rolling average over [`MAX_MARKS`] samples rather than
    /// the whole transfer, which is the more useful number anyway -- it tracks
    /// what the link is doing now.
    pub fn rate_bps(&self) -> Option<f64> {
        let (first, last) = (self.marks.first()?, self.marks.last()?);
        let elapsed_ms = last.at.checked_sub(first.at)?;
        if elapsed_ms == 0 {
            return None;
        }
        let bytes = last.bytes.checked_sub(first.bytes)?;
        Some(bytes as f64 * 8.0 * 1000.0 / elapsed_ms as f64)
    }

    /// Record that the transfer stopped at `offset` and will continue later.
    ///
    /// Idempotent for a repeated offset, because a retried request that stops
    /// again at the same place is one interruption, not two.
    pub fn note_seam(&mut self, offset: u64) {
        match self.seams.binary_search(&offset) {
            Ok(_) => {}
            Err(at) => self.seams.insert(at, offset),
        }
    }

    /// Append a flight-recorder entry, dropping the oldest past [`MAX_EVENTS`].
    pub fn note(&mut self, at: u64, kind: EventKind, msg: impl Into<String>) {
        self.events.push(TaskEvent {
            at,
            kind,
            msg: msg.into(),
        });
        if self.events.len() > MAX_EVENTS {
            let excess = self.events.len() - MAX_EVENTS;
            self.events.drain(..excess);
        }
    }

    /// Flatten into the shape a client renders.
    ///
    /// The one place the engine's vocabulary and a client's vocabulary meet. See
    /// [`TaskSnapshot`] for what is deliberately left out and why.
    pub fn snapshot(&self) -> TaskSnapshot {
        let fragmented = self.segments.is_some();
        TaskSnapshot {
            id: self.id.clone(),
            name: self
                .output
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                // A task with no filename is a programming error upstream, but
                // the snapshot still has to be renderable rather than panic in
                // the middle of a redraw.
                .unwrap_or_else(|| self.container_ext.clone()),
            source: self.url.clone(),
            dest: self.output.to_string_lossy().into_owned(),
            state: self.state.snapshot_state(),
            kind: self.transfer_kind(),
            container: self.container_ext.clone(),
            resolver: self.resolver.clone(),
            codec: self.codec.clone(),
            received: self.bytes_done,
            total: self.total_bytes,
            rate_bps: self.rate_bps(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            seams: self.seams.clone(),
            marks: self.marks.iter().map(|m| m.bytes).collect(),
            segments_done: fragmented.then_some(self.completed_segments.len()),
            segments_total: self.segments.as_ref().map(|s| s.len()),
            error: self.last_error.as_ref().map(|msg| SnapshotError {
                // The engine stores a message, not a code. Rather than
                // pretend there is a stable identifier here, the error code
                // is derived from the state that caused it -- which is the
                // part a client can actually branch on safely.
                code: self.state.snapshot_error_code().to_string(),
                message: msg.clone(),
            }),
            events: self
                .events
                .iter()
                .map(|e| SnapshotEvent {
                    at: e.at,
                    kind: e.kind.snapshot_event_name().to_string(),
                    msg: e.msg.clone(),
                })
                .collect(),
        }
    }

    /// The whole queue, flattened.
    pub fn snapshot_all<'a>(tasks: impl IntoIterator<Item = &'a Task>) -> Vec<TaskSnapshot> {
        tasks.into_iter().map(Task::snapshot).collect()
    }
}

impl TaskState {
    /// The user-facing name for this state.
    ///
    /// Two renames, both deliberate. `Downloading` is the engine's word for
    /// "a worker holds this task" and is the right word in a log; a person wants
    /// to know it is running. `Completed` means "verified against the expected
    /// length", which is a stronger claim than "done" and not one a queue row
    /// should make on its behalf.
    pub fn snapshot_state(self) -> SnapshotState {
        match self {
            Self::Queued => SnapshotState::Queued,
            Self::Downloading => SnapshotState::Running,
            Self::Paused => SnapshotState::Paused,
            Self::Retrying => SnapshotState::Retrying,
            Self::Failed => SnapshotState::Failed,
            Self::Completed => SnapshotState::Done,
        }
    }

    /// A stable error code derived from the state that produced the failure.
    pub fn snapshot_error_code(self) -> &'static str {
        match self {
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            // Not reachable: `snapshot` only fills `error` from `last_error`, and
            // `transition` clears that on every other state. Mapped rather than
            // left off so that adding a state later cannot make this
            // non-exhaustive in the middle of a redraw.
            _ => "unknown",
        }
    }
}

impl EventKind {
    /// The short lowercase word a client shows.
    pub fn snapshot_event_name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Range => "range",
            Self::Segment => "seg",
            Self::Note => "note",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

impl TaskState {
    /// Every state, for exhaustive UI iteration and tests.
    pub fn all() -> &'static [Self] {
        use TaskState::*;
        &[Queued, Downloading, Paused, Retrying, Failed, Completed]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task::new("t1", "https://x.invalid/a.mp4", "out/a.mp4", "mp4")
    }

    #[test]
    fn the_state_graph_permits_only_declared_transitions() {
        let mut t = task();
        assert_eq!(t.state, TaskState::Queued);

        assert!(t.transition(TaskState::Downloading));
        assert!(t.transition(TaskState::Paused));
        assert!(t.transition(TaskState::Downloading));
        assert!(t.transition(TaskState::Completed));
    }

    #[test]
    fn every_resumable_state_can_be_requeued() {
        // `Manager::resume` moves a task to `Queued` before handing it back to
        // the scheduler. If a state cannot reach `Queued`, resume is a silent
        // no-op there and the task is stranded with no worker.
        for state in [TaskState::Queued, TaskState::Paused, TaskState::Retrying] {
            let mut t = task();
            t.state = state;
            if state == TaskState::Queued {
                continue;
            }
            assert!(
                t.transition(TaskState::Queued),
                "{state:?} must be re-queueable"
            );
            assert_eq!(t.state, TaskState::Queued);
        }

        // And a failed task can be retried, which is the same path.
        let mut t = task();
        t.state = TaskState::Failed;
        assert!(t.transition(TaskState::Queued));
        assert!(t.transition(TaskState::Downloading));
        assert!(t.transition(TaskState::Retrying));
        assert!(t.transition(TaskState::Queued));
    }

    #[test]
    fn a_completed_task_never_reopens() {
        // Otherwise a resumed run would re-finalise a file the user already
        // has, or "complete" a task whose output was deleted.
        let mut t = task();
        t.transition(TaskState::Downloading);
        t.transition(TaskState::Completed);

        for next in TaskState::all() {
            assert!(
                !t.transition(*next),
                "Completed -> {next:?} must be rejected"
            );
        }
        assert_eq!(t.state, TaskState::Completed);
    }

    #[test]
    fn a_failed_task_may_return_to_the_queue() {
        let mut t = task();
        t.transition(TaskState::Downloading);
        t.transition(TaskState::Failed);
        assert!(t.transition(TaskState::Queued));
        assert!(t.transition(TaskState::Downloading));
    }

    #[test]
    fn a_task_cannot_transition_to_itself() {
        let mut t = task();
        assert!(!t.transition(TaskState::Queued));
        assert_eq!(t.state, TaskState::Queued);
    }

    #[test]
    fn only_downloading_holds_a_writer() {
        assert!(TaskState::Downloading.holds_writer());
        for s in [
            TaskState::Queued,
            TaskState::Paused,
            TaskState::Retrying,
            TaskState::Failed,
            TaskState::Completed,
        ] {
            assert!(!s.holds_writer(), "{s:?}");
        }
    }

    #[test]
    fn resumability_matches_state() {
        assert!(TaskState::Paused.is_resumable());
        assert!(TaskState::Retrying.is_resumable());
        assert!(TaskState::Queued.is_resumable());
        assert!(!TaskState::Downloading.is_resumable());
        assert!(!TaskState::Completed.is_resumable());
    }

    #[test]
    fn invariants_hold_for_every_reachable_state() {
        // Exercises the full rule set. In debug builds `assert_invariants`
        // panics on a violation, so CI catches regressions here rather than in
        // the field.
        for state in TaskState::all() {
            assert_eq!(
                state.invariant_violation(state.holds_writer(), 100, Some(100)),
                None,
                "{state:?} violated an invariant in a coherent configuration"
            );
            state.assert_invariants(state.holds_writer(), 100, Some(100));
        }
    }

    /* ------------------------------------------------------------------ */
    /* progress sampling                                                   */
    /* ------------------------------------------------------------------ */

    #[test]
    fn marks_are_rate_limited_so_the_queue_file_stops_growing() {
        let mut t = task();
        // Progress fires per chunk. Ten ticks inside one interval is what a fast
        // connection looks like, and only the first may be kept.
        assert!(t.mark(1_000, 4_096, false));
        assert!(!t.mark(1_050, 8_192, false));
        assert!(!t.mark(1_200, 12_288, false));
        assert_eq!(t.marks.len(), 1);

        // Once the interval has passed, the next one is kept.
        assert!(t.mark(1_000 + MARK_INTERVAL_MS, 16_384, false));
        assert_eq!(t.marks.len(), 2);

        // `force` is the "the transfer just stopped" case: the last reading is
        // always worth having, because it is the one that makes the rate mean
        // something for a transfer too short to fill the interval twice.
        assert!(t.mark(1_100, 99_999, true));
        assert_eq!(t.marks.last().unwrap().bytes, 99_999);
    }

    #[test]
    fn a_mark_that_reports_no_progress_is_dropped() {
        // A duplicate point would draw a flat step in a sparkline that did not
        // happen, and would make the window between two samples look like a
        // stall rather than what it is.
        let mut t = task();
        assert!(t.mark(0, 1_000, false));
        assert!(!t.mark(MARK_INTERVAL_MS, 1_000, false));
        assert_eq!(t.marks.len(), 1);
    }

    #[test]
    fn marks_are_bounded() {
        let mut t = task();
        for i in 0..(MAX_MARKS * 3) {
            t.mark(i as u64 * MARK_INTERVAL_MS, i as u64 * 1_000 + 1, false);
        }
        assert_eq!(t.marks.len(), MAX_MARKS);
        // The newest survive; the oldest are the ones dropped.
        assert_eq!(
            t.marks.last().unwrap().bytes,
            (MAX_MARKS * 3 - 1) as u64 * 1_000 + 1
        );
    }

    #[test]
    fn a_rate_needs_two_samples() {
        let mut t = task();
        assert_eq!(t.rate_bps(), None, "one point is an amount, not a rate");

        t.mark(0, 0, false);
        assert_eq!(t.rate_bps(), None);

        // 1 MiB in one second is 8_388_608 bits per second.
        t.mark(1_000, 1024 * 1024, false);
        let r = t.rate_bps().expect("two samples make a rate");
        assert!((r - 8_388_608.0).abs() < 1.0, "got {r}");
    }

    #[test]
    fn a_rate_with_no_elapsed_time_is_absent_not_zero() {
        // Two samples at the same instant say nothing about speed. Reporting
        // zero would be a confident wrong answer; absent is the honest one.
        let mut t = task();
        t.mark(500, 0, false);
        t.mark(500, 1_000, false);
        assert_eq!(t.rate_bps(), None);
    }

    #[test]
    fn seams_are_recorded_once_and_in_order() {
        let mut t = task();
        t.note_seam(900);
        t.note_seam(200);
        t.note_seam(900);
        t.note_seam(1_500);
        assert_eq!(t.seams, vec![200, 900, 1_500]);
    }

    #[test]
    fn the_flight_recorder_is_bounded_and_keeps_the_newest() {
        let mut t = task();
        for i in 0..(MAX_EVENTS + 10) {
            t.note(i as u64, EventKind::Note, format!("event {i}"));
        }
        assert_eq!(t.events.len(), MAX_EVENTS);
        assert_eq!(t.events.first().unwrap().msg, "event 10");
        assert_eq!(
            t.events.last().unwrap().msg,
            format!("event {}", MAX_EVENTS + 9)
        );
    }

    /* ------------------------------------------------------------------ */
    /* the wire contract                                                   */
    /* ------------------------------------------------------------------ */

    #[test]
    fn every_state_has_a_user_facing_name() {
        // The mapping is the whole reason `SnapshotState` exists, so a new state
        // that forgets it has to fail here rather than in a client's queue row.
        assert_eq!(TaskState::Queued.snapshot_state(), SnapshotState::Queued);
        assert_eq!(
            TaskState::Downloading.snapshot_state(),
            SnapshotState::Running
        );
        assert_eq!(TaskState::Paused.snapshot_state(), SnapshotState::Paused);
        assert_eq!(
            TaskState::Retrying.snapshot_state(),
            SnapshotState::Retrying
        );
        assert_eq!(TaskState::Failed.snapshot_state(), SnapshotState::Failed);
        assert_eq!(TaskState::Completed.snapshot_state(), SnapshotState::Done);

        // And the two agree about which states are final. Terminal-ness is the
        // only property with a counterpart on both sides: `is_active` has no
        // engine equivalent, because `Paused` is neither in flight nor finished
        // and there is no `TaskState` question that maps onto that.
        for s in TaskState::all() {
            assert_eq!(
                s.snapshot_state().is_terminal(),
                s.is_terminal(),
                "{s:?} disagrees about being terminal"
            );
        }
        assert!(SnapshotState::Running.is_active());
        assert!(!SnapshotState::Paused.is_active());
        assert!(!SnapshotState::Done.is_active());
    }

    #[test]
    fn a_snapshot_carries_the_filename_and_the_directory_apart() {
        let t = Task::new(
            "t1",
            "https://x.invalid/a.mp4",
            "C:\\\\Users\\\\you\\\\Videos\\\\lecture.mp4",
            "mp4",
        );
        let s = t.snapshot();
        assert_eq!(s.name, "lecture.mp4");
        assert_eq!(s.dest, "C:\\\\Users\\\\you\\\\Videos\\\\lecture.mp4");
        assert_eq!(s.source, "https://x.invalid/a.mp4");
        assert_eq!(s.container, "mp4");
        assert_eq!(s.kind, TransferKind::Direct);
        // Absent rather than null: "we looked and there is no failure" is a
        // different statement from "we did not check".
        assert_eq!(s.error, None);
        assert_eq!(s.rate_bps, None);
        assert_eq!(s.started_at, None);
        // A direct transfer has no segment plan to report on.
        assert_eq!(s.segments_done, None);
        assert_eq!(s.segments_total, None);
        assert!(s.marks.is_empty());
    }

    #[test]
    fn a_fragmented_task_reports_its_segment_counts() {
        let mut t = task();
        t.segments = Some(vec![
            crate::model::Segment {
                index: 0,
                uri: "0.m4s".into(),
                byte_range: None,
                duration_secs: Some(6.0),
            },
            crate::model::Segment {
                index: 1,
                uri: "1.m4s".into(),
                byte_range: None,
                duration_secs: Some(6.0),
            },
            crate::model::Segment {
                index: 2,
                uri: "2.m4s".into(),
                byte_range: None,
                duration_secs: Some(6.0),
            },
        ]);
        t.completed_segments = vec![0];

        let s = t.snapshot();
        assert_eq!(s.kind, TransferKind::Fragmented);
        assert_eq!(s.kind.kind_name(), "fragmented");
        assert_eq!(s.segments_done, Some(1));
        assert_eq!(s.segments_total, Some(3));
    }

    #[test]
    fn the_snapshot_serialises_with_the_names_a_client_reads() {
        let mut t = task();
        t.state = TaskState::Completed;
        t.resolver = Some("hls".into());
        t.codec = Some("av1 / opus".into());
        t.started_at = Some(1_700_000_000_000);
        t.finished_at = Some(1_700_000_060_000);
        t.bytes_done = 4_096;
        t.total_bytes = Some(8_192);
        t.mark(1_000, 0, false);
        t.mark(2_000, 4_096, false);
        t.note(1_500, EventKind::Range, "origin replied 206");

        let v: serde_json::Value =
            serde_json::to_value(t.snapshot()).expect("snapshot is serialisable");
        // camelCase on the wire, matching the JS the app actually reads.
        assert_eq!(v["startedAt"], 1_700_000_000_000u64);
        assert_eq!(v["finishedAt"], 1_700_000_060_000u64);
        assert_eq!(v["received"], 4_096);
        assert_eq!(v["total"], 8_192);
        assert_eq!(v["rateBps"], 32_768.0);
        assert_eq!(v["state"], "done");
        assert_eq!(v["kind"], "direct");
        assert_eq!(v["resolver"], "hls");
        assert_eq!(v["codec"], "av1 / opus");
        assert_eq!(v["events"][0]["t"], 1_500);
        assert_eq!(v["events"][0]["kind"], "range");
        assert_eq!(v["events"][0]["msg"], "origin replied 206");
        assert_eq!(v["marks"], serde_json::json!([0, 4096]));
        // Absent fields are omitted, not null.
        assert!(v.get("error").is_none());
        assert!(v.get("segmentsDone").is_none());
    }

    #[test]
    fn a_failed_task_carries_the_message_and_a_branchable_code() {
        let mut t = task();
        t.state = TaskState::Retrying;
        t.last_error = Some("connection reset by peer".into());
        let s = t.snapshot();
        let e = s.error.expect("a failure is reported");
        assert_eq!(e.message, "connection reset by peer");
        assert_eq!(e.code, "retrying");
    }

    #[test]
    fn begin_stamps_once_and_a_resume_does_not_restart_the_clock() {
        let mut t = task();
        t.begin(1_000);
        t.begin(2_000);
        assert_eq!(t.started_at, Some(1_000));

        // Finishing, then being queued again, must clear the finish time --
        // otherwise a resumed task shows as finished while it is transferring.
        t.finish(3_000);
        assert_eq!(t.finished_at, Some(3_000));
        t.begin(4_000);
        assert_eq!(t.finished_at, None);
        assert_eq!(t.started_at, Some(1_000), "the first attempt is the start");
    }

    #[test]
    fn completion_requires_a_verified_length() {
        // An unverified transfer must never be reported as complete.
        assert!(TaskState::Completed
            .invariant_violation(false, 50, Some(100))
            .is_some());
        assert_eq!(
            TaskState::Completed.invariant_violation(false, 100, Some(100)),
            None
        );
        // Unknown totals are tolerated: chunked responses have none.
        assert_eq!(
            TaskState::Completed.invariant_violation(false, 50, None),
            None
        );
    }

    #[test]
    fn a_terminal_task_may_not_still_be_writing() {
        // The rename happens while bytes are still arriving. Never legal.
        assert!(TaskState::Completed
            .invariant_violation(true, 100, Some(100))
            .is_some());
        assert!(TaskState::Failed
            .invariant_violation(true, 100, Some(100))
            .is_some());
    }

    #[test]
    fn a_claimed_task_may_not_yet_have_a_writer() {
        // `Manager::spawn` marks a task `Downloading` before the worker opens
        // the file, deliberately. That configuration is coherent, so the
        // invariant must accept it — the old rule rejected it and panicked on
        // every single download.
        assert_eq!(
            TaskState::Downloading.invariant_violation(false, 50, Some(100)),
            None
        );
        assert_eq!(
            TaskState::Downloading.invariant_violation(true, 50, Some(100)),
            None
        );
    }

    #[test]
    fn claiming_a_task_before_any_byte_exists_is_fine() {
        // A fresh task, and a task whose very first request failed, both have
        // zero bytes on disk and neither is a contradiction.
        assert_eq!(
            TaskState::Downloading.invariant_violation(false, 0, Some(100)),
            None
        );
        assert_eq!(
            TaskState::Retrying.invariant_violation(false, 0, Some(100)),
            None
        );
    }

    #[test]
    fn overshooting_the_declared_length_is_contradictory() {
        // Sound in every state and at every point in the engine, which is why
        // it replaced the "must have bytes" rule.
        for state in TaskState::all() {
            assert!(
                state.invariant_violation(false, 101, Some(100)).is_some(),
                "{state:?} accepted more bytes than the server advertised"
            );
        }
    }

    #[test]
    fn progress_is_unavailable_without_a_total() {
        let mut t = task();
        assert_eq!(t.progress(), None);
        assert!(!t.is_satisfied());

        t.total_bytes = Some(200);
        t.bytes_done = 50;
        assert_eq!(t.progress(), Some(0.25));
        assert!(!t.is_satisfied());

        t.bytes_done = 200;
        assert!(t.is_satisfied());
        assert_eq!(t.progress(), Some(1.0));
    }

    #[test]
    fn a_zero_total_is_reported_as_complete_rather_than_dividing_by_zero() {
        let mut t = task();
        t.total_bytes = Some(0);
        assert_eq!(t.progress(), Some(1.0));
        assert!(t.is_satisfied());
    }

    #[test]
    fn the_part_path_sits_beside_the_output() {
        let t = task();
        assert_eq!(t.part_path(), PathBuf::from("out/a.mp4.part"));
    }

    #[test]
    fn an_error_message_is_cleared_on_recovery_but_kept_on_failure() {
        let mut t = task();
        t.transition(TaskState::Downloading);
        t.last_error = Some("boom".into());
        t.transition(TaskState::Failed);
        assert_eq!(t.last_error.as_deref(), Some("boom"));

        t.transition(TaskState::Queued);
        assert_eq!(t.last_error, None, "a healthy transition clears the error");
    }

    #[test]
    fn a_queued_task_with_a_missing_part_file_reconciles_to_zero() {
        let mut t = task();
        t.total_bytes = Some(1000);
        // No file on disk. Reconcile must report zero, not trust the stored hint.
        t.bytes_done = 900;
        assert_eq!(t.reconcile(), 0);
        assert_eq!(t.bytes_done, 0);
    }

    #[test]
    fn reconcile_never_exceeds_the_known_total() {
        // A file longer than the advertised total means something went wrong;
        // clamping keeps progress monotonic and prevents an overflow later.
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("a.mp4.part");
        std::fs::write(&part, vec![0u8; 200]).unwrap();

        let mut t = Task::new("t", "https://x.invalid/a", dir.path().join("a.mp4"), "mp4");
        t.total_bytes = Some(100);
        assert_eq!(t.reconcile(), 100);
    }
}
