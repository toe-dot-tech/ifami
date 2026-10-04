//! Measuring how fast bytes actually arrive.
//!
//! The only part of this crate that reads a clock, and it exists to do exactly
//! that: everything else here is deliberately clock-free so that a download's
//! behaviour is a function of the bytes and the responses, not of when it ran.
//!
//! # Why this is not a speedometer
//!
//! A speedometer shows one number and hides the thing that decides how fast a
//! download will actually be. Most origins cap a *single* connection, so one
//! stream flattens out long before the line is full and the remaining capacity
//! is only reachable with several connections at once. That is the number
//! someone choosing where to fetch a 40 GB file actually wants, and it is the
//! number a dial cannot show.
//!
//! So this ramps: it measures at one connection, doubles, and keeps doubling
//! while the aggregate keeps improving. The result is the best level reached,
//! not the last one tried.
//!
//! # What it does not do
//!
//! It does not report a result to anyone. There is no server to report to, no
//! account, and no identifier: the measurement lives in the caller's memory and
//! nowhere else. The only network traffic is bytes requested from the server the
//! caller named, which is the same posture as any other download.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// `StreamExt` walks a response body. Nothing reshapes a future here any more --
// every participant in a level is the same `async fn`.
use bytes::Bytes;
use futures_util::{future::join_all, StreamExt as _};

use crate::error::NetError;
use crate::net::client::{HttpClient, HttpRequest, RequestBody};
use crate::net::range::ByteRange;

/// A public speed-test endpoint that needs no account and sets no cookies.
///
/// Used as the default target so the feature works the moment it is opened.
/// It is shown in the UI in full and is editable: ifami makes no secret request
/// to an address the user cannot see, and a speed test that phoned home to the
/// vendor would be the single most on-brand way for this app to betray its own
/// premise.
pub const DEFAULT_TARGET: &str = "https://speed.cloudflare.com/__down?bytes=100000000";

/// Where uploaded bytes go, by default.
///
/// The same host as [`DEFAULT_TARGET`], which matters for more than tidiness:
/// the "where did this come from" line in the UI has one host to name, and if the
/// two directions went to different operators the app would be quietly handing a
/// download measurement and an upload measurement to two different companies.
///
/// Cloudflare's upload endpoint answers with a small JSON receipt and reads the
/// whole body before replying, so the time from writing the request to seeing the
/// response is the time the bytes took. That receipt is discarded unread -- it is
/// the origin's business what to call the measurement, not ours.
pub const DEFAULT_UPLOAD_TARGET: &str = "https://speed.cloudflare.com/__up";

/// Most connections the ramp will open.
///
/// Sixteen is where returns flatten on real connections, and opening more is
/// rude to the server rather than useful to the person waiting.
pub const MAX_CONNECTIONS: usize = 16;

/// How long to measure at each rung of the ramp.
///
/// Long enough that a slow connection's first-request overhead does not dominate
/// the average, short enough that a full ramp does not feel like a chore.
pub const DEFAULT_STEP: Duration = Duration::from_millis(1500);

/// Ceiling on bytes pulled for one measurement.
///
/// A speed test that downloads without bound is a denial-of-service tool aimed
/// at the user's own data plan. This bounds a full ramp to something small
/// enough to be free on any connection and large enough to be accurate on a
/// fast one.
pub const BYTE_CEILING: u64 = 256 * 1024 * 1024;

/// Ceiling on bytes *pushed* for one measurement.
///
/// An eighth of the download allowance, because the two directions are not
/// symmetrical in cost. Download bytes are usually free and arrive faster than
/// they leave; upload bytes are the metered direction on a phone plan, they are
/// slow by nature, and they are the ones the user's friends are waiting behind.
/// A test that spent its whole budget downstream and then tried to push another
/// 256 MB back would be measuring the download and billing for the upload.
pub const UPLOAD_BYTE_CEILING: u64 = BYTE_CEILING / 8;

/// Most connections the upload ramp will open.
///
/// Four. Upload bandwidth is bounded by the far end's *send* path, which is
/// usually a single bottleneck rather than sixteen independent pipes, so the
/// doubling has nothing to find past the second or third level -- and every level
/// past that point is real bytes charged to someone.
pub const MAX_UPLOAD_CONNECTIONS: usize = 4;

/// Ceiling on wall-clock for one measurement.
///
/// A full ramp is five levels, but the connection count is the caller's to
/// raise, and a caller who sets it to a thousand should get a number back
/// rather than a tab they have to kill. The level already in flight is always
/// allowed to finish.
pub const MAX_DURATION: Duration = Duration::from_secs(30);

/// Size of each individual request.
///
/// Large enough that per-request overhead is negligible next to transfer time,
/// small enough that a fast connection is not asking for gigabyte ranges.
const CHUNK: u64 = 8 * 1024 * 1024;

/// Fractional improvement below which the ramp stops doubling.
///
/// Without a floor the ramp would keep opening connections against a server that
/// has simply stopped helping, and report a worse number than the one it
/// already had. Five percent is inside the noise for most connections, so
/// stopping there costs nothing and stops the pointless part.
const MIN_GAIN: f64 = 0.05;

/// How many round trips to time before the ramp starts, to describe the
/// connection at rest.
///
/// Five is the smallest number that gives a median and a spread worth the name.
/// Three would fit in the same wall time, but with three samples the "median"
/// is one reading and the jitter is one difference, and a number that unstable
/// is worse than no number -- it would move between two runs of the same test
/// on a perfectly still connection.
const IDLE_PROBES: usize = 5;

/// How long the latency probe is allowed to take.
///
/// Short on purpose. Latency is a nice-to-have beside throughput, and a probe
/// that hangs for a minute before the real measurement starts is the sort of
/// thing a person closes the window over.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How to run a measurement.
#[derive(Debug, Clone)]
pub struct SpeedTestConfig {
    /// URL to pull bytes from.
    pub url: String,
    /// URL to push bytes at, or `None` to measure download only.
    ///
    /// Optional because a large share of origins will not accept a body at all,
    /// and an origin that answers `405` must not turn a perfectly good download
    /// measurement into an error. A failed upload is reported as no upload.
    pub upload_url: Option<String>,
    /// Highest number of simultaneous connections to try.
    pub max_connections: usize,
    /// Highest number of simultaneous upload connections to try.
    pub max_upload_connections: usize,
    /// How long to measure at each level.
    pub step: Duration,
    /// Ceiling on bytes pulled across the whole run.
    pub byte_ceiling: u64,
    /// Ceiling on bytes pushed across the whole run.
    pub upload_byte_ceiling: u64,
    /// Ceiling on wall-clock for the whole run.
    pub max_duration: Duration,
}

impl Default for SpeedTestConfig {
    fn default() -> Self {
        Self {
            url: DEFAULT_TARGET.to_string(),
            upload_url: Some(DEFAULT_UPLOAD_TARGET.to_string()),
            max_connections: MAX_CONNECTIONS,
            max_upload_connections: MAX_UPLOAD_CONNECTIONS,
            step: DEFAULT_STEP,
            byte_ceiling: BYTE_CEILING,
            upload_byte_ceiling: UPLOAD_BYTE_CEILING,
            max_duration: MAX_DURATION,
        }
    }
}

impl SpeedTestConfig {
    /// Measure against `url`, using the default ramp.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    /// Measure the download direction only.
    ///
    /// Takes this rather than defaulting `upload_url` to `None`, because the
    /// common case for embedding this crate is a caller with one endpoint in
    /// mind, and making them clear a field they never knew about would be a
    /// worse default than not offering the field at all.
    pub fn download_only(mut self) -> Self {
        self.upload_url = None;
        self
    }

    /// Set the upload endpoint, or `None` to skip the upload direction.
    pub fn with_upload_url(mut self, url: Option<impl Into<String>>) -> Self {
        self.upload_url = url.map(Into::into);
        self
    }

    /// Cap the ramp. Values below 1 are raised to 1: a measurement of zero
    /// connections is not a measurement.
    pub fn with_max_connections(mut self, n: usize) -> Self {
        self.max_connections = n.max(1);
        self
    }

    /// Shorten each rung of the ramp.
    ///
    /// Raised to 1ms, because a zero-length step measures the time to open a
    /// socket and calls it throughput.
    pub fn with_step(mut self, step: Duration) -> Self {
        self.step = step.max(Duration::from_millis(1));
        self
    }
}

/// A shared flag that ends a measurement early.
///
/// Cloning is cheap and every clone is the same flag, so a caller can hold one
/// in application state and hand it to [`run`] while something else -- a button,
/// a window closing, another task -- holds another.
///
/// A stopped run still returns what it measured. Throwing the number away
/// because someone got impatient, or clicked stop two seconds early, is worse
/// than showing them a result with a note that it was cut short.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A flag that has not been raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the run to stop at the next opportunity.
    ///
    /// Idempotent, and safe to call from any thread while the run is in flight.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether the run has been asked to stop.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Which way the bytes in a reading were moving.
///
/// On the `Sample` rather than in the channel the callback is given, so one
/// stream of readings covers both directions and a caller cannot accidentally
/// paint an upload bar with the download figure because it forgot which callback
/// it was in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Bytes arriving from the origin.
    #[default]
    Down,
    /// Bytes being sent to the origin.
    Up,
}

