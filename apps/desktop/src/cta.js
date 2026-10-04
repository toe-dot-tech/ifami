// ifami desktop — every label a button is allowed to wear.
//
// The rule this file exists to enforce: a button's text is written once, here.
// Not once in the markup and once in the code that updates it, and not once per
// surface. A label that appears in three places is a label that will agree with
// itself today and disagree with itself after the first edit, and the
// disagreement is always discovered by someone clicking the wrong thing.
//
// Two kinds of string live here:
//
//   - fixed labels, which are the same in every situation ("Stop"), and
//   - chosen labels, where the text depends on what the button is about to do
//     (`downloadCta`), which is the more interesting half.
//
// The speed test's own strings moved here from `speed.js`, where they had been
// correct but scoped to one dialog — which is how the same sentence ended up
// written into three different places before this file existed.

import { siteForUrl, isDirectFile, KIND } from "./sites.js";

/* ------------------------------------------------------------------ */
/* the speed test                                                      */
/* ------------------------------------------------------------------ */

/**
 * Every string the speed dialog's two buttons can say.
 *
 * The progression is deliberate rather than cute. The first ask is the whole
 * sentence, because the button is the only thing on screen and it should say
 * what it does. While it is running the label says so, because a button that
 * reads "Test your internet speed" while a test is already running is a button
 * that looks broken. And afterwards it is the short form again, because by then
 * everyone knows what it does and the dialog wants the number read instead.
 *
 * `label` is the titlebar control's own text, which sheds first as the window
 * narrows. It is here rather than in the markup for the same reason the rest
 * are: one sentence, one place.
 */
export const SPEED = {
  /** Beside the scale glyph in the titlebar. */
  label: "Test your internet speed",
  /** Before the first run, and again after one. */
  start: "Test your internet speed",
  /** While a run is in flight. Never actionable, so never styled as one. */
  running: "Testing your internet speed\u2026",
  /** After a run finishes, successfully or not. */
  again: "Test internet speed",
  /** The other button, which is only ever live mid-run. */
  stop: "Stop",
};

/* ------------------------------------------------------------------ */
/* the statusbar                                                       */
/* ------------------------------------------------------------------ */

export const SOFTWARE = {
  /** The statusbar control. */
  label: "Get the software",
  /**
   * What it says when pressed. There is no URL on it yet, deliberately: the
   * desktop build has not been published, and a link to a page that does not
   * exist is worse than an honest sentence.
   */
  unavailable: "The ifami desktop build is not published yet.",
};

/* ------------------------------------------------------------------ */
/* downloading                                                         */
/* ------------------------------------------------------------------ */

/**
 * The verbs, one per kind of thing.
 *
 * "Download media" rather than "Add download", because the offer already says
 * the link is in the clipboard and the user already pressed nothing yet — what
 * they are choosing is what to do with it. And the verb is chosen from the link
 * because a button that says the wrong noun is the one error a downloader can
 * afford least: nobody forgives being offered the wrong file.
 */
export const DOWNLOAD = {
  /** A track, an album, a playlist. */
  music: "Download music",
  /** A video, a reel, a clip. */
  media: "Download media",
  /** Something with a filename behind it. */
  file: "Download file",
  /** Nothing recognised — say the verb and nothing more. */
  plain: "Download",
};

/**
 * The label for the button that will fetch `href`.
 *
 * Answered from the URL alone, in this order:
 *
 *   1. a path that names a file wins outright — "Download file", whatever the
 *      host is, because a `.zip` on a video site is still a zip;
 *   2. then the service's own kind, so YouTube Music says music and YouTube
 *      says media;
 *   3. then the plain verb, for a link we have no opinion about.
 *
 * Never a guess dressed as a fact: an unrecognised host gets "Download", not a
 * guess that it is a video. Being wrong about what we can get is the failure
 * mode that matters here, and the cheapest guard against it is refusing to
 * assert.
 *
 * @param {string} href
 * @returns {string}
 */
export function downloadCta(href) {
  if (isDirectFile(href)) return DOWNLOAD.file;
  const kind = siteForUrl(href)?.kind;
  if (kind === KIND.music) return DOWNLOAD.music;
  if (kind === KIND.media) return DOWNLOAD.media;
  if (kind === KIND.files) return DOWNLOAD.file;
  return DOWNLOAD.plain;
}
