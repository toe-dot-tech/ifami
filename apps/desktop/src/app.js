// ifami desktop — application wiring.
//
// Deliberately framework-free. The whole UI is ~5 modules of plain DOM with no
// build step, no node_modules, and no framework runtime shipped to the user's
// machine. Open `index.html` and it runs; Tauri loads the same files from disk.
//
// Render strategy: a backend snapshot replaces the row list wholesale, at most
// once per animation frame. For the queue sizes this app is built for that is
// both fast enough and much easier to reason about than in-place patching — and
// the queue is authoritative state, not a source of truth worth diffing.

import { createBackend, ACTIVE, State } from "./backend.js";
import { renderRow, renderDetail, renderPasteBar, h } from "./render.js";
import { Dismissed } from "./clipboard.js";
import { Scope, SpeedTest, Phase, readout } from "./speed.js";
import { SPEED, SOFTWARE } from "./cta.js";
import { brandForUrl } from "./sites.js";
import { supportedSites } from "./brands-ui.js";
import * as F from "./format.js";

const $ = (id) => document.getElementById(id);

const el = {
  rows: $("rows"),
  pane: $("pane"),
  empty: $("empty"),
  emptyNew: $("empty-new"),
  listEmpty: $("list-empty"),
  sitesSlot: $("sites-slot"),
  scroller: $("scroller"),
  tabs: $("tabs"),
  detailScrim: $("detail-scrim"),
  detail: $("detail"),
  detailTitle: $("detail-title"),
  detailClose: $("detail-close"),
  detailBody: $("detail-body"),
  livecount: $("livecount"),
  liven: $("liven"),
  ver: $("ver"),
  tip: $("tip"),
  snackbar: $("snackbar"),
  addSlot: $("add-slot"),
  addUrl: $("add-url"),
  addGo: $("add-go"),
  addCancel: $("add-cancel"),
  helpScrim: $("help-scrim"),
  speedScrim: $("speed-scrim"),
  speedSvg: $("speed-svg"),
  speedScope: $("speed-scope"),
  speedScopeEmpty: $("speed-scope-empty"),
  speedNow: $("speed-now"),
  speedConns: $("speed-conns"),
  speedUp: $("speed-up"),
  speedIdle: $("speed-idle"),
  speedJitter: $("speed-jitter"),
  speedLoaded: $("speed-loaded"),
  speedBytes: $("speed-bytes"),
  speedHost: $("speed-host"),
  speedNote: $("speed-note"),
  speedStart: $("speed-start"),
  speedStop: $("speed-stop"),
  pasteSlot: $("paste-slot"),
  btnExpand: $("btn-expand"),
  queueFloat: $("queue-float"),
  queueFloatList: $("queue-float-list"),
  queueFloatClose: $("queue-float-close"),
  sbQueue: $("sb-queue"),
  sbDisk: $("sb-disk"),
};

const state = {
  tasks: [],
  filter: "all",
  selected: null,
  detailOpen: false,
  info: null,
  // The link currently in the clipboard, if it is one we want to offer.
  paste: null,
  // Whether the queue is open in the bigger floating box.
  expanded: false,
  // Which download the details dialog is describing, once it is open. Kept
  // separately from `selected` because the dialog is a snapshot: it names one
  // download and must not quietly start describing another.
  detailFor: null,
};

// Where focus came from when the details dialog opened, so it can go back there.
let detailReturnFocus = null;

// Which host we are running on decides everything downstream — mock or Tauri,
// window buttons or no window buttons — so it is resolved once, at the top,
// where the rest of the module can be sure it exists.
//
// It used to live at the head of `boot()`. That worked right up until the
// speed test needed a backend at module scope to construct its controller, and
// the reference landed in the temporal dead zone: `const` is hoisted but not
// initialised, so the whole module failed to evaluate and the page came up
// blank with the window controls still on it. Declaring it here means the
// ordering is right by construction rather than by luck of where someone
// happened to add a listener.
const backend = createBackend();

/* ------------------------------------------------------------------ */
/* selection & filtering                                               */
/* ------------------------------------------------------------------ */

const FILTERS = {
  all: () => true,
  active: (t) => ACTIVE.has(t.state) || t.state === State.PAUSED,
  done: (t) => t.state === State.DONE,
  problem: (t) => t.state === State.FAILED,
};

function visible() {
  return state.tasks.filter(FILTERS[state.filter]);
}

function selectedTask() {
  return state.tasks.find((t) => t.id === state.selected) ?? null;
}

function move(delta) {
  const list = visible();
  if (!list.length) return;
  const i = list.findIndex((t) => t.id === state.selected);
  const next = i === -1 ? 0 : Math.max(0, Math.min(list.length - 1, i + delta));
  state.selected = list[next].id;
  render();
  el.rows.querySelector(`[data-id="${CSS.escape(state.selected)}"]`)?.scrollIntoView({ block: "nearest" });
}

/* ------------------------------------------------------------------ */
/* render                                                              */
/* ------------------------------------------------------------------ */