impl Direction {
    /// The opposite direction.
    pub fn reverse(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
        }
    }
}

/// What a reading represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    /// An instantaneous rate, measured across the last [`SAMPLE_INTERVAL`].
    ///
    /// The rate is a difference between two running totals, so the very first
    /// reading of a level is measured from the moment the level opened and every
    /// one after that from the previous reading. A single live reading is not a
    /// measurement of the connection; it is a measurement of the last quarter of
    /// a second of it, which is why they are worth taking several of.
    Live,
    /// A level finished, and this is its average over the whole level.
    ///
    /// The average is the number the result reports and the number the ramp
    /// judges, because judging levels by their worst quarter-second would stop
    /// the ramp on a hiccup. It is also the only reading carrying a
    /// [`Sample::latency`].
    Level,
}

/// One reading from an in-progress measurement.
///
/// Emitted while the test runs so the caller can draw it, not just report a
/// final number. `bps` is the aggregate across every open connection, which is
/// the figure that matters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Which way these bytes were moving.
    pub direction: Direction,
    /// Whether this is an instantaneous rate or a finished level's average.
    pub kind: SampleKind,
    /// Milliseconds since the run began.
    pub elapsed_ms: u64,
    /// Connections open when this reading was taken.
    pub connections: usize,
    /// Aggregate throughput in bits per second.
    pub bps: f64,
    /// Bytes transferred so far, across the whole run and *this direction only*.
    pub total_bytes: u64,
    /// Round-trip time measured *while this level was saturating the link*, or
    /// `None` if the origin would not answer, or if this is a live reading.
    ///
    /// This is the number that explains a bad experience. Throughput says how
    /// much is arriving; latency under load says what it costs to make anything
    /// else happen at the same time, which is why a video call drops while a
    /// download is still showing a healthy speed.
    pub latency: Option<Duration>,
}

/// A distribution of round-trip times, not a single reading.
///
/// One probe is an anecdote. Three is a curiosity. Enough of them is a
/// distribution, and the spread is the part worth keeping: a line with a
/// consistent 40 ms feels completely different from one that alternates between
/// 8 ms and 90 ms, and both average to something in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// The fastest successful probe.
    pub min: Duration,
    /// The middle value, or the mean of the two middle values.
    pub median: Duration,
    /// The slowest successful probe.
    pub max: Duration,
    /// Mean absolute difference between consecutive probes.
    ///
    /// The jitter people mean when they say jitter: not the spread between best
    /// and worst, which [`Self::max`] minus [`Self::min`] already gives, but how
    /// *unevenly* the line answers. A steady line has jitter near zero however
    /// slow it is; a line that alternates fast and slow has high jitter even
    /// when the two are close together.
    pub jitter: Duration,
}

impl Latency {
    /// Build a distribution from raw probe timings, or `None` if none succeeded.
    ///
    /// Takes the timings rather than the requests so it is a pure function of
    /// numbers, which is the only way to test a median.
    pub fn from_samples(samples: &[Duration]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }

        let mut sorted = samples.to_vec();
        sorted.sort_unstable();

        let median = match sorted.len() {
            0 => return None,
            n if n % 2 == 1 => sorted[n / 2],
            n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2,
        };

        // Mean absolute difference between neighbours, in the order the probes
        // actually happened. Not over the sorted values: sorting already destroys
        // the sequence, and "how much did this one jump from the last" is
        // precisely the question jitter is asking.
        let jitter = if samples.len() < 2 {
            Duration::ZERO
        } else {
            let total: Duration = samples.windows(2).map(|w| w[1].abs_diff(w[0])).sum();
            total / (samples.len() as u32 - 1)
        };

        Some(Self {
            min: sorted[0],
            median,
            max: sorted[sorted.len() - 1],
            jitter,
        })
    }
}

/// What one direction's ramp found.
///
/// Kept separate from [`SpeedResult`] rather than being a second `SpeedResult`:
/// an upload has no idle-latency distribution worth reporting -- the probes would
/// have to run against the *download* endpoint or add a second set of round trips
/// to a measurement people already find long -- and pretending otherwise with a
/// `None` field would invite a caller to print an em dash where a number belongs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UploadResult {
    /// Best aggregate upload throughput seen, in bits per second.
    pub bps: f64,
    /// Connections open when the best throughput was seen.
    pub connections: usize,
    /// Round-trip time at the moment the line was busiest pushing, or `None` if
    /// the origin would not answer under load.
    pub latency: Option<Duration>,
    /// Bytes pushed in total.
    pub total_bytes: u64,
}

impl UploadResult {
    /// Throughput in megabits per second, the unit people actually quote.
    pub fn mbps(&self) -> f64 {
        self.bps / 1_000_000.0
    }
}

/// The outcome of a completed measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeedResult {
    /// Best aggregate download throughput seen, in bits per second.
    pub bps: f64,
    /// Connections open when the best throughput was seen.
    pub connections: usize,
    /// Round-trip time distribution with nothing else running, when it could be
    /// measured.
    pub idle: Option<Latency>,
    /// Round-trip time at the moment the line was busiest, or `None` if the
    /// origin would not answer under load.
    ///
    /// The gap between this and [`Self::idle`] is the whole story of a congested
    /// link: a download that looks perfectly fast can still be adding a quarter
    /// of a second to everything else on the network.
    pub loaded: Option<Duration>,
    /// Bytes pulled in total.
    pub total_bytes: u64,
    /// Host the measurement was taken against.
    pub host: String,
    /// What the upload ramp found, or `None` if it was not configured, was
    /// stopped before it started, or the origin would not take a body.
    ///
    /// `None` is not an error. Plenty of origins answer `GET` and refuse
    /// `POST`, and a download measurement that came back clean is still a
    /// download measurement.
    pub upload: Option<UploadResult>,
    /// Whether the run was stopped before finishing its ramp.
    ///
    /// Reported rather than implied by the error, because a partial measurement
    /// is still a measurement. The number is true; it is just not the ceiling.
    pub stopped_early: bool,
}

impl SpeedResult {
    /// Throughput in megabits per second, the unit people actually quote.
    pub fn mbps(&self) -> f64 {
        self.bps / 1_000_000.0
    }
}

/// Measure download throughput against `config.url` and upload against
/// `config.upload_url` *at the same time*.
///
/// The two ramps share the link and the wall clock rather than running one after
/// the other, so the chart's two lanes fill together and a full run costs one
/// ramp's time instead of two. The trade is honest: each direction reports what
/// it can do while the other is also working, not what it could do alone.
///
/// `on_sample` is called with a reading at the end of every level, in both
/// directions, including the last. Each reading carries its [`Direction`], so a
/// caller draws two series from one stream rather than two streams it has to
/// interleave. The returned [`SpeedResult`] reports the *best* level reached in
/// each direction rather than the last one tried: a server that throttles
/// aggressive clients is common enough that "keep doubling until it gets worse"
/// is a real failure mode, and reporting that dip as the user's connection speed
/// would be a lie.
///
/// # Errors
///
/// Returns [`NetError::Body`] when the download origin produced no bytes at all.
/// That is the one outcome that must never be papered over with a confident zero,
/// which is why it is an error the UI can explain rather than a number it can
/// display.
///
/// The upload direction never produces an error. An origin that refuses a body,
/// or a run stopped before the upload began, is reported as
/// [`SpeedResult::upload`] being `None`.
pub async fn run<F>(
    client: &dyn HttpClient,
    config: &SpeedTestConfig,
    cancel: &Cancel,
    on_sample: F,
) -> Result<SpeedResult, NetError>
where
    F: FnMut(Sample),
{
    let host = url_host(&config.url);
    let started = Instant::now();

    // Measured before any ramp and on its own connection, so a slow first
    // response is attributed to the origin rather than smeared across the
    // throughput average. A failed probe is not fatal: plenty of origins answer
    // GET and refuse HEAD, and refusing to measure a working link over it would
    // be its own kind of dishonesty.
    //
    // Several probes rather than one, because the spread between the fastest
    // and slowest is the measurement. See [`Latency`].
    let idle = probe_latency(client, &config.url, IDLE_PROBES).await;

    // Both directions run at once rather than one after the other. The two ramps
    // share the link and the wall clock, so the chart's two lanes fill together
    // and a full run costs one ramp's time instead of two. The trade is honest
    // and stated: each direction reports what it can do *while the other is also
    // working*, which is what a link does whenever it is sending and receiving
    // at the same time.
    //
    // The callback is behind a lock because two concurrent ramps must share it.
    // It is a synchronous call, so the guard never crosses an `await`; the lock
    // exists for the borrow checker, not for contention.
    let on_sample = Mutex::new(on_sample);

    let down_spec = RampSpec {
        url: &config.url,
        direction: Direction::Down,
        max_connections: config.max_connections,
        byte_ceiling: config.byte_ceiling,
    };
    let up_spec = config.upload_url.as_deref().map(|url| RampSpec {
        url,
        direction: Direction::Up,
        max_connections: config.max_upload_connections,
        byte_ceiling: config.upload_byte_ceiling,
    });

    let download = ramp(client, &down_spec, config, cancel, started, &on_sample);

    // Skipped rather than attempted-and-failed when there is nowhere to push or
    // the run already ran out of wall clock: an upload that starts with no time
    // left measures the handshake and calls it a number.
    let upload_dir = up_spec.as_ref().and_then(|spec| {
        (!cancel.is_cancelled() && config.max_duration > started.elapsed())
            .then_some(ramp(client, spec, config, cancel, started, &on_sample))
    });

    let (down, upload) = match upload_dir {
        Some(upload_dir) => {
            let mut finished = join_all([download, upload_dir]).await;
            let up = finished.remove(1);
            (finished.remove(0), up)
        }
        None => (download.await, None),
    };

    let down = down.ok_or_else(|| NetError::Body {
        url: config.url.clone(),
        reason: "the server returned no data to measure".to_string(),
    })?;

    let upload = upload.map(|r| UploadResult {
        bps: r.bps,
        connections: r.connections,
        latency: r.latency,
        total_bytes: r.total_bytes,
    });

    Ok(SpeedResult {
        bps: down.bps,
        connections: down.connections,
        idle,
        loaded: down.latency,
        total_bytes: down.total_bytes,
        host,
        upload,
        stopped_early: cancel.is_cancelled(),
    })
}

