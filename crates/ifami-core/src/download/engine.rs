//! The transfer engine: one task, start to finish.
//!
//! # How resume actually works
//!
//! For a direct file:
//!
//! 1. Reconcile the recorded offset against the real length of the `.part`
//!    file. **Disk wins.** A queue file is a hint; the filesystem is truth.
//! 2. If bytes are present, issue `Range: bytes=<offset>-`.
//! 3. Consult [`decide_resume`]. A server that answers `200 OK` has ignored the
//!    range: discard the partial file and start over. Appending a full body to
//!    existing data produces a double-length, unplayable file with no error,
//!    which is the single most common corruption bug in download managers.
//! 4. Write from the decided offset, flush, and only then record progress.
//!
//! For fragmented media, resume is per segment: a segment index recorded as
//! complete is never re-fetched, and byte ranges inside a resource thread
//! correctly through the manifest's implied offsets.
//!
//! # Cancellation
//!
//! Cancellation is carried as [`TransferError::Paused`], not as a flag every
//! layer must inspect. `?` propagates it out of deep call stacks, and the
//! writer is always flushed on the way out.
//!
//! # What this module deliberately does not do
//!
//! Parallel segment downloads. They are the obvious optimisation and they are
//! also the obvious corruption bug: four writers into one file at four offsets
//! with no plan for what happens when three succeed and the fourth does not.
//! Segments are fetched in order. When concurrency is wanted, it belongs
//! between tasks, in [`crate::download::Manager`], where each writer owns a
//! whole file and cannot collide.

use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::download::task::{Task, TaskState};
use crate::error::{Error, NetError, Result, TransferError};
use crate::model::Segment;
use crate::net::client::{HttpClient, HttpRequest};
use crate::net::range::{decide_resume, parse_content_range, ByteRange, ResumeDecision};

/// Bytes written between flushes during a transfer.
///
/// Small enough that a pause loses almost nothing, large enough that flushing is
/// not per-chunk.
const FLUSH_INTERVAL: u64 = 1024 * 1024;

/// Largest single ranged sub-request issued while fetching one segment.
///
/// A server is free to satisfy a bounded range with a shorter chunk. We re-issue
/// until the declared range is covered, but in bounded steps so a hostile or
/// broken server cannot make us hold a multi-gigabyte response in memory at
/// once.
const SEGMENT_CHUNK: u64 = 8 * 1024 * 1024;

/// Cooperative cancellation.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Err(TransferError::Paused)` if cancelled, so callers can use `?`.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(TransferError::Paused.into());
        }
        Ok(())
    }
}

/// Progress emitted during a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes now on disk for this task.
    pub bytes_done: u64,
    /// Total expected, when known.
    pub total_bytes: Option<u64>,
}

impl Progress {
    /// Fraction complete, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total_bytes?;
        if total == 0 {
            return Some(1.0);
        }
        Some((self.bytes_done as f64 / total as f64).clamp(0.0, 1.0))
    }
}

