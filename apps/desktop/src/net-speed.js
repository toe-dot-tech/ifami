// ifami desktop — the speed test, measured on the wire.
//
// The dialog (`speed.js`) only draws readings; `backend.js` decides where they
// come from. With the Rust shell running they come over IPC from
// `ifami_core::net::speedtest`. With no shell — the browser preview — this is
// what stands in for it, and it is a measurement rather than a simulation:
// every byte here came off the network. It talks to the same Cloudflare
// endpoints the core does, ramps the same 1-2-4-8-16, and judges each level by
// the same five-percent rule, so the shape the preview draws is the shape the
// product produces.
//
// What the browser cannot reproduce is the socket count. The core opens N real
// connections and asks each for a different byte range; a page's `fetch` goes
// through the pool, which multiplexes over HTTP/2 and forbids a scripted
// `Range`, so `connections` here is the concurrency asked of the server, not N
// guaranteed sockets. The aggregate rate, the latency under load and the level
// average are still what the wire said — only the noun is approximate.

import { DEFAULT_TARGET, DEFAULT_UPLOAD_TARGET, host } from "./speed.js";

/** Long enough that a slow connection's first response does not dominate. */
const STEP_MS = 1500;

/** One live reading inside a level. The core's `SAMPLE_INTERVAL`. */
const SAMPLE_MS = 250;

/** The ramp doubles to here and stops. The core's `MAX_CONNECTIONS`. */
const MAX_CONNECTIONS = 16;

/** Upload tops out much sooner. The core's `MAX_UPLOAD_CONNECTIONS`. */
const MAX_UPLOAD_CONNECTIONS = 4;

/** Ceilings on the bytes a single run may move. The core's `BYTE_CEILING`. */
const DOWN_CEILING = 256 * 1024 * 1024;
const UP_CEILING = 32 * 1024 * 1024;

/** Gain below which the ramp stops doubling. The core's `MIN_GAIN`. */
const MIN_GAIN = 0.05;

/** Round trips taken before the ramp, to describe the link at rest. */
const IDLE_PROBES = 5;

/** How long one probe may take. The core's `PROBE_TIMEOUT`. */
const PROBE_TIMEOUT_MS = 5000;

/** Wall-clock ceiling on a run, so a slow link still gets a number back. */
const MAX_DURATION_MS = 30000;

/** Pushed in one request. Progress arrives incrementally, so partials count. */
const UPLOAD_CHUNK = 1024 * 1024;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Start a real measurement.
 *
 * Same contract as the IPC backend: a handle with `cancel` and a `promise` that
 * resolves `{ ok, value }` (or `{ ok: false, error }`). `onSample` is called
 * with the flat shape `normalizeSample` produces, because there is no wire to
 * normalise out of a live `fetch`.
 */
export function realSpeedTest(onSample) {
  let cancelled = false;
  const started = Date.now();
  const isCancelled = () => cancelled;
  // Aborted by `cancel()`. The per-level controllers bound the data lanes; this
  // one bounds the probes, which are allowed to outlive their level the way the
  // core's do, and must still stop the moment the user asks.
  const runAc = new AbortController();

  const down = {
    direction: "down",
    url: DEFAULT_TARGET,
    probeUrl: DEFAULT_TARGET,
    ceiling: DOWN_CEILING,
    maxConnections: MAX_CONNECTIONS,
    started,
    cancelled: isCancelled,
    signal: runAc.signal,
    drain: drainDown,
  };
  const up = {
    direction: "up",
    url: DEFAULT_UPLOAD_TARGET,
    ceiling: UP_CEILING,
    maxConnections: MAX_UPLOAD_CONNECTIONS,
    started,
    cancelled: isCancelled,
    signal: runAc.signal,
    drain: drainUp,
    chunk: new Blob([new Uint8Array(UPLOAD_CHUNK)]),
  };

  const promise = (async () => {
    // Measured before any ramp and on its own, so a slow first response is
    // attributed to the origin rather than smeared across the average.
    const idle = await idleDistribution(DEFAULT_TARGET, IDLE_PROBES, runAc.signal);

    // Both directions at once, sharing the link and the clock, exactly as the
    // engine does. Each reports what it can do while the other is working.
    const [downOut, upOut] = await Promise.all([
      rampDirection(down, onSample),
      rampDirection(up, onSample),
    ]);

    // No download bytes is a failure, not a zero: we are not measuring a slow
    // connection, we never reached the origin. Upload is allowed to be missing.
    if (!downOut) {
      return { ok: false, error: "That server did not send anything we could measure." };
    }

    return {
      ok: true,
      value: {
        bps: downOut.bps,
        connections: downOut.connections,
        idle: idle ? idle.median : null,
        jitter: idle ? idle.jitter : null,
        loaded: downOut.latency,
        totalBytes: downOut.totalBytes,
        host: host(DEFAULT_TARGET),
        stoppedEarly: cancelled,
        upload: upOut
          ? {
              bps: upOut.bps,
              connections: upOut.connections,
              latency: upOut.latency,
              totalBytes: upOut.totalBytes,
            }
          : null,
      },
    };
  })();

  return {
    cancel: () => {
      cancelled = true;
      runAc.abort();
    },
    promise,
  };
}

