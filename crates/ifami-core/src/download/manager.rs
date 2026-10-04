//! The queue: what to download, what is running, and what to tell the user.
//!
//! This is the only stateful part of the engine, and it is stateful in exactly
//! one way: a [`QueueFile`] on disk plus a set of in-flight transfers. There is
//! no global singleton and no background thread that outlives the manager.
//!
//! # Recovery
//!
//! A task recorded as [`TaskState::Downloading`] at load time is a task whose
//! writer died with the previous process. It is moved to [`TaskState::Paused`]
//! unconditionally, because nothing can be holding the file any more. This is
//! the single most important line in the module: silently leaving a task marked
//! as running would make the UI claim progress that no longer exists.
//!
//! # Concurrency
//!
//! Concurrency is across tasks, never within one. See the note in
//! [`crate::download::engine`] for why parallel segment writes are not worth
//! the corruption risk.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::download::engine::{self, Cancel, Progress};
use crate::download::store::{QueueFile, Store};
use crate::download::task::{Task, TaskState};
use crate::error::{Error, Result};
use crate::model::{Format, Media};
use crate::naming::{self, TemplateVars};
use crate::net::HttpClient;
use crate::resolve::{self, ResolverSet};

/// Downloads running at once by default.
///
/// Three is a deliberate choice, not a benchmark result: enough to hide one
/// slow origin behind the others, few enough that a gigabit line is not turned
/// into a server-side throttle that slows everything down.
pub const DEFAULT_CONCURRENCY: usize = 3;

/// Capacity of the event channel.
///
/// A slow UI misses events rather than stalling the engine. The UI re-reads the
/// queue on every wake-up, so a dropped event costs a redraw, not correctness.
const EVENT_BUFFER: usize = 256;

/// Something the UI should react to.
///
/// Every variant carries enough state to render without re-reading the queue,
/// but a UI is expected to treat the queue as authoritative and this stream as a
/// hint that it changed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum QueueEvent {
    /// A task was inserted or updated.
    ///
    /// Boxed because a [`Task`] is two orders of magnitude larger than any other
    /// variant, and this enum crosses a channel on every progress tick: without
    /// the box, a one-word notice would carry a `Task`'s worth of bytes. The
    /// indirection is the whole point of the lint.
    Updated(Box<Task>),
    /// A task was removed.
    Removed {
        /// Identifier of the removed task.
        id: String,
    },
    /// Something the user should be told that is not a task.
    ///
    /// Used for a lossy queue load and for the "this source is out of scope"
    /// answers, both of which would otherwise be invisible.
    Notice(String),
}

/// Configuration for a [`Manager`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerOptions {
    /// Where new downloads are written.
    pub destination: PathBuf,

    /// Filename template. See [`crate::naming::SUPPORTED_TOKENS`].
    pub template: String,

    /// Maximum simultaneous transfers.
    pub concurrency: usize,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            destination: default_destination(),
            template: "{title}.{container}".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

/// The default download directory.
///
/// `~/Downloads` when it exists, because that is where a person looks. Never the
/// current directory: a download manager that scatters files into whatever shell
/// launched it is a download manager nobody trusts.
pub fn default_destination() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);

    match home {
        Some(home) => home.join("Downloads"),
        // No home directory at all is pathological, but returning a path is
        // better than refusing to start.
        None => std::env::temp_dir().join("ifami-downloads"),
    }
}

/// Owns the queue and the transfers running against it.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    client: Arc<dyn HttpClient>,
    resolvers: ResolverSet,
    store: Store,
    options: Mutex<ManagerOptions>,
    tasks: Mutex<BTreeMap<String, Task>>,
    running: Mutex<HashMap<String, Running>>,
    events: broadcast::Sender<QueueEvent>,
    recovery_note: Mutex<Option<String>>,
}

struct Running {
    cancel: Cancel,
    handle: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("tasks", &self.tasks().len())
            .field("running", &self.running_count())
            .field("store", &self.inner.store.path())
            .finish()
    }
}

impl Manager {
    /// Open a manager over an existing queue file.
    ///
    /// A missing file is an empty queue. A damaged file is salvaged where
    /// possible and the loss is recorded in [`Manager::recovery_note`]; it never
    /// prevents the manager from starting, because a download manager that will
    /// not launch is worse than one that lost a task.
    pub fn load(store: Store, client: Arc<dyn HttpClient>) -> Result<Self> {
        Self::with_options(store, client, ManagerOptions::default())
    }