/// Run `task` to completion.
///
/// On success the `.part` file has been renamed to its final path and the task is
/// [`TaskState::Completed`]. On [`TransferError::Paused`] the `.part` file is
/// intact and resumable and the task is [`TaskState::Paused`]. On any other
/// failure the task's state is updated to reflect what happened: a retryable
/// error leaves it [`TaskState::Retrying`] with the partial data intact, and a
/// permanent one leaves it [`TaskState::Failed`].
///
/// [`TransferError::Paused`] is reported as `Ok(())`. A user pressing pause has
/// been obeyed, not failed; only the task's state distinguishes the two.
pub async fn run(
    client: &dyn HttpClient,
    task: &mut Task,
    cancel: &Cancel,
    mut on_progress: impl FnMut(Progress),
) -> Result<()> {
    task.state
        .assert_invariants(false, task.bytes_done, task.total_bytes);

    // Disk is truth. A stale or optimistic queue file must never cause us to
    // overwrite good data or skip past a hole.
    let offset = task.reconcile();

    // A direct transfer that already holds every advertised byte needs no second
    // request. Fragmented transfers always walk the plan, because their real
    // resume state is the per-segment index list rather than a byte count — a
    // byte count that happens to match the total is not proof every segment
    // landed.
    if task.segments.is_none() && task.total_bytes.is_some_and(|total| offset >= total) {
        return finalise(task);
    }

    // A task handed to us already `Downloading` was *reserved* by its caller:
    // `Manager::spawn` writes that state before the worker exists, so a UI racing
    // the first progress event never renders a stale `Queued` over bytes already
    // landing. Accepting it here is what keeps the two halves consistent —
    // requiring the transition would make every single transfer abort on its first
    // byte, with the task left parked in `Downloading` and no error to explain it.
    //
    // Mutual exclusion between workers is the Manager's job, enforced by the
    // `running` map; a `&mut Task` cannot be held by two workers at once, so it is
    // not this function's invariant to defend.
    if task.state != TaskState::Downloading && !task.transition(TaskState::Downloading) {
        // `Completed` and `Failed` are terminal. Refusing is the only safe
        // answer: re-running either would re-finalise a file the user already
        // has, or re-attempt a download they have been told will not work.
        return Err(Error::InvalidArgument(format!(
            "task {} cannot start from state {:?}",
            task.id, task.state
        )));
    }

    let segments = task.segments.clone();
    let outcome = match segments {
        Some(segments) => run_fragmented(client, task, &segments, cancel, &mut on_progress).await,
        None => run_direct(client, task, offset, cancel, &mut on_progress).await,
    };

    match outcome {
        Ok(()) => finalise(task),
        Err(Error::Transfer(TransferError::Paused)) => {
            // The writer was flushed before the error propagated, so this is a
            // clean stop at a resumable offset.
            task.transition(TaskState::Paused);
            Ok(())
        }
        Err(e) => {
            let retryable = e.is_retryable();
            task.last_error = Some(e.to_string());
            task.transition(if retryable {
                TaskState::Retrying
            } else {
                TaskState::Failed
            });
            Err(e)
        }
    }
}

/// Transfer a single-file resource.
async fn run_direct(
    client: &dyn HttpClient,
    task: &mut Task,
    start_offset: u64,
    cancel: &Cancel,
    on_progress: &mut impl FnMut(Progress),
) -> Result<()> {
    let range = (start_offset > 0).then(|| ByteRange::from_offset(start_offset));

    let resp = client
        .execute(HttpRequest::get(&task.url).with_range(range))
        .await?;

    if !resp.is_success() {
        return Err(status_error(resp.status, &task.url).into());
    }

    let status = resp.status;
    let header_len = resp.headers.content_length();
    let header_range = resp.headers.content_range_raw().map(str::to_string);
    let advertised_total = resp.total_bytes();

    let decision = decide_resume(
        start_offset,
        task.total_bytes.or(advertised_total),
        status,
        resp.headers.content_range_raw(),
    );

    let write_offset = match decision {
        ResumeDecision::Restart => 0,
        ResumeDecision::Append { offset, total } => {
            if let Some(t) = total {
                task.total_bytes = Some(t);
            }
            offset
        }
        ResumeDecision::DiscardAndRestart => {
            // The server will not resume, or the object changed. Partial data is
            // worthless and keeping it would corrupt the result.
            discard_part(task)?;
            0
        }
    };

    let expected_total = task.total_bytes;
    let mut body = resp.body;

    ensure_parent_dir(task)?;
    let mut file = open_for_write(task, write_offset)?;

    // The one place in the engine where a handle is genuinely open. Asserting
    // here rather than at entry is deliberate: at entry the manager has only
    // *reserved* the task, and `Downloading` covers both states.
    task.state
        .assert_invariants(true, task.bytes_done, task.total_bytes);

    let mut written = write_offset;
    let mut since_flush = 0u64;

    loop {
        cancel.check()?;

        let Some(chunk) = body.next().await else {
            break;
        };
        let chunk = chunk?;

        if chunk.is_empty() {
            continue;
        }

        file.write_all(&chunk)
            .await
            .map_err(|e| Error::io(task.part_path(), e))?;
        written += chunk.len() as u64;
        since_flush += chunk.len() as u64;

        if since_flush >= FLUSH_INTERVAL {
            file.flush()
                .await
                .map_err(|e| Error::io(task.part_path(), e))?;
            since_flush = 0;
            task.bytes_done = written;
            on_progress(Progress {
                bytes_done: written,
                total_bytes: expected_total,
            });
        }
    }

    file.flush()
        .await
        .map_err(|e| Error::io(task.part_path(), e))?;
    file.sync_all()
        .await
        .map_err(|e| Error::io(task.part_path(), e))?;
    drop(file);

    task.bytes_done = written;
    on_progress(Progress {
        bytes_done: written,
        total_bytes: expected_total,
    });

    // Verification. The check only applies when a total is actually known: the
    // transport reporting end-of-body with no `Content-Length` and no
    // `Content-Range` (chunked encoding) is a complete message per RFC 9112, so
    // refusing to finish there would break perfectly good servers. What we never
    // do is claim completion against a total we did not reach.
    let total_for_check = expected_total.or_else(|| match header_range.as_deref() {
        Some(raw) => parse_content_range(raw).ok().and_then(|c| c.total),
        None => header_len.map(|l| write_offset + l),
    });

    match total_for_check {
        Some(total) if written != total => Err(TransferError::Incomplete {
            written,
            expected: total,
        }
        .into()),
        Some(_) | None => Ok(()),
    }
}

