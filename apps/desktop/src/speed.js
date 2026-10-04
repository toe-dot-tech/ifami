// ifami desktop — the connection speed test.
//
// The design brief was "like Fast.com, but in the app", and the useful part of
// that is not the dial. A dial tells you one number and hides the thing that
// actually decides how fast a download will be: most origins cap a *single*
// connection, so one stream flattens out long before the line is full and the
// rest is only reachable with several at once.
//
// So this is a scope, not a speedometer. Throughput is a continuous quantity, so
// it is drawn as a line — the engine emits a reading four times a second inside
// every level, and the *shape* of that line is a measurement no average can
// stand in for. A steady line and a line that swings wildly through the same
// level average are the same number and completely different connections.
//
// Two lanes, stacked, sharing one time axis: download in moss on top, upload in
// fuchsia below. Each has its own y scale, because upload is routinely an order
// of magnitude slower and one shared scale would flatten it into the baseline.
//
// Everything here is presentation and orchestration. The measurement itself
// lives in `ifami_core::net::speedtest`, behind the backend seam, so the real
// app and this one draw from the same numbers.

import { bytes as formatBytes } from "./format.js";

/* ------------------------------------------------------------------ */
/* the default target                                                  */
/* ------------------------------------------------------------------ */

/**
 * Where a test goes when you have not been told otherwise.
 *
 * Must match `speedtest::DEFAULT_TARGET` in the core. It is written out here
 * anyway because the browser preview has no Rust to ask, and the one place
 * that matters — the shipping app — asks the backend for its own value via
 * `speedDefault()` and overwrites this. So a drift shows up as a wrong default
 * in the preview, never in the product.
 *
 * HTTPS, no credentials, and no query string beyond the size, which is the
 * server's own parameter and not a tracking identifier. There is a test in
 * `speedtest.rs` that fails if that stops being true.
 */
export const DEFAULT_TARGET = "https://speed.cloudflare.com/__down?bytes=100000000";

/**
 * Where uploaded bytes go during a test.
 *
 * Must match `speedtest::DEFAULT_UPLOAD_TARGET` in the core. The same host as
 * `DEFAULT_TARGET`, deliberately: the "testing against" line names one
 * operator, not two, so a download figure and an upload figure are not quietly
 * handed to different companies.
 */
export const DEFAULT_UPLOAD_TARGET = "https://speed.cloudflare.com/__up";

/* ------------------------------------------------------------------ */
/* units                                                                */
/* ------------------------------------------------------------------ */

/**
 * The unit a bit rate is stated in.
 *
 * Mbps is what every ISP plan is quoted in, so it is the default and the unit
 * anything at or above a megabit is given. Below that, `0.27 Mbps` is a worse
 * way of saying `270 kbps`: it borrows a decimal place from a magnitude the
 * reader cannot hold, and a genuinely slow line -- 40 kbps, which is a real
 * result on a bad mobile connection -- renders as `0.04` and reads as nothing at
 * all. Nobody quotes a plan in kbps, but everybody recognises the unit once they
 * see it, and a number that reads as zero is a number nobody believes.
 *
 * Not extended to Gbps. A 2 Gbps line reading "2000 Mbps" is unambiguous, and
 * every ISP in the world quotes that plan as a number of megabits, so the label
 * would be trading a unit the reader knows for one they have to translate.
 */
export function rateUnit(bps) {
  if (bps == null || !Number.isFinite(bps) || bps < 0) return "Mbps";
  // The switch is placed just *below* the round number rather than at it. A
  // reading of 999_999 would otherwise print as "1000 kbps", which is a
  // megabit wearing the wrong unit -- and it would print that at the exact
  // moment the figures either side of it say "1.0 Mbps", so the discontinuity
  // lands where the reader is most likely to be looking.
  if (bps < 1e6 && bps / 1e3 < 999.5) return "kbps";
  return "Mbps";
}

