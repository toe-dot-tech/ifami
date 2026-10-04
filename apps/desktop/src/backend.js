// Data layer.
//
// Two backends behind one interface:
//   - `tauriBackend`  talks to the Rust core over IPC (the real product)
//   - `mockBackend`  a scripted in-page queue with a real speed test (so the UI
//                    can be opened, reviewed and demoed without a build of the
//                    shell)
//
// Task shape mirrors `ifami_core::model::TaskSnapshot`. The `events` array is
// the flight recorder: it is the thing that makes the resume logic visible
// rather than merely claimed.
//
// The clipboard watcher is the one place this file reads something the user
// did not type. Its privacy rules live in `link.js` and `clipboard.js`; the
// only thing each backend decides is *how* to read, and the Tauri side
// enforces the window-focus check in Rust because that is the only place it
// cannot be forgotten.

import { watchClipboard } from "./clipboard.js";
import { realSpeedTest } from "./net-speed.js";

const Tauri = () => globalThis.__TAURI__?.core?.invoke;

/** Browser clipboard access, used by the preview build. Never throws. */
async function browserClipboard() {
  if (!navigator.clipboard?.readText) return null;
  return navigator.clipboard.readText();
}

/* ------------------------------------------------------------------ */
/* model                                                               */
/* ------------------------------------------------------------------ */

export const State = {
  QUEUED: "queued",
  RUNNING: "running",
  PAUSED: "paused",
  RETRYING: "retrying",
  DONE: "done",
  FAILED: "failed",
};

export const ACTIVE = new Set([State.QUEUED, State.RUNNING, State.RETRYING]);
export const TERMINAL = new Set([State.DONE, State.FAILED]);

/* ------------------------------------------------------------------ */
/* mock                                                                */
/* ------------------------------------------------------------------ */

function ev(t, kind, msg, extra = {}) {
  return { t, kind, msg, ...extra };
}