/// What one direction's ramp found, before it is split into result types.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RampOutcome {
    bps: f64,
    connections: usize,
    latency: Option<Duration>,
    total_bytes: u64,
}

/// One direction's ramp settings, fixed for the whole ramp.
///
/// Bundled rather than passed as five loose arguments, because the direction, the
/// endpoint and the two ceilings have to mean the same thing at every level --
/// and a struct makes that a property of the type rather than of remembering to
/// pass the same five values to every call.
#[derive(Debug, Clone, Copy)]
struct RampSpec<'a> {
    /// Endpoint bytes move to or from.
    url: &'a str,
    /// Which way they go.
    direction: Direction,
    /// Highest number of simultaneous connections to open.
    max_connections: usize,
    /// Ceiling on bytes for this direction across the whole ramp.
    byte_ceiling: u64,
}

/// Hand one reading to the caller's callback.
///
/// The callback is behind a lock so two concurrent ramps can share it. A callback
/// that panicked does not take the run down with it twice: a poisoned lock is
/// recovered from because the panic has already unwound through this call, and
/// refusing to publish the next reading would only turn one panic into a second.
fn publish<F: FnMut(Sample)>(on_sample: &Mutex<F>, sample: Sample) {
    let mut guard = on_sample
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (*guard)(sample);
}

/// Ramp one direction until it stops improving, and report the best level.
///
/// One function for both directions, deliberately. The download and upload ramps
/// make the same decisions -- when to stop doubling, how to compare a level to
/// the best, when to give up as unreachable -- and a second copy of that loop
/// would be a second place for the two to disagree about what a speed test
/// means. The direction reaches the transport as a parameter on the lane rather
/// than as a second set of lane types, so `join_all` still sees one concrete
/// future type.
///
/// `None` means every level transferred nothing, which for the download direction
/// is a failure and for the upload direction is an origin that will not take a
/// body. The caller decides which, because only it knows what a missing number
/// costs in its context.
async fn ramp<F>(
    client: &dyn HttpClient,
    spec: &RampSpec<'_>,
    config: &SpeedTestConfig,
    cancel: &Cancel,
    started: Instant,
    on_sample: &Mutex<F>,
) -> Option<RampOutcome>
where
    F: FnMut(Sample),
{
    let budget = Budget::new(spec.byte_ceiling);
    let mut total_bytes = 0u64;
    let mut best: Option<(f64, usize)> = None;
    let mut connections = 1usize;
    // Latency is recorded alongside the level that produced it, so the reported
    // "under load" figure is the one from the run's best level rather than a
    // separate measurement taken at some arbitrary moment afterwards.
    let mut loaded: Option<Duration> = None;

    while connections <= spec.max_connections && !cancel.is_cancelled() {
        let wall = config
            .step
            .min(config.max_duration.saturating_sub(started.elapsed()));
        if wall.is_zero() {
            break;
        }

        let level_started = Instant::now();
        let (moved, level_latency, trace) =
            run_level(client, spec, connections, wall, &budget, cancel).await;

        // The trace is turned into live readings before the level's own average
        // is emitted, so a caller sees the quarter-seconds in the order they
        // happened and then the average that was judged from them. Emitting the
        // average first would put the conclusion before the evidence.
        let mut prev_bytes = 0u64;
        let mut prev_ms = 0u64;
        for reading in &trace {
            // Saturating, because a reader that somehow observed fewer bytes than
            // the previous reading would otherwise wrap to a number near
            // eighteen quintillion bits per second and draw a spike to the top of
            // the chart. It cannot happen -- the counter only grows -- and a chart
            // that cannot survive a bug is not a chart.
            let gained = reading.bytes.saturating_sub(prev_bytes);
            let secs = (reading.elapsed_ms.saturating_sub(prev_ms) as f64) / 1000.0;
            prev_bytes = reading.bytes;
            prev_ms = reading.elapsed_ms;

            publish(
                on_sample,
                Sample {
                    direction: spec.direction,
                    kind: SampleKind::Live,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    connections,
                    bps: (gained as f64) * 8.0 / secs.max(f64::MIN_POSITIVE),
                    total_bytes: total_bytes.saturating_add(reading.bytes),
                    latency: None,
                },
            );
        }

        total_bytes = total_bytes.saturating_add(moved);

        // Nothing moved means we are not measuring a slow connection, we are
        // failing to reach the origin. Ramping sixteen connections at a server
        // that is not answering tells the user nothing except that we did not
        // notice they have no internet.
        if moved == 0 {
            break;
        }

        let secs = level_started.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
        let bps = (moved as f64) * 8.0 / secs;

        publish(
            on_sample,
            Sample {
                direction: spec.direction,
                kind: SampleKind::Level,
                elapsed_ms: started.elapsed().as_millis() as u64,
                connections,
                bps,
                total_bytes,
                latency: level_latency,
            },
        );

        // Judged against the best *before* this level is recorded. Asking after
        // would compare every level against itself, which is never an
        // improvement -- and the ramp would stop at one connection and report
        // the single-stream speed, the one number this feature exists to beat.
        match judge(best, bps) {
            Ramp::Improve => {
                best = Some((bps, connections));
                loaded = level_latency;
                connections = connections.saturating_mul(2);
            }
            Ramp::Stop => break,
        }
    }

    let (bps, connections) = best?;
    Some(RampOutcome {
        bps,
        connections,
        latency: loaded,
        total_bytes,
    })
}

/// What the ramp should do with a level it has just finished measuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ramp {
    /// Better than anything seen so far, and worth opening more connections to
    /// see whether that can be beaten.
    Improve,
    /// No better. The ramp stops and this level is discarded, leaving the
    /// previous best as the reported result.
    Stop,
}

/// Whether a completed level is a new best.
///
/// Extracted as a pure function because this is the decision the whole feature
/// turns on, and a decision tested through a live socket and a real clock is a
/// decision that gets tested by luck.
///
/// The rule: the first level always has somewhere to go, and after that only a
/// level that actually improved on the best is worth the cost of the connections
/// it took to measure.
fn judge(best: Option<(f64, usize)>, bps: f64) -> Ramp {
    match best {
        None => Ramp::Improve,
        Some((prev, _)) if bps > prev * (1.0 + MIN_GAIN) => Ramp::Improve,
        Some(_) => Ramp::Stop,
    }
}

/// What one concurrent participant in a level contributed.
///
/// A level is several things at once -- some connections moving bytes, one asking
/// the server a question, and one taking readings -- and they all have to be
/// polled as one group, because polling them on separate tasks is what turns a
/// measurement into a benchmark of the scheduler. One enum lets them share a
/// single `join_all` without boxing a future per lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// Bytes moved by a data lane.
    Bytes(u64),
    /// A round-trip time taken while the data lanes were busy.
    Probe(Option<Duration>),
    /// Nothing: the reader wrote its readings into the level's shared trace and
    /// returns empty-handed.
    ///
    /// A variant rather than a field on the others because the other two carry
    /// their result out of the `join_all`, and this one cannot -- see
    /// [`trace`].
    Trace,
}

/// Open `connections` streams at once and count what moves until the budget
/// expires.
///
/// Each download connection requests a distinct range. That matters: a server
/// that ignores `Range` and serves everyone the same bytes would otherwise let
/// the test report a throughput nobody could ever get from a real download.
///
/// One extra request rides along on every level purely to time it. That is the
/// only way to learn what the link costs *while it is busy*, because the moment
/// the lanes close the answer is gone — and an idle probe taken afterwards
/// describes a network that no longer exists.
///
/// A third rides along to take readings, for the same reason: the shape of a
/// level is not knowable from its average, and a chart that can only show an
/// average is a bar chart wearing a candle's clothes.
/// The shared state of one level, seen by every participant.
///
/// Bundled rather than passed as six loose arguments, because the whole point of
/// a level is that all its lanes agree: the same deadline, the same budget, the
/// same counters. A struct makes that agreement a property of the type rather
/// than of remembering to pass the same values to every call.
struct Level<'a> {
    /// The transport every lane shares.
    client: &'a dyn HttpClient,
    /// Endpoint bytes move to or from.
    url: &'a str,
    /// Which way they go.
    direction: Direction,
    /// When the level ends, on the same clock for every lane.
    deadline: Instant,
    /// The byte allowance every lane shares.
    budget: &'a Budget,
    /// Running total, published by the data lanes and read by the reader.
    seen: &'a Arc<AtomicU64>,
    /// The readings the reader lane has taken.
    trace: &'a Arc<Mutex<Vec<Trace>>>,
    /// The caller's stop flag.
    cancel: &'a Cancel,
}