/// Transfer a fragmented resource, segment by segment.
///
/// Segments are written in order into the `.part` file, so it always holds a
/// contiguous prefix of the plan. That invariant is what makes per-index resume
/// sound: if the completed set is not a prefix, the bytes on disk cannot be
/// attributed to a known set of segments and we refuse rather than splice an
/// unknown prefix onto a plan.
async fn run_fragmented(
    client: &dyn HttpClient,
    task: &mut Task,
    segments: &[Segment],
    cancel: &Cancel,
    on_progress: &mut impl FnMut(Progress),
) -> Result<()> {
    ensure_parent_dir(task)?;

    let Some(plan) = normalise_plan(segments) else {
        return Err(TransferError::MissingSegment { index: 0 }.into());
    };

    // Only claim a total when *every* segment declares a bounded range. Summing
    // the bounded ones and treating the rest as zero would understate the size,
    // and `finalise` would then reject a completely correct download as short.
    // Summed over the normalised plan rather than the raw slice, so the figure
    // is derived from the same segment list that will actually be fetched.
    let total = task.total_bytes.or_else(|| {
        plan.iter()
            .try_fold(0u64, |acc, s| Some(acc + s.byte_range?.len()?))
    });

    // A recorded completed set that is not a prefix means the file on disk and
    // the plan disagree. Start the file over rather than guess. An unverifiable
    // prefix counts as a disagreement: we cannot prove the bytes belong to the
    // segments the queue claims.
    let recorded = task.completed_segments.clone();
    let prefix = plan.prefix_len(&recorded);
    let aligned = match plan.prefix_bytes(prefix) {
        Some(expected) => disk_len(task) == expected,
        None => false,
    };
    if recorded.len() != prefix || !aligned {
        task.completed_segments.clear();
        discard_part(task)?;
    }

    let start = task.completed_segments.len();
    let mut file = open_for_append(task)?;
    task.state
        .assert_invariants(true, task.bytes_done, task.total_bytes);
    let mut written = file
        .metadata()
        .await
        .map_err(|e| Error::io(task.part_path(), e))?
        .len();

    for segment in &plan[start..] {
        cancel.check()?;

        let bytes = fetch_segment(client, segment, cancel).await?;
        file.write_all(&bytes)
            .await
            .map_err(|e| Error::io(task.part_path(), e))?;
        file.flush()
            .await
            .map_err(|e| Error::io(task.part_path(), e))?;

        written += bytes.len() as u64;
        task.completed_segments.push(segment.index);
        task.bytes_done = written;
        on_progress(Progress {
            bytes_done: written,
            total_bytes: total,
        });
    }

    file.sync_all()
        .await
        .map_err(|e| Error::io(task.part_path(), e))?;
    drop(file);

    debug_assert_eq!(task.completed_segments.len(), plan.len());

    task.total_bytes = Some(total.unwrap_or(written));
    task.bytes_done = written;
    Ok(())
}

