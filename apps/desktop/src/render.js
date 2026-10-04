// Row / map / rail rendering.
//
// Two things here are worth reading closely:
//
//   1. The byte map draws the file to scale and draws a *seam* wherever a
//      previous connection ended. It is the only place in the product where
//      the resume logic is visible rather than asserted, so it gets the
//      detail budget: segment boundaries, the write head, and — critically —
//      a full-width field labelled "length unknown" when the origin never
//      told us how big the file is. No fake percentage. Ever.
//
//   2. The rail is a timeline of the connection, not a status dot. Ticks are
//      placed by *when* they happened, so a transfer that was healthy for
//      thirty seconds and then died looks different from one that died
//      immediately, which is information a coloured dot throws away.

import * as F from "./format.js";
import { prettyHost, pathSnippet, monogram } from "./link.js";
import { downloadCta } from "./cta.js";
import { brandForUrl } from "./sites.js";
import { BRANDS } from "./brands.js";
import { ACTIVE, State } from "./backend.js";

export function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "text") el.textContent = v;
    // `html` is only ever used for the module-local ICON table below — never
    // for anything derived from a URL, a filename, or a server response.
    else if (k === "html") el.innerHTML = v;
    else if (k === "style") el.style.cssText = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    el.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

const short = (s, n = 46) => (s.length > n ? s.slice(0, n - 1) + "…" : s);

/* ------------------------------------------------------------------ */
/* icons                                                               */
/* ------------------------------------------------------------------ */

/* Inline SVG rather than a font or a sprite sheet: the whole app ships with
 * zero dependencies, and these three glyphs are not worth a build step. */
const ICON = {
  pause: '<rect x="4" y="3" width="2.5" height="8"/><rect x="9.5" y="3" width="2.5" height="8"/>',
  play: '<path d="M5 3l7 4-7 4z"/>',
  details:
    '<path d="M2 4h1.6M2 7h1.6M2 10h1.6" stroke="currentColor" stroke-width="1.4"/>' +
    '<path d="M6 4h6M6 7h6M6 10h4" stroke="currentColor" stroke-width="1.2"/>',
  remove: '<path d="M3.5 3.5l7 7M10.5 3.5l-7 7" stroke="currentColor" stroke-width="1.3"/>',
  folder:
    '<path d="M2 4.5h3.6l1.2 1.4h5.2v6.6H2z" fill="none" stroke="currentColor" stroke-width="1.2"/>',
};

function icon(name) {
  return h("span", {
    class: "ico",
    "aria-hidden": "true",
    html: `<svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor">${ICON[name]}</svg>`,
  });
}

/* ------------------------------------------------------------------ */
/* paste bar                                                           */
/* ------------------------------------------------------------------ */

/* The clipboard is the only channel between a browser and a desktop app, so
 * this is where a link you copied actually becomes a download. It sits above
 * the queue rather than inside it, because it is an offer, not an item.
 *
 * The favicon is a nice-to-have with a rule attached: it is fetched from the
 * *same origin you are downloading from*, never from a favicon lookup service.
 * Those services work by telling a third party every site you visit, which is
 * the exact thing this app exists not to do. Until it arrives, the monogram
 * tile stands in — and if the site has no icon, it simply stays a monogram. */
/**
 * The 20px tile that stands for a link, in three tiers.
 *
 *   1. The service's own mark, from the generated `brands.js`. A YouTube link
 *      gets the YouTube logo rather than the letter "Y" -- not decoration, but
 *      recognition: the logo is the thing people already use to recognise the
 *      site, and a letter in a box asks them to read instead of look.
 *   2. The site's real favicon, fetched from the origin we are downloading from
 *      and nowhere else. This is the tier for services we can fetch but have no
 *      bundled mark for.
 *   3. A monogram, when neither exists. Same box either way, so nothing moves
 *      when a mark or an icon arrives.
 *
 * The mark is built with the SVG namespace rather than `innerHTML`, so the one
 * place in this file that injects markup stays the module-local icon table.
 * Nothing here is derived from the URL beyond picking which bundled brand to
 * draw: the slug comes from our own host table, never from the link.
 */