function seed() {
  const t0 = Date.now() - 1000 * 60 * 6;
  const MiB = 1024 ** 2;
  const GiB = 1024 ** 3;

  return [
    {
      id: "a1",
      name: "teardown-lecture-01.mp4",
      source: "https://media.example.edu/teardown/lecture-01.mp4",
      dest: "C:\\Users\\you\\Videos\\teardown-lecture-01.mp4",
      state: State.RUNNING,
      kind: "direct",
      container: "mp4",
      resolver: "direct",
      codec: "h264 / aac",
      received: 1.31 * GiB,
      total: 4.20 * GiB,
      rate: 12.4 * 1024 * 1024,
      startedAt: t0,
      finishedAt: null,
      seams: [],
      marks: [],
      error: null,
      events: [
        ev(t0, "open", "GET /teardown/lecture-01.mp4"),
        ev(t0 + 40, "range", "origin replied 206, content-length 4509715660"),
        ev(t0 + 90, "note", "writing to lecture-01.mp4.part"),
      ],
    },
    {
      id: "a2",
      name: "stream-master-fmp4.mp4",
      source: "https://vod.example.com/stream/master.m3u8",
      dest: "C:\\Users\\you\\Videos\\stream-master-fmp4.mp4",
      state: State.RUNNING,
      kind: "fragmented",
      container: "mp4",
      resolver: "hls",
      codec: "av1 / opus (fMP4)",
      received: 0.74 * GiB,
      total: 1.92 * GiB,
      rate: 7.9 * 1024 * 1024,
      startedAt: t0 + 40_000,
      finishedAt: null,
      seams: [],
      marks: [0, 0.26 * GiB, 0.52 * GiB, 0.78 * GiB, 1.04 * GiB, 1.30 * GiB, 1.56 * GiB],
      segmentsDone: 3,
      segmentsTotal: 8,
      error: null,
      events: [
        ev(t0 + 40_000, "open", "GET /stream/master.m3u8"),
        ev(t0 + 40_210, "seg", "EXT-X-MAP init segment, 1,482 bytes"),
        ev(t0 + 40_300, "seg", "8 media segments, target duration 6s"),
        ev(t0 + 60_000, "note", "segment 4/8 appended"),
      ],
    },
    {
      id: "a3",
      name: "dash-video-only-2160p.m4s",
      source: "https://stream.example.net/manifest.mpd",
      dest: "C:\\Users\\you\\Videos\\dash-video-only-2160p.m4s",
      state: State.PAUSED,
      kind: "fragmented",
      container: "m4s",
      resolver: "dash",
      codec: "vp9 (video only)",
      received: 0.39 * GiB,
      total: 0.81 * GiB,
      rate: null,
      startedAt: t0 + 95_000,
      finishedAt: null,
      seams: [0.21 * GiB],
      marks: [],
      error: null,
      events: [
        ev(t0 + 95_000, "open", "GET /manifest.mpd"),
        ev(t0 + 95_180, "note", "video-only representation; no audio group to mux"),
        ev(t0 + 96_000, "seg", "segment 0/41 appended"),
        ev(t0 + 171_000, "cut", "connection reset by peer at 225,443,712"),
        ev(t0 + 178_000, "resume", "reopening with Range: bytes=225443712-"),
        ev(t0 + 186_000, "note", "origin honoured the range; appended in place"),
        ev(t0 + 187_000, "note", "paused by operator"),
      ],
    },
    {
      id: "a4",
      name: "kyoto-timelapse.mp4",
      source: "https://clips.example.org/kyoto-timelapse.mp4",
      dest: "C:\\Users\\you\\Videos\\kyoto-timelapse.mp4",
      state: State.DONE,
      kind: "direct",
      container: "mp4",
      resolver: "direct",
      codec: "h264",
      received: 1.44 * GiB,
      total: 1.44 * GiB,
      rate: null,
      startedAt: t0 - 1000 * 60 * 90,
      finishedAt: t0 - 1000 * 60 * 42,
      seams: [0.61 * GiB, 0.98 * GiB],
      marks: [],
      error: null,
      events: [
        ev(t0 - 5400_000, "open", "GET /kyoto-timelapse.mp4"),
        ev(t0 - 5399_900, "range", "origin replied 206"),
        ev(t0 - 5100_000, "cut", "laptop suspended"),
        ev(t0 - 4980_000, "resume", "reopening with Range: bytes=655200000-"),
        ev(t0 - 4700_000, "cut", "origin closed the connection"),
        ev(t0 - 4650_000, "resume", "reopening with Range: bytes=1052596480-"),
        ev(t0 - 2520_000, "done", "complete, 1,546,418,304 bytes, 2 seams"),
      ],
    },
    {
      id: "a5",
      name: "encrypted-master.m3u8",
      source: "https://vod.example.com/drm/master.m3u8",
      dest: null,
      state: State.FAILED,
      kind: null,
      container: null,
      resolver: "hls",
      codec: null,
      received: 0,
      total: null,
      rate: null,
      startedAt: t0 - 200_000,
      finishedAt: t0 - 197_000,
      seams: [],
      marks: [],
      error: {
        code: "DrmProtected",
        message: "EXT-X-KEY METHOD=SAMPLE-AES. ifami does not decrypt protected media.",
      },
      events: [
        ev(t0 - 200_000, "open", "GET /drm/master.m3u8"),
        ev(t0 - 197_500, "fail", "EXT-X-KEY METHOD=SAMPLE-AES — refused, not bypassed"),
        ev(t0 - 197_000, "note", "no bytes written"),
      ],
    },
    {
      id: "a6",
      name: "members-only-broadcast.mp4",
      source: "https://members.example.tv/watch/9182736",
      dest: null,
      state: State.FAILED,
      kind: null,
      container: null,
      resolver: "none",
      codec: null,
      received: 0,
      total: null,
      rate: null,
      startedAt: t0 - 90_000,
      finishedAt: t0 - 88_000,
      seams: [],
      marks: [],
      error: {
        code: "AuthRequired",
        message: "origin replied 401. ifami has no account and no cookie jar.",
      },
      events: [
        ev(t0 - 90_000, "open", "GET /watch/9182736"),
        ev(t0 - 88_400, "fail", "HTTP 401 — authentication required"),
        ev(t0 - 88_000, "note", "not retryable"),
      ],
    },
    {
      id: "a7",
      name: "length-unknown-stream.ts",
      source: "https://live.example.tv/hls/stream.m3u8",
      dest: "C:\\Users\\you\\Videos\\length-unknown-stream.ts",
      state: State.RUNNING,
      kind: "fragmented",
      container: "ts",
      resolver: "hls",
      codec: "h264 / aac (MPEG-TS)",
      received: 187 * MiB,
      total: null, // length genuinely unknown; the UI says so
      rate: 3.1 * 1024 * 1024,
      startedAt: t0 + 20_000,
      finishedAt: null,
      seams: [],
      marks: [],
      segmentsDone: 92,
      segmentsTotal: null,
      error: null,
      events: [
        ev(t0 + 20_000, "open", "GET /hls/stream.m3u8"),
        ev(t0 + 20_140, "note", "no EXT-X-ENDLIST and no Content-Length; length unknown"),
        ev(t0 + 20_300, "note", "appending until the stream ends"),
      ],
    },
    {
      id: "a8",
      name: "field-recording-07.webm",
      source: "https://uploads.example.net/v/field-recording-07.webm",
      dest: "C:\\Users\\you\\Videos\\field-recording-07.webm",
      state: State.QUEUED,
      kind: "direct",
      container: "webm",
      resolver: "direct",
      codec: "vp9 / opus",
      received: 0,
      total: 0.68 * GiB,
      rate: null,
      startedAt: null,
      finishedAt: null,
      seams: [],
      marks: [],
      error: null,
      events: [ev(Date.now(), "note", "queued, 3 concurrent transfers already running")],
    },
  ];
}

