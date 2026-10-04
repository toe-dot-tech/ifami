// Deciding whether the clipboard holds something worth offering to download.
//
// This is the most privacy-sensitive surface in the product: it reads
// something the user never typed into us. Three rules keep it honest, and they
// are the reason this feature is allowed to exist at all:
//
//   1. We read the clipboard only while our window is focused. A clipboard
//      poller that runs in the background is a keylogger with a nicer README.
//   2. We read it to answer exactly one question — "is this a link?" — and if
//      the answer is no, the text is dropped on the floor. It is not stored,
//      not logged, not hashed into a history, and never sent anywhere.
//   3. We never write to the clipboard, and once the user dismisses what we
//      showed, we do not ask again for that link.
//
// Everything here is a pure function so the rules above are testable without a
// clipboard, a window, or a network.

/** Longest string we will even look at. A clipboard full of a novel is not a link. */
const MAX_LEN = 2048;

/**
 * Return the normalised URL if `text` is plausibly a single http(s) link the
 * user just copied, else `null`.
 *
 * Deliberately strict: if the clipboard holds prose with a URL buried in it,
 * we decline rather than scanning for something link-shaped. Going fishing
 * through arbitrary copied text is a scanner's behaviour, not a paste
 * helper's, and the difference matters to anyone who copies a private message
 * and then opens this app.
 *
 * @param {unknown} text
 * @returns {string | null}
 */
export function plausibleLink(text) {
  if (typeof text !== "string") return null;

  // Windows line endings and stray spaces around a copied address are normal.
  const trimmed = text.trim();
  if (!trimmed || trimmed.length > MAX_LEN) return null;

  // Any internal whitespace means this is not "just the link". (A single URL
  // cannot contain unencoded spaces — `new URL` would encode them.)
  if (/\s/.test(trimmed)) return null;

  let url;
  try {
    url = new URL(trimmed);
  } catch {
    return null;
  }

  // file:, data:, javascript:, and the rest are not things we can fetch, and
  // offering them would imply we try.
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;

  return url.href;
}

/**
 * A short, human-readable label for the origin. `cdn1.media.example.tv` becomes
 * `example.tv`, because the subdomain is usually an implementation detail and
 * occasionally a tracking token in its own right.
 */
export function prettyHost(href) {
  try {
    const h = new URL(href).hostname;
    const parts = h.split(".");
    return parts.length > 2 ? parts.slice(-2).join(".") : h;
  } catch {
    return "";
  }
}

/**
 * The path (and query, if short) to show under the sentence. This is the
 * "small snippet preview": enough to recognise the thing you copied, not so
 * much that it becomes a second copy of the thing you copied.
 */
export function pathSnippet(href, max = 64) {
  try {
    const u = new URL(href);
    const q = u.search.length > 12 ? `${u.search.slice(0, 12)}…` : u.search;
    const s = `${u.pathname}${q}`;
    return s.length > max ? `…${s.slice(-(max - 1))}` : s;
  } catch {
    return "";
  }
}

/** One character for the monogram tile: first letter of the pretty host. */
export function monogram(href) {
  const host = prettyHost(href);
  return (host.match(/[a-z0-9]/i)?.[0] ?? "?").toUpperCase();
}