let frame = null;
let flushTimer = null;

function schedule() {
  if (frame || flushTimer) return;
  // rAF is the right clock: many backend ticks collapse into one paint. But it
  // does not fire in a hidden or minimised window, so without the timer the
  // queue silently never paints while the app is in the background — and a
  // restored window shows a stale queue. Whichever fires first wins and the
  // other is cancelled, so this is still exactly one render per batch.
  frame = requestAnimationFrame(() => {
    frame = null;
    clearTimeout(flushTimer);
    flushTimer = null;
    render();
  });
  flushTimer = setTimeout(() => {
    flushTimer = null;
    if (frame) {
      cancelAnimationFrame(frame);
      frame = null;
    }
    render();
  }, 120);
}

const TAG_COUNTS = {
  all: () => state.tasks.length,
  active: () => state.tasks.filter(FILTERS.active).length,
  done: () => state.tasks.filter(FILTERS.done).length,
  problem: () => state.tasks.filter(FILTERS.problem).length,
};

function render() {
  const list = visible();
  const top = el.scroller.scrollTop;

  // The floating queue keeps its own scroll offset for the same reason, and it
  // cannot reuse the pane's: it is a different box showing the same rows, and
  // somebody reading the bottom of a queue in the big window has no opinion
  // about where the small one is scrolled to.
  const floatTop = el.queueFloatList.scrollTop;

  // Rows are rebuilt wholesale on every tick, so a keyboard user who has
  // tabbed onto a row button would be dumped back to the top of the document
  // every 250ms. Remember where focus was and put it back on the same control.
  const focused = document.activeElement;
  const keep = focused?.closest?.(".row") ? { id: focused.closest(".row").dataset.id, act: focused.dataset.act } : null;

  el.rows.replaceChildren();
  for (const t of list) el.rows.append(renderRow(t, t.id === state.selected, on));

  // The expanded queue is a second list, not a copy of the first element: the
  // rows are rebuilt every tick here, and a node can only be in one place.
  // Rebuilt only while it is open, because building eight rows nobody is looking
  // at four times a second is exactly the sort of work this app should not do.
  if (state.expanded) {
    el.queueFloatList.replaceChildren();
    for (const t of list) el.queueFloatList.append(renderRow(t, t.id === state.selected, on));
    if (el.queueFloatList.childElementCount === 0) {
      el.queueFloatList.append(h("p", { class: "list-empty", text: emptySentence() }));
    }
    el.queueFloatList.scrollTop = floatTop;
  }

  if (keep) {
    const again = el.rows.querySelector(`[data-id="${CSS.escape(keep.id)}"] [data-act="${keep.act}"]`);
    if (again && again.disabled !== true) again.focus({ preventScroll: true });
  }

  el.rows.parentElement.scrollTop = top;

  // The hero shrinks rather than disappearing once there is something in the
  // queue. It used to be hidden outright, on the reasoning that the welcome is
  // only for an empty app -- which is true of the constellation and the
  // paragraph and the reassurance line, and false of the heading and the "Add a
  // download" button. Hiding the lot meant the one control you want for the
  // third download was the one that vanished for it, and the paste bar had
  // already hidden itself because you had pasted something.
  //
  // Which parts go is CSS, keyed off `pane-empty` below: `.empty` keeps its box
  // and loses its decoration.
  el.empty.hidden = false;
  // The empty state is the one screen tall enough to outgrow a short window and
  // has no scroller of its own, so the pane itself scrolls while it is showing.
  el.pane.classList.toggle("pane-empty", state.tasks.length === 0);

  // What the list says when it is showing nothing. Two different situations get
  // two different sentences: an empty queue is not the same as a filter that
  // matched none of your downloads, and saying "No downloads yet" while the All
  // tab says 7 would be the app contradicting itself on screen.
  if (list.length === 0) {
    el.listEmpty.hidden = false;
    el.listEmpty.textContent = emptySentence();
  } else {
    el.listEmpty.hidden = true;
  }

  for (const tab of el.tabs.querySelectorAll(".tab")) {
    const f = tab.dataset.filter;
    tab.setAttribute("aria-selected", String(f === state.filter));
    tab.querySelector(".n").textContent = TAG_COUNTS[f]();
  }

  const inflight = state.tasks.filter((t) => t.state === State.RUNNING || t.state === State.RETRYING).length;
  el.livecount.hidden = inflight === 0;
  el.liven.textContent = inflight;

  el.sbQueue.textContent = `${state.tasks.filter((t) => ACTIVE.has(t.state) || t.state === State.PAUSED).length} queued`;
  el.sbDisk.textContent = `${F.bytes(state.tasks.reduce((a, t) => a + (t.received ?? 0), 0))} on disk`;

  // Removing the last download while the queue is open would otherwise leave an
  // empty section under the list, with the control that opened it gone.
  if (state.expanded && state.tasks.length === 0) setExpanded(false, false);
  syncExpandToggle();
  // Asserted rather than set, and on every tick, so that this widget's
  // visibility has exactly one definition. `hidden` in the markup is the first
  // one and the only one that applies before this module has finished loading;
  // this is the one that holds afterwards, including through any state change
  // that does not go near `setExpanded`.
  if (el.queueFloat.hidden !== state.expanded) setExpanded(state.expanded, false);

  renderPasteSlot();
  renderDetailPopup();
}

