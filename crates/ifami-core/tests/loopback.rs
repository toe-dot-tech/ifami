//! End-to-end tests against a real socket.
//!
//! Everything here runs against a hand-rolled HTTP origin on `127.0.0.1`, using
//! the production `ReqwestClient`. No public network, no mocks, no scripted
//! responses — these tests exist to catch the class of bug that only appears
//! when a real HTTP stack is on the other end of a real `TcpStream`.
//!
//! What is deliberately *not* covered here: the public internet, TLS (the
//! fixture is plaintext, because a test certificate authority is a supply-chain
//! dependency we would rather not add), and anything requiring a JS runtime.
//! Those are the boundaries in `docs/SCOPE.md`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ifami_core::download::{Manager, ManagerOptions, Store, TaskState};
use ifami_core::error::{Error, ResolveError};
use ifami_core::net::speedtest::{self, MAX_UPLOAD_CONNECTIONS, UPLOAD_BYTE_CEILING};
use ifami_core::net::{Cancel, ReqwestClient, SpeedTestConfig};

use common::{payload, Fixture, Hit, Route};

/// A manager over a temporary queue file.
struct Harness {
    manager: Manager,
}

impl Harness {
    async fn new(options: ManagerOptions) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path().join("queue.json"));
        let client = Arc::new(ReqwestClient::new().expect("build reqwest client"));
        let manager = Manager::with_options(store, client, options).expect("load manager");
        Harness { manager }
    }

    /// Every file under `dir`, sorted by path.
    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("read destination")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        out.sort();
        out
    }

    /// The finished output files in `dir`, i.e. everything that is not a
    /// leftover `.part`.
    fn outputs(dir: &Path) -> Vec<PathBuf> {
        Harness::files(dir)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e != "part"))
            .collect()
    }

    /// Leftover partial files.
    fn partials(dir: &Path) -> Vec<PathBuf> {
        Harness::files(dir)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "part"))
            .collect()
    }

    /// Read the single finished output in `dir`.
    fn finished_output(dir: &Path) -> Vec<u8> {
        let outputs = Harness::outputs(dir);
        assert_eq!(
            outputs.len(),
            1,
            "expected exactly one finished output in {dir:?}, found {outputs:?}"
        );
        std::fs::read(&outputs[0]).expect("read output")
    }

    /// Read the bytes currently on disk for a single task: the finished output
    /// if it was finalised, otherwise the `.part` it is still being written to.
    ///
    /// The two are mutually exclusive by construction, so this is unambiguous.
    fn bytes_on_disk(dir: &Path) -> Vec<u8> {
        let partials = Harness::partials(dir);
        if partials.len() == 1 {
            return std::fs::read(&partials[0]).expect("read partial");
        }
        Harness::finished_output(dir)
    }
}

#[tokio::test]
async fn a_direct_url_produces_exactly_the_served_bytes() {
    let fixture = Fixture::start().await;
    let body = payload(64 * 1024 + 7);
    fixture.route(
        "/clip.mp4",
        Route::bytes(body.clone(), "video/mp4").resumable(),
    );

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let task = harness
        .manager
        .add_url(&fixture.url("/clip.mp4"))
        .await
        .expect("resolve and queue");
    harness.manager.wait_idle().await;

    let done = harness.manager.get(&task.id).expect("task still present");
    assert_eq!(
        done.state,
        TaskState::Completed,
        "error: {:?}",
        done.last_error
    );
    assert_eq!(done.total_bytes, Some(body.len() as u64));
    assert_eq!(Harness::finished_output(destination.path()), body);

    // The partial file must be gone. Leaving it behind is how a download
    // manager fills a user's disk with gigabytes of nothing.
    let leftovers = Harness::partials(destination.path());
    assert!(
        leftovers.is_empty(),
        "left partial files behind: {leftovers:?}"
    );
}