function mockBackend() {
  // The browser preview opens on the empty state. The hero is the front door a
  // new user actually meets, so it is what the UI should be reviewed against by
  // default; the scripted queue sits behind `?seeded` for looking at the list
  // itself. The real app needs neither switch -- it decides the empty state from
  // the actual queue.
  const seeded = new URLSearchParams(location.search).has("seeded");
  let tasks = seeded ? seed() : [];
  const listeners = new Set();
  let chaos = { fired: false, at: Date.now() + 7000 };
  let last = Date.now();
  const GiB = 1024 ** 3;

  function emit() {
    const snap = { tasks, at: Date.now() };
    listeners.forEach((fn) => fn(snap));
  }

  setInterval(() => {
    const nowMs = Date.now();
    const dt = (nowMs - last) / 1000;
    last = nowMs;
    let moved = false;

    // A scripted interruption, so the seam behaviour is visible without
    // waiting for a real network to misbehave.
    if (!chaos.fired && nowMs > chaos.at) {
      chaos.fired = true;
      const t = tasks.find((x) => x.id === "a1");
      if (t && t.state === State.RUNNING) {
        t.rate = null;
        t.state = State.RETRYING;
        t.events.push(ev(nowMs, "cut", "connection reset by peer"));
        moved = true;
        setTimeout(() => {
          const at = Date.now();
          t.events.push(ev(at, "resume", "reopening with Range: bytes=1408729600-"));
          t.seams = [...t.seams, t.received];
          t.state = State.RUNNING;
          t.rate = 12.4 * 1024 * 1024;
          emit();
        }, 2200);
      }
    }

    for (const t of tasks) {
      if (t.state !== State.RUNNING || !t.rate) continue;

      const step = t.rate * dt * (0.9 + Math.random() * 0.2);
      t.received = Math.min(t.total ?? t.received + step, t.received + step);

      if (t.total != null && t.received >= t.total) {
        t.received = t.total;
        t.state = State.DONE;
        t.rate = null;
        t.finishedAt = Date.now();
        t.events.push(
          ev(Date.now(), "done", `complete, ${t.received.toLocaleString("en-US")} bytes, ${t.seams.length} seam${t.seams.length === 1 ? "" : "s"}`)
        );
      } else if (t.kind === "fragmented" && t.segmentsTotal) {
        t.segmentsDone = Math.min(t.segmentsTotal, Math.floor((t.received / t.total) * t.segmentsTotal));
      }
      moved = true;
    }

    if (moved) emit();
  }, 250);

  const find = (id) => tasks.find((t) => t.id === id);

  return {
    kind: "mock",
    info: async () => ({ version: "0.1.0", backend: "preview", sourceDir: "C:\\Users\\you\\Videos" }),
    list: async () => tasks,
    on(fn) {
      listeners.add(fn);
      // Deliver the current state immediately. A subscriber that waits for the
      // first *change* shows an empty window until something happens to move,
      // which looks like a hang rather than an idle queue.
      fn({ tasks, at: Date.now() });
      return () => listeners.delete(fn);
    },

    async add(url) {
      const id = `n${Date.now()}`;
      const name = (url.split("/").pop() || "download").split("?")[0] || "download";
      const total = 220 * 1024 * 1024 + Math.floor(Math.random() * 900 * 1024 * 1024);
      const t = {
        id,
        name,
        source: url,
        dest: `C:\\Users\\you\\Videos\\${name}`,
        state: State.QUEUED,
        kind: "direct",
        container: (name.split(".").pop() || "bin").toLowerCase(),
        resolver: "direct",
        codec: null,
        received: 0,
        total,
        rate: null,
        startedAt: null,
        finishedAt: null,
        seams: [],
        marks: [],
        error: null,
        events: [ev(Date.now(), "note", "queued")],
      };
      tasks = [t, ...tasks];
      setTimeout(() => {
        if (t.state !== State.QUEUED) return;
        t.state = State.RUNNING;
        // Bytes per second, like every other rate in this file. The seeded
        // tasks are in MiB/s; a new one has to be in the same unit or it
        // renders as "12 B/s" and looks broken.
        t.rate = (6 + Math.random() * 18) * 1024 * 1024;
        t.startedAt = Date.now();
        t.events.push(ev(Date.now(), "open", `GET ${new URL(url).pathname}`));
        emit();
      }, 700);
      emit();
      return t;
    },

    async pause(id) {
      const t = find(id);
      if (!t || !ACTIVE.has(t.state)) return;
      t.rate = null;
      t.state = State.PAUSED;
      t.events.push(ev(Date.now(), "note", "paused by operator"));
      emit();
    },

    async resume(id) {
      const t = find(id);
      if (!t || t.state !== State.PAUSED) return;
      t.state = State.RUNNING;
      t.rate = (5 + Math.random() * 12) * 1024 * 1024;
      t.events.push(
        t.received > 0
          ? ev(Date.now(), "resume", `reopening with Range: bytes=${t.received}-`)
          : ev(Date.now(), "open", "opening from byte 0")
      );
      t.seams = t.received > 0 ? [...t.seams, t.received] : t.seams;
      emit();
    },

    async remove(id) {
      tasks = tasks.filter((t) => t.id !== id);
      emit();
    },

    // Nothing to open: there is no real disk behind this backend. It still has
    // to exist, so the button above it is not a lie in the demo.
    async reveal(id) {
      const t = find(id);
      if (!t) return;
      t.events.push(ev(Date.now(), "note", `show in folder: ${t.dest ?? "(no file yet)"}`));
      emit();
    },

    watchClipboard: (onLink) => watchClipboard({ read: browserClipboard, onLink }),
    // No origin has been contacted, so there is no favicon to show. The
    // monogram tile in the paste bar is the honest fallback.
    favicon: async () => null,

    speedTest: (onSample) => realSpeedTest(onSample),
  };
}

