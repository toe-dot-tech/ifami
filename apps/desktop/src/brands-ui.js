// ifami desktop — the sites ifami reads, as they appear in the empty state.
//
// This used to be the resting content of the right-hand detail pane, which was
// always on screen so that the answer to "what can I paste?" was never more than
// a glance away. That panel spent the entire session occupying a third of the
// window to answer a question people ask once, before their first download, and
// never again. It now lives in the empty state, where the question is actually
// asked, and the window gets to be only the list.
//
// Across a whole window a neat wrapped row of tiles read like a footer: a tidy
// band of logos under the button, easy to mistake for fine print. The sites are a
// constellation instead -- each one placed around the headline at its own
// position and its own small tilt -- so the empty state looks like it was
// arranged rather than assembled. The order still matters: the sites someone is
// most likely to arrive with sit nearest the top of their arc.
//
// The positions are percentages of the hero, and the tilt is degrees. They are
// data rather than CSS because they belong to the site, not to a breakpoint; the
// narrow layout ignores them and wraps the same tiles into a plain cloud, which
// is why they are custom properties on the tile rather than a rule per tile.
//
// The marks are real brand logos. All but one come from Simple Icons (CC0 1.0
// Universal -- public domain, so no attribution is required and none is
// claimed); LinkedIn is the exception, carried by hand in the generator because
// Simple Icons removed it upstream. Brand names and logos remain the trademarks
// of their owners and are used here only to say what ifami can read.

import { BRANDS } from "./brands.js";

/**
 * Named sites, and what you can get out of each one.
 *
 * The caption says what the site is *for*, not what state its integration is in.
 * Nobody opening this app wants to read "needs a signed-in session" beside a logo
 * -- that is our problem, not theirs, and it is the sort of thing that belongs in
 * an issue tracker rather than in the answer to "what can I paste?". How each of
 * these is actually fetched is decided behind the scenes.
 *
 * `at` is `[xPercent, yPercent, rotationDegrees]` in the hero's coordinate space,
 * with the point being the tile's centre. Order is by how likely the reader is to
 * recognise the name, not by alphabetical tidiness: someone who came here with a
 * YouTube link should find YouTube in the first tile.
 */
const SITES = [
  // Left rail, top to bottom. The x alternates so a column of pills reads as a
  // hand-placeable arc rather than a ruler.
  { name: "YouTube", blurb: "video, audio, and whole playlists", at: [14, 11, -6] },
  { name: "Instagram", blurb: "reels, posts, and video", at: [9, 23, 4] },
  { name: "TikTok", blurb: "video", at: [14, 35, -4] },
  { name: "Facebook", blurb: "video from public posts", at: [9, 47, 5] },
  { name: "Threads", blurb: "video and images from public posts", at: [14, 59, -5] },
  { name: "Snapchat", blurb: "stories and video", at: [9, 71, 4] },
  { name: "Telegram", blurb: "files from public channels", at: [14, 83, -3] },
  { name: "Kick", blurb: "live streams and past broadcasts", at: [9, 94, 4] },

  // Right rail, top to bottom.
  { name: "X", blurb: "video", at: [89, 11, 6] },
  { name: "Reddit", blurb: "video and audio from public posts", at: [94, 23, -5] },
  { name: "Discord", blurb: "files from public channels", at: [89, 35, 5] },
  { name: "Twitch", blurb: "clips and past broadcasts", at: [94, 47, -5] },
  { name: "Pinterest", blurb: "video and images", at: [89, 59, 4] },
  { name: "Spotify", blurb: "tracks and whole playlists", at: [94, 71, -4] },
  { name: "Rumble", blurb: "video", at: [89, 83, 3] },
  { name: "Tumblr", blurb: "video and images", at: [94, 94, -4] },

  // Across the top and bottom, clear of the headline and the offer.
  { name: "Vimeo", blurb: "video and showcases", at: [26, 7, -5] },
  { name: "Google Drive", blurb: "files you have access to", at: [39, 7, 3] },
  { name: "Loom", blurb: "screen recordings", at: [52, 7, 2] },
  { name: "LinkedIn", blurb: "video and images from public posts", at: [65, 7, -4] },
  { name: "Dailymotion", blurb: "video", at: [78, 7, 4] },
  { name: "SoundCloud", blurb: "tracks and whole playlists", at: [30, 93, 4] },
  { name: "Bandcamp", blurb: "albums, tracks, and lossless", at: [45, 93, -3] },
  { name: "Bilibili", blurb: "video and higher-quality streams", at: [60, 93, 3] },
  { name: "Odysee", blurb: "video and audio", at: [75, 93, -4] },
];

/** Marks for a brand name, matched case-insensitively against the slug. */
function mark(name) {
  const slug = name.toLowerCase().replace(/[^a-z0-9]/g, "");
  return BRANDS.find((b) => b.slug === slug) ?? null;
}

/** One brand mark, or a monogram if the generated file is missing that entry. */
function tile(brand) {
  if (!brand) {
    // Should never happen with the list above. A tile that renders as an empty
    // box reads as a broken image, so the letters are a better failure than a
    // hole -- and they mean something to whoever reports it.
    return `<span class="brand-mark brand-mark--none" aria-hidden="true">?</span>`;
  }
  return (
    `<svg class="brand-mark" viewBox="0 0 24 24" aria-hidden="true" focusable="false">` +
    `<path d="${brand.path}" fill="${brand.hex}"/></svg>`
  );
}

/** One site: its mark and its name, plus where it sits in the constellation. */
function pill(site) {
  const [x, y, r] = site.at;
  return (
    `<li class="site" style="--x:${x}%;--y:${y}%;--r:${r}deg" ` +
    `title="${site.name} &mdash; ${site.blurb}">` +
    `${tile(mark(site.name))}<span class="site-name">${site.name}</span></li>`
  );
}

/**
 * The supported-sites constellation, for the empty state.
 *
 * A function, not a constant, so the list is built where it is used and the
 * generated file stays a data file rather than drifting into being markup with a
 * `.js` extension.
 *
 * @returns {string}
 */
export function supportedSites() {
  return `<ul class="site-scatter">${SITES.map(pill).join("")}</ul>`;
}