    /// Open a manager with explicit options.
    pub fn with_options(
        store: Store,
        client: Arc<dyn HttpClient>,
        options: ManagerOptions,
    ) -> Result<Self> {
        let queue = store.load()?;
        let note = store.last_recovery_note();

        let mut tasks = BTreeMap::new();
        for mut task in queue.tasks.into_values() {
            // Nothing can be holding the writer after a process restart.
            if task.state == TaskState::Downloading {
                task.state = TaskState::Paused;
                task.last_error = None;
            }
            task.reconcile();
            tasks.insert(task.id.clone(), task);
        }

        let (events, _) = broadcast::channel(EVENT_BUFFER);

        Ok(Self {
            inner: Arc::new(Inner {
                client,
                resolvers: ResolverSet::with_defaults(),
                store,
                options: Mutex::new(options),
                tasks: Mutex::new(tasks),
                running: Mutex::new(HashMap::new()),
                events,
                recovery_note: Mutex::new(note),
            }),
        })
    }

    /// Replace the options. Does not affect tasks already queued.
    pub fn set_options(&self, options: ManagerOptions) {
        *lock(&self.inner.options) = options;
    }

    /// Current options.
    pub fn options(&self) -> ManagerOptions {
        lock(&self.inner.options).clone()
    }

    /// Note describing a lossy load, if the last load had to recover.
    pub fn recovery_note(&self) -> Option<String> {
        lock(&self.inner.recovery_note).clone()
    }

    /// Every task, in id order.
    pub fn tasks(&self) -> Vec<Task> {
        lock(&self.inner.tasks).values().cloned().collect()
    }

    /// Look up a task.
    pub fn get(&self, id: &str) -> Option<Task> {
        lock(&self.inner.tasks).get(id).cloned()
    }

    /// Number of tasks currently transferring.
    pub fn running_count(&self) -> usize {
        lock(&self.inner.running).len()
    }

    /// Subscribe to queue changes.
    pub fn subscribe(&self) -> broadcast::Receiver<QueueEvent> {
        self.inner.events.subscribe()
    }

    /// Resolve a URL and report what is available, without downloading.
    pub async fn probe_url(&self, raw: &str) -> Result<Media> {
        let url = parse_url(raw)?;
        self.inner
            .resolvers
            .resolve(self.inner.client.as_ref(), &url)
            .await
            .map_err(Error::Resolve)
    }

    /// Resolve `raw`, choose the best self-contained format, and queue it.
    ///
    /// "Self-contained" means playable without a muxing step. A video-only
    /// rendition is recorded in the returned [`Media`] but is not queued by
    /// default, because handing the user a file that will not play is worse than
    /// handing them nothing. See `docs/SCOPE.md` on muxer dependencies.
    pub async fn add_url(&self, raw: &str) -> Result<Task> {
        let media = self.probe_url(raw).await?;

        let Some(format) = media.best_standalone().cloned() else {
            return Err(Error::Resolve(crate::error::ResolveError::NoMedia {
                url: media.source_url.clone(),
            }));
        };

        self.enqueue(&media, format).await
    }

    /// Queue an explicit format choice from a resolved [`Media`].
    pub async fn enqueue(&self, media: &Media, format: Format) -> Result<Task> {
        let format = resolve::expand_segmented(self.inner.client.as_ref(), &format)
            .await
            .map_err(Error::Resolve)?;

        let options = self.options();
        let name = self.output_name(media, &format, &options);
        let output = unique_path(&options.destination.join(name));

        let task_id = crate::digest(8, &format!("{}\u{1}{}", media.source_url, format.id));

        let mut task = Task::new(
            task_id,
            format.url.clone(),
            output,
            format.container.extension(),
        );
        task.format_id = format.id.clone();
        task.total_bytes = format.total_bytes;
        task.segments = format.segments.clone();
        if task.segments.is_some() {
            task.completed_segments.clear();
        }

        lock(&self.inner.tasks).insert(task.id.clone(), task.clone());
        self.emit(QueueEvent::Updated(Box::new(task.clone())));
        self.persist()?;
        self.pump();

        Ok(task)
    }