function favTile(p) {
  const brand = BRANDS.find((b) => b.slug === brandForUrl(p.url));

  if (brand) {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("class", "fav-mark");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", brand.path);
    path.setAttribute("fill", brand.hex);
    svg.append(path);
    return svg;
  }

  if (p.favicon) {
    return h("img", { class: "fav", src: p.favicon, alt: "", width: 20, height: 20, decoding: "async" });
  }

  return h("span", { class: "fav mono", text: monogram(p.url) });
}

export function renderPasteBar(p, on) {
  const tile = favTile(p);

  return h(
    "div",
    {
      class: "pastebar",
      role: "status",
      "aria-live": "polite",
      "data-tip":
        "ifami noticed this because your clipboard holds it.\nWe only read the clipboard while this window has focus, and only to ask whether it is a link. Nothing is stored or sent anywhere.",
    },
    tile,
    h(
      "div",
      { class: "paste-text" },
      h(
        "div",
        { class: "paste-head" },
        "You copied a link from ",
        h("b", { text: prettyHost(p.url) }),
        p.busy ? h("span", { class: "paste-busy", text: "checking…" }) : null
      ),
      h("div", { class: "paste-path num", text: pathSnippet(p.url) || p.url })
    ),
    h(
      "div",
      { class: "paste-acts" },
      h(
        "button",
        {
          class: "primary-btn",
          type: "button",
          // What it will fetch decides what it says. A YouTube Music link gets
          // "Download music", a video gets "Download media", and a link that
          // plainly names a file gets "Download file" -- so the button has said
          // what is coming before the user has committed to anything. Decided
          // from the URL alone: nothing is fetched to pick a word.
          onclick: () => on.accept(p.url),
        },
        downloadCta(p.url)
      ),
      h(
        "button",
        {
          class: "ghost-btn",
          type: "button",
          "aria-label": "Dismiss",
          title: "Dismiss",
          onclick: () => on.dismiss(p.url),
        },
        icon("remove")
      )
    )
  );
}

/* ------------------------------------------------------------------ */
/* byte map                                                            */
/* ------------------------------------------------------------------ */

const TICK_CLASS = {
  cut: "ev-cut",
  resume: "ev-resume",
  stall: "ev-stall",
  done: "ev-done",
  fail: "ev-fail",
};

/* One sentence a non-engineer can act on. Hovering the bar should explain
 * itself; nobody should need to read the docs to know what the orange tick
 * means. */
function describe(t) {
  const seams = (t.seams ?? []).length;
  const saved = F.bytes(t.received);

  if (t.state === State.FAILED) {
    return `${t.name} could not be downloaded.\n${t.error?.message ?? "The server refused the request."}`;
  }

  if (t.total == null) {
    return `${saved} saved so far.\nThe site never said how big this file is, so there is no percentage to show.`;
  }

  const pct = F.percent(t.received, t.total);
  let out = `${pct}% \u2014 ${saved} of ${F.bytes(t.total)} saved.`;
  if (seams === 0) {
    out += t.state === State.DONE ? "\nSaved in one go, with no interruptions." : "\nNo interruptions so far.";
  } else {
    out +=
      seams === 1
        ? "\nOne orange tick: the connection broke there, and ifami carried on from that exact byte."
        : `\n${seams} orange ticks: where the connection broke, and where ifami carried on from.`;
  }
  if (t.state === State.PAUSED) out += "\nCurrently paused.";
  return out;
}