#[tokio::test]
async fn a_transfer_cut_mid_stream_resumes_to_the_correct_bytes() {
    // The headline claim. A connection that dies halfway must cost the user
    // the bytes already written, not the whole file.
    //
    // Sizes cross `FLUSH_INTERVAL` so the interrupted run has already flushed
    // at least once by the time the socket dies; otherwise this would only
    // prove the final flush survived, which is the easy case.
    const TOTAL: usize = 3 * 1024 * 1024;
    const CUT: usize = 3 * 1024 * 1024 / 2;

    let fixture = Fixture::start().await;
    let body = payload(TOTAL);
    fixture.route(
        "/clip.mp4",
        Route::bytes(body.clone(), "video/mp4")
            .resumable()
            .cut_after(CUT),
    );

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let task = harness
        .manager
        .add_url(&fixture.url("/clip.mp4"))
        .await
        .expect("resolve and queue");
    harness.manager.wait_idle().await;

    let stalled = harness.manager.get(&task.id).unwrap();
    assert_ne!(
        stalled.state,
        TaskState::Completed,
        "a truncated transfer must not be reported as complete"
    );

    // Whatever arrived before the socket died must be on disk and must be a
    // prefix of the real file. An exact length is not asserted: the cut lands
    // on a server-side boundary and the client may see it at any chunk edge.
    let partial = Harness::bytes_on_disk(destination.path());
    assert!(
        !partial.is_empty() && partial.len() < body.len(),
        "expected a non-empty short partial, got {} of {} bytes",
        partial.len(),
        body.len()
    );
    assert_eq!(
        &body[..partial.len()],
        &partial[..],
        "the partial must be a byte-exact prefix of the source"
    );
    assert!(
        Harness::outputs(destination.path()).is_empty(),
        "an unfinished transfer must not leave a file at the final name"
    );

    // Now stop cutting the connection and resume.
    fixture.route(
        "/clip.mp4",
        Route::bytes(body.clone(), "video/mp4").resumable(),
    );
    harness.manager.resume(&task.id).expect("resume");
    harness.manager.wait_idle().await;

    let done = harness.manager.get(&task.id).unwrap();
    assert_eq!(
        done.state,
        TaskState::Completed,
        "error: {:?}",
        done.last_error
    );
    assert_eq!(
        Harness::finished_output(destination.path()),
        body,
        "resumed file must be byte-identical to the source"
    );

    // Prove it actually resumed rather than silently starting over: some
    // request must have asked for a non-zero offset.
    let ranges: Vec<u64> = fixture
        .hits_for("/clip.mp4")
        .iter()
        .filter_map(|h| h.range_start())
        .collect();
    assert!(
        ranges.iter().any(|&start| start > 0),
        "no request asked to resume from an offset; ranges were {ranges:?}"
    );
}

#[tokio::test]
async fn an_origin_that_ignores_range_restarts_instead_of_corrupting() {
    // The nastiest real-world case: the origin advertises `Accept-Ranges`,
    // then answers a ranged request with `200 OK` and the whole file. A client
    // that blindly appends produces a file that is two copies of the movie.
    let fixture = Fixture::start().await;
    let body = payload(32 * 1024);
    fixture.route(
        "/clip.mp4",
        Route::bytes(body.clone(), "video/mp4").refusing_range(),
    );

    let destination = tempfile::tempdir().unwrap();
    // A stale partial holding entirely the wrong bytes, as an interrupted run of
    // some other transfer would leave behind.
    std::fs::write(destination.path().join("clip.mp4.part"), payload(4096)).unwrap();

    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let task = harness
        .manager
        .add_url(&fixture.url("/clip.mp4"))
        .await
        .expect("resolve and queue");
    harness.manager.wait_idle().await;

    let done = harness.manager.get(&task.id).unwrap();
    assert_eq!(
        done.state,
        TaskState::Completed,
        "error: {:?}",
        done.last_error
    );
    assert_eq!(
        Harness::finished_output(destination.path()),
        body,
        "an origin that ignores Range must cause a restart, not a concatenation"
    );
}