    /// Move a task back to `Queued` and start it if a slot is free.
    pub fn resume(&self, id: &str) -> Result<()> {
        {
            let mut tasks = lock(&self.inner.tasks);
            let task = tasks.get_mut(id).ok_or_else(|| missing_task(id))?;
            if task.state == TaskState::Completed {
                return Err(Error::InvalidArgument(format!(
                    "{id} has already completed and cannot be resumed"
                )));
            }
            if task.state == TaskState::Downloading {
                return Ok(());
            }
            task.reconcile();

            // Checked, not assumed. A silent no-op here is the worst possible
            // failure: the user presses retry, the UI says nothing, and the task
            // sits in the queue forever with no worker attached. If a state
            // reaches the table without a route to `Queued`, that is a bug in
            // `TaskState::allowed_transitions` and it should surface here.
            if !task.transition(TaskState::Queued) {
                return Err(Error::InvalidArgument(format!(
                    "{id} is {:?} and cannot be requeued",
                    task.state
                )));
            }
            let snapshot = task.clone();
            drop(tasks);
            self.emit(QueueEvent::Updated(Box::new(snapshot)));
        }
        self.persist()?;
        self.pump();
        Ok(())
    }

    /// Ask a running task to stop at its next flush.
    ///
    /// Returns immediately. The task reaches [`TaskState::Paused`] once the
    /// in-flight chunk has been written and flushed, which is the only point at
    /// which a pause is safe.
    pub fn pause(&self, id: &str) -> Result<()> {
        // A worker that exists is authoritative, whatever the projection says:
        // the queue's copy may be a few milliseconds behind, and cancelling the
        // wrong thing here means the user's pause is ignored.
        if let Some(running) = lock(&self.inner.running).get(id) {
            running.cancel.cancel();
            return Ok(());
        }

        let task = self.get(id).ok_or_else(|| missing_task(id))?;
        match task.state {
            TaskState::Queued => {
                let mut tasks = lock(&self.inner.tasks);
                if let Some(t) = tasks.get_mut(id) {
                    t.transition(TaskState::Paused);
                }
            }
            TaskState::Paused | TaskState::Retrying | TaskState::Failed | TaskState::Completed => {
                return Err(Error::InvalidArgument(format!(
                    "{id} is {:?} and cannot be paused",
                    task.state
                )))
            }
            TaskState::Downloading => {}
        }
        Ok(())
    }

    /// Re-queue every task that can make progress, leaving `Completed` alone.
    pub fn resume_all(&self) -> Result<usize> {
        let ids: Vec<String> = lock(&self.inner.tasks)
            .values()
            .filter(|t| t.state.is_resumable())
            .map(|t| t.id.clone())
            .collect();

        let mut resumed = 0;
        for id in ids {
            if self.resume(&id).is_ok() {
                resumed += 1;
            }
        }
        Ok(resumed)
    }

    /// Stop every running task.
    pub fn pause_all(&self) -> usize {
        let running = lock(&self.inner.running);
        let count = running.len();
        for r in running.values() {
            r.cancel.cancel();
        }
        count
    }

    /// Wait until every running transfer has finished or paused.
    ///
    /// Used by the CLI and by tests. Returns immediately if nothing is running.
    ///
    /// A worker that *panicked* is reported rather than dropped. Swallowing the
    /// join error would leave the task parked in `Downloading` forever with no
    /// error attached - a queue that looks busy forever and explains nothing.
    pub async fn wait_idle(&self) {
        loop {
            let handles: Vec<(String, tokio::task::JoinHandle<()>)> = {
                let mut running = lock(&self.inner.running);
                if running.is_empty() {
                    return;
                }
                running.drain().map(|(id, r)| (id, r.handle)).collect()
            };
            for (id, handle) in handles {
                if let Err(join) = handle.await {
                    self.emit(QueueEvent::Notice(format!(
                        "the transfer of {id} stopped unexpectedly: {join}"
                    )));
                }
            }
        }
    }

    /// Remove a task, optionally deleting its partial and output files.
    ///
    /// A running task is cancelled first; removing a task whose writer is still
    /// open would leave bytes appearing in a file nobody is tracking.
    pub async fn remove(&self, id: &str, delete_files: bool) -> Result<Option<Task>> {
        // Taken out and the lock dropped before the await. Holding the guard
        // across `handle.await` would block every other thread that wants the
        // running set for as long as the transfer takes to notice the cancel.
        let running = lock(&self.inner.running).remove(id);
        if let Some(running) = running {
            running.cancel.cancel();
            let _ = running.handle.await;
        }

        let removed = lock(&self.inner.tasks).remove(id);

        if delete_files {
            if let Some(task) = &removed {
                for path in [task.part_path(), task.output.clone()] {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(Error::io(&path, e)),
                    }
                }
            }
        }