async fn run_level(
    client: &dyn HttpClient,
    spec: &RampSpec<'_>,
    connections: usize,
    wall: Duration,
    budget: &Budget,
    cancel: &Cancel,
) -> (u64, Option<Duration>, Vec<Trace>) {
    let deadline = Instant::now() + wall;

    // Every lane publishes its running total here, and the reader reads it. One
    // number both sides agree on, rather than the reader asking each lane how far
    // it has got -- which would mean a lock per lane per reading and a figure
    // that is only as consistent as the slowest lane's reply.
    let seen = Arc::new(AtomicU64::new(0));
    let trace = Arc::new(Mutex::new(Vec::new()));

    let level = Level {
        client,
        url: spec.url,
        direction: spec.direction,
        deadline,
        budget,
        seen: &seen,
        trace: &trace,
        cancel,
    };

    // The lanes are polled together on this task rather than spawned. That reads
    // like a downgrade and is not one: each future spends essentially all of
    // its life blocked on a socket, so polling them in the same task is what
    // makes them concurrent. It is also the only way to keep the borrow of
    // `client` short enough -- `JoinSet::spawn` demands a `'static` future, and
    // the only way to satisfy that here would be an `Arc` around the client
    // existing for no reason other than to appease a bound. Spawning would also
    // let the lanes land on different cores, and for a measurement that is a
    // way to benchmark the scheduler instead of the network.
    // Every participant is the same `async fn`, so this iterator is one
    // concrete future type and `join_all` can hold it without boxing. Which is
    // the point of the `Role` enum: two different `async` blocks would be two
    // unrelated opaque types, and joining them would need an allocation per
    // level purely to give the compiler a common shape.
    let roles = (0..connections)
        .map(|lane| Role::Data {
            // Stagger the offsets so lanes ask for different parts of the
            // object rather than making different requests for the same part.
            offset: lane as u64 * CHUNK,
        })
        .chain(std::iter::once(Role::Probe))
        .chain(std::iter::once(Role::Trace));

    let lanes = roles.map(|role| lane(&level, role));

    let mut moved = 0u64;
    let mut latency = None;
    for lane in join_all(lanes).await {
        match lane {
            Lane::Bytes(n) => moved = moved.saturating_add(n),
            Lane::Probe(t) => latency = t,
            Lane::Trace => {}
        }
    }

    // The reader holds the lock for the length of one `Vec::drain`, and the lanes
    // it reads from are already finished, so there is no contention left to
    // avoid. A poisoned lock here would mean a reader panicked, which cannot
    // happen inside a loop that only pushes; falling back to an empty trace is
    // the honest response to "we lost the detail" and does not lose the result.
    let trace = match Arc::try_unwrap(trace) {
        Ok(lock) => lock.into_inner().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    (moved, latency, trace)
}

/// What a participant in a level is being asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Move bytes, from this offset.
    Data { offset: u64 },
    /// Time one round trip.
    Probe,
    /// Take readings of the running total.
    Trace,
}

/// One participant in a level, of any kind.
///
/// A single entry point rather than three call sites, so the "everything is
/// polled as one group" property is a property of the type and not of
/// remembering to pass all three futures to the same `join_all`.
async fn lane(level: &Level<'_>, role: Role) -> Lane {
    match role {
        Role::Data { offset } => Lane::Bytes(drain(level, offset).await),
        Role::Probe => Lane::Probe(one_shot_latency(level.client, level.url).await),
        Role::Trace => {
            take_readings(level.seen, level.trace, level.deadline, level.cancel).await;
            Lane::Trace
        }
    }
}

/// One reading taken while a level was running: when, and how many bytes had
/// moved by then.
///
/// Raw rather than pre-converted, because the rate belongs to whoever knows when
/// the level started and what the previous reading said. Doing it here would mean
/// carrying the previous sample into a role that has no business knowing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Trace {
    /// Milliseconds since the level opened.
    elapsed_ms: u64,
    /// Bytes moved across every lane so far.
    bytes: u64,
}

/// Take a reading every [`SAMPLE_INTERVAL`] until the level ends.
///
/// This is the reason the engine emits more than one sample per level, and it is
/// not about drawing. A level's average is a single number, and a single number
/// cannot distinguish a line that held steady from one that swung from 20 Mbps
/// to 400 Mbps and back twice inside the same second -- which are two completely
/// different experiences of the same connection, and produce the same average.
/// The shape is the measurement.
///
/// The reader is a joined lane rather than a spawned task for the same reason the
/// data lanes are: see [`run_level`]. `join_all` polls in order, so a data lane
/// that never yields -- which is the normal case, since it is blocked on a socket
/// -- still hands the reader a turn.
async fn take_readings(
    seen: &Arc<AtomicU64>,
    out: &Arc<Mutex<Vec<Trace>>>,
    deadline: Instant,
    cancel: &Cancel,
) {
    let opened = Instant::now();
    loop {
        // Wake on the interval, or on the deadline, whichever is sooner. Sleeping
        // the full interval past the deadline would hold the whole level open
        // past its own wall clock for no reading.
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || cancel.is_cancelled() {
            return;
        }
        tokio::time::sleep(left.min(SAMPLE_INTERVAL)).await;

        let reading = Trace {
            elapsed_ms: opened.elapsed().as_millis() as u64,
            bytes: seen.load(Ordering::Relaxed),
        };

        // Only the reader takes this lock, and only for a push. A level produces
        // at most a handful of readings, so the contention that would justify
        // anything cleverer does not exist.
        if let Ok(mut guard) = out.lock() {
            guard.push(reading);
        }

        if Instant::now() >= deadline || cancel.is_cancelled() {
            return;
        }
    }
}

/// How often a live reading is taken while a level runs.
///
/// Quarter of a second. Fast enough that a second of transfer holds four
/// readings -- enough for a candle to have a body and a wick rather than being a
/// line -- and slow enough that the reading itself is not a measurable tax on a
/// connection that is only doing 4 Mbps.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// Move bytes on one connection until the deadline or the byte ceiling, then
/// stop.
///
/// The deadline is checked per chunk rather than enforced by cancelling, so the
/// last partial measurement is kept rather than thrown away. Dropping the
/// response closes the socket, which is exactly what a person pressing "stop"
/// wants.
async fn drain(level: &Level<'_>, offset: u64) -> u64 {
    match level.direction {
        Direction::Down => drain_down(level, offset).await,
        Direction::Up => drain_up(level).await,
    }
}

/// Pull bytes on one connection until the deadline or the byte ceiling.
///
/// Each request asks for a distinct [`CHUNK`]-sized range, and the offset
/// advances by exactly what came back, so a server that ignores `Range` and sends
/// the whole object cannot inflate the result.
async fn drain_down(level: &Level<'_>, mut offset: u64) -> u64 {
    let mut total = 0u64;

    while Instant::now() < level.deadline && level.budget.has_room() && !level.cancel.is_cancelled()
    {
        let range = ByteRange::closed(offset, offset + CHUNK);
        let request = HttpRequest::get(level.url)
            .with_range(Some(range))
            .with_header("Cache-Control", "no-cache")
            .with_header("Pragma", "no-cache");

        let Ok(response) = level.client.execute(request).await else {
            // A lane that cannot start is not fatal to the measurement: the
            // others are still transferring, and one refused connection is
            // something the caller can show rather than a reason to discard the
            // whole result.
            return total;
        };
        if !response.is_success() {
            return total;
        }

        let mut body = response.body;
        while let Some(chunk) = body.next().await {
            if Instant::now() >= level.deadline || level.cancel.is_cancelled() {
                return total;
            }
            let Ok(chunk) = chunk else {
                return total;
            };
            let n = chunk.len() as u64;
            total += n;
            offset += n;

            // Published per chunk, and by addition rather than by assignment.
            // Every lane in the level writes here, so a `store` would leave the
            // reader reporting whichever lane happened to speak last -- and at
            // sixteen lanes that is a figure unrelated to the aggregate the
            // result is about. A `fetch_add` cannot lose an update, for the same
            // reason `Budget::charge` cannot.
            level.seen.fetch_add(n, Ordering::Relaxed);

            // Charged as the bytes arrive, so the ceiling bounds what was
            // actually received rather than what somebody intended to request,
            // and the lane stops the moment the budget is gone.
            if level.budget.charge(n) > level.budget.limit {
                return total;
            }
        }
    }

    total
}