/**
 * What an empty list says, which depends on *why* it is empty.
 *
 * A queue with nothing in it is a different statement from a filter that matched
 * none of your downloads, and the app saying "No downloads yet" while the All tab
 * counted seven is the app contradicting itself in front of the user.
 */
function emptySentence() {
  if (state.tasks.length === 0) return "No downloads yet.";
  if (state.filter === "problem") return "Nothing went wrong. Nice.";
  if (state.filter === "active") return "Nothing is downloading right now.";
  return "No finished downloads yet.";
}

/* ------------------------------------------------------------------ */
/* the expanded queue                                                  */
/* ------------------------------------------------------------------ */

/**
 * Whether the toggle is offered at all.
 *
 * One reason to withhold it, and it is the only one: there is nothing to
 * enlarge. Hiding rather than disabling — a disabled control with no reason
 * attached is a small puzzle, and the empty state has already said "no downloads
 * yet" about as loudly as it is going to.
 *
 * There used to be a second condition here: that the window left enough room
 * below the fixed list for a third section. That was for the version where the
 * expanded queue was a section rather than a panel, and it is gone with it.
 */
function syncExpandToggle() {
  el.btnExpand.hidden = state.tasks.length === 0;
}

/**
 * Open or close the bigger panel over the app.
 *
 * This is a second presentation, not a resize. The list in the pane is sized by
 * the window and nothing else, so the toolbar and the column labels do not move,
 * and adding a download cannot push anything towards the titlebar. Opening this
 * changes nothing underneath it either, which is the property that makes it
 * safe: if it resized the pane instead, opening it would be the very thing that
 * pushed rows into the header.
 *
 * It sits over the window rather than under the list because there is nowhere
 * under the list to put it -- the list already has the height it is entitled to,
 * and a queue that grows downwards past the statusbar is the same bug as one
 * that grows upwards past the titlebar.
 *
 * One function for both directions, because a button whose label is written in
 * three places ends up saying "Expand" while it expands.
 *
 * `focus` is false only when the queue is being closed *for* the user — because
 * the last download was removed, or the window got too short to hold it — where
 * sending focus to a button that is about to disappear would be worse than
 * leaving it where it was.
 */
function setExpanded(open, focus = true) {
  state.expanded = open;

  el.queueFloat.hidden = !open;
  syncExpandToggle();

  const verb = open ? "Collapse" : "Expand";
  el.btnExpand.setAttribute("aria-expanded", String(open));
  el.btnExpand.title = `${verb} the download list`;
  el.btnExpand.setAttribute("aria-label", `${verb} the download list`);
  el.btnExpand.querySelector(".expand-btn-label").textContent = verb;

  if (open) {
    // The close button rather than the list: the queue can be empty under a
    // filter, and focus has to land somewhere a keyboard user can get out of.
    el.queueFloatClose.focus({ preventScroll: true });
  } else if (focus) {
    // Focus goes back to the control that opened it. Without this, a keyboard
    // user who closes the queue lands on <body> and has to tab the whole window
    // again to find where they were.
    el.btnExpand.focus({ preventScroll: true });
  }
}

function openQueueFloat() {
  if (state.expanded || state.tasks.length === 0) return;
  setExpanded(true);
  render();
}

function closeQueueFloat() {
  if (!state.expanded) return;
  setExpanded(false);
}

/* ------------------------------------------------------------------ */
/* clipboard                                                           */
/* ------------------------------------------------------------------ */

// Session-scoped. Nothing about what you copy is written to disk, ever.
const dismissed = new Dismissed();

function renderPasteSlot() {
  const p = state.paste;
  el.pasteSlot.hidden = !p;
  if (!p) return;
  el.pasteSlot.replaceChildren(renderPasteBar(p, { accept: acceptPaste, dismiss: dismissPaste }));
}

async function acceptPaste(url) {
  state.paste = null;
  renderPasteSlot();
  await addDownload(url);
}

function dismissPaste(url) {
  dismissed.add(url);
  if (state.paste?.url === url) {
    state.paste = null;
    renderPasteSlot();
  }
}

/**
 * Called by the backend whenever the clipboard's contents change.
 *
 * `url` is null when the clipboard no longer holds a link, which takes the bar
 * away. We never inspect anything but the link: the text itself is discarded
 * inside `plausibleLink` and is not reachable from here.
 */