/**
 * A bit rate as a bare number, in `unit`.
 *
 * Two decimals under 1, one under 10, whole numbers above: the digit count is
 * what keeps the headline from twitching sideways four times a second, and it is
 * the reason the readout is built from tabular figures.
 */
function scaled(bps, unit) {
  const v = bps / (unit === "kbps" ? 1e3 : 1e6);
  if (!Number.isFinite(v)) return "—";
  if (v < 1) return v.toFixed(2);
  if (v < 10) return v.toFixed(1);
  return Math.round(v).toString();
}

/**
 * A bit rate as a number *and* its unit, together.
 *
 * The pairing is the point. This used to be a function returning a bare number
 * plus a `"Mbps"` string repeated at six call sites, which is six chances for a
 * readout to say `612` under a label that says megabits. Returning them as one
 * value makes that unrepresentable rather than merely unlikely.
 */
export function rate(bps) {
  if (bps == null || !Number.isFinite(bps) || bps < 0) {
    return { value: "—", unit: "Mbps" };
  }
  const unit = rateUnit(bps);
  return { value: scaled(bps, unit), unit };
}

/**
 * A bit rate as one ready-to-print string, for figures with no separate unit
 * slot of their own.
 *
 * The upload figure is the reason this exists. It sits in a list beside latency
 * and transferred bytes, so it has no unit label to hang off, and it can differ
 * from the download figure by three orders of magnitude on a normal asymmetric
 * connection -- a shared one shows 900 kbps down and 18 Mbps up, which under a
 * single shared `Mbps` label would have been a readout claiming two different
 * units at once.
 */
export function rateText(bps) {
  const r = rate(bps);
  return r.value === "—" ? r.value : `${r.value} ${r.unit}`;
}

/** The same figure, spelled out, for the text equivalent of the chart. */
function spoken(bps) {
  const r = rate(bps);
  if (r.value === "—") return "no reading";
  return `${r.value} ${r.unit === "kbps" ? "kilobits" : "megabits"} per second`;
}

/** Round-trip time, in the unit people quote it in. */
export function latency(ms) {
  if (ms == null || !Number.isFinite(ms)) return "—";
  if (ms < 1) return "<1 ms";
  if (ms < 10) return `${ms.toFixed(1)} ms`;
  return `${Math.round(ms)} ms`;
}