/// Push bytes at one connection until the deadline or the byte ceiling.
///
/// The loop is one request per [`CHUNK`], and a request is counted as soon as
/// the origin has answered it. That is the only honest completion signal
/// available: `reqwest` will not tell us how much of a request body the socket
/// actually took, and the origin answering at all is proof that it read the body
/// to the end.
///
/// So the figure is bytes *offered*, not bytes confirmed delivered. The gap
/// between the two is TCP's, not ours, and it is bounded: a request that fails
/// partway returns an error and is not counted at all, which biases the result
/// low rather than high. A speed test that flatters itself is worse than one that
/// under-reads by a few percent.
async fn drain_up(level: &Level<'_>) -> u64 {
    let mut total = 0u64;

    // One buffer, reused for every request. Allocating a fresh payload per
    // request would put the allocator in the measurement at exactly the moment
    // the numbers are smallest and most sensitive to noise.
    let chunk = Bytes::from(vec![0u8; UPLOAD_CHUNK as usize]);

    while Instant::now() < level.deadline && level.budget.has_room() && !level.cancel.is_cancelled()
    {
        let Some(body) = RequestBody::repeated(chunk.clone(), CHUNK) else {
            return total;
        };
        let request = HttpRequest::post(level.url, body)
            .with_header("Content-Type", "application/octet-stream");

        let Ok(response) = level.client.execute(request).await else {
            return total;
        };
        if !response.is_success() {
            return total;
        }

        total += CHUNK;
        // Published before the ceiling is checked, so the reading includes the
        // request that exhausted the budget rather than stopping one short of the
        // bytes that were genuinely spent.
        level.seen.fetch_add(CHUNK, Ordering::Relaxed);

        // Charged on the way out rather than on the way back, because the budget
        // is a promise about what we will *spend*, and these bytes are already
        // spent by the time the response arrives.
        if level.budget.charge(CHUNK) > level.budget.limit {
            return total;
        }
    }

    total
}

/// Size of each individual upload request.
///
/// Larger than [`CHUNK`] for downloads, because an upload request pays a full
/// round trip's worth of latency before any of it is throughput: at 4 Mbps, a
/// 1.5-second step fits about 750 KB, so an 8 MB request would spend most of its
/// life being scheduled rather than being sent. Eight megabytes amortises the
/// setup over enough bytes to still see the rate.
const UPLOAD_CHUNK: u64 = 16 * 1024 * 1024;

/// The byte allowance for one run, shared across every lane.
///
/// A bare `AtomicU64` would handle the counting, but the limit belongs next to
/// the counter: two numbers that must agree, kept together so they cannot drift.
#[derive(Debug)]
struct Budget {
    spent: AtomicU64,
    limit: u64,
}

impl Budget {
    fn new(limit: u64) -> Self {
        Self {
            spent: AtomicU64::new(0),
            limit,
        }
    }

    /// Record `n` more bytes and return the running total.
    ///
    /// Deliberately not a compare-and-swap. Every lane incrementing the same
    /// counter must succeed, and a lost update here would under-count and
    /// quietly raise the ceiling -- and the ceiling is the one bound in this
    /// feature that stands between a person and an enormous phone bill. A plain
    /// `fetch_add` cannot lose an update.
    fn charge(&self, n: u64) -> u64 {
        self.spent.fetch_add(n, Ordering::Relaxed) + n
    }

    fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// Whether any budget is left, checked before opening a request.
    ///
    /// Strict, not `spent <= limit`. The inclusive version reads as though it
    /// means "the budget can cover something", and at exactly-exhausted it says
    /// yes -- so the loop opens one more request, pulls one more chunk, and
    /// only notices on the way out. A ceiling that needs a request to discover
    /// it has been reached is not a ceiling.
    ///
    /// The overshoot that remains is unavoidable: bytes cannot be un-received,
    /// so each lane is at most one *delivered* chunk past the line when it
    /// notices -- and then it opens no further request, which is the part that
    /// keeps the bound meaningful.
    ///
    /// An earlier version also allowed a whole request's worth of slack per
    /// lane. Sizing that allowance in requested bytes rather than delivered ones
    /// made it 128 times too generous at sixteen lanes, quietly turning a
    /// 256 MB budget into a 384 MB one -- the exact failure this type exists to
    /// prevent, arrived at by being careful about the wrong quantity.
    fn has_room(&self) -> bool {
        self.spent() < self.limit
    }
}

/// One round-trip time, taken while the data lanes are busy.
///
/// Returns `None` only if the origin would not answer.
///
/// There is deliberately no "was it in time?" check against the level's
/// deadline. An earlier version had one, and it reported nothing at all
/// whenever a probe could not be polled before the deadline passed -- which
/// happens whenever the data lanes never yield, because `join_all` polls its
/// futures in order and a lane that is never `Pending` runs to completion
/// without the probe getting a turn. So the guard silently erased the reading
/// for exactly the clients that answered fastest.
///
/// And the case it was guarding against is not a case worth rejecting. If a
/// `HEAD` takes longer than [`PROBE_TIMEOUT`] to come back while the line is
/// being saturated, that is not a measurement to throw away -- a server that
/// cannot answer a trivial question promptly under load is precisely the
/// bufferbloat this number exists to reveal. Dropping the slow readings would
/// bias the result towards looking healthy.
async fn one_shot_latency(client: &dyn HttpClient, url: &str) -> Option<Duration> {
    let request = HttpRequest::head(url).with_timeout(PROBE_TIMEOUT);
    let started = Instant::now();
    let response = client.execute(request).await.ok()?;
    response.is_success().then(|| started.elapsed())
}

/// A distribution of round-trip times taken back to back, or `None` if none
/// answered.
///
/// Failures are dropped rather than counted. A probe that never came back says
/// something true and alarming, but folding it into a percentile requires
/// inventing a value, and inventing a value is what this whole feature exists
/// to avoid. It shows up as a missing sample instead, and a distribution with
/// fewer points than requested is visible in the count.
async fn probe_latency(client: &dyn HttpClient, url: &str, count: usize) -> Option<Latency> {
    let deadline = Instant::now() + PROBE_TIMEOUT.saturating_mul(count as u32);
    let mut samples = Vec::with_capacity(count);

    for _ in 0..count {
        if Instant::now() >= deadline {
            break;
        }
        let request = HttpRequest::head(url).with_timeout(PROBE_TIMEOUT);
        let started = Instant::now();
        if let Ok(response) = client.execute(request).await {
            if response.is_success() {
                samples.push(started.elapsed());
            }
        }
    }

    Latency::from_samples(&samples)
}