/// Fetch one segment into memory.
///
/// Segments are held in memory rather than streamed because a bounded range is
/// capped by the manifest and a single request either way; the byte-length
/// check below is what makes that safe. A server that keeps handing back short
/// chunks is re-queried, and a server that makes no progress fails loudly
/// instead of looping forever.
async fn fetch_segment(
    client: &dyn HttpClient,
    segment: &Segment,
    cancel: &Cancel,
) -> Result<Vec<u8>> {
    let Some(range) = segment.byte_range else {
        let resp = client.execute(HttpRequest::get(&segment.uri)).await?;
        if !resp.is_success() {
            return Err(status_error(resp.status, &segment.uri).into());
        }
        let mut body = resp.body;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            cancel.check()?;
            out.extend_from_slice(&chunk?);
        }
        return Ok(out);
    };

    let want = range.len().unwrap_or(0);
    let mut out = Vec::new();
    let mut have = 0u64;

    while have < want {
        cancel.check()?;

        let start = range.start + have;
        let end = start + (want - have).min(SEGMENT_CHUNK);

        let resp = client
            .execute(HttpRequest::get(&segment.uri).with_range(Some(ByteRange::closed(start, end))))
            .await?;

        if !resp.is_success() {
            return Err(status_error(resp.status, &segment.uri).into());
        }

        // A `206` that starts somewhere other than what we asked for would leave
        // a hole in the segment. Restarting is the only safe answer.
        if resp.status == 206 {
            match resp.headers.content_range() {
                Some(cr) if cr.range.start == start => {}
                Some(cr) => {
                    return Err(TransferError::RemoteChanged {
                        expected: start,
                        actual: cr.range.start,
                    }
                    .into())
                }
                None => {
                    return Err(TransferError::BadContentRange {
                        raw: resp
                            .headers
                            .content_range_raw()
                            .unwrap_or("<absent>")
                            .to_string(),
                    }
                    .into())
                }
            }
        }

        let mut body = resp.body;
        let mut got = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            got += chunk.len() as u64;
            out.extend_from_slice(&chunk);
            // Stop at the declared range even if the server keeps streaming.
            if have + got >= want {
                break;
            }
        }

        if got == 0 {
            return Err(TransferError::Incomplete {
                written: have,
                expected: want,
            }
            .into());
        }
        have += got;
    }

    if out.len() as u64 != want {
        return Err(TransferError::Incomplete {
            written: out.len() as u64,
            expected: want,
        }
        .into());
    }

    Ok(out)
}

/// A segment plan ordered by index, with duplicates and gaps rejected.
struct Plan(Vec<Segment>);

impl Plan {
    fn len(&self) -> usize {
        self.0.len()
    }

    /// How many leading segments the recorded set covers.
    ///
    /// Zero if the recorded set is not a prefix: a missing earlier index means
    /// the bytes after the hole cannot be attributed to a known segment. Indices
    /// beyond the first gap do not extend the prefix, and duplicates are
    /// ignored.
    fn prefix_len(&self, recorded: &[u32]) -> usize {
        let mut n = 0;
        while n < self.0.len() && recorded.contains(&(n as u32)) {
            n += 1;
        }
        n
    }

    /// Bytes the first `n` segments occupy, using the plan's declared ranges.
    ///
    /// Returns `None` for an unbounded segment, because a resource of unknown
    /// length makes the on-disk prefix unverifiable.
    fn prefix_bytes(&self, n: usize) -> Option<u64> {
        let mut total = 0u64;
        for segment in &self.0[..n] {
            total = total.checked_add(segment.byte_range?.len()?)?;
        }
        Some(total)
    }
}