function byteMap(t) {
  const known = t.total != null && t.total > 0;
  const pct = known ? Math.min(100, (t.received / t.total) * 100) : 0;
  const live = t.state === State.RUNNING;

  const map = h("div", {
    class: `map${known ? "" : " is-unknown"}${t.state === State.DONE ? " is-complete" : ""}${
      t.state === State.FAILED ? " is-failed" : ""
    }`,
    role: "img",
    "aria-label": describe(t),
    "data-tip": describe(t),
  });

  map.append(h("div", { class: "hatch" }));

  if (known && t.received > 0) {
    map.append(h("div", { class: "fill", style: `width:${pct.toFixed(3)}%` }));
  }

  if (!known) {
    map.append(h("div", { class: "note", text: "size unknown" }));
  }

  // Segment boundaries, for fragmented transfers.
  if (known) {
    for (const at of t.marks ?? []) {
      if (at <= 0 || at >= t.total) continue;
      map.append(h("div", { class: "mark", style: `left:${((at / t.total) * 100).toFixed(3)}%` }));
    }
    // The seams. The whole point.
    for (const at of t.seams ?? []) {
      if (at <= 0 || at >= t.total) continue;
      map.append(
        h("div", {
          class: "seam",
          style: `left:${((at / t.total) * 100).toFixed(3)}%`,
          "data-tip": `Connection broke here, at ${F.bytes(at)}.\nifami carried on from exactly this byte \u2014 nothing was re-downloaded.`,
        })
      );
    }
  }

  if (live && known) {
    map.append(h("div", { class: "head", style: `left:${pct.toFixed(3)}%` }));
  }

  return map;
}

/* ------------------------------------------------------------------ */
/* event rail                                                          */
/* ------------------------------------------------------------------ */

function rail(t) {
  const el = h("div", { class: "rail" }, h("div", { class: "sel" }));

  const start = t.startedAt;
  const end = t.state === State.DONE || t.state === State.FAILED ? (t.finishedAt ?? Date.now()) : Date.now();
  if (!start) return el;

  const span = Math.max(1, end - start);
  const interesting = t.events.filter((e) => TICK_CLASS[e.kind]);

  for (const e of interesting) {
    const pos = Math.max(0, Math.min(1, (e.t - start) / span));
    el.append(
      h("div", {
        class: `tick ${TICK_CLASS[e.kind]}`,
        style: `top:${(pos * 100).toFixed(2)}%`,
        "data-tip": `${e.kind} · ${F.clock(e.t)}\n${e.msg}`,
      })
    );
  }
  return el;
}

/* ------------------------------------------------------------------ */
/* meta line                                                           */
/* ------------------------------------------------------------------ */

function metaLine(t) {
  const bits = [];
  const seams = (t.seams ?? []).length;

  if (t.state === State.FAILED && t.error) {
    return h("div", { class: "meta" }, h("span", { style: "color:var(--bad)", text: t.error.message }));
  }

  if (t.state === State.QUEUED) {
    bits.push(short(t.events[t.events.length - 1]?.msg ?? "Waiting to start"));
  } else if (t.state === State.DONE) {
    // The honest version of a claim most managers make: it finished, and here
    // is the receipt for how it got there.
    bits.push(
      seams
        ? h("span", { class: "accent", text: `finished after ${seams} interruption${seams === 1 ? "" : "s"}` })
        : "finished in one go",
      t.kind === "fragmented" ? "every segment received" : "size checked against the server's figure"
    );
  } else {
    if (t.total == null) {
      bits.push("the site never said how big this is \u2014 saving until it ends");
    } else if (t.state === State.PAUSED) {
      bits.push(`paused at ${F.percent(t.received, t.total)}%`);
    }
    if (seams) {
      bits.push(
        h("span", {
          class: "accent",
          text: `picked up where it stopped (${seams} time${seams === 1 ? "" : "s"})`,
        })
      );
    }
    if (t.kind === "fragmented" && t.segmentsTotal) {
      bits.push(`part ${t.segmentsDone ?? 0} of ${t.segmentsTotal}`);
    }
    if (t.kind === "direct" && t.state === State.RUNNING && seams === 0) {
      bits.push(`from ${host(t.source)}`);
    }
  }

  const out = [];
  bits.filter(Boolean).forEach((b, i) => {
    if (i) out.push(h("span", { class: "sep", text: "\u00b7" }));
    // `b` may already be an element (an accented phrase); wrapping it again would
    // stringify it to "[object HTMLSpanElement]".
    out.push(typeof b === "string" ? h("span", { text: b }) : b);
  });
  return h("div", { class: "meta" }, ...out);
}