/// The host portion of `url`, or the whole string if it will not parse.
///
/// A result is always attributed to somewhere. If the URL is unparseable the
/// measurement is about to fail anyway, and an empty host would leave someone
/// looking at a number with nothing attached to it.
fn url_host(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.host_str().unwrap_or_default().to_string())
        .unwrap_or_else(|_| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use bytes::Bytes;
    use std::sync::Arc;

    use crate::net::client::{Headers, Method, RawResponse};

    /// A client that serves a fixed number of bytes per request, optionally after
    /// a fixed delay.
    ///
    /// Sized and configured so the arithmetic under test is the only thing that
    /// decides the outcome: no jitter to absorb, and — when `delay` is set — a
    /// cost per request that a real socket has and a pure function does not.
    struct Trickle {
        chunk: usize,
        delay: Option<Duration>,
        seen: AtomicU64,
    }

    impl Trickle {
        fn new(chunk: usize) -> Arc<Self> {
            Arc::new(Self {
                chunk,
                delay: None,
                seen: AtomicU64::new(0),
            })
        }

        /// A trickle that takes real time to answer, so lanes genuinely overlap.
        ///
        /// This is not a cosmetic knob. `#[tokio::test]` runs a single-threaded
        /// runtime, so a client that returns immediately never lets the executor
        /// reach a yield point: two "connections" to it do not run in parallel,
        /// they take turns, and the pair moves roughly what one lane moved. A
        /// test asserting that two lanes out-run one therefore passes or fails
        /// according to how much CPU the machine gave it, which is how
        /// `parallel_lanes_pull_more_than_one_lane_would` came to fail only when
        /// the full suite was running. A sleep is a yield point: the lanes
        /// interleave, the run is bounded by the clock rather than by the
        /// scheduler, and the assertion means what it says.
        fn slow(chunk: usize, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                chunk,
                delay: Some(delay),
                seen: AtomicU64::new(0),
            })
        }
    }

    #[async_trait]
    impl HttpClient for Trickle {
        async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
            self.seen.fetch_add(1, Ordering::Relaxed);
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            Ok(RawResponse::full(
                206,
                Bytes::from(vec![0u8; self.chunk]),
                req.url,
                Headers::new([("Content-Length".to_string(), self.chunk.to_string())]),
            ))
        }
    }

    /// A client that never connects, for the paths that must not invent a number.
    struct Broken;

    #[async_trait]
    impl HttpClient for Broken {
        async fn execute(&self, _: HttpRequest) -> Result<RawResponse, NetError> {
            Err(NetError::Dns {
                url: "https://example.invalid/x".to_string(),
            })
        }
    }

    fn config() -> SpeedTestConfig {
        SpeedTestConfig {
            url: "https://example.invalid/big".to_string(),
            // One level. These tests are about the reporting and the failure
            // modes; the ramp has its own test, and it needs the clock.
            max_connections: 1,
            step: Duration::from_millis(40),
            byte_ceiling: BYTE_CEILING,
            max_duration: Duration::from_secs(5),
            // No upload: every other field here is about the download path, and a
            // second ramp would make each of these tests slower and no more
            // true. The upload direction is tested on its own, below.
            upload_url: None,
            max_upload_connections: MAX_UPLOAD_CONNECTIONS,
            upload_byte_ceiling: UPLOAD_BYTE_CEILING,
        }
    }

    #[tokio::test]
    async fn a_successful_measurement_reports_a_host_and_some_bytes() {
        let client = Trickle::new(64 * 1024);

        let mut samples = Vec::new();
        let result = run(&*client, &config(), &Cancel::new(), |s| samples.push(s))
            .await
            .expect("the fixture serves bytes");

        assert_eq!(result.host, "example.invalid");
        assert!(
            result.bps > 0.0,
            "a transfer happened, so throughput is positive"
        );
        assert!(result.total_bytes > 0);
        assert!(
            !samples.is_empty(),
            "the caller is told about progress as it happens, not only at the end"
        );
    }

    #[tokio::test]
    async fn parallel_lanes_pull_more_than_one_lane_would() {
        // The headline number is the aggregate. If two lanes reported the same
        // rate as one, the feature would be reporting the speed of a single
        // stream while claiming to have measured the line.
        //
        // The fake takes real time per request, and the assertion is about how
        // many requests were issued rather than about bytes or a rate. A request
        // count is the one figure here that is a property of the engine and not
        // of the clock: every request is worth the same `chunk`, so asking for
        // twice as many is asking for twice the bytes, and the wall-clock
        // division is then already folded in. Bytes divided by elapsed time is
        // *not* safe here — that is what this assertion used to be, and it
        // failed only when the whole suite ran at once, because it was measuring
        // how much CPU the machine handed to two tests in a row.
        let one = Trickle::slow(16 * 1024, Duration::from_millis(10));
        let two = Trickle::slow(16 * 1024, Duration::from_millis(10));

        let cfg = |n| {
            config()
                .with_max_connections(n)
                .with_step(Duration::from_millis(400))
        };

        run(&*one, &cfg(1), &Cancel::new(), |_| {})
            .await
            .expect("bytes");
        run(&*two, &cfg(2), &Cancel::new(), |_| {})
            .await
            .expect("bytes");

        let single = one.seen.load(Ordering::Relaxed);
        let pair = two.seen.load(Ordering::Relaxed);

        assert!(single > 0, "the single-lane run issued requests at all");
        assert!(
            pair > single * 3 / 2,
            "two lanes should issue noticeably more than one in the same 400ms; \
             one issued {} and two issued {}",
            single,
            pair
        );
    }

    #[tokio::test]
    async fn each_lane_asks_for_its_own_range() {
        // A test that does not check this can pass while every lane downloads
        // byte 0, which is a server sending the same bytes sixteen times and a
        // throughput nobody could ever get from a real download.
        struct Recorder {
            ranges: std::sync::Mutex<Vec<Option<ByteRange>>>,
        }

        #[async_trait]
        impl HttpClient for Recorder {
            async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
                self.ranges.lock().expect("poisoned").push(req.range);
                Ok(RawResponse::full(
                    206,
                    Bytes::from(vec![0u8; 4096]),
                    req.url,
                    Headers::new([("Content-Length".to_string(), "4096".to_string())]),
                ))
            }
        }

        let client = Recorder {
            ranges: std::sync::Mutex::new(Vec::new()),
        };
        run(
            &client,
            &config().with_max_connections(4),
            &Cancel::new(),
            |_| {},
        )
        .await
        .expect("bytes");

        let seen = client.ranges.lock().expect("poisoned").clone();
        let mut distinct: Vec<_> = seen.iter().collect();
        distinct.sort_by_key(|r| r.map(|r| (r.start, r.end)));
        distinct.dedup();

        assert!(
            distinct.len() >= 4,
            "expected four distinct ranges, saw {:?}",
            distinct
        );
    }

    #[tokio::test]
    async fn a_server_that_serves_nothing_yields_no_number() {
        // The outcome that must never happen is a confident 0 Mbps. A machine
        // with no internet is a situation the person needs told about, not a
        // result.
        let err = run(&Broken, &config(), &Cancel::new(), |_| {})
            .await
            .expect_err("a dead server cannot be measured");

        assert!(err.to_string().contains("no data to measure"), "{err}");
    }

    #[tokio::test]
    async fn a_server_that_refuses_the_first_request_stops_immediately() {
        // No point opening sixteen connections against an origin that has
        // already said no, and no point reporting the result as though we had
        // learned something.
        struct Refusing {
            calls: AtomicU64,
        }

        #[async_trait]
        impl HttpClient for Refusing {
            async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Ok(RawResponse::empty(403, req.url, Headers::new([])))
            }
        }

        let client = Refusing {
            calls: AtomicU64::new(0),
        };
        let result = run(
            &client,
            &config().with_max_connections(8),
            &Cancel::new(),
            |_| {},
        )
        .await
        .expect_err("a 403 cannot be measured");

        assert!(
            result.to_string().contains("no data to measure"),
            "{result}"
        );
    }

    #[tokio::test]
    async fn the_byte_ceiling_stops_the_run() {
        // The bound that protects someone's data allowance.
        //
        // The budget is set to exactly two delivered chunks, so the run stops on
        // a boundary rather than somewhere in the middle of a chunk and the
        // arithmetic is checkable. It lands on the budget precisely: the room
        // check happens before a request is opened, so once the budget is spent
        // no further request is made at all, and the "one chunk of unavoidable
        // overshoot" the implementation documents never even occurs here.
        const CHUNK_DELIVERED: u64 = 64 * 1024;
        let client = Trickle::new(CHUNK_DELIVERED as usize);
        let cfg = SpeedTestConfig {
            byte_ceiling: 2 * CHUNK_DELIVERED,
            ..config()
        };

        let result = run(&*client, &cfg, &Cancel::new(), |_| {})
            .await
            .expect("bytes");

        assert_eq!(
            result.total_bytes, cfg.byte_ceiling,
            "the run should stop exactly on its budget"
        );
        assert!(
            result.bps > 0.0,
            "and still have measured something worth reporting"
        );
    }

    #[tokio::test]
    async fn latency_is_reported_when_the_origin_answers_a_head() {
        let client = Trickle::new(1024);
        let result = run(&*client, &config(), &Cancel::new(), |_| {})
            .await
            .expect("bytes");
        assert!(
            result.idle.is_some(),
            "a responsive origin should give us a round-trip time"
        );
    }

    /* ---------------- the latency distribution ---------------- */

    #[test]
    fn no_successful_probes_is_not_a_distribution() {
        assert_eq!(Latency::from_samples(&[]), None);
    }

    #[test]
    fn a_single_probe_has_a_median_but_no_jitter() {
        // One reading cannot vary, and reporting a jitter of zero from a single
        // sample would claim a steadiness nobody observed.
        let l = Latency::from_samples(&[Duration::from_millis(40)]).expect("one sample");
        assert_eq!(l.min, Duration::from_millis(40));
        assert_eq!(l.median, Duration::from_millis(40));
        assert_eq!(l.max, Duration::from_millis(40));
        assert_eq!(l.jitter, Duration::ZERO);
    }

    #[test]
    fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
        let l = Latency::from_samples(&[
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
            Duration::from_millis(40),
        ])
        .expect("four samples");
        assert_eq!(l.median, Duration::from_millis(25));
        assert_eq!(l.min, Duration::from_millis(10));
        assert_eq!(l.max, Duration::from_millis(40));
    }

    #[test]
    fn jitter_measures_the_jumps_not_the_spread() {
        // The distinguishing case, and the reason this is not `max - min`.
        //
        // Sorted, these four samples are 10, 11, 12, 13 -- a range of 3ms and a
        // perfectly flat line. But the connection answered 13, 10, 10, 11, so
        // the first request took 3ms longer than the one after it, and the next
        // arrived 3ms sooner. Sorting that away before measuring would report a
        // fast, steady line for a link that visibly stuttered.
        let l = Latency::from_samples(&[
            Duration::from_millis(13),
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(11),
        ])
        .expect("four samples");

        // |13-10| + |10-10| + |10-11| = 3 + 0 + 1 ms over 3 gaps, which is
        // 4_000_000ns / 3. `Duration`'s division truncates, so the answer is
        // 1_333_333ns and not the 1_333_000ns that `from_micros(1333)` would
        // write -- an earlier version of this assertion used the latter and had
        // been failing for as long as it existed.
        assert_eq!(l.jitter, Duration::from_nanos(1_333_333));

        // And the same samples in order would be a different reading entirely.
        let steady = Latency::from_samples(&[
            Duration::from_millis(10),
            Duration::from_millis(11),
            Duration::from_millis(12),
            Duration::from_millis(13),
        ])
        .expect("four samples");
        assert!(
            steady.jitter < l.jitter,
            "a steady line and a stuttering one must not report the same jitter"
        );
    }

    #[test]
    fn a_consistent_slow_line_has_almost_no_jitter() {
        // Jitter is not a synonym for bad. A rock-steady 200ms is still 200ms,
        // but it is a very different complaint from a link that swings 20ms to
        // 200ms, and a tool that cannot tell them apart will be ignored.
        let l = Latency::from_samples(&[
            Duration::from_millis(200),
            Duration::from_millis(201),
            Duration::from_millis(199),
            Duration::from_millis(200),
        ])
        .expect("four samples");
        assert_eq!(l.median, Duration::from_millis(200));
        assert!(
            l.jitter <= Duration::from_millis(2),
            "a steady line should not report jitter, got {:?}",
            l.jitter
        );
    }

    #[tokio::test]
    async fn every_level_reports_the_latency_it_was_measured_under() {
        // The whole reason the probe rides along with the lanes. A single idle
        // reading is a property of a quiet network; these are readings of a
        // busy one, and they are what the "under load" number is built from.
        let client = Trickle::new(4096);
        let mut samples = Vec::new();
        run(
            &*client,
            &config().with_max_connections(4),
            &Cancel::new(),
            |s| samples.push(s),
        )
        .await
        .expect("bytes");

        assert!(!samples.is_empty(), "progress is reported as it happens");

        let mut levels = 0;
        for s in &samples {
            match s.kind {
                // Only a finished level carries a latency, and it must carry one:
                // a level measured without being timed cannot report what it cost
                // to everything else on the network.
                SampleKind::Level => {
                    levels += 1;
                    assert!(
                        s.latency.is_some(),
                        "a level measured without timing it cannot report latency \
                         under load; {} connections measured nothing",
                        s.connections
                    );
                }
                // A live reading is a rate over the last quarter of a second. The
                // probe is asked once per level, not once per reading, so putting
                // a latency on every one of these would be either a lie or four
                // times the round trips.
                SampleKind::Live => assert!(
                    s.latency.is_none(),
                    "a live reading is a rate, not a measurement of the link's cost"
                ),
            }
        }

        assert!(levels > 0, "at least one level finished");
    }

    #[tokio::test]
    async fn a_level_emits_live_readings_before_its_own_average() {
        // The order is the evidence. A caller that receives the average first has
        // been given the conclusion before the quarter-seconds it was judged
        // from, and a chart built from that stream draws the wrong shape.
        let client = Trickle::new(4096);
        let mut samples = Vec::new();
        run(
            &*client,
            &config()
                .with_max_connections(1)
                .with_step(Duration::from_millis(600)),
            &Cancel::new(),
            |s| samples.push(s),
        )
        .await
        .expect("bytes");

        let first_level = samples
            .iter()
            .position(|s| s.kind == SampleKind::Level)
            .expect("the level finished");
        let live_before = samples[..first_level]
            .iter()
            .filter(|s| s.kind == SampleKind::Live)
            .count();

        assert!(
            live_before > 0,
            "a 600ms level must produce more than one reading"
        );
        assert_eq!(samples[first_level].kind, SampleKind::Level);
    }

    #[tokio::test]
    async fn live_readings_are_non_negative_and_never_run_away() {
        // A live reading is a difference of two running totals divided by the gap
        // between them. If either half is wrong the chart grows a spike to the top
        // of the axis and the result still looks plausible, which is exactly the
        // failure this guards.
        let client = Trickle::new(4096);
        let mut samples = Vec::new();
        run(
            &*client,
            &config()
                .with_max_connections(2)
                .with_step(Duration::from_millis(500)),
            &Cancel::new(),
            |s| samples.push(s),
        )
        .await
        .expect("bytes");

        for s in samples.iter().filter(|s| s.kind == SampleKind::Live) {
            assert!(s.bps.is_finite(), "a rate of {} is not a rate", s.bps);
            assert!(s.bps >= 0.0, "bytes only ever go up, got {}", s.bps);
            assert!(
                s.total_bytes <= crate::net::speedtest::BYTE_CEILING,
                "a reading claimed {} bytes against a {} ceiling",
                s.total_bytes,
                crate::net::speedtest::BYTE_CEILING
            );
        }
    }

    #[tokio::test]
    async fn a_missing_latency_does_not_cost_us_the_measurement() {
        // Plenty of origins answer GET and refuse HEAD. Declining to measure a
        // working link over that would be its own kind of dishonesty.
        struct HeadRefusing;

        #[async_trait]
        impl HttpClient for HeadRefusing {
            async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
                if req.method == Method::Head {
                    return Err(NetError::Status {
                        status: 405,
                        url: req.url,
                    });
                }
                Ok(RawResponse::full(
                    206,
                    Bytes::from(vec![0u8; 64 * 1024]),
                    req.url,
                    Headers::new([("Content-Length".to_string(), "65536".to_string())]),
                ))
            }
        }

        let result = run(&HeadRefusing, &config(), &Cancel::new(), |_| {})
            .await
            .expect("bytes still arrive over GET");
        assert_eq!(result.idle, None);
        assert!(result.bps > 0.0, "throughput is still measured");
    }

    #[test]
    fn the_ramp_stops_when_more_connections_stop_helping() {
        // A server that throttles aggressive clients is common enough that
        // "keep doubling until it gets worse" is a real failure mode, and
        // reporting that dip as someone's connection speed would be a lie.
        assert_eq!(
            judge(None, 0.0),
            Ramp::Improve,
            "the first level has somewhere to go"
        );

        let best = (100.0, 1);
        assert_eq!(judge(Some(best), 120.0), Ramp::Improve, "a real gain pays");
        assert_eq!(
            judge(Some(best), 110.0),
            Ramp::Improve,
            "10% is past the floor"
        );
        assert_eq!(
            judge(Some(best), 104.0),
            Ramp::Stop,
            "4% is inside the noise"
        );
        assert_eq!(
            judge(Some(best), 100.0),
            Ramp::Stop,
            "no change is not progress"
        );
        assert_eq!(
            judge(Some(best), 50.0),
            Ramp::Stop,
            "halving is not progress"
        );
    }

    #[test]
    fn the_threshold_has_to_bite_just_above_five_percent() {
        // Otherwise "keep opening connections" quietly becomes "never stop".
        let best = (100.0, 1);
        assert_eq!(judge(Some(best), 105.0), Ramp::Stop);
        assert_eq!(judge(Some(best), 106.0), Ramp::Improve);
    }

    #[test]
    fn a_level_never_improves_on_itself() {
        // The regression this exists for. An earlier ramp recorded the new result
        // into `best` and *then* asked whether to continue, so every level was
        // compared against itself -- never an improvement -- and the ramp always
        // stopped at one connection. The feature then reported the single-stream
        // speed, which is the one number it was built to improve on.
        assert_eq!(
            judge(Some((50.0, 1)), 50.0),
            Ramp::Stop,
            "identical to the best is not an improvement, so asking after recording would always stop"
        );
    }

    #[test]
    fn a_server_that_gets_slower_is_not_reported_as_the_users_speed() {
        // The failure this guards is specific and embarrassing: a server that
        // throttles by connection count is common, so "double until it gets
        // worse" reaches a level that is worse, and reporting that level's number
        // would tell someone their gigabit line is slower than their old one.
        let levels = [50.0, 48.0, 30.0];
        let mut best: Option<(f64, usize)> = None;
        let mut last = 0.0;

        for (i, bps) in levels.iter().copied().enumerate() {
            last = bps;
            match judge(best, bps) {
                Ramp::Improve => best = Some((bps, 1 << i)),
                Ramp::Stop => break,
            }
        }

        let (reported, at) = best.expect("the first level always counts");
        assert_eq!(at, 1, "the best level is the one that actually helped");
        assert!(
            reported > last,
            "reported {reported}, but the last level tried was {last}"
        );
    }

    #[test]
    fn a_server_that_gets_faster_keeps_being_climbed() {
        // The other direction: a line with headroom has to actually be climbed,
        // or the whole ramp is decoration.
        let levels = [50.0, 120.0, 130.0];
        let mut best: Option<(f64, usize)> = None;
        let mut visited = Vec::new();

        for (i, bps) in levels.iter().copied().enumerate() {
            visited.push(1 << i);
            match judge(best, bps) {
                Ramp::Improve => best = Some((bps, 1 << i)),
                Ramp::Stop => break,
            }
        }

        assert_eq!(visited, vec![1, 2, 4], "every level was tried");
        assert_eq!(
            best.expect("a result"),
            (130.0, 4),
            "the best level is the one reported"
        );
    }

    #[test]
    fn the_default_target_is_https_and_carries_no_credentials() {
        // A default that embedded a key, a token, or a tracking parameter would
        // mean the app phones a vendor the first time anyone opens the feature.
        let url = url::Url::parse(DEFAULT_TARGET).expect("the default is a valid URL");
        assert_eq!(url.scheme(), "https");
        assert!(url.username().is_empty(), "no credentials in the default");
        assert!(url.password().is_none(), "no credentials in the default");
    }

    #[test]
    fn the_default_target_carries_no_tracking_parameter() {
        // Anything here that identifies a machine rather than naming a file is
        // exactly the behaviour this app exists to refuse. Asserted on the
        // default so a future edit cannot quietly introduce one.
        let url = url::Url::parse(DEFAULT_TARGET).expect("valid");
        for pair in url.query_pairs() {
            let key = pair.0.to_ascii_lowercase();
            assert!(
                !matches!(key.as_str(), "id" | "uid" | "uuid" | "token" | "sid" | "cb"),
                "the default target carries an identifying parameter: {key}"
            );
        }
    }

    #[test]
    fn the_connection_cap_is_raised_rather_than_allowed_to_be_zero() {
        let cfg = SpeedTestConfig::new("https://example.invalid/x").with_max_connections(0);
        assert_eq!(cfg.max_connections, 1);
    }

    #[test]
    fn a_zero_length_step_is_raised() {
        // A zero-length step measures the time to open a socket and calls it
        // throughput. Division by zero is the other thing it does.
        let cfg = SpeedTestConfig::new("https://example.invalid/x").with_step(Duration::ZERO);
        assert!(cfg.step >= Duration::from_millis(1));
    }

    #[test]
    fn an_unparseable_target_is_still_attributed_to_something() {
        assert_eq!(url_host("not a url"), "not a url");
        assert_eq!(url_host("https://a.invalid/x"), "a.invalid");
        assert_eq!(url_host("https://a.invalid:8443/x"), "a.invalid");
    }

    #[test]
    fn the_budget_refuses_work_it_cannot_pay_for() {
        // Checked before a request is opened, not only after a chunk lands, so a
        // lane cannot start work it has no allowance for. This is the difference
        // between a budget and a suggestion.
        let b = Budget::new(1000);
        assert!(b.has_room(), "an untouched budget has allowance left");

        b.charge(999);
        assert!(b.has_room(), "one byte of allowance remains");

        b.charge(1);
        assert!(
            !b.has_room(),
            "an exhausted budget must start nothing -- the inclusive version \
             said yes here, which is how a ceiling stops being a ceiling"
        );
    }

    #[test]
    fn a_zero_budget_starts_immediately_and_nobody() {
        // A caller who asks for no bytes should get no bytes and a clean error,
        // not an open-ended pull.
        let b = Budget::new(0);
        assert!(!b.has_room());
        b.charge(1);
        assert!(!b.has_room(), "and it cannot come back");
    }

    #[test]
    fn charging_the_budget_accumulates() {
        let b = Budget::new(u64::MAX);
        assert_eq!(b.charge(10), 10);
        assert_eq!(b.charge(5), 15);
        assert_eq!(b.spent(), 15);
    }

    #[tokio::test]
    async fn a_stopped_run_still_reports_what_it_measured() {
        // Someone who clicks stop after two seconds did not want nothing, they
        // wanted the answer sooner. Discarding a partial measurement because it
        // was incomplete would be answering a question nobody asked.
        let cancel = Cancel::new();
        let client = Trickle::new(64 * 1024);
        let cfg = config().with_max_connections(8);

        // Raise the flag from the sample callback, which is the earliest moment
        // a real caller can react to progress.
        let stopper = cancel.clone();
        let result = run(&*client, &cfg, &cancel, move |_| stopper.cancel())
            .await
            .expect("bytes arrived before the stop");

        assert!(result.stopped_early, "the run should know it was cut short");
        assert!(result.bps > 0.0, "and still have a number worth showing");
        assert_eq!(result.connections, 1, "it stopped at the first level");
    }

    #[tokio::test]
    async fn a_finished_run_is_not_reported_as_cut_short() {
        // The opposite mistake, and just as bad: telling someone their real
        // result is "partial" when it is not makes the number feel unreliable.
        let client = Trickle::new(64 * 1024);
        let result = run(&*client, &config(), &Cancel::new(), |_| {})
            .await
            .expect("bytes");
        assert!(
            !result.stopped_early,
            "a run that was never cancelled finished on its own"
        );
    }

    #[tokio::test]
    async fn cancelling_before_the_run_starts_still_produces_a_number_or_an_error() {
        // Raised up front, so no bytes ever move. The contract is that this is
        // not a hang and not a panic: it is either the error path or a result,
        // never silence.
        let cancel = Cancel::new();
        cancel.cancel();
        let client = Trickle::new(64 * 1024);

        let outcome = run(&*client, &config(), &cancel, |_| {}).await;
        assert!(
            outcome.is_err() || outcome.as_ref().is_ok_and(|r| r.stopped_early),
            "a pre-cancelled run must resolve, not hang"
        );
    }

    /// A client that will not let a download start until an upload has, so a run
    /// that sequenced the directions would deadlock and a run that overlaps them
    /// finishes. The proof is structural rather than a stopwatch.
    struct Rendezvous {
        upload_started: AtomicBool,
        notify: tokio::sync::Notify,
    }

    impl Rendezvous {
        fn new() -> Self {
            Self {
                upload_started: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
            }
        }
    }

    #[async_trait]
    impl HttpClient for Rendezvous {
        async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
            match req.method {
                Method::Post => {
                    self.upload_started.store(true, Ordering::Release);
                    // `notify_one` leaves a permit behind when no one is waiting
                    // yet, so the signal cannot be lost to the order the two
                    // directions happen to be polled in.
                    self.notify.notify_one();
                    Ok(RawResponse::full(
                        200,
                        Bytes::from_static(b"ok"),
                        req.url,
                        Headers::new([("Content-Length".to_string(), "2".to_string())]),
                    ))
                }
                Method::Get => {
                    if !self.upload_started.load(Ordering::Acquire) {
                        self.notify.notified().await;
                    }
                    Ok(RawResponse::full(
                        206,
                        Bytes::from(vec![0u8; 64 * 1024]),
                        req.url,
                        Headers::new([("Content-Length".to_string(), "65536".to_string())]),
                    ))
                }
                Method::Head => Ok(RawResponse::full(
                    200,
                    Bytes::new(),
                    req.url,
                    Headers::new([]),
                )),
            }
        }
    }

    #[tokio::test]
    async fn the_two_directions_are_measured_at_the_same_time() {
        // The download cannot proceed until an upload has been issued, so this
        // test hangs the moment the engine goes back to measuring one direction
        // after the other. That is a stronger statement than timing the run.
        let client = Rendezvous::new();
        let cfg = SpeedTestConfig {
            url: "https://example.invalid/down".to_string(),
            upload_url: Some("https://example.invalid/up".to_string()),
            max_connections: 2,
            max_upload_connections: 2,
            step: Duration::from_millis(50),
            byte_ceiling: BYTE_CEILING,
            upload_byte_ceiling: UPLOAD_BYTE_CEILING,
            max_duration: Duration::from_secs(5),
        };

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run(&client, &cfg, &Cancel::new(), |_| {}),
        )
        .await
        .expect("a run that overlaps the two directions must not deadlock")
        .expect("both directions served bytes");

        assert!(result.bps > 0.0, "the download direction was measured");
        assert!(
            result.upload.is_some(),
            "the upload direction was measured in the same pass"
        );
    }

    #[tokio::test]
    async fn an_origin_that_refuses_a_body_still_measures_the_download() {
        // 405-on-POST is common. It must not turn a working download
        // measurement into a failure, and it must not be reported as an upload
        // of zero.
        struct RefusesUpload;

        #[async_trait]
        impl HttpClient for RefusesUpload {
            async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
                if req.method == Method::Post {
                    return Ok(RawResponse::empty(405, req.url, Headers::new([])));
                }
                Ok(RawResponse::full(
                    206,
                    Bytes::from(vec![0u8; 32 * 1024]),
                    req.url,
                    Headers::new([("Content-Length".to_string(), "32768".to_string())]),
                ))
            }
        }

        let cfg = SpeedTestConfig {
            upload_url: Some("https://example.invalid/up".to_string()),
            ..config()
        };
        let result = run(&RefusesUpload, &cfg, &Cancel::new(), |_| {})
            .await
            .expect("the download is still a measurement");

        assert!(result.bps > 0.0, "the download was measured");
        assert!(
            result.upload.is_none(),
            "an origin that will not take a body is not an upload of zero"
        );
    }

    #[test]
    fn the_cancel_flag_is_shared_between_clones() {
        // The whole reason this is an `Arc` rather than a `bool`: a button in
        // application state and the run in flight have to be looking at the same
        // flag, or "stop" is decoration.
        let a = Cancel::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled(), "one handle is enough to stop the run");
        // And idempotent, because a UI can easily raise it twice.
        a.cancel();
        assert!(a.is_cancelled());
    }
    #[test]
    fn mbps_uses_the_unit_people_quote() {
        let r = SpeedResult {
            bps: 940_000_000.0,
            connections: 8,
            idle: None,
            loaded: None,
            total_bytes: 1,
            host: "x".to_string(),
            upload: None,
            stopped_early: false,
        };
        assert!((r.mbps() - 940.0).abs() < f64::EPSILON);
    }

    #[test]
    fn upload_mbps_uses_the_same_unit() {
        let u = UploadResult {
            bps: 42_400_000.0,
            connections: 2,
            latency: None,
            total_bytes: 1,
        };
        assert!((u.mbps() - 42.4).abs() < 1e-9);
    }
}