async function onClipboardLink(url) {
  if (!url) {
    if (state.paste) {
      state.paste = null;
      renderPasteSlot();
    }
    return;
  }
  if (dismissed.has(url)) return;

  // Same url already on screen (a re-copy of the same link): leave the bar
  // alone rather than restarting the favicon request.
  if (state.paste?.url === url) return;

  state.paste = { url, favicon: null, busy: true };
  renderPasteSlot();

  // When we already know who this is, there is nothing to ask for. The mark is
  // bundled, so fetching the site's icon would be a request to somebody else's
  // server for something we would then discard -- and it would put a
  // "checking..." next to a logo that was available all along.
  if (brandForUrl(url)) {
    state.paste = { url, favicon: null, busy: false };
    renderPasteSlot();
    return;
  }

  const favicon = await backend.favicon(url).catch(() => null);
  // The user may have copied something else, or dismissed it, while we waited.
  if (state.paste?.url !== url) return;
  state.paste = { url, favicon: favicon ?? null, busy: false };
  renderPasteSlot();
}

/**
 * Show the details for the current selection, in a dialog on the shared scrim.
 *
 * The popup is the answer to a question somebody asked deliberately -- Enter, or
 * the row's own button -- so it appears only when asked and is gone the rest of
 * the time. It is not a panel that tracks the selection: a window that reshapes
 * itself as you arrow through a queue is a window you cannot aim at.
 */
function openDetail() {
  if (!selectedTask()) return;

  // Where to put focus when it closes. Captured rather than assumed, because the
  // dialog is reachable two ways and they end in different places: Enter is
  // pressed with the list focused, a button is clicked with that button focused.
  //
  // Nulled rather than stored raw if nothing was focused. `document.activeElement`
  // is `<body>` when the dialog was opened with no control focused, and `<body>`
  // answers `.focus()` without moving focus anywhere -- so the close would leave
  // the caret on a button that is about to be inside a hidden subtree, and the
  // browser would drop it to `<body>`. A keyboard user would then have to Tab
  // the whole window again to find where they were.
  detailReturnFocus = isRealControl(document.activeElement) ? document.activeElement : null;

  state.detailOpen = true;
  state.detailFor = selectedTask().id;
  el.detailScrim.hidden = false;
  renderDetailPopup();
  // Close, not the body. The body is a log and a handful of read-only facts, so
  // there is nothing in it to land in, and a control that already has focus is
  // the one control that is obviously live.
  el.detailClose.focus();
}

/**
 * Is this element somewhere focus can usefully go back to?
 *
 * Two exclusions, both of which look like focus works and are not. `<body>` is
 * the document, not a control, and focusing it is a no-op that reads as a
 * successful hand-off. And the rows are rebuilt four times a second, so a row
 * button that was connected when the dialog opened is a different node by the
 * time it closes -- focusing it puts the caret in a detached tree, which is
 * also indistinguishable from focusing nothing.
 */
function isRealControl(el) {
  return !!el && el !== document.body && el !== document.documentElement && el.isConnected;
}

/**
 * Put the details dialog away and hand focus back where it came from.
 *
 * The fallback is the list, because the list is where the dialog came from
 * anyway, and it is the one element that is guaranteed to still be there.
 */
function closeDetail() {
  if (!state.detailOpen) return;
  state.detailOpen = false;
  state.detailFor = null;
  el.detailScrim.hidden = true;
  el.detailBody.replaceChildren();

  const back = isRealControl(detailReturnFocus) ? detailReturnFocus : el.scroller;
  detailReturnFocus = null;
  back.focus({ preventScroll: true });
}

/**
 * Fill the dialog with the selected download, or empty it and hide the scrim.
 *
 * Called on every tick, not only when opening, because the log grows while the
 * dialog is open and a paused download that resumes has to show it.
 */
function renderDetailPopup() {
  // The pane is for one thing: showing the download you selected. When there is
  // no selection there is nothing to show, so it is not in the layout at all --
  // and the list takes the whole window, which is the neatest state this UI has.
  //
  // It used to stay open at all times, resting on the services list, on the
  // argument that a pane appearing on click makes the window change width under
  // the cursor. That was true, and it was a worse trade than it sounded: a few
  // hundred px of permanent right-hand furniture, on screen for the whole
  // session, so that a click would not move a column edge. The services list has
  // moved into the empty state, where it is read at exactly the moment it
  // matters -- before the first download -- and every moment after that the
  // window is just the list.
  const t = state.detailOpen ? selectedTask() : null;

  // The dialog describes exactly one download and never changes its mind. If the
  // selection has moved under it -- which is what happens when the download it
  // was showing is removed from the queue -- it closes rather than quietly
  // describing something nobody asked about.
  if (state.detailOpen && state.detailFor !== null && t?.id !== state.detailFor) {
    closeDetail();
    return;
  }

  if (!t) {
    // A download removed from under an open dialog. There is nothing left to
    // describe, so the dialog goes away rather than sitting there with a stale
    // title over an empty body. `state.detailOpen` stays true, because the
    // selection is still the same row and re-opening it should not need a second
    // deliberate act.
    el.detailScrim.hidden = true;
    el.detailBody.replaceChildren();
    return;
  }

  el.detailScrim.hidden = false;
  el.detailBody.classList.remove("is-idle");
  el.detailTitle.textContent = "Details";
  el.detailBody.replaceChildren(renderDetail(t, on));
}

