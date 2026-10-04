// Clipboard polling, shared by both backends.
//
// The watcher is a dumb pipe: it asks a `read` function for the current
// clipboard text and hands back whatever `plausibleLink` accepts. Everything
// about *when* it is safe to read lives in `read` — on the Rust side that is
// the window-focus check, which is the only place it can be enforced
// honestly.

import { plausibleLink } from "./link.js";

/** 500ms is fast enough to feel instant and slow enough to be invisible. */
export const POLL_MS = 500;

/**
 * @param {object} o
 * @param {() => Promise<string | null>} o.read  Returns clipboard text, or
 *   null when reading is not allowed right now (window not focused, or no
 *   permission). Must never throw.
 * @param {(url: string) => void} o.onLink
 * @param {number} [o.intervalMs]
 * @returns {() => void} stop
 */
export function watchClipboard({ read, onLink, intervalMs = POLL_MS }) {
  let stopped = false;
  // Never report the same link twice in a row: reading the clipboard does not
  // change it, so an unguarded watcher would fire on every single tick.
  let last = null;

  const tick = async () => {
    if (stopped) return;
    let text = null;
    try {
      text = await read();
    } catch {
      return; // Not permitted. Silent, and permanently.
    }
    if (stopped) return;

    const url = plausibleLink(text);
    if (url === last) return;
    last = url;
    // `null` means the clipboard no longer holds a link, which is how the bar
    // gets taken away again when the user copies something else.
    onLink(url);
  };

  const id = setInterval(tick, intervalMs);
  tick();

  return () => {
    stopped = true;
    clearInterval(id);
  };
}

/**
 * Remembers what the user said no to, so we only ask once.
 *
 * Bounded and time-limited on purpose: this is session state held in memory
 * and nothing else. It is not a clipboard history — after five minutes, or
 * thirty links, it forgets everything, because a tool that remembers what you
 * copied is a tool we would not ship.
 */
export class Dismissed {
  constructor({ ttlMs = 5 * 60_000, max = 32 } = {}) {
    this.ttlMs = ttlMs;
    this.max = max;
    this.seen = new Map();
  }

  has(url) {
    const at = this.seen.get(url);
    if (at == null) return false;
    if (Date.now() - at > this.ttlMs) {
      this.seen.delete(url);
      return false;
    }
    return true;
  }

  add(url) {
    this.seen.delete(url);
    this.seen.set(url, Date.now());
    while (this.seen.size > this.max) this.seen.delete(this.seen.keys().next().value);
  }
}