function host(url) {
  try {
    return new URL(url).host;
  } catch {
    return "";
  }
}

function tagFor(t) {
  const map = {
    [State.RUNNING]: ["live", "downloading"],
    [State.RETRYING]: ["live", "reconnecting"],
    [State.PAUSED]: ["", "paused"],
    [State.QUEUED]: ["", "waiting"],
    [State.DONE]: ["", "finished"],
    [State.FAILED]: ["failed", "refused"],
  };
  const [cls, label] = map[t.state] ?? ["", t.state];
  return h("span", { class: `tag ${cls}`, text: label });
}

/* ------------------------------------------------------------------ */
/* row                                                                 */
/* ------------------------------------------------------------------ */

/* Every key binding in the app also exists as a button here. That is the whole
 * point of this cluster: a mouse user must never have to discover a keyboard
 * shortcut, and a keyboard user must never have to discover a button.
 *
 * The shortcut is deliberately *not* part of the visible label. "Pause (Space)"
 * on every row of every download is noise to the people who are only clicking,
 * and the parenthesis is exactly what made the app feel like it was designed
 * for someone else. It lives in `aria-keyshortcuts` instead: still announced by
 * assistive technology, still in the markup for anyone reading it, and absent
 * from the visual design. The keys remain a quiet accelerator, never a
 * requirement. */
function actions(t, on) {
  const running = ACTIVE.has(t.state);
  const paused = t.state === State.PAUSED;

  const btn = (act, name, label, key, disabled = false, cls = "") =>
    h("button", {
      class: cls,
      type: "button",
      "data-act": act,
      "aria-label": label,
      "aria-keyshortcuts": key,
      "data-tip": label,
      title: label,
      disabled: disabled || false,
      onclick: (e) => {
        e.stopPropagation();
        on[act](t.id);
      },
    }, icon(name));

  return h(
    "div",
    { class: "acts" },
    running || paused
      ? btn("toggle", paused ? "play" : "pause", paused ? "Resume" : "Pause", "Space")
      : btn("toggle", "play", "Start again", "Space", true),
    btn("details", "details", "Show details", "Enter"),
    btn("remove", "remove", "Remove", "Del", false, "act-rm")
  );
}

export function renderRow(t, selected, on) {
  return h(
    "div",
    {
      class: "row",
      role: "option",
      tabindex: "-1",
      "data-id": t.id,
      "aria-selected": selected ? "true" : "false",
    },
    rail(t),
    h(
      "div",
      { class: "body" },
      h("div", { class: "name-line" }, h("span", { class: "name", text: t.name, title: t.name }), tagFor(t)),
      byteMap(t),
      metaLine(t)
    ),
    h(
      "div",
      { class: "cols" },
      h("div", { class: "v num small", text: F.transferred(t.received, t.total) }),
      h("div", { class: "v num", text: F.rate(t.rate) }),
      h("div", { class: "v num small dim", text: F.remaining(t.received, t.total, t.rate) })
    ),
    actions(t, on)
  );
}

/* ------------------------------------------------------------------ */
/* detail pane                                                         */
/* ------------------------------------------------------------------ */

const RECORDER_CLASS = {
  cut: "k-cut",
  resume: "k-resume",
  fail: "k-fail",
  done: "k-cut",
  seg: "",
  open: "",
  note: "",
};

/* "Resolver" and "declared length" are words only the author of this code
 * uses. The pane is for whoever is looking at the screen, so it says
 * "Detected as" and "File size". The precise HTTP-level detail is one hover
 * away, not in the way. */