/** A time span, for the x-axis and for describing a run in words. */
function seconds(ms) {
  const s = Math.max(0, ms) / 1000;
  return Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`;
}

/* The words this dialog's buttons wear used to live here. They are now in
 * `cta.js`, with every other label in the app, because the same sentence was
 * being written in three places — the markup, this file, and the code that
 * repainted the button — and three copies of one string is three chances to
 * disagree with itself. */

/**
 * Host of a URL, for the "where did this come from" line.
 *
 * Deliberately forgiving: the user is allowed to type something with a typo in
 * it, and a speed test that refuses to tell them which host it failed on is
 * making a small problem harder than it needs to be.
 */
export function host(url) {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url.replace(/^[a-z]+:\/\//i, "").split("/")[0];
  }
}

/* ------------------------------------------------------------------ */
/* scales                                                               */
/* ------------------------------------------------------------------ */

/**
 * Round a scale up to a number a person would have chosen.
 *
 * Axis maxima land on 1, 2, 2.5 or 5 times a power of ten, so the top of the
 * axis reads 200 rather than 187.4 and its half reads 100 rather than 93.7.
 * Both of those can be held in your head while you look at the trace, which is
 * the whole reason a scale is worth labelling: a number you have to do
 * arithmetic on is a number you will not read.
 *
 * @param {number} value the largest thing that has to fit
 * @returns {number} a ceiling at or above `value`
 */
export function niceMax(value) {
  if (!Number.isFinite(value) || value <= 0) return 1e6; // 1 Mbps
  const magnitude = 10 ** Math.floor(Math.log10(value));
  const n = value / magnitude;
  const step = n <= 1 ? 1 : n <= 2 ? 2 : n <= 2.5 ? 2.5 : n <= 5 ? 5 : 10;
  return step * magnitude;
}

/**
 * Round a time span up to an interval a person would have chosen.
 *
 * The x axis is wall-clock, so its labels are seconds and they have to land on
 * 5 s and 10 s rather than 4.31 s. Picked from a short ladder rather than
 * computed, because the ladder *is* the list of intervals people read without
 * thinking, and a computed one always finds 7 s eventually.
 *
 * @param {number} ms the span that has to fit
 * @returns {number} an interval at or above `ms`, in milliseconds
 */
export function niceTime(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return 1000;
  for (const s of [1, 2, 5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300]) {
    if (ms <= s * 1000) return s * 1000;
  }
  return 600 * 1000;
}

/* ------------------------------------------------------------------ */
/* drawing helpers                                                      */
/* ------------------------------------------------------------------ */

/** Two decimals. Sub-pixel coordinates make the SVG larger for nothing. */
function round(n) {
  return Math.round(n * 100) / 100;
}

/**
 * One `<text>` node, with the presentation attributes spelled out.
 *
 * A helper rather than six inline templates because the alternative is
 * re-deciding the font stack six times, and a font stack that drifts apart
 * between the axis labels and the lane names is the kind of thing nobody
 * notices and everybody sees.
 */
function label(x, y, text, anchor, opacity = 0.6) {
  return (
    `<text x="${x}" y="${y}" fill="currentColor" fill-opacity="${opacity}" font-size="10" ` +
    `text-anchor="${anchor}" font-family="ui-monospace, SFMono-Regular, Consolas, monospace" ` +
    `style="font-variant-numeric:tabular-nums">${text}</text>`
  );
}

/* ------------------------------------------------------------------ */
/* the scope                                                            */
/* ------------------------------------------------------------------ */

/**
 * The two lanes, top to bottom, and the ink each is drawn in.
 *
 * Download first because it is the number people open this dialog for. Moss and
 * fuchsia are the only two hues in the app that are not the accent, and they
 * appear nowhere else. A directional colour is a legend that does not need
 * writing: the download line is the green one, the upload line is the pink one,
 * and the readout below repeats the pairing.
 */
const LANES = [
  { key: "down", name: "Download", ink: "var(--down)" },
  { key: "up", name: "Upload", ink: "var(--up)" },
];

/**
 * The latency that fills a level's tick.
 *
 * 100 ms twice over, so a rung measured at or above 200 ms pins its tick before
 * it can run into the line above it. Around 100 ms is where people start
 * noticing a call break up, which is what the ticks are for.
 */
const LATENCY_CEILING_MS = 100;

/**
 * A two-lane line chart of throughput against time.
 *
 * Not bars. A bar per level shows five decisions and throws away everything that
 * happened between them; the engine emits a live reading every quarter-second
 * inside each level, and the line drawn through those readings is the shape of
 * the connection. That shape is the measurement.
 *
 * Not one shared scale, either. Download and upload are stacked with independent
 * y axes because the difference between them is often tenfold, and sharing an
 * axis would render the upload lane as a flat line along its own floor -- a true
 * picture of the ratio and a useless picture of the upload.
 *
 * The x axis is real elapsed time rather than sample order. Order was right for
 * a chart of levels, where each rung was one decision; it is wrong here, because
 * the live readings are the data and spacing them evenly would invent a rhythm
 * the connection does not have.
 *
 * Drawn as SVG rather than a canvas so it stays crisp at any zoom and so the
 * browser hands us an accessible object the parent `figure` can describe in
 * words. Text below 640px of width does not survive a stretched viewBox, so the
 * drawing is in real pixels -- one unit to one CSS pixel -- and the viewBox is
 * re-fitted to the measured size on every resize.
 */
export class Scope {
  /**
   * @param {SVGSVGElement} svg element to draw into
   * @param {HTMLElement} empty the "nothing yet" placeholder to hide
   */
  constructor(svg, empty) {
    this.svg = svg;
    this.empty = empty;
    /** @type {{down: object[], up: object[]}} */
    this.series = { down: [], up: [] };

    // Drawn in real pixels, one SVG unit to one CSS pixel, with the viewBox
    // re-fitted whenever the frame changes size.
    this.w = 0;
    this.h = 0;

    // Where the plot goes, inside the frame. The left gutter is the y axis and
    // the bottom strip is the time axis; neither can overlap the data it
    // describes. The gap is the space between the two lanes: wide enough that
    // the eye reads two plots rather than one with a stripe through it.
    this.pad = { left: 52, right: 14, top: 18, bottom: 22 };
    this.gap = 30;

    this.measure();
    if (typeof ResizeObserver === "function") {
      // The dialog is laid out once and then the window changes size underneath
      // it. This is a convenience, not the mechanism: the instrument is also
      // sized when the dialog opens and on every sample, because an observer
      // that never delivers -- which is exactly what happens in the browser
      // preview, and in a real WebView2 would mean a silently blank chart rather
      // than an obviously broken one -- must not be the only thing standing
      // between a hidden dialog and a drawn one.
      this.observer = new ResizeObserver(() => {
        if (this.measure()) this.draw();
      });
      this.observer.observe(svg.parentElement ?? svg);
    }

    this.empty?.removeAttribute("hidden");
  }

  /**
   * Re-fit to the frame and redraw, for when the element has just become
   * visible.
   *
   * Public because the caller knows something this class cannot: the dialog has
   * just been unhidden. A `display: none` subtree measures zero, so the size
   * taken at construction is 0x0 and every later draw is correctly skipped as
   * "nowhere to draw" -- which from the outside is indistinguishable from a
   * chart that has decided not to render.
   */
  resize() {
    if (this.measure()) this.draw();
  }

  /**
   * Re-read the frame's size and re-fit the viewBox to it.
   *
   * A hidden dialog measures zero, and that is not an error worth throwing
   * over: it is the ordinary state of a dialog nobody has opened yet. The
   * previous size is kept and drawing is skipped until there is somewhere to
   * draw.
   *
   * @returns {boolean} whether there is a usable box
   */
  measure() {
    const box = (this.svg.parentElement ?? this.svg).getBoundingClientRect();
    const w = Math.round(box.width);
    const h = Math.round(box.height);
    if (!w || !h) return false;

    this.w = w;
    this.h = h;
    this.svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
    return true;
  }

  /** Forget the previous run. */
  reset() {
    this.series = { down: [], up: [] };
    this.svg.replaceChildren();
    this.empty?.removeAttribute("hidden");
  }

  /**
   * Add one reading and redraw.
   *
   * Every reading -- live or level, in either direction -- goes into the same
   * per-lane list. `kind` is carried through rather than split into a second
   * list, because the level averages have to be drawn *on* the live line they
   * belong to, and two lists would have to be kept in step to do that.
   *
   * @param {{direction?:string, kind?:string, elapsedMs?:number, bps:number, connections:number, totalBytes?:number, latency?:number|null}} sample
   */
  push(sample) {
    const lane = sample?.direction === "up" ? "up" : "down";
    this.series[lane].push({
      t: Number.isFinite(sample?.elapsedMs) ? Math.max(0, sample.elapsedMs) : 0,
      bps: Math.max(0, Number(sample?.bps) || 0),
      connections: Math.max(1, Number(sample?.connections) || 1),
      totalBytes: Math.max(0, Number(sample?.totalBytes) || 0),
      latency: Number.isFinite(sample?.latency) ? sample.latency : null,
      kind: sample?.kind === "level" ? "level" : "live",
    });

    if (this.empty?.hasAttribute("hidden") === false) this.empty.setAttribute("hidden", "");
    // Size on the way to drawing rather than trusting a measurement taken
    // earlier. A dialog opened at one window size and resized before the first
    // sample landed would otherwise draw into a frame that no longer matches.
    if (!this.w || !this.h) this.measure();
    this.draw();
  }

  /** The largest reading in one lane, over its whole life. */
  peak(lane) {
    return this.series[lane].reduce((m, p) => Math.max(m, p.bps), 0);
  }

  /** The latest time any lane has reached, in milliseconds. */
  span() {
    let t = 0;
    for (const { key } of LANES) {
      const pts = this.series[key];
      if (pts.length) t = Math.max(t, pts[pts.length - 1].t);
    }
    return t;
  }

  /**
   * The text equivalent of what is on screen.
   *
   * A picture of a number is not a number. This is what a screen reader is told,
   * and it is written as a sentence because that is what it will be read as.
   */
  describe() {
    const said = LANES.map(({ key, name }) => {
      const pts = this.series[key];
      if (!pts.length) return `No ${name.toLowerCase()} measurement.`;
      return (
        `${name}: peak ${spoken(this.peak(key))}, ` +
        `from ${pts.length} readings over ${seconds(this.span())}.`
      );
    });
    return `Throughput over time. ${said.join(" ")}`;
  }

  draw() {
    // No box yet means the dialog has not been opened. Nothing to draw into, and
    // no reason to invent a 0x0 chart the first real draw would have to undo.
    if (!this.w || !this.h) return;

    const { w, h, pad, gap } = this;
    const left = pad.left;
    const right = Math.max(left + 1, w - pad.right);
    const top = pad.top;
    const bottom = Math.max(top + 1, h - pad.bottom);
    const plotW = Math.max(1, right - left);
    const totalH = Math.max(2, bottom - top);
    const laneH = Math.max(20, (totalH - gap) / 2);

    const boxes = {
      down: { top, floor: top + laneH },
      up: { top: bottom - laneH, floor: bottom },
    };

    // The scale never collapses below one second, so a run caught between its
    // first and second reading does not draw a vertical line against a zero-width
    // axis. It is also the floor the first reading lands on.
    const tMax = Math.max(1000, this.span());
    const x = (t) => left + (Math.min(Math.max(t, 0), tMax) / tMax) * plotW;
    const out = [];

    // ---- the shared time grid ----------------------------------------
    // Drawn behind both lanes so the two series can be read against the same
    // moments. Two independent time axes would be two charts wearing one border.
    const tStep = niceTime(tMax / 4);
    const tCount = Math.min(12, Math.floor(tMax / tStep));
    for (let i = 0; i <= tCount; i++) {
      const t = i * tStep;
      const gx = round(x(t));
      out.push(
        `<line x1="${gx}" y1="${top}" x2="${gx}" y2="${bottom}" ` +
          `stroke="currentColor" stroke-opacity="0.07" stroke-width="1"/>`
      );
      out.push(label(gx, round(bottom + 14), seconds(t), i === 0 ? "start" : "middle", 0.5));
    }

    // ---- one lane ----------------------------------------------------
    const lane = ({ key, name, ink }, box) => {
      const pt = box.top;
      const pf = box.floor;
      const ph = Math.max(1, pf - pt);
      const pts = this.series[key];
      const ceiling = niceMax(this.peak(key));
      const y = (bps) => pf - Math.min(1, bps / ceiling) * ph;

      // One unit for the whole lane, chosen from the lane's own ceiling and then
      // used for every label on it. The lanes scale independently -- a 900 kbps
      // download beside an 18 Mbps upload is the normal case, not the odd one --
      // so a single unit across the chart would leave one lane reading 0.02 or
      // reading 18000. Mixing units within one axis would be worse still.
      const unit = rateUnit(ceiling);

      // Five rules, three numbers. Five numbers on a 150px axis is four too
      // many, and the ones that would be dropped are the ones in the middle,
      // which are the ones nobody was going to read anyway.
      for (let i = 0; i <= 4; i++) {
        const gy = pf - (ph * i) / 4;
        out.push(
          `<line x1="${left}" y1="${round(gy)}" x2="${right}" y2="${round(gy)}" ` +
            `stroke="currentColor" stroke-opacity="${i === 0 ? 0.24 : 0.09}" stroke-width="1"/>`
        );
        if (i === 0 || i === 2 || i === 4) {
          // `ceiling` is already bits per second, which is what `scaled` takes.
          out.push(
            label(
              round(left - 9),
              round(gy + 3.5),
              scaled((ceiling * i) / 4, unit),
              "end"
            )
          );
        }
      }

      // Which lane this is, in the lane's own ink, and its unit once per lane so
      // both independent scales are anchored to a name.
      out.push(
        `<text x="${round(left + 2)}" y="${round(pt + 11)}" fill="${ink}" fill-opacity="0.9" ` +
          `font-size="10" font-weight="600" letter-spacing="0.04em">` +
          `${name.toUpperCase()} &middot; ${unit}</text>`
      );

      if (!pts.length) {
        // Nothing measured here yet. A dashed rule at the floor says "listening"
        // without pretending a reading exists.
        out.push(
          `<line x1="${left}" y1="${round(pf)}" x2="${right}" y2="${round(pf)}" ` +
            `stroke="${ink}" stroke-opacity="0.18" stroke-width="1" stroke-dasharray="3 4"/>`
        );
        return;
      }

      const path = pts
        .map((p, i) => `${i ? "L" : "M"}${round(x(p.t))} ${round(y(p.bps))}`)
        .join(" ");

      // A light area under the line. The line is the measurement; the wash only
      // makes the space beneath it readable at a glance, and stays far enough
      // below the line's own opacity that the two never read as two series.
      out.push(
        `<path d="${path} L${round(x(pts[pts.length - 1].t))} ${round(pf)} ` +
          `L${round(x(pts[0].t))} ${round(pf)} Z" fill="${ink}" fill-opacity="0.09"/>`
      );
      out.push(
        `<path d="${path}" fill="none" stroke="${ink}" stroke-width="2" ` +
          `stroke-linejoin="round" stroke-linecap="round"/>`
      );

      // Level averages, marked. These are the readings the ramp judged and the
      // result reports; the live readings between them are the texture, these
      // are the decisions.
      for (const p of pts) {
        if (p.kind !== "level") continue;
        const cx = round(x(p.t));
        const cy = round(y(p.bps));
        out.push(`<circle cx="${cx}" cy="${cy}" r="2.6" fill="${ink}"/>`);

        // Under-load latency for this level, as a short tick above the dot. A
        // second axis and a second colour would be two more things to learn;
        // what the ticks show is a *shape* -- latency creeping up as throughput
        // climbs is the line getting crowded, and that is legible at a glance in
        // a way three separate figures are not.
        if (p.latency != null) {
          const ratio = Math.min(1, p.latency / (LATENCY_CEILING_MS * 2));
          const tick = Math.max(3, ph * 0.14 * ratio);
          out.push(
            `<line x1="${round(cx - 4)}" y1="${round(cy - tick)}" x2="${round(cx + 4)}" y2="${round(cy - tick)}" ` +
              `stroke="${ink}" stroke-opacity="0.5" stroke-width="1.5"/>`
          );
        }
      }

      // The live tip, where the line currently ends. On a time axis the tip's
      // *position* is time, so there is no sweeping playhead to draw -- a head
      // that raced ahead of the clock would be drawing data that does not exist.
      // The tip is the honest end of the line: a dot with a paper-coloured halo
      // so it stays legible where the line doubles back over itself.
      const tip = pts[pts.length - 1];
      out.push(
        `<circle cx="${round(x(tip.t))}" cy="${round(y(tip.bps))}" r="3.4" ` +
          `fill="${ink}" stroke="var(--paper-1)" stroke-width="1.5"/>`
      );
    };

    lane(LANES[0], boxes.down);
    lane(LANES[1], boxes.up);

    this.svg.innerHTML = out.join("");
  }
}