/* ------------------------------------------------------------------ */
/* actions                                                             */
/* ------------------------------------------------------------------ */

/* The one set of verbs. A row button, a key, and a palette entry all call
 * into these and nowhere else, so there is exactly one code path per action
 * and the three surfaces cannot drift apart. */
const on = {
  toggle: (id) => togglePause(id),
  details: (id) => {
    state.selected = id;
    // The row's own button toggles, because pressing the button on a dialog you
    // already opened should put it back. Enter and the shortcut only ever open
    // it -- a key that opened and closed the same thing on alternate presses
    // would be a very easy thing to get wrong.
    if (state.detailOpen) closeDetail();
    else openDetail();
  },
  remove: (id) => remove(id),
  reveal: (id) => (id ? backend.reveal(id) : Promise.resolve()),
};

async function togglePause(id = state.selected) {
  if (!id) return;
  const t = state.tasks.find((x) => x.id === id);
  if (!t) return;
  if (t.state === State.PAUSED) await backend.resume(id);
  else if (ACTIVE.has(t.state)) await backend.pause(id);
}

async function remove(id = state.selected) {
  if (!id) return;
  const t = state.tasks.find((x) => x.id === id);
  if (!t) return;
  if (t.state === State.RUNNING || t.state === State.RETRYING) await backend.pause(id);
  // A download can be removed from inside its own details dialog, so the dialog
  // can be describing the row that is about to stop existing. Put it away first,
  // or it would be left on screen with a title for something that is gone.
  if (state.detailOpen) closeDetail();
  await backend.remove(id);
  state.selected = null;
}

/* ------------------------------------------------------------------ */
/* adding a download                                                   */
/* ------------------------------------------------------------------ */

/* Adding is inline. The bar appears above the list, the row lands in the list,
 * and a line of feedback confirms it. There is no modal, so the queue never
 * disappears behind a scrim at the exact moment the user is looking at it, and
 * a new row is in its place before the bar has finished closing. */
function openAdd() {
  el.addSlot.hidden = false;
  el.addUrl.value = "";
  // On the empty state the bar sits under the hero and can be below the fold in
  // a short window. Bring it to where the caret is about to be before focusing.
  el.addSlot.scrollIntoView({ block: "nearest" });
  el.addUrl.focus();
}

function closeAdd() {
  el.addSlot.hidden = true;
  el.addUrl.value = "";
}

/**
 * Add one link and say what happened.
 *
 * Returns the task on success and null otherwise, so a caller that needs to
 * select the row (the clipboard offer does not) can, and one that does not can
 * ignore it. The feedback is here rather than at each call site because "it
 * failed" is exactly the thing a caller forgets to render.
 */
async function addDownload(url) {
  try {
    const t = await backend.add(url);
    if (t?.id) {
      state.selected = t.id;
      state.filter = "all";
      schedule();
      snack(`Added ${t.name ?? "download"}`);
      return t;
    }
    snack("That link could not be added.", "bad");
  } catch (err) {
    snack(String(err?.message ?? "That link could not be added."), "bad");
  }
  schedule();
  return null;
}

async function submitAdd() {
  const url = el.addUrl.value.trim();
  if (!url) {
    // An empty submit is not an error, it is a nudge: put the caret back where
    // the answer goes rather than blinking a message about nothing.
    el.addUrl.focus();
    return;
  }
  closeAdd();
  await addDownload(url);
}

/* ------------------------------------------------------------------ */
/* snackbar                                                            */
/* ------------------------------------------------------------------ */

/* One line, bottom-right, gone on its own. It is a status region rather than a
 * dialog: it never takes focus, so it cannot interrupt a keyboard user, and it
 * never blocks the list. The timer is reset on every call so a burst of adds
 * shows the last one for a full beat instead of each one for a fraction. */
let snackTimer = null;
function snack(message, tone) {
  el.snackbar.textContent = message;
  if (tone) el.snackbar.dataset.tone = tone;
  else delete el.snackbar.dataset.tone;
  el.snackbar.hidden = false;
  clearTimeout(snackTimer);
  snackTimer = setTimeout(() => {
    el.snackbar.hidden = true;
  }, 3200);
}

/* ------------------------------------------------------------------ */
/* speed test                                                           */
/* ------------------------------------------------------------------ */

const scope = new Scope(el.speedSvg, el.speedScopeEmpty);
const speed = new SpeedTest(backend, scope, paintSpeed);