const RESOLVER_PLAIN = {
  direct: "A direct file link",
  hls: "An HLS (.m3u8) stream",
  dash: "A DASH (.mpd) stream",
  page: "A web page — the media inside it",
  none: "Nothing recognisable",
};

export function renderDetail(t, on) {
  const frag = document.createDocumentFragment();

  const field = (k, v, cls = "", tip = null) =>
    frag.append(
      h(
        "div",
        { class: "field" },
        h("span", { class: "k", text: k, ...(tip ? { "data-tip": tip } : {}) }),
        h("div", { class: `val ${cls}`, text: v })
      )
    );

  const pct = F.percent(t.received, t.total);
  frag.append(
    h(
      "div",
      { class: "field" },
      h("span", { class: "k", text: "Saved so far" }),
      h("div", { class: "val big num", text: F.transferred(t.received, t.total) }),
      h("div", {
        class: "val",
        style: "color:var(--ink-3);margin-top:4px",
        text: pct == null
          ? "the server never said how big this is, so there is no percentage"
          : `${pct}% of the file's full size`,
      })
    )
  );

  field("Where it came from", t.source);
  field("Detected as", RESOLVER_PLAIN[t.resolver] ?? t.resolver ?? "unknown", "", `resolver: ${t.resolver ?? "none"}`);
  if (t.container) field("Format", `${t.container}${t.codec ? " \u00b7 " + t.codec : ""}`);
  if (t.total != null)
    field("File size", `${F.bytes(t.total)}`, "", `Content-Length: ${t.total.toLocaleString("en-US")} bytes`);
  if (t.kind === "fragmented" && t.segmentsTotal) field("Parts", `${t.segmentsDone ?? 0} of ${t.segmentsTotal} received`);
  if ((t.seams ?? []).length)
    field(
      "Picked up after",
      `${t.seams.length} interruption${t.seams.length === 1 ? "" : "s"} at ${t.seams
        .map((b) => F.bytes(b))
        .join(", ")}`
    );

  if (t.error) {
    frag.append(
      h(
        "div",
        { class: "field" },
        h("span", { class: "k", text: "Why it stopped" }),
        h("div", { class: "val", style: "color:var(--bad)", text: plainError(t.error.code) }),
        h("div", { class: "val", style: "color:var(--ink-2);margin-top:4px", text: t.error.message })
      )
    );
  }

  // Where did my file actually go? This is the single most-asked question in
  // any download tool, and it deserves a button rather than a path to read.
  if (t.dest) {
    frag.append(
      h(
        "div",
        { class: "field" },
        h("span", { class: "k", text: "Saved to" }),
        h("div", { class: "val", text: t.dest }),
        h("div", { class: "fieldbtns" },
          h("button", {
            class: "ghost-btn",
            type: "button",
            "data-tip": "Open the folder in Explorer",
            onclick: () => on.reveal(t.id),
          }, icon("folder"), "Show in folder")
        )
      )
    );
  }

  // ---- flight recorder ----
  const log = h(
    "div",
    { class: "recorder" },
    h("span", { class: "label", text: "What happened" }),
    h("div", { class: "val", style: "color:var(--ink-4);font-size:var(--t-micro);margin-top:var(--u)", text: "Every request and every interruption, newest first." })
  );
  const ol = h("ol", {});
  for (const e of [...t.events].reverse().slice(0, 40)) {
    ol.append(
      h(
        "li",
        { class: RECORDER_CLASS[e.kind] ?? "" },
        h("span", { class: "t num", text: F.clock(e.t) }),
        h("span", { class: "m", text: e.msg })
      )
    );
  }
  log.append(ol);
  frag.append(log);

  return frag;
}

function plainError(code) {
  return (
    {
      DrmProtected: "This media is encrypted",
      AuthRequired: "This media needs a sign-in",
    }[code] ?? code
  );
}

export { ACTIVE, State };