/* ------------------------------------------------------------------ */
/* the run                                                              */
/* ------------------------------------------------------------------ */

/** State the dialog can be in. One variable, so the UI cannot disagree. */
export const Phase = {
  Idle: "idle",
  Running: "running",
  Done: "done",
  Failed: "failed",
};

/**
 * Drives one speed test from start to result.
 *
 * Owns the phase and the cancel handle, and refuses to start a second run while
 * one is in flight -- double-clicking "Start" must not put two ramps on the
 * same connection and report the result as if it were one.
 */
export class SpeedTest {
  /**
   * @param {object} backend the active backend
   * @param {Scope} scope the instrument
   * @param {(phase: string) => void} onChange called whenever phase moves
   */
  constructor(backend, scope, onChange) {
    this.backend = backend;
    this.scope = scope;
    this.onChange = onChange;
    this.phase = Phase.Idle;
    /** @type {{cancel: () => void} | null} */
    this.run = null;
    this.result = null;
  }

  /** Whether a run is in flight. */
  get busy() {
    return this.phase === Phase.Running;
  }

  /**
   * Start a run.
   *
   * No target argument. Which server to measure against is a decision the
   * backend makes, not something the dialog hands it, and the field that used
   * to pass it could only ever be a way to break the test: a typo there gave
   * either a confident reading of nothing or an error the caller had to
   * explain in words. Picking the target is a policy question, so it lives
   * where the policy is.
   */
  async start() {
    if (this.busy) return;

    this.result = null;
    this.failure = null;
    this.scope.reset();
    this.set(Phase.Running);

    // The backend hands back a handle rather than a promise, because a promise
    // has nowhere to put "stop". Keeping the handle is what makes the Stop
    // button stop anything.
    this.run = this.backend.speedTest((sample) => {
      this.scope.push(sample);
      this.onChange(this.phase);
    });

    let out;
    try {
      out = await this.run.promise;
    } catch (err) {
      // A backend that rejects rather than reporting is still only ever a
      // failure to measure. Nothing has been downloaded, so there is no
      // partial state to unwind.
      this.result = null;
      this.failure = err?.message ?? String(err);
      this.set(Phase.Failed);
      return;
    }

    this.run = null;

    // A run may have been stopped and then finished. A result that arrives
    // after "stop" is still shown, because the person who pressed stop wanted
    // an answer sooner -- not no answer at all.
    this.result = out?.ok ? out.value : null;
    if (!this.result) {
      this.failure = out?.error ?? "That server did not send anything we could measure.";
      this.set(Phase.Failed);
      return;
    }

    this.set(Phase.Done);
  }