/** Repaint the readout. Cheap enough to call on every sample. */
function paintSpeed() {
  const r = readout(speed);

  el.speedNow.textContent = r.now;
  el.speedConns.textContent = r.conns;
  el.speedUp.textContent = r.up;
  el.speedIdle.textContent = r.idle;
  el.speedJitter.textContent = r.jitter;
  el.speedLoaded.textContent = r.loaded;
  el.speedBytes.textContent = r.transferred;
  el.speedHost.textContent = r.host;
  el.speedNote.textContent = r.note;
  if (r.tone) el.speedNote.dataset.tone = r.tone;
  else delete el.speedNote.dataset.tone;

  // The figure carries the text equivalent of the trace, so the instrument is
  // not a picture of a number to anyone using a screen reader.
  el.speedScope.setAttribute("aria-label", r.describe);

  // Running: Stop is available and Start is not. Finished: the reverse. A pair
  // of buttons where the wrong one is live is how people end two runs at once.
  const busy = speed.phase === Phase.Running;
  el.speedStart.hidden = busy;
  el.speedStop.hidden = !busy;

  // One source for all three, so the sentence painted on the first frame and the
  // one painted after a run cannot drift apart.
  el.speedStart.textContent =
    speed.phase === Phase.Idle ? SPEED.start : busy ? SPEED.running : SPEED.again;

  // The running label is a status, not a promise, so it must not look like
  // something you can press. Dimming it is the whole signal -- the accent fill
  // is reserved for controls that do something when you click them.
  el.speedStart.dataset.busy = String(busy);
}

function openSpeed() {
  el.speedScrim.hidden = false;
  paintSpeed();
  // Size the instrument now that it has a box. It is measured as zero while
  // the dialog is closed, and an element inside `display: none` keeps
  // measuring zero until something forces a reflow -- so without this the axis
  // and the gridlines are simply never drawn, and the dialog opens onto an
  // empty frame that looks deliberate.
  scope.resize();
  // The button, not the dialog. There is no longer a field to land in, and a
  // control that already has focus is the one control that is obviously
  // live -- which is the whole job of a dialog that has one action in it.
  el.speedStart.focus();
}

function closeSpeed() {
  el.speedScrim.hidden = true;
  speed.stop();
}

el.speedStart.addEventListener("click", () => {
  speed.start().catch(() => paintSpeed());
});

el.speedStop.addEventListener("click", () => speed.stop());

// Enter anywhere in the dialog starts the test. The dialog is one action, so
// there is no reason to make someone find the button. Buttons are excluded
// because Enter on a focused button already activates it, and handling both
// would start two runs from one keypress.
el.speedScrim.addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  if (e.target.closest("button")) return;
  e.preventDefault();
  el.speedStart.click();
});

/* ------------------------------------------------------------------ */
/* help                                                                */
/* ------------------------------------------------------------------ */

function openHelp() {
  el.helpScrim.hidden = false;
  $("help-close").focus();
}

/* ------------------------------------------------------------------ */
/* dialogs: the shared scrim behaviour                                 */
/* ------------------------------------------------------------------ */

/**
 * Keep Tab inside a dialog.
 *
 * `aria-modal="true"` tells a screen reader that everything outside the dialog
 * has gone away. If the keyboard is then allowed to Tab out of it, focus lands on
 * a row or a tab strip the screen reader has just been told does not exist --
 * which is worse than having no `aria-modal` at all, because the app has
 * claimed a state it is not honouring.
 *
 * Bound once per scrim rather than per dialog, so the details popup, the speed
 * test and the help sheet all behave the same way and cannot drift.
 *
 * `getClientRects()` rather than `offsetParent`: the only thing it is asked here
 * is "does this have a box", and a fixed-position element reports no
 * `offsetParent` at all, which would have silently emptied the list.
 */
function trapFocus(e) {
  if (e.key !== "Tab") return;

  const items = [...e.currentTarget.querySelectorAll("button, [href], input, select, textarea, [tabindex]:not([tabindex='-1'])")]
    .filter((el) => !el.disabled && el.getClientRects().length > 0);
  if (items.length === 0) return;

  const first = items[0];
  const last = items[items.length - 1];

  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
}

function closeHelp() {
  el.helpScrim.hidden = true;
}

function setFilter(f) {
  state.filter = f;
  const list = visible();
  if (!list.some((t) => t.id === state.selected)) state.selected = list[0]?.id ?? null;
  render();
}

/* ------------------------------------------------------------------ */
/* tooltips                                                            */
/* ------------------------------------------------------------------ */

function bindTooltips() {
  let hideTimer = null;

  el.scroller.addEventListener("pointerover", (e) => {
    const target = e.target.closest("[data-tip]");
    if (!target) return;
    clearTimeout(hideTimer);
    el.tip.textContent = target.dataset.tip;
    el.tip.hidden = false;
    const r = target.getBoundingClientRect();
    const tr = el.tip.getBoundingClientRect();
    el.tip.style.left = `${Math.max(8, Math.min(window.innerWidth - tr.width - 8, r.left))}px`;
    el.tip.style.top = `${r.top - tr.height - 8}px`;
  });

  el.scroller.addEventListener("pointerout", () => {
    hideTimer = setTimeout(() => (el.tip.hidden = true), 60);
  });
}

/* ------------------------------------------------------------------ */
/* keyboard                                                            */
/* ------------------------------------------------------------------ */

function isTyping(e) {
  const t = e.target;
  return t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement;
}