        if removed.is_some() {
            self.emit(QueueEvent::Removed { id: id.to_string() });
            self.persist()?;
            self.pump();
        }
        Ok(removed)
    }

    /// Write the queue to disk.
    pub fn persist(&self) -> Result<()> {
        let queue = QueueFile {
            version: crate::download::store::STORE_VERSION,
            tasks: lock(&self.inner.tasks).clone(),
        };
        self.inner.store.save(&queue)
    }

    /// Resolve a user-supplied URL string.
    fn output_name(&self, media: &Media, format: &Format, options: &ManagerOptions) -> String {
        let quality = format
            .quality
            .and_then(|q| q.height)
            .map(|h| format!("{h}p"));
        let vars = TemplateVars {
            title: if media.title.is_empty() {
                "media".to_string()
            } else {
                media.title.clone()
            },
            uploader: None,
            ext: None,
            id: Some(media.id.clone()),
            container: Some(format.container.extension().to_string()),
            date: None,
            quality,
        };
        naming::render_lossy(&options.template, &vars, naming::DEFAULT_FILENAME_BUDGET)
    }

    fn emit(&self, event: QueueEvent) {
        // An error here only means nobody is listening.
        let _ = self.inner.events.send(event);
    }

    /// Start as many queued tasks as the concurrency limit allows.
    fn pump(&self) {
        let queued = {
            let running = lock(&self.inner.running);
            let limit = lock(&self.inner.options).concurrency.max(1);
            let slots = limit.saturating_sub(running.len());
            if slots == 0 {
                return;
            }
            lock(&self.inner.tasks)
                .values()
                .filter(|t| t.state == TaskState::Queued)
                .take(slots)
                .cloned()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };

        for id in queued {
            self.spawn(id);
        }
    }

    /// Hand a task to a worker.
    ///
    /// The task is marked `Downloading` before the worker exists rather than
    /// after, so a UI that races the first progress event never renders a stale
    /// `Queued` while bytes are already arriving.
    fn spawn(&self, id: String) {
        let Some(mut task) = self.get(&id) else {
            return;
        };

        if lock(&self.inner.running).contains_key(&id) {
            return;
        }
        if task.state != TaskState::Queued && !task.state.is_resumable() {
            return;
        }

        task.reconcile();
        if !task.transition(TaskState::Downloading) {
            // `Completed` is terminal and `Downloading` means a worker already
            // owns this file. Spawning anyway would put two writers on one path.
            return;
        }
        lock(&self.inner.tasks).insert(id.clone(), task.clone());
        self.emit(QueueEvent::Updated(Box::new(task.clone())));

        let cancel = Cancel::new();
        let worker = self.clone();
        let worker_cancel = cancel.clone();
        let worker_id = id.clone();

        let handle = tokio::spawn(async move {
            // The worker owns its own copy and publishes progress; the queue's
            // copy is a projection, not the source of truth for the transfer.
            let mut task = task;
            let outcome = engine::run(
                worker.inner.client.as_ref(),
                &mut task,
                &worker_cancel,
                |progress| worker.report(&worker_id, progress),
            )
            .await;

            if let Err(e) = outcome {
                if let Some(t) = worker.get(&worker_id) {
                    worker.emit(QueueEvent::Notice(format!("{}: {e}", t.output.display())));
                }
            }
            worker.task_finished(&worker_id, task);
        });

        lock(&self.inner.running).insert(id, Running { cancel, handle });
    }

    /// Publish progress from a worker without taking the worker's task lock.
    fn report(&self, id: &str, progress: Progress) {
        let updated = {
            let mut tasks = lock(&self.inner.tasks);
            tasks.get_mut(id).map(|t| {
                t.bytes_done = progress.bytes_done;
                if let Some(total) = progress.total_bytes {
                    t.total_bytes = Some(total);
                }
                t.clone()
            })
        };
        if let Some(task) = updated {
            self.emit(QueueEvent::Updated(Box::new(task)));
        }
    }

    /// Record a finished transfer, persist, and start whatever is next.
    fn task_finished(&self, id: &str, task: Task) {
        lock(&self.inner.running).remove(id);
        lock(&self.inner.tasks).insert(id.to_string(), task.clone());
        self.emit(QueueEvent::Updated(Box::new(task)));

        if let Err(e) = self.persist() {
            self.emit(QueueEvent::Notice(format!("could not save the queue: {e}")));
        }

        self.pump();
    }

    /// Park an error on a task so the UI can show it, for a task whose engine
    /// run is not the source of the failure (resolution, planning).
    pub fn fail(&self, id: &str, error: &Error) {
        let updated = {
            let mut tasks = lock(&self.inner.tasks);
            tasks.get_mut(id).and_then(|t| {
                t.last_error = Some(error.to_string());
                t.transition(TaskState::Failed).then(|| t.clone())
            })
        };
        if let Some(task) = updated {
            self.emit(QueueEvent::Updated(Box::new(task)));
        }
    }
}