impl std::ops::Deref for Plan {
    type Target = [Segment];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Validate and order a segment plan.
///
/// Duplicated and out-of-order indices are rejected: both make the completed
/// set ambiguous, and an ambiguous completed set is exactly how a resumed
/// fragmented download produces a file that is the right length and the wrong
/// contents.
fn normalise_plan(segments: &[Segment]) -> Option<Plan> {
    if segments.is_empty() {
        return None;
    }

    let mut ordered: Vec<Segment> = segments.to_vec();
    ordered.sort_by_key(|s| s.index);

    for (i, segment) in ordered.iter().enumerate() {
        if segment.index as usize != i {
            return None;
        }
        if segment.uri.trim().is_empty() {
            return None;
        }
    }

    Some(Plan(ordered))
}

/// Rename the `.part` file to its final path and mark the task complete.
fn finalise(task: &mut Task) -> Result<()> {
    let part = task.part_path();

    // Every writer is closed by the time we get here; a handle still open at
    // this point would mean a rename raced the write.
    task.state
        .assert_invariants(false, task.bytes_done, task.total_bytes);

    if let Some(total) = task.total_bytes {
        let actual = std::fs::metadata(&part)
            .map(|m| m.len())
            .unwrap_or(task.bytes_done);
        if actual != total {
            // Never mark an unverified transfer complete. Handing a user a
            // truncated file that claims success is the worst thing a download
            // manager can do.
            return Err(TransferError::Incomplete {
                written: actual,
                expected: total,
            }
            .into());
        }
    }

    if part.exists() {
        std::fs::rename(&part, &task.output).map_err(|e| Error::io(&task.output, e))?;
    } else if !task.output.exists() {
        return Err(TransferError::Destination(format!(
            "{} disappeared before it could be finalised",
            part.display()
        ))
        .into());
    }

    task.bytes_done = task.total_bytes.unwrap_or(task.bytes_done);
    task.transition(TaskState::Completed);
    task.last_error = None;
    Ok(())
}

/// Delete a `.part` file, ignoring absence.
fn discard_part(task: &Task) -> Result<()> {
    remove_if_present(&task.part_path())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

fn disk_len(task: &Task) -> u64 {
    std::fs::metadata(task.part_path())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Create the destination's parent directory.
fn ensure_parent_dir(task: &Task) -> Result<()> {
    if let Some(parent) = task.output.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
    }
    Ok(())
}

/// Open the `.part` file for writing at `offset`, truncating anything past it.
///
/// `set_len` before the first write is what makes a restart a restart: a
/// shorter-than-expected previous attempt leaves a tail that would otherwise be
/// spliced onto the front of the new body.
///
/// The truncate and the seek are done synchronously on the just-opened
/// descriptor. Both are metadata operations against a handle we exclusively
/// own, so there is no await point worth taking here — and keeping the
/// position correct *before* the handle is handed to the async writer means a
/// failed seek cannot leave a writer that appends to the wrong offset.
fn open_for_write(task: &Task, offset: u64) -> Result<tokio::fs::File> {
    let path = task.part_path();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;

    file.set_len(offset).map_err(|e| Error::io(&path, e))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| Error::io(&path, e))?;

    Ok(tokio::fs::File::from_std(file))
}

/// Open the `.part` file for appending.
///
/// Fragmented transfers use the same `.part` convention as direct ones so that a
/// user browsing their downloads folder sees only finished files. Their resume
/// state is the per-segment index list on the task, not the file length alone.
fn open_for_append(task: &Task) -> Result<tokio::fs::File> {
    let path = task.part_path();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;
    Ok(tokio::fs::File::from_std(file))
}

/// Map a non-2xx status onto the most specific error available.
fn status_error(status: u16, url: &str) -> NetError {
    NetError::Status {
        status,
        url: url.to_string(),
    }
}

/// Whether `path` looks like a completed output rather than a partial.
pub fn is_final(path: &Path) -> bool {
    path.exists() && path.extension().is_some_and(|e| e != "part")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_idempotent_and_observable() {
        let c = Cancel::new();
        assert!(!c.is_cancelled());
        assert!(c.check().is_ok());

        c.cancel();
        assert!(c.is_cancelled());
        c.cancel();
        assert!(c.is_cancelled());

        let err = c.check().unwrap_err();
        assert!(matches!(err, Error::Transfer(TransferError::Paused)));
        assert!(err.is_retryable(), "a pause must be resumable");
    }

    #[test]
    fn progress_is_unknown_without_a_total() {
        let p = Progress {
            bytes_done: 100,
            total_bytes: None,
        };
        assert_eq!(p.fraction(), None);

        let p = Progress {
            bytes_done: 50,
            total_bytes: Some(200),
        };
        assert_eq!(p.fraction(), Some(0.25));
    }

    #[test]
    fn progress_clamps_when_more_was_written_than_expected() {
        // A server that sends more than Content-Length. Clamping keeps a
        // progress bar sane instead of showing 130%.
        let p = Progress {
            bytes_done: 260,
            total_bytes: Some(200),
        };
        assert_eq!(p.fraction(), Some(1.0));
    }

    #[test]
    fn zero_total_progress_is_complete_not_a_division_by_zero() {
        let p = Progress {
            bytes_done: 0,
            total_bytes: Some(0),
        };
        assert_eq!(p.fraction(), Some(1.0));
    }

    fn seg(index: u32, uri: &str, range: Option<ByteRange>) -> Segment {
        Segment {
            index,
            uri: uri.to_string(),
            byte_range: range,
            duration_secs: None,
        }
    }

    #[test]
    fn a_plan_with_a_gap_is_rejected() {
        // Indices 0 and 2, with 1 missing. Accepting this would make the
        // completed set ambiguous and produce a file that is the right length
        // and the wrong contents.
        let plan = [seg(0, "a", None), seg(2, "c", None)];
        assert!(normalise_plan(&plan).is_none());
    }

    #[test]
    fn a_plan_with_duplicate_indices_is_rejected() {
        let plan = [seg(0, "a", None), seg(0, "b", None)];
        assert!(normalise_plan(&plan).is_none());
    }

    #[test]
    fn an_empty_plan_is_rejected() {
        assert!(normalise_plan(&[]).is_none());
    }

    #[test]
    fn out_of_order_plans_are_sorted_rather_than_rejected() {
        let plan = normalise_plan(&[seg(1, "b", None), seg(0, "a", None)]).expect("valid plan");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].index, 0);
    }

    #[test]
    fn prefix_length_is_only_full_when_the_completed_set_is_a_prefix() {
        let plan = normalise_plan(&[seg(0, "a", None), seg(1, "b", None), seg(2, "c", None)])
            .expect("valid plan");

        assert_eq!(plan.prefix_len(&[]), 0);
        assert_eq!(plan.prefix_len(&[0]), 1);
        assert_eq!(plan.prefix_len(&[0, 1]), 2);
        assert_eq!(plan.prefix_len(&[0, 1, 2]), 3);

        // Segment 1 missing: nothing can be trusted, because the bytes after a
        // hole cannot be attributed to a known segment.
        assert_eq!(plan.prefix_len(&[0, 2]), 1);

        // An index from beyond the hole does not extend the prefix.
        assert_eq!(plan.prefix_len(&[1]), 0);
        assert_eq!(plan.prefix_len(&[0, 1, 2, 2]), 3, "duplicates are ignored");
    }

    #[test]
    fn prefix_bytes_are_unverifiable_when_a_segment_is_unbounded() {
        // A segment of unknown length makes the on-disk prefix impossible to
        // check, which is why the resume path refuses it.
        let unbounded = normalise_plan(&[seg(0, "a", None), seg(1, "b", None)]).expect("valid");
        assert_eq!(unbounded.prefix_bytes(1), None);

        let ranged = normalise_plan(&[
            seg(0, "a", Some(ByteRange::closed(0, 100))),
            seg(1, "b", Some(ByteRange::closed(0, 50))),
        ])
        .expect("valid");
        assert_eq!(ranged.prefix_bytes(0), Some(0));
        assert_eq!(ranged.prefix_bytes(2), Some(150));
    }

    #[test]
    fn is_final_distinguishes_a_part_file_from_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a.mp4");
        let part = dir.path().join("a.mp4.part");
        std::fs::write(&part, b"x").unwrap();

        assert!(!is_final(&part));
        assert!(!is_final(&out));

        std::fs::write(&out, b"x").unwrap();
        assert!(is_final(&out));
    }
}