/**
 * Ramp one direction until a level fails to improve, then report the best one.
 *
 * The reported number is the best level, not the last: a server that throttles
 * an aggressive client is common enough that "keep doubling until it gets
 * worse" would otherwise report that dip as the user's link.
 */
async function rampDirection(state, onSample) {
  let best = null;
  let loaded = null;
  let dirTotal = 0;
  let connections = 1;

  while (connections <= state.maxConnections && !state.cancelled()) {
    const left = MAX_DURATION_MS - (Date.now() - state.started);
    if (left <= 0) break;
    const wall = Math.min(STEP_MS, left);
    const levelStart = Date.now();

    const level = await runLevel(state, onSample, connections, levelStart + wall, dirTotal);
    dirTotal += level.bytes;
    if (level.bytes === 0) break;

    // The data lanes stop at the level's deadline, but the probe may outlive it
    // (see `runLevel`). Dividing by the intended window keeps a slow probe from
    // deflating a throughput it was not part of.
    const secs = Math.max(wall / 1000, 1e-9);
    const bps = (level.bytes * 8) / secs;

    onSample({
      direction: state.direction,
      kind: "level",
      elapsedMs: Date.now() - state.started,
      connections,
      bps,
      totalBytes: dirTotal,
      latency: level.latency,
    });

    // Judged before this level is recorded, so the first level always has
    // somewhere to go and every later one is compared with the best so far.
    if (best === null || bps > best.bps * (1 + MIN_GAIN)) {
      best = { bps, connections };
      loaded = level.latency;
      connections *= 2;
    } else {
      break;
    }
  }

  if (!best) return null;
  return { bps: best.bps, connections: best.connections, latency: loaded, totalBytes: dirTotal };
}

/**
 * Open `connections` participants for one level and read the total every
 * quarter second until the deadline.
 *
 * The live readings are emitted from here, from the running total, because the
 * shape inside a level is the measurement: a level average alone cannot tell a
 * steady link from one that swings around it. The round trip is timed while the
 * data lanes are busy, which is the only way to learn what the link costs when
 * something else is using it.
 */
async function runLevel(state, onSample, connections, deadline, baseTotal) {
  const level = { base: baseTotal, bytes: 0, deadline };
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), Math.max(0, deadline - Date.now()));
  const started = Date.now();

  const workers = [];
  for (let i = 0; i < connections; i++) {
    // Downloads are cut off at the deadline, which is clean: the origin simply
    // stops sending. An upload cut off mid-body is not clean -- the origin sees
    // a Content-Length that was never fulfilled and answers 400 -- so an upload
    // is allowed to finish the request it already started and only stops
    // starting new ones.
    const signal = state.direction === "up" ? state.signal : ac.signal;
    workers.push(state.drain(state, level, signal));
  }

  // Bounded by the run's stop and its own timeout, not by the level: a link
  // that is slow under load is exactly the link whose loaded latency is worth
  // having, and cutting the question off at the level's deadline would throw
  // that answer away.
  const probe = probeOnce(state.probeUrl ?? state.url, state.signal);

  let prevBytes = 0;
  let prevAt = started;
  for (;;) {
    const now = Date.now();
    if (now >= deadline || state.cancelled()) break;
    await sleep(Math.min(SAMPLE_MS, deadline - now));

    const at = Date.now();
    const gained = level.bytes - prevBytes;
    const secs = (at - prevAt) / 1000;
    prevBytes = level.bytes;
    prevAt = at;

    onSample({
      direction: state.direction,
      kind: "live",
      elapsedMs: Date.now() - state.started,
      connections,
      bps: (gained * 8) / Math.max(secs, 1e-9),
      totalBytes: baseTotal + level.bytes,
      latency: null,
    });
  }

  clearTimeout(timer);
  ac.abort();
  await Promise.all(workers);
  const latency = await probe;

  return { bytes: level.bytes, latency };
}

/**
 * One download participant: fetch, read, count, repeat until the level ends.
 *
 * `no-store` is not politeness — a cached response would let the browser serve
 * bytes from disk and report a five-gigabit connection. The clock and the
 * ceiling are both checked per chunk so the last partial read is kept rather
 * than thrown away.
 */