/* Shortcuts are accelerators, not documentation. Nothing in the visible UI
 * advertises them, nothing depends on them, and every one of them is reachable
 * by clicking. Removing this block would make the app slower to use and no
 * clearer, which is why it is kept and why it is kept quiet. */
function bindKeys() {
  window.addEventListener("keydown", (e) => {
    if (isTyping(e)) return;

    const overlayOpen = !el.helpScrim.hidden || !el.speedScrim.hidden || !el.detailScrim.hidden;

    // Escape is the universal "get me out of this", so it closes whatever
    // overlay is open, topmost first, before it does anything else.
    if (e.key === "Escape") {
      if (!el.helpScrim.hidden) {
        e.preventDefault();
        closeHelp();
        return;
      }
      if (!el.speedScrim.hidden) {
        e.preventDefault();
        closeSpeed();
        return;
      }
      if (!el.detailScrim.hidden) {
        e.preventDefault();
        closeDetail();
        return;
      }
      if (!el.addSlot.hidden) {
        e.preventDefault();
        closeAdd();
        return;
      }
      // Below the dialogs because the expanded queue is not one of them: you can
      // still be typing in the add bar behind it, and a queue you did not open
      // in a dialog should not be the thing that swallows your Escape.
      if (state.expanded) {
        e.preventDefault();
        closeQueueFloat();
        return;
      }
    }

    if (overlayOpen) return;

    if (e.ctrlKey || e.metaKey) {
      switch (e.key.toLowerCase()) {
        case "n":
          e.preventDefault();
          openAdd();
          return;
      }
      return;
    }

    switch (e.key) {
      case "j":
      case "ArrowDown":
        e.preventDefault();
        move(1);
        break;
      case "k":
      case "ArrowUp":
        e.preventDefault();
        move(-1);
        break;
      case " ":
        e.preventDefault();
        on.toggle();
        break;
      case "Enter":
        e.preventDefault();
        on.details(state.selected);
        break;
      case "Delete":
      case "Backspace":
        e.preventDefault();
        on.remove();
        break;
      case "?":
        e.preventDefault();
        openHelp();
        break;
      case "1":
        setFilter("all");
        break;
      case "2":
        setFilter("active");
        break;
      case "3":
        setFilter("done");
        break;
      case "4":
        setFilter("problem");
        break;
      case "e":
      case "E":
        e.preventDefault();
        if (state.expanded) closeQueueFloat();
        else openQueueFloat();
        break;
    }
  });
}

/* ------------------------------------------------------------------ */
/* boot                                                                */
/* ------------------------------------------------------------------ */

/* ------------------------------------------------------------------ */
/* what kind of host are we on                                         */
/* ------------------------------------------------------------------ */

// Decided here, at module scope, and not inside `boot()`.
//
// The window buttons only mean anything under Tauri -- in a browser they are
// dead controls, and a page that offers "minimise" and "close" when you cannot
// possibly do either is lying about itself. This used to live in `boot()`,
// which meant any thrown error above it left them on screen: the visible
// symptom was "why does the web build have window controls", and the actual
// cause was a listener bound to an element id that had been renamed.
//
// `boot()` can now fail without taking the chrome down with it, which is the
// point of putting it here rather than merely moving it up.
const IS_TAURI = backend.kind === "tauri";
$("winctl").hidden = !IS_TAURI;

if (IS_TAURI) {
  const { getCurrentWindow } = globalThis.__TAURI__.window;
  const win = getCurrentWindow();
  $("w-min").onclick = () => win.minimize();
  $("w-max").onclick = () => win.toggleMaximize();
  $("w-close").onclick = () => win.close();
}