#[tokio::test]
async fn an_hls_stream_with_an_init_segment_becomes_one_playable_file() {
    let fixture = Fixture::start().await;

    let init = payload(1024);
    let first = payload(2048);
    let second = payload(512);
    fixture.route(
        "/init.mp4",
        Route::bytes(init.clone(), "video/mp4").resumable(),
    );
    fixture.route(
        "/s0.m4s",
        Route::bytes(first.clone(), "video/iso.segment").resumable(),
    );
    fixture.route(
        "/s1.m4s",
        Route::bytes(second.clone(), "video/iso.segment").resumable(),
    );
    fixture.route(
        "/v.m3u8",
        Route::text(
            "#EXTM3U\n\
             #EXT-X-VERSION:7\n\
             #EXT-X-TARGETDURATION:4\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n\
             #EXT-X-MAP:URI=\"init.mp4\"\n\
             #EXTINF:4.0,\n\
             s0.m4s\n\
             #EXTINF:2.0,\n\
             s1.m4s\n\
             #EXT-X-ENDLIST\n",
            "application/vnd.apple.mpegurl",
        ),
    );

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let task = harness
        .manager
        .add_url(&fixture.url("/v.m3u8"))
        .await
        .expect("resolve and queue");
    harness.manager.wait_idle().await;

    let done = harness.manager.get(&task.id).unwrap();
    assert_eq!(
        done.state,
        TaskState::Completed,
        "error: {:?}",
        done.last_error
    );

    // fMP4 only plays if the initialisation segment comes first, exactly once.
    let mut expected = init.clone();
    expected.extend_from_slice(&first);
    expected.extend_from_slice(&second);
    assert_eq!(Harness::finished_output(destination.path()), expected);

    let paths: Vec<String> = fixture.hits().into_iter().map(|h| h.path).collect();
    assert_eq!(
        paths,
        vec!["/v.m3u8", "/init.mp4", "/s0.m4s", "/s1.m4s"],
        "every segment must be fetched exactly once, in order"
    );
}

#[tokio::test]
async fn a_dash_manifest_downloads_the_representation() {
    // No audio adaptation set anywhere, so the video rendition is a complete
    // presentation and is queued. See ADR-0008.
    let fixture = Fixture::start().await;

    let body = payload(8192);
    fixture.route(
        "/dash/v.mp4",
        Route::bytes(body.clone(), "video/mp4").resumable(),
    );
    fixture.route(
        "/dash/manifest.mpd",
        Route::text(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT10S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <Representation id="v0" bandwidth="800000">
        <BaseURL>v.mp4</BaseURL>
        <SegmentList>
          <SegmentURL media="v.mp4" mediaRange="0-8191"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#,
            "application/dash+xml",
        ),
    );

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let task = harness
        .manager
        .add_url(&fixture.url("/dash/manifest.mpd"))
        .await
        .expect("resolve and queue");
    harness.manager.wait_idle().await;

    let done = harness.manager.get(&task.id).unwrap();
    assert_eq!(
        done.state,
        TaskState::Completed,
        "error: {:?}",
        done.last_error
    );
    assert_eq!(Harness::finished_output(destination.path()), body);
}

#[tokio::test]
async fn a_gated_source_is_refused_rather_than_retried() {
    // 401 and 403 are a decision, not a broken link. See `docs/SCOPE.md`.
    for status in [401u16, 403] {
        let fixture = Fixture::start().await;
        fixture.route(
            "/members-only.mp4",
            Route::bytes(payload(1024), "video/mp4").status(status),
        );

        let destination = tempfile::tempdir().unwrap();
        let harness = Harness::new(ManagerOptions {
            destination: destination.path().to_path_buf(),
            ..ManagerOptions::default()
        })
        .await;

        let err = harness
            .manager
            .add_url(&fixture.url("/members-only.mp4"))
            .await
            .expect_err("a gated source must not resolve");
        assert!(
            matches!(err, Error::Resolve(ResolveError::AuthRequired)),
            "status {status} should be AuthRequired, got {err:?}"
        );
        assert!(
            !err.is_retryable(),
            "a gate must not be retryable or a client will hammer the origin"
        );
        assert_eq!(harness.manager.tasks().len(), 0, "nothing should be queued");
    }
}