async function drainDown(state, level, signal) {
  while (Date.now() < level.deadline && !state.cancelled() && !signal.aborted) {
    if (level.base + level.bytes >= state.ceiling) return;

    let res;
    try {
      res = await fetch(state.url, { signal, cache: "no-store" });
    } catch {
      return; // aborted at the deadline, or the origin is unreachable
    }
    if (!res.ok || !res.body) return;

    const reader = res.body.getReader();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        level.bytes += value.byteLength;
        if (
          Date.now() >= level.deadline ||
          state.cancelled() ||
          level.base + level.bytes >= state.ceiling
        ) {
          await reader.cancel().catch(() => {});
          return;
        }
      }
    } catch {
      return; // aborted while reading
    }
  }
}

/**
 * One upload participant: POST a blob, count the bytes the progress events
 * report, repeat until the level ends.
 *
 * `fetch` cannot see upload progress, so this uses `XMLHttpRequest`, whose
 * `upload.onprogress` reports bytes as they are handed to the socket. That is
 * what lets a level keep what it pushed when the deadline cuts a request off
 * mid-flight — the same "keep the last partial measurement" rule the core
 * applies to a download chunk.
 */
async function drainUp(state, level, signal) {
  while (!signal.aborted && Date.now() < level.deadline && !state.cancelled()) {
    if (level.base + level.bytes >= state.ceiling) return;
    await uploadOnce(state, level, signal);
  }
}

function uploadOnce(state, level, signal) {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();

    const xhr = new XMLHttpRequest();
    let counted = 0;
    const onAbort = () => xhr.abort();
    signal.addEventListener("abort", onAbort, { once: true });

    const done = () => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    };

    xhr.open("POST", state.url, true);
    xhr.upload.onprogress = (e) => {
      const delta = e.loaded - counted;
      if (delta > 0) {
        level.bytes += delta;
        counted = e.loaded;
      }
    };
    xhr.onload = done;
    xhr.onerror = done;
    xhr.onabort = done;
    xhr.ontimeout = done;
    try {
      xhr.send(state.chunk);
    } catch {
      done();
    }
  });
}

/**
 * Time one HEAD, or `null` if it errored or was aborted.
 *
 * The probe is bounded by `external` (the run's stop) and by its own timeout,
 * whichever fires first. Both matter: the timeout is what keeps a server that
 * simply will not answer from holding the dialog open, and the external signal
 * is what makes Stop immediate.
 */
async function probeOnce(url, external, timeoutMs = PROBE_TIMEOUT_MS) {
  const { signal, clear } = boundedSignal(external, timeoutMs);
  const started = performance.now();
  try {
    const res = await fetch(url, { method: "HEAD", signal, cache: "no-store" });
    return res.ok ? performance.now() - started : null;
  } catch {
    return null;
  } finally {
    clear();
  }
}

/** Time `count` round trips back to back, each bounded by `PROBE_TIMEOUT_MS`. */
async function idleDistribution(url, count, external) {
  const samples = [];
  const deadline = performance.now() + PROBE_TIMEOUT_MS * count;
  for (let i = 0; i < count; i++) {
    if (performance.now() >= deadline || external?.aborted) break;
    const t = await probeOnce(url, external);
    if (t != null) samples.push(t);
  }
  return distribution(samples);
}

/**
 * A signal that fires when `external` aborts or `ms` elapses, plus a way to
 * cancel the timer.
 *
 * `AbortSignal.any` is the honest expression and is available everywhere this
 * runs (WebView2 is Chromium). The fallback keeps the timeout half, so an
 * engine without it still cannot hang the dialog.
 */
function boundedSignal(external, ms) {
  const timer = new AbortController();
  const handle = setTimeout(() => timer.abort(), ms);
  const signal =
    external && typeof AbortSignal.any === "function"
      ? AbortSignal.any([external, timer.signal])
      : timer.signal;
  return { signal, clear: () => clearTimeout(handle) };
}

/**
 * Min, median, max and jitter from raw timings, or `null` if none answered.
 *
 * Jitter is the mean absolute difference between consecutive timings in the
 * order they happened, not the spread of the sorted list: the question it
 * answers is how unevenly the line answered, and sorting destroys exactly that.
 */
function distribution(samples) {
  if (!samples.length) return null;
  const sorted = [...samples].sort((a, b) => a - b);
  const n = sorted.length;
  const median = n % 2 ? sorted[(n - 1) / 2] : (sorted[n / 2 - 1] + sorted[n / 2]) / 2;
  let jitter = 0;
  if (n > 1) {
    let total = 0;
    for (let i = 1; i < n; i++) total += Math.abs(samples[i] - samples[i - 1]);
    jitter = total / (n - 1);
  }
  return { min: sorted[0], median, max: sorted[n - 1], jitter };
}