async function boot() {
  bindKeys();
  bindTooltips();

  // One sentence per button, written once in `cta.js`. The markup repeats the
  // same words so that the first paint is never an empty control, and these
  // four lines are what make that copy non-authoritative: whatever is typed in
  // the HTML is overwritten here, so the constant is the only thing worth
  // editing and the two cannot drift into disagreeing.
  $("btn-speed-label").textContent = SPEED.label;
  el.speedStart.textContent = SPEED.start;
  $("speed-stop").textContent = SPEED.stop;
  $("get-software").textContent = SOFTWARE.label;

  el.tabs.addEventListener("click", (e) => {
    const tab = e.target.closest(".tab");
    if (tab) setFilter(tab.dataset.filter);
  });

  el.rows.addEventListener("click", (e) => {
    // A click on a row button is that button's business; it stopped
    // propagation already, so reaching here means the row itself was hit.
    const row = e.target.closest(".row");
    if (!row) return;
    state.selected = row.dataset.id;
    render();
    el.scroller.focus();
  });

  el.rows.addEventListener("dblclick", (e) => {
    const row = e.target.closest(".row");
    if (row) on.toggle(row.dataset.id);
  });

  // The floating queue's rows. The same delegated handlers as the pane's, because
  // it is the same list: a button in the big box must pause and remove exactly as
  // the same button in the small one does, and two copies of that logic would be
  // two chances to have them disagree.
  el.queueFloatList.addEventListener("click", (e) => {
    const row = e.target.closest(".row");
    if (!row) return;
    state.selected = row.dataset.id;
    render();
  });

  el.queueFloatList.addEventListener("dblclick", (e) => {
    const row = e.target.closest(".row");
    if (row) on.toggle(row.dataset.id);
  });

  el.btnExpand.addEventListener("click", () => {
    if (state.expanded) closeQueueFloat();
    else openQueueFloat();
  });
  el.queueFloatClose.addEventListener("click", closeQueueFloat);

  el.detailClose.addEventListener("click", closeDetail);
  // Clicking the dimmed window is a request to be back in the app, which is the
  // one thing a scrim is for. `e.target === el.detailScrim` rather than "not
  // inside the sheet", because a click on the sheet's own padding is still a
  // click on the sheet.
  el.detailScrim.addEventListener("mousedown", (e) => {
    if (e.target === el.detailScrim) closeDetail();
  });
  el.detailScrim.addEventListener("keydown", trapFocus);
  el.speedScrim.addEventListener("keydown", trapFocus);
  el.helpScrim.addEventListener("keydown", trapFocus);

  // One add button, in the empty state. There used to be a second in the
  // titlebar; two controls for one action is the sort of thing that makes a
  // first-time user wonder whether they do different things.
  el.emptyNew.addEventListener("click", openAdd);
  $("btn-help").addEventListener("click", openHelp);
  // No URL yet: the desktop build is not published, and a link to a page that
  // does not exist is worse than saying so. The button stays because it is the
  // one thing in the strip that is not a readout; it answers when pressed.
  $("get-software").addEventListener("click", () => snack(SOFTWARE.unavailable));
  $("btn-speed").addEventListener("click", openSpeed);
  $("btn-speed-close").addEventListener("click", closeSpeed);
  el.speedScrim.addEventListener("mousedown", (e) => {
    if (e.target === el.speedScrim) closeSpeed();
  });

  el.helpScrim.addEventListener("mousedown", (e) => {
    if (e.target === el.helpScrim) closeHelp();
  });
  $("help-close").addEventListener("click", closeHelp);
  el.addUrl.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      closeAdd();
    } else if (e.key === "Enter") {
      e.preventDefault();
      submitAdd();
    }
    e.stopPropagation();
  });
  el.addGo.addEventListener("click", submitAdd);
  el.addCancel.addEventListener("click", closeAdd);

  const info = await backend.info().catch(() => ({ version: "0.1.0", backend: "unknown" }));
  state.info = info;
  el.ver.textContent = info.version ?? "0.1.0";

  // Built once. The list is static data and the empty state is the only place it
  // appears, so there is nothing here to keep in sync with the queue.
  el.sitesSlot.innerHTML = supportedSites();

  // Seed from an explicit read, then subscribe. `on` may or may not deliver an
  // initial snapshot depending on the backend, and the queue must never depend
  // on which one is in play.
  const apply = ({ tasks } = { tasks: [] }) => {
    state.tasks = tasks ?? [];
    if (!state.tasks.some((t) => t.id === state.selected)) {
      state.selected = visible()[0]?.id ?? null;
    }
    schedule();
  };

  await backend.list().then((tasks) => apply({ tasks })).catch(() => {});
  backend.on(apply);

  // Clipboard: offers the link you just copied. The backend decides when a
  // read is allowed at all (focused window, on the Rust side), so this only
  // has to handle what the read returns.
  backend.watchClipboard?.(onClipboardLink);
}

// A throw in `boot()` used to be invisible: the promise rejected, nothing was
// shown, and the page sat there half-wired with no way to tell which half.
// Since every statement in `boot()` binds a listener to an element by id, a
// renamed id is a thrown TypeError, and the person editing the markup is the
// last person to find out. So it is reported, in the place someone is already
// looking, in words.
boot().catch((err) => {
  console.error("[ifami] boot failed", err);
  el.empty.hidden = false;
  el.emptyNew.hidden = true;
  el.empty.replaceChildren(
    h("h1", { text: "ifami could not start." }),
    h("p", { text: String(err?.message ?? err) }),
    h("p", {
      class: "fine",
      text: "The details are in the developer console. If you are running this in a browser, that is most likely a stale copy of the page — try a hard refresh.",
    })
  );
  el.tip.textContent = "Start-up failed. Open the developer console for the details.";
  el.tip.dataset.tone = "bad";
});

// Keep the seam markers and rail ticks honest while a transfer is in flight:
// they are positioned by wall-clock, so a static render goes stale.
setInterval(() => {
  if (state.tasks.some((t) => t.state === State.RUNNING || t.state === State.RETRYING)) schedule();
}, 1000);

// Whether the expanded queue can be opened is a function of whether there is
// anything to enlarge, and `render()` re-asks that on every tick. Nothing here
// needs to know about window size, because a fixed-height panel has room on any
// window the app is usable on.