#[tokio::test]
async fn a_media_extension_serving_html_is_refused() {
    let fixture = Fixture::start().await;
    fixture.route(
        "/watch.mp4",
        Route::text(
            "<html><body>Sign in</body></html>",
            "text/html; charset=utf-8",
        ),
    );

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let err = harness
        .manager
        .add_url(&fixture.url("/watch.mp4"))
        .await
        .expect_err("an HTML page behind a .mp4 path is not media");
    assert!(
        matches!(err, Error::Resolve(ResolveError::NoMedia { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_dash_manifest_with_audio_queues_the_audio_rather_than_a_silent_file() {
    // The other half of ADR-0008: when audio exists, the video rendition is not
    // queued, because handing someone a silent file and calling it the video is
    // worse than handing them the track that does play.
    let fixture = Fixture::start().await;

    let audio = payload(4096);
    fixture.route(
        "/dash/a.mp4",
        Route::bytes(audio.clone(), "audio/mp4").resumable(),
    );
    fixture.route(
        "/dash/manifest.mpd",
        Route::text(
            r#"<MPD><Period>
  <AdaptationSet contentType="video"><Representation id="v" bandwidth="800000">
    <SegmentList><SegmentURL media="v.mp4" mediaRange="0-8191"/></SegmentList>
  </Representation></AdaptationSet>
  <AdaptationSet contentType="audio"><Representation id="a" bandwidth="128000">
    <BaseURL>a.mp4</BaseURL>
  </Representation></AdaptationSet>
</Period></MPD>"#,
            "application/dash+xml",
        ),
    );

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        ..ManagerOptions::default()
    })
    .await;

    let task = harness
        .manager
        .add_url(&fixture.url("/dash/manifest.mpd"))
        .await
        .expect("a manifest with audio still resolves something downloadable");
    harness.manager.wait_idle().await;

    let done = harness.manager.get(&task.id).unwrap();
    assert_eq!(
        done.state,
        TaskState::Completed,
        "error: {:?}",
        done.last_error
    );
    assert_eq!(
        Harness::finished_output(destination.path()),
        audio,
        "the audio rendition must be queued, not the silent video"
    );

    assert!(
        fixture.hits_for("/dash/v.mp4").is_empty(),
        "the video-only rendition must not be fetched"
    );
}

#[tokio::test]
async fn a_corrupt_queue_file_still_starts() {
    // A download manager that refuses to launch because of a bad JSON byte is
    // worse than one that lost a task.
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue.json");
    std::fs::write(&queue, b"{ this is not json").unwrap();

    let destination = tempfile::tempdir().unwrap();
    let client = Arc::new(ReqwestClient::new().unwrap());
    let manager = Manager::with_options(
        Store::new(&queue),
        client,
        ManagerOptions {
            destination: destination.path().to_path_buf(),
            ..ManagerOptions::default()
        },
    )
    .expect("a corrupt queue must not prevent startup");

    assert!(manager.tasks().is_empty());
    assert!(
        manager.recovery_note().is_some(),
        "the loss must be reported"
    );

    // And it must still be usable afterwards.
    let fixture = Fixture::start().await;
    let body = payload(2048);
    fixture.route(
        "/ok.mp4",
        Route::bytes(body.clone(), "video/mp4").resumable(),
    );
    let task = manager.add_url(&fixture.url("/ok.mp4")).await.unwrap();
    manager.wait_idle().await;
    assert_eq!(manager.get(&task.id).unwrap().state, TaskState::Completed);
}

#[tokio::test]
async fn a_queued_task_survives_a_restart_and_resumes() {
    // The whole point of persisting: close the manager mid-transfer, reopen it,
    // and carry on from the bytes on disk rather than from zero.
    const TOTAL: usize = 3 * 1024 * 1024;
    const CUT: usize = 3 * 1024 * 1024 / 2;
    let fixture = Fixture::start().await;
    let body = payload(TOTAL);
    fixture.route(
        "/clip.mp4",
        Route::bytes(body.clone(), "video/mp4")
            .resumable()
            .cut_after(CUT),
    );

    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue.json");
    let destination = dir.path().join("out");
    std::fs::create_dir_all(&destination).unwrap();
    let options = || ManagerOptions {
        destination: destination.clone(),
        ..ManagerOptions::default()
    };

    let first = {
        let client = Arc::new(ReqwestClient::new().unwrap());
        let m = Manager::with_options(Store::new(&queue), client, options()).unwrap();
        let task = m.add_url(&fixture.url("/clip.mp4")).await.unwrap();
        m.wait_idle().await;
        task
    };

    assert!(queue.exists(), "the queue file must have been written");

    // The origin recovers.
    fixture.route(
        "/clip.mp4",
        Route::bytes(body.clone(), "video/mp4").resumable(),
    );

    let second = {
        let client = Arc::new(ReqwestClient::new().unwrap());
        let m = Manager::with_options(Store::new(&queue), client, options()).unwrap();
        let reloaded = m.get(&first.id).expect("task survived the restart");
        assert_ne!(reloaded.state, TaskState::Completed);
        m.resume(&first.id).expect("resume after restart");
        m.wait_idle().await;
        m
    };

    assert_eq!(second.get(&first.id).unwrap().state, TaskState::Completed);
    assert_eq!(Harness::finished_output(&destination), body);

    let resumes: Vec<u64> = fixture
        .hits_for("/clip.mp4")
        .iter()
        .filter(|h| h.was_range())
        .filter_map(|h| h.range_start())
        .filter(|&start| start > 0)
        .collect();
    assert!(
        !resumes.is_empty(),
        "the reopened manager started from scratch instead of resuming"
    );
}

#[tokio::test]
async fn several_tasks_download_concurrently_and_all_finish() {
    let fixture = Fixture::start().await;
    let mut expected = Vec::new();
    for i in 0..5 {
        let body = payload(4 * 1024 + i * 137);
        fixture.route(
            &format!("/v{i}.mp4"),
            Route::bytes(body.clone(), "video/mp4").resumable(),
        );
        expected.push((format!("/v{i}.mp4"), body));
    }

    let destination = tempfile::tempdir().unwrap();
    let harness = Harness::new(ManagerOptions {
        destination: destination.path().to_path_buf(),
        concurrency: 5,
        ..ManagerOptions::default()
    })
    .await;

    for (path, _) in &expected {
        harness.manager.add_url(&fixture.url(path)).await.unwrap();
    }
    harness.manager.wait_idle().await;

    for task in harness.manager.tasks() {
        assert_eq!(
            task.state,
            TaskState::Completed,
            "{} failed: {:?}",
            task.url,
            task.last_error
        );
    }

    let on_disk: Vec<Vec<u8>> = Harness::outputs(destination.path())
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    for (_, body) in &expected {
        assert!(
            on_disk.contains(body),
            "a downloaded file did not match any served payload"
        );
    }
    assert_eq!(
        on_disk.len(),
        expected.len(),
        "unexpected extra or missing files"
    );
}

/* ------------------------------------------------------------------ */
/* speed test                                                          */
/* ------------------------------------------------------------------ */

// The unit tests for `speedtest` deliberately avoid the clock, which means
// they cannot answer the one question that matters most here: does the ramp
// actually open parallel connections, and does it ask each one for its own
// bytes? Those are only observable against a real origin, so they are checked
// here, on the loopback fixture, where the number of requests and their
// `Range` headers are both recorded.

/// A configuration short enough to keep the suite quick but long enough for the
/// ramp to take more than one level.
fn speed_config(url: String) -> SpeedTestConfig {
    SpeedTestConfig {
        url,
        max_connections: 4,
        step: Duration::from_millis(120),
        byte_ceiling: 4 * 1024 * 1024,
        max_duration: Duration::from_secs(10),
        // Download only. Every assertion in this block is about the download
        // direction, and giving each of them a second ramp to wait through would
        // make the suite slower without making any of them truer. The upload
        // direction is exercised by its own tests, further down.
        upload_url: None,
        max_upload_connections: MAX_UPLOAD_CONNECTIONS,
        upload_byte_ceiling: UPLOAD_BYTE_CEILING,
    }
}

#[tokio::test]
async fn a_speed_test_measures_a_real_origin() {
    let fixture = Fixture::start().await;
    // Far more than the ceiling could ever pull, so the run is bounded by the
    // budget rather than by running out of file.
    fixture.route(
        "/bulk",
        Route::bytes(payload(2 * 1024 * 1024), "application/octet-stream").resumable(),
    );

    let client = ReqwestClient::new().unwrap();
    let mut samples = Vec::new();
    let result = speedtest::run(
        &client,
        &speed_config(fixture.url("/bulk")),
        &Cancel::new(),
        |s| samples.push(s),
    )
    .await
    .expect("the fixture serves bytes");

    assert!(
        result.bps > 0.0,
        "a completed transfer has positive throughput"
    );
    assert!(
        result.idle.is_some(),
        "the fixture answers HEAD, so latency should be measured"
    );
    assert!(
        result.loaded.is_some(),
        "latency is timed while the link is busy, not only at rest"
    );
    assert!(!samples.is_empty(), "progress is reported as it happens");
    assert_eq!(
        result.host, "127.0.0.1",
        "the result names where it came from"
    );
}

#[tokio::test]
async fn a_speed_test_actually_opens_more_than_one_connection() {
    // The whole premise of the feature. A ramp that silently stayed on one
    // connection would pass every other test here and still report the one
    // number it was built to improve on.
    let fixture = Fixture::start().await;
    fixture.route(
        "/bulk",
        Route::bytes(payload(2 * 1024 * 1024), "application/octet-stream").resumable(),
    );

    let client = ReqwestClient::new().unwrap();
    speedtest::run(
        &client,
        &speed_config(fixture.url("/bulk")),
        &Cancel::new(),
        |_| {},
    )
    .await
    .expect("the fixture serves bytes");

    let gets: Vec<Hit> = fixture
        .hits_for("/bulk")
        .into_iter()
        .filter(|h| h.method == "GET")
        .collect();

    assert!(gets.len() >= 2, "expected several GETs, saw {}", gets.len());

    // And they must not all be asking for the same bytes.
    let mut starts: Vec<u64> = gets.iter().filter_map(Hit::range_start).collect();
    starts.sort_unstable();
    starts.dedup();
    assert!(
        starts.len() >= 2,
        "every connection asked for the same offset: {starts:?}"
    );
}

#[tokio::test]
async fn a_speed_test_respects_its_byte_ceiling() {
    // The bound that protects someone's data allowance, checked end to end
    // rather than only against a scripted client. The allowance is one chunk per
    // lane because a lane can only notice after a chunk has landed.
    let fixture = Fixture::start().await;
    fixture.route(
        "/bulk",
        Route::bytes(payload(2 * 1024 * 1024), "application/octet-stream").resumable(),
    );

    let cfg = SpeedTestConfig {
        byte_ceiling: 256 * 1024,
        ..speed_config(fixture.url("/bulk"))
    };

    let client = ReqwestClient::new().unwrap();
    let result = speedtest::run(&client, &cfg, &Cancel::new(), |_| {})
        .await
        .expect("the fixture serves bytes");

    let overshoot = cfg.byte_ceiling + 128 * 1024 * (cfg.max_connections as u64);
    assert!(
        result.total_bytes <= overshoot,
        "pulled {} bytes against a {} byte budget (bound {})",
        result.total_bytes,
        cfg.byte_ceiling,
        overshoot
    );
}

#[tokio::test]
async fn a_speed_test_against_a_missing_origin_reports_failure() {
    // No number, no zero, no quiet success. Someone with no internet is in a
    // situation, not a result.
    let fixture = Fixture::start().await;
    // Never routed.
    let client = ReqwestClient::new().unwrap();

    let err = speedtest::run(
        &client,
        &speed_config(fixture.url("/nothing")),
        &Cancel::new(),
        |_| {},
    )
    .await
    .expect_err("a 404 is not a measurement");

    assert!(
        err.to_string().contains("no data to measure"),
        "unhelpful error: {err}"
    );
}

#[tokio::test]
async fn a_speed_test_does_not_write_anything_to_disk() {
    // A speed test is not a download. If it left `.part` files behind it would
    // be quietly filling someone's disk with data nobody asked to keep.
    let fixture = Fixture::start().await;
    fixture.route(
        "/bulk",
        Route::bytes(payload(1024 * 1024), "application/octet-stream").resumable(),
    );

    let destination = tempfile::tempdir().unwrap();
    let client = ReqwestClient::new().unwrap();
    speedtest::run(
        &client,
        &speed_config(fixture.url("/bulk")),
        &Cancel::new(),
        |_| {},
    )
    .await
    .expect("bytes");

    let left: Vec<_> = std::fs::read_dir(destination.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.is_empty(), "the speed test wrote {left:?}");
}