/* ------------------------------------------------------------------ */
/* tauri                                                               */
/* ------------------------------------------------------------------ */

function tauriBackend() {
  const invoke = Tauri();
  let poll = null;
  const listeners = new Set();

  const emit = () => {
    const tasks = cache;
    listeners.forEach((fn) => fn({ tasks, at: Date.now() }));
  };
  let cache = [];

  return {
    kind: "tauri",
    info: () => invoke("app_info"),
    list: () => invoke("task_list"),
    on(fn) {
      listeners.add(fn);
      invoke("task_list").then((t) => {
        cache = t;
        emit();
      });
      if (!poll) poll = setInterval(() => invoke("task_list").then((t) => { cache = t; emit(); }), 400);
      return () => {
        listeners.delete(fn);
        if (!listeners.size && poll) {
          clearInterval(poll);
          poll = null;
        }
      };
    },
    add: (url) => invoke("add_url", { url }),
    pause: (id) => invoke("pause_task", { id }),
    resume: (id) => invoke("resume_task", { id }),
    remove: (id) => invoke("remove_task", { id }),
    reveal: (id) => invoke("reveal_task", { id }),

    // The focus check happens in Rust, inside `clipboard_link`. If the window
    // is not focused the command answers null without touching the clipboard
    // at all — which is the guarantee that makes this feature acceptable.
    watchClipboard: (onLink) =>
      watchClipboard({ read: () => invoke("clipboard_link").catch(() => null), onLink }),

    // Same-origin only, enforced in Rust. See `favicon.rs`.
    favicon: (url) => invoke("favicon", { url }).catch(() => null),

    speedTest: (onSample) => runSpeedTest(invoke, onSample),
  };
}

/* ------------------------------------------------------------------ */
/* speed test                                                           */
/* ------------------------------------------------------------------ */

/* The preview measures for real: `net-speed.js` runs the same ramp against the
 * same endpoints the Rust engine uses, so nothing here needs to model one. */

/**
 * Coerce one speed sample from the IPC payload into the flat shape the scope
 * reads.
 *
 * Rust's `Direction` and `SampleKind` cross the wire as their variant names and
 * the fields are snake_case; the drawing wants a lowercase object with camelCase
 * keys. The preview engine produces that shape directly and passes straight
 * through. One function, so a rename on either side is one edit -- and so an
 * unrecognised direction lands in the download lane rather than in no lane at
 * all, where it would be silently dropped from a chart that still looked
 * complete.
 */