/// Parse a URL the way every entry point should.
pub fn parse_url(raw: &str) -> Result<url::Url> {
    let trimmed = raw.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    url::Url::parse(&candidate).map_err(|e| Error::InvalidArgument(format!("{raw:?}: {e}")))
}

/// A path that does not already exist, by appending ` (2)`, ` (3)`, ...
///
/// Never overwrite: a download manager that silently replaces a file the user
/// kept is unforgivable, and the alternative of failing is worse.
pub fn unique_path(desired: &Path) -> PathBuf {
    if !desired.exists() {
        return desired.to_path_buf();
    }

    let parent = desired.parent().unwrap_or_else(|| Path::new(""));
    let stem = desired
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = desired.extension().map(|s| s.to_string_lossy().to_string());

    for n in 2..10_000u32 {
        let name = match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }

    // 10 000 collisions in one directory is pathological. Refuse to guess.
    desired.to_path_buf()
}

fn missing_task(id: &str) -> Error {
    Error::InvalidArgument(format!("no task with id {id:?}"))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panic in one transfer must not take down the whole queue, so poisoning
    // is recovered from rather than propagated.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::client::{BodyStream, Headers, HttpRequest, RawResponse};
    use bytes::Bytes;
    use futures_util::stream;

    /// One scripted route: status, body, and response headers.
    ///
    /// Named rather than written inline because "a route is these three things"
    /// is a fact worth stating once, and the inline tuple is at the edge of what
    /// clippy will read as intentional.
    type ScriptedRoute = (u16, Vec<u8>, Vec<(&'static str, String)>);

    /// A client that answers from a fixed routing table, and counts requests so
    /// tests can prove a probe did not turn into a full download.
    #[derive(Default)]
    struct Fixture {
        routes: Mutex<HashMap<String, ScriptedRoute>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl Fixture {
        fn serve(&self, url: &str, status: u16, body: Vec<u8>, headers: &[(&'static str, String)]) {
            self.routes
                .lock()
                .unwrap()
                .insert(url.to_string(), (status, body, headers.to_vec()));
        }

        /// Every request issued so far, in order.
        ///
        /// The list rather than a count, because "how many requests" is only
        /// ever interesting next to "and what were they" -- a test that asserts
        /// a number without looking at it will happily lock in the wrong shape.
        fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl HttpClient for Fixture {
        async fn execute(
            &self,
            req: HttpRequest,
        ) -> std::result::Result<RawResponse, crate::error::NetError> {
            self.requests.lock().unwrap().push(req.clone());
            let routes = self.routes.lock().unwrap();
            let (status, body, headers) =
                routes
                    .get(&req.url)
                    .cloned()
                    .unwrap_or((404, Vec::new(), Vec::new()));

            let honours_range = headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Accept-Ranges"));

            // Serve a 206 for the ranged requests when the fixture says it is
            // range-capable, which is what a real CDN does.
            if honours_range {
                if let Some(range) = req.range {
                    let total = body.len() as u64;
                    let start = range.start;
                    let end = range.end.map(|e| e.min(total)).unwrap_or(total);
                    let slice = body[start as usize..end as usize].to_vec();
                    let mut all: Vec<(&'static str, String)> = headers.clone();
                    all.push((
                        "Content-Range",
                        format!("bytes {start}-{}/{total}", end.saturating_sub(1)),
                    ));
                    all.push(("Content-Length", slice.len().to_string()));
                    let chunks: Vec<std::result::Result<Bytes, crate::error::NetError>> = slice
                        .chunks(64)
                        .map(|c| Ok(Bytes::copy_from_slice(c)))
                        .collect();
                    return Ok(RawResponse {
                        status: 206,
                        final_url: req.url.clone(),
                        headers: Headers::new(all.into_iter().map(|(k, v)| (k.to_string(), v))),
                        body: Box::pin(stream::iter(chunks)) as BodyStream,
                    });
                }
            }

            let mut all = headers.clone();
            all.push(("Content-Length", body.len().to_string()));
            let chunks: Vec<std::result::Result<Bytes, crate::error::NetError>> = body
                .chunks(64)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect();
            Ok(RawResponse {
                status,
                final_url: req.url.clone(),
                headers: Headers::new(all.into_iter().map(|(k, v)| (k.to_string(), v))),
                body: Box::pin(stream::iter(chunks)) as BodyStream,
            })
        }
    }

    fn manager(dir: &Path, fixture: Arc<Fixture>) -> Manager {
        Manager::with_options(
            Store::new(dir.join("queue.json")),
            fixture,
            ManagerOptions {
                destination: dir.join("out"),
                template: "{title}.{container}".into(),
                concurrency: 2,
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_direct_file_downloads_and_is_finalised() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        fixture.serve(
            "https://cdn.test/clip.mp4",
            200,
            payload.clone(),
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), fixture);
        let task = m.add_url("https://cdn.test/clip.mp4").await.unwrap();
        m.wait_idle().await;

        let done = m.get(&task.id).expect("task survives");
        assert_eq!(done.state, TaskState::Completed, "{:?}", done.last_error);
        assert_eq!(std::fs::read(&done.output).unwrap(), payload);
        assert!(!done.part_path().exists(), ".part must be renamed away");
    }

    #[tokio::test]
    async fn a_fresh_transfer_probes_capabilities_exactly_once() {
        // Locks in the shape of a cold transfer: one bounded capability probe,
        // then the transfer itself. Worth pinning because the cost here is
        // invisible in every other assertion -- an engine that re-probed after
        // every chunk, or retried the probe on failure, would still write
        // correct bytes and would only show up as a mysteriously chatty
        // connection to whoever is hosting the file.
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        fixture.serve(
            "https://cdn.test/once.mp4",
            200,
            vec![7u8; 4096],
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), Arc::clone(&fixture));
        let task = m.add_url("https://cdn.test/once.mp4").await.unwrap();
        m.wait_idle().await;

        assert_eq!(
            m.get(&task.id).expect("task survives").state,
            TaskState::Completed
        );

        let requests = fixture.requests();
        assert_eq!(
            requests.len(),
            2,
            "expected one capability probe and one transfer, saw {:?}",
            requests.iter().map(|r| r.range).collect::<Vec<_>>()
        );
        assert!(
            requests[0].range.is_some(),
            "the probe asks for a little, not the whole object"
        );
        assert!(
            requests[1].range.is_none(),
            "the transfer then starts from the beginning"
        );
    }

    #[tokio::test]
    async fn a_resumed_transfer_asks_for_the_rest_not_the_whole_file_again() {
        // The other half of resuming. `a_resumed_transfer_produces_the_whole_file`
        // already proves the bytes come out right; this proves we did not get
        // there by silently downloading the entire object a second time, which
        // produces an identical file, costs the user twice the bandwidth, and
        // is invisible to every other assertion in this file.
        //
        // Set up by hand rather than by racing a live transfer: with 70 000 real
        // bytes on disk the correct request is fully determined, and the only
        // thing that varies between a correct engine and a wasteful one is where
        // this range starts.
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fixture.serve(
            "https://cdn.test/ranged.mp4",
            200,
            payload.clone(),
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), Arc::clone(&fixture));
        let mut task = Task::new(
            "t1",
            "https://cdn.test/ranged.mp4",
            dir.path().join("out/ranged.mp4"),
            "mp4",
        );
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(task.part_path(), &payload[..70_000]).unwrap();
        task.total_bytes = Some(payload.len() as u64);
        task.bytes_done = 70_000;
        task.state = TaskState::Paused;
        lock(&m.inner.tasks).insert(task.id.clone(), task);
        m.persist().unwrap();

        m.resume("t1").unwrap();
        m.wait_idle().await;

        let sent = fixture.requests();
        assert_eq!(sent.len(), 1, "one request finishes the file");
        let range = sent[0].range.expect("a resumed transfer must be ranged");
        assert_eq!(
            range.start, 70_000,
            "must ask from the first missing byte, not from the start of the file"
        );
        assert!(
            range.end.is_none() || range.end == Some(payload.len() as u64 - 1),
            "and must not ask for more than the file has left, got {range:?}"
        );
    }

    #[tokio::test]
    async fn a_queued_task_recovers_as_paused_not_downloading() {
        // The crash case: a queue file claiming a transfer is running. Nothing
        // can be holding that writer, so claiming otherwise would show the user
        // progress that does not exist.
        let dir = tempfile::tempdir().unwrap();
        let mut q = QueueFile::new();
        q.upsert(Task::new("t1", "https://x.invalid/a.mp4", "a.mp4", "mp4"));
        let mut task = q.get("t1").unwrap().clone();
        task.state = TaskState::Downloading;
        task.bytes_done = 999;
        q.upsert(task);
        Store::new(dir.path().join("queue.json")).save(&q).unwrap();

        let m = manager(dir.path(), Arc::new(Fixture::default()));
        let recovered = m.get("t1").expect("task survived");
        assert_eq!(recovered.state, TaskState::Paused);
        assert_eq!(recovered.bytes_done, 0, "disk wins over the recorded hint");
    }

    #[tokio::test]
    async fn pause_stops_a_transfer_and_keeps_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        // 8 MiB, served in 64 KiB chunks, so there is time to pause.
        let payload: Vec<u8> = vec![7u8; 8 * 1024 * 1024];
        fixture.serve(
            "https://cdn.test/big.mp4",
            200,
            payload,
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), fixture);
        let task = m.add_url("https://cdn.test/big.mp4").await.unwrap();
        m.pause(&task.id).unwrap();
        m.wait_idle().await;

        let paused = m.get(&task.id).expect("task survives");
        assert_eq!(paused.state, TaskState::Paused);
        assert!(
            paused.part_path().exists(),
            "a paused transfer must keep its partial data"
        );
        assert!(!paused.output.exists(), "no output before completion");
    }

    #[tokio::test]
    async fn a_resumed_transfer_produces_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fixture.serve(
            "https://cdn.test/big.mp4",
            200,
            payload.clone(),
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), fixture);

        // Set the scenario up by hand rather than by racing a live transfer: a
        // paused task with 70 000 real bytes on disk and a queue that
        // under-reports its own progress.
        let mut task = Task::new(
            "t1",
            "https://cdn.test/big.mp4",
            dir.path().join("out/big.mp4"),
            "mp4",
        );
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(task.part_path(), &payload[..70_000]).unwrap();
        task.total_bytes = Some(payload.len() as u64);
        task.bytes_done = 12_345; // deliberately wrong; disk wins
        task.state = TaskState::Paused;
        lock(&m.inner.tasks).insert(task.id.clone(), task);
        m.persist().unwrap();

        m.resume("t1").unwrap();
        m.wait_idle().await;

        let done = m.get("t1").unwrap();
        assert_eq!(done.state, TaskState::Completed, "{:?}", done.last_error);
        assert_eq!(
            std::fs::read(&done.output).unwrap(),
            payload,
            "resume must not duplicate or lose bytes"
        );
        assert!(!done.part_path().exists());
    }

    #[tokio::test]
    async fn a_queue_that_over_reports_progress_resumes_from_disk() {
        // The mirror image of the above: the queue claims 180 000 bytes but only
        // 70 000 exist. Trusting the queue would leave a 110 000-byte hole in the
        // middle of a file that then reports success.
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        fixture.serve(
            "https://cdn.test/h.mp4",
            200,
            payload.clone(),
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), fixture);
        let mut task = Task::new(
            "t2",
            "https://cdn.test/h.mp4",
            dir.path().join("out/h.mp4"),
            "mp4",
        );
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(task.part_path(), &payload[..70_000]).unwrap();
        task.total_bytes = Some(payload.len() as u64);
        task.bytes_done = 180_000;
        task.state = TaskState::Paused;
        lock(&m.inner.tasks).insert(task.id.clone(), task);

        m.resume("t2").unwrap();
        m.wait_idle().await;

        let done = m.get("t2").unwrap();
        assert_eq!(done.state, TaskState::Completed, "{:?}", done.last_error);
        assert_eq!(std::fs::read(&done.output).unwrap(), payload);
    }

    #[tokio::test]
    async fn a_404_fails_the_task_rather_than_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let m = manager(dir.path(), fixture);

        let err = m.add_url("https://cdn.test/missing.mp4").await.unwrap_err();
        assert!(matches!(err, Error::Resolve(_)), "{err}");
    }

    #[tokio::test]
    async fn a_drm_source_is_refused_with_a_clear_answer() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        fixture.serve(
            "https://cdn.test/movie.mp4",
            200,
            br#"<html data-keysystem="com.widevine.alpha"></html>"#.to_vec(),
            &[("Content-Type", "text/html".into())],
        );

        let m = manager(dir.path(), fixture);
        let err = m.add_url("https://cdn.test/movie.mp4").await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::Resolve(crate::error::ResolveError::DrmProtected)
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn removing_a_task_deletes_its_files_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let m = manager(dir.path(), fixture);

        let task = m.add_url("https://cdn.test/nope.mp4").await.ok();
        assert!(task.is_none());

        // Build one by hand so we have something on disk to clean up. The output path
        // has to be absolute: `Task::new` stores it verbatim, so a relative one
        // would put the `.part` file next to the test binary instead of in the
        // temporary directory.
        let mut t = Task::new(
            "t9",
            "https://cdn.test/x.mp4",
            dir.path().join("out/x.mp4"),
            "mp4",
        );
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(t.part_path(), b"partial").unwrap();
        t.state = TaskState::Paused;
        lock(&m.inner.tasks).insert(t.id.clone(), t);

        let removed = m.remove("t9", true).await.unwrap().expect("removed");
        assert!(!removed.part_path().exists());
        assert!(m.get("t9").is_none());
        assert!(m.persist().is_ok());
    }

    #[tokio::test]
    async fn events_report_additions_and_removals() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        fixture.serve(
            "https://cdn.test/clip.mp4",
            200,
            vec![1u8; 128],
            &[
                ("Content-Type", "video/mp4".into()),
                ("Accept-Ranges", "bytes".into()),
            ],
        );

        let m = manager(dir.path(), fixture);
        let mut events = m.subscribe();
        let task = m.add_url("https://cdn.test/clip.mp4").await.unwrap();
        m.wait_idle().await;

        let mut saw_completed = false;
        while let Ok(event) = events.try_recv() {
            if let QueueEvent::Updated(t) = event {
                if t.id == task.id && t.state == TaskState::Completed {
                    saw_completed = true;
                }
            }
        }
        assert!(saw_completed, "the queue never reported completion");

        m.remove(&task.id, false).await.unwrap();
        let mut saw_removed = false;
        while let Ok(event) = events.try_recv() {
            if matches!(event, QueueEvent::Removed { .. }) {
                saw_removed = true;
            }
        }
        assert!(saw_removed, "the queue never reported the removal");
    }

    #[test]
    fn unique_paths_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let desired = dir.path().join("clip.mp4");

        let first = unique_path(&desired);
        assert_eq!(first, desired);
        std::fs::write(&first, b"x").unwrap();

        let second = unique_path(&desired);
        assert_eq!(second.file_name().unwrap(), "clip (2).mp4");
        std::fs::write(&second, b"x").unwrap();

        let third = unique_path(&desired);
        assert_eq!(third.file_name().unwrap(), "clip (3).mp4");
    }

    #[test]
    fn unique_paths_handle_files_without_an_extension() {
        let dir = tempfile::tempdir().unwrap();
        let desired = dir.path().join("stream");
        assert_eq!(unique_path(&desired), desired);
        std::fs::write(&desired, b"x").unwrap();
        assert_eq!(unique_path(&desired).file_name().unwrap(), "stream (2)");
    }

    #[test]
    fn urls_without_a_scheme_are_upgraded_not_rejected() {
        // Pasting `example.com/x.mp4` is the most common thing a person does.
        assert_eq!(
            parse_url("example.com/x.mp4").unwrap().as_str(),
            "https://example.com/x.mp4"
        );
        assert!(parse_url("  https://x.invalid/a  ").is_ok());
    }

    #[test]
    fn garbage_is_not_silently_turned_into_a_url() {
        assert!(parse_url("").is_err());
        assert!(parse_url(":://nope").is_err());
    }
}