  /** Ask the run to stop. The result is still reported when it lands. */
  stop() {
    this.run?.cancel?.();
  }

  /** Back to the opening state, ready to run again. */
  reset() {
    this.stop();
    this.run = null;
    this.result = null;
    this.failure = null;
    this.scope.reset();
    this.set(Phase.Idle);
  }

  set(phase) {
    this.phase = phase;
    this.onChange(phase);
  }
}

/* ------------------------------------------------------------------ */
/* rendering the readout                                                */
/* ------------------------------------------------------------------ */

/**
 * Everything the readout shows, for one phase.
 *
 * Pure, so the whole panel can be checked by feeding it states rather than by
 * clicking through the app. Returns strings rather than touching the DOM.
 *
 * One number is the headline -- download -- and upload is a fact beside it. That
 * ordering is deliberate: the dialog is opened by someone deciding whether a
 * download will be quick, and that is the download figure. Upload is still
 * reported, because a line that is fast down and unusable up is a real and
 * common complaint, and it is now drawn right above the numbers.
 */
export function readout(test) {
  const scope = test.scope;
  const best = test.result;
  const down = scope.series?.down ?? [];
  const up = scope.series?.up ?? [];
  const tip = (pts) => (pts.length ? pts[pts.length - 1] : null);
  const lastLevel = (pts) => {
    for (let i = pts.length - 1; i >= 0; i--) if (pts[i].kind === "level") return pts[i];
    return null;
  };
  const bytesNow = () => (tip(down)?.totalBytes ?? 0) + (tip(up)?.totalBytes ?? 0);

  switch (test.phase) {
    case Phase.Idle:
      return {
        now: "—",
        unit: "Mbps",
        conns: "0",
        up: "—",
        idle: "—",
        jitter: "—",
        loaded: "—",
        transferred: "0 B",
        host: "—",
        note: "",
        tone: null,
        describe: scope.describe(),
      };

    case Phase.Running: {
      // Both directions are measured at the same time, so the headline is the
      // download lane's latest reading and upload is its live companion. There
      // is no "which direction is running now" question any more -- the answer
      // is always both -- which is why this no longer switches on the presence
      // of an upload reading.
      const live = tip(down);
      const upLive = tip(up);
      const judged = lastLevel(down);
      const now = rate(live?.bps);

      return {
        now: now.value,
        unit: now.unit,
        conns: live ? String(live.connections) : "0",
        up: rateText(upLive?.bps),
        // The idle distribution is taken before the ramp but arrives with the
        // result, so there is genuinely nothing to put here yet. Under-load
        // latency is different: it belongs to a finished level, so it fills in
        // as soon as one lands.
        idle: "—",
        jitter: "—",
        loaded: judged?.latency == null ? "—" : latency(judged.latency),
        transferred: formatBytes(bytesNow()),
        // Left blank until a result lands. Printing a host we have not actually
        // contacted yet would be a claim rather than a measurement.
        host: "—",
        note: live ? "Testing download and upload together…" : "Contacting the server…",
        tone: null,
        describe: scope.describe(),
      };
    }

    case Phase.Failed:
      return {
        now: "—",
        unit: "Mbps",
        conns: "0",
        up: "—",
        idle: "—",
        jitter: "—",
        loaded: "—",
        transferred: "0 B",
        host: "—",
        note: test.failure || "That server did not send anything we could measure.",
        tone: "bad",
        describe: "The measurement failed.",
      };

    case Phase.Done:
    default: {
      if (!best) {
        return {
          now: "—",
          unit: "Mbps",
          conns: "0",
          up: "—",
          idle: "—",
          jitter: "—",
          loaded: "—",
          transferred: "0 B",
          host: "—",
          note: "No result came back.",
          tone: "bad",
          describe: "The measurement finished without a result.",
        };
      }

      // The one line that turns three numbers into a diagnosis, and it is only
      // said when it is true. A large gap between idle and loaded latency is
      // congestion: the link is not slow, it is crowded. That is what makes
      // video calls break while a download still shows a healthy speed, and it
      // is a complaint no amount of extra bandwidth fixes.
      const crowded =
        best.loaded != null && best.idle != null && best.loaded - best.idle > 60;

      let note;
      if (best.stoppedEarly) {
        note = "Stopped early, so this is what it reached — not necessarily your limit.";
      } else if (crowded) {
        note =
          "Your speed is good, but it gets in the way of everything else while it runs. That is what makes calls drop during a download.";
      } else {
        note =
          "Your connection topped out here. Multiple connections beat a single stream, so this is the speed a download will actually reach.";
      }

      // Upload is only mentioned when it was measured. An origin that refused a
      // body is not a failure and must not be reported as one, so the sentence
      // simply does not appear.
      if (best.upload) {
        note += ` Upload reached ${rateText(best.upload.bps)}.`;
      }

      const done = rate(best.bps);

      return {
        now: done.value,
        unit: done.unit,
        conns: String(best.connections),
        up: rateText(best.upload?.bps),
        idle: best.idle == null ? "—" : latency(best.idle),
        jitter: best.jitter == null ? "—" : latency(best.jitter),
        loaded: best.loaded == null ? "—" : latency(best.loaded),
        transferred: formatBytes(best.totalBytes + (best.upload?.totalBytes ?? 0)),
        host: best.host || "—",
        note,
        tone: null,
        describe: scope.describe(),
      };
    }
  }
}