export function normalizeSample(raw) {
  if (!raw || typeof raw !== "object") return null;
  return {
    direction: String(raw.direction ?? "").toLowerCase() === "up" ? "up" : "down",
    kind: String(raw.kind ?? "").toLowerCase() === "level" ? "level" : "live",
    elapsedMs: Number(raw.elapsedMs ?? raw.elapsed_ms ?? 0) || 0,
    connections: Number(raw.connections ?? 1) || 1,
    bps: Number(raw.bps ?? 0) || 0,
    totalBytes: Number(raw.totalBytes ?? raw.total_bytes ?? 0) || 0,
    latency: ms(raw.latency ?? null),
  };
}

/**
 * Drive a real measurement over IPC.
 *
 * The Rust side owns the cancel handle, the target and the bytes; this only
 * marshals samples to the instrument and resolves with whatever the engine
 * reports. A rejected run comes back as `{ ok: false }` rather than a thrown
 * error, because "that server sent nothing" is a result the dialog has to
 * explain, not an exception it has to survive.
 */
function runSpeedTest(invoke, onSample) {
  const off = Promise.resolve(
    globalThis.__TAURI__?.event?.listen?.("speed-sample", (e) => {
      const sample = normalizeSample(e.payload);
      if (sample) onSample(sample);
    })
  );

  const run = (async () => {
    // No target argument. The engine picks its own, so there is nothing here
    // that could be wrong, and nothing to validate.
    const raw = await invoke("speed_test").catch((err) => ({ __error: String(err) }));
    return normalizeSpeedResult(raw);
  })();

  return {
    cancel: () => invoke("speed_stop").catch(() => {}),
    promise: run,
    ready: off.then((unlisten) => ({ unlisten })),
  };
}

/** A `Duration` as serde renders it, in whole milliseconds. */
function ms(d) {
  if (d == null) return null;
  if (typeof d === "number") return d;
  const secs = Number(d.secs ?? d.seconds ?? 0);
  const nanos = Number(d.nanos ?? 0);
  const total = secs * 1000 + nanos / 1e6;
  return Number.isFinite(total) ? Math.round(total * 1000) / 1000 : null;
}

/**
 * Coerce an IPC payload into the shape the dialog reads.
 *
 * Rust's serde names and JS's do not always agree, and a `latency_ms` arriving
 * as `latency_ms` when the reader wanted `latencyMs` is the kind of mismatch
 * that silently renders as an em dash. One place that knows about it beats
 * three call sites each guessing, and it is the only place allowed to know.
 *
 * The idle figure is a `Latency` struct with a median and a jitter, so it
 * arrives nested; the loader reads the two scalars it wants out of it. If a
 * future engine version flattens those into plain milliseconds, this is the
 * single function that has to change.
 */
function normalizeSpeedResult(raw) {
  if (!raw || raw.__error != null) {
    return { ok: false, value: null, error: raw?.__error ?? "the measurement failed" };
  }
  if (raw.ok === false || raw.error != null) {
    return { ok: false, value: null, error: raw.error ?? "the measurement failed" };
  }
  const v = raw.value ?? raw;
  const idle = v.idle ?? null;
  const up = v.upload ?? v.up ?? null;
  return {
    ok: true,
    value: {
      bps: Number(v.bps ?? 0),
      connections: Number(v.connections ?? 1),
      idle: ms(idle?.median ?? v.idleMedian ?? v.idle_median ?? null),
      jitter: ms(idle?.jitter ?? v.jitter ?? null),
      loaded: ms(v.loaded ?? null),
      totalBytes: Number(v.totalBytes ?? v.total_bytes ?? 0),
      host: v.host ?? "",
      stoppedEarly: Boolean(v.stoppedEarly ?? v.stopped_early ?? false),
      // Absent when the origin would not take a body, which is not an error --
      // it is a download measurement that happens to have no upload beside it.
      upload: up
        ? {
            bps: Number(up.bps ?? 0),
            connections: Number(up.connections ?? 1),
            latency: ms(up.latency ?? null),
            totalBytes: Number(up.totalBytes ?? up.total_bytes ?? 0),
          }
        : null,
    },
  };
}

export function createBackend() {
  return Tauri() ? tauriBackend() : mockBackend();
}
