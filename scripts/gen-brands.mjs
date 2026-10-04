// One-off extraction: pull the brand marks out of simple-icons and emit a
// self-contained ES module. The repo carries no dependency and no build step, so
// the data is inlined once, here, and the generated file is committed.
//
// Regenerate with:  node scripts/gen-brands.mjs
//
// The marks come from the published `simple-icons` package: a local
// `node_modules/simple-icons` install if there is one, and the same package over
// jsDelivr otherwise. That keeps regeneration to a single command on a machine
// that has never run `npm install`, without putting a fetch into the app itself.
// A short table below fills in the few marks Simple Icons no longer ships
// (currently LinkedIn), so the set stays complete when upstream drops one.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const localDir = join(root, "node_modules", "simple-icons");
const hasLocal = existsSync(join(localDir, "data", "simple-icons.json"));
const BASE = "https://cdn.jsdelivr.net/npm/simple-icons@latest";

/** Read one file from the local install, or the same path over jsDelivr. */
async function part(...segments) {
  if (hasLocal) return readFileSync(join(localDir, ...segments), "utf8");
  const res = await fetch(`${BASE}/${segments.join("/")}`);
  if (!res.ok) throw new Error(`${segments.join("/")}: HTTP ${res.status}`);
  return res.text();
}

const meta = JSON.parse(await part("data", "simple-icons.json"));
const bySlug = new Map();
for (const icon of meta) {
  if (typeof icon.slug === "string" && !bySlug.has(icon.slug)) bySlug.set(icon.slug, icon);
}

// Ordered by how likely someone is to reach for it, not alphabetically. The
// first few are what people paste most, so they are the ones worth the pixels.
const WANT = [
  ["youtube", "YouTube"],
  ["x", "X"],
  ["instagram", "Instagram"],
  ["tiktok", "TikTok"],
  ["facebook", "Facebook"],
  ["threads", "Threads"],
  ["spotify", "Spotify"],
  ["reddit", "Reddit"],
  ["discord", "Discord"],
  ["linkedin", "LinkedIn"],
  ["snapchat", "Snapchat"],
  ["telegram", "Telegram"],
  ["vimeo", "Vimeo"],
  ["twitch", "Twitch"],
  ["kick", "Kick"],
  ["rumble", "Rumble"],
  ["soundcloud", "SoundCloud"],
  ["bandcamp", "Bandcamp"],
  ["pinterest", "Pinterest"],
  ["tumblr", "Tumblr"],
  ["dailymotion", "Dailymotion"],
  ["bilibili", "Bilibili"],
  ["odysee", "Odysee"],
  ["loom", "Loom"],
  ["googledrive", "Google Drive"],
];

// Marks Simple Icons has dropped, or never shipped, but the constellation still
// shows. The geometry is the icon's public-domain 24x24 path, carried here so a
// regeneration does not depend on an upstream pack that may have removed it.
const LOCAL = {
  linkedin: {
    hex: "#0A66C2",
    path: "M20.447 20.452h-3.554v-5.569c0-1.328-.027-3.037-1.852-3.037-1.853 0-2.136 1.445-2.136 2.939v5.667H9.351V9h3.414v1.561h.046c.477-.9 1.637-1.85 3.37-1.85 3.601 0 4.267 2.37 4.267 5.455v6.286zM5.337 7.433a2.062 2.062 0 01-2.063-2.065 2.064 2.064 0 112.063 2.065zm1.782 13.019H3.555V9h3.564v11.452zM22.225 0H1.771C.792 0 0 .774 0 1.729v20.542C0 23.227.792 24 1.771 24h20.451C23.2 24 24 23.227 24 22.271V1.729C24 .774 23.2 0 22.225 0z",
  },
};

const found = [];
const missing = [];
for (const [slug, name] of WANT) {
  const info = bySlug.get(slug);
  const local = LOCAL[slug];
  if (!info && !local) {
    missing.push(slug);
    continue;
  }
  let hex;
  let path;
  if (info) {
    const svg = await part("icons", `${slug}.svg`);
    // Every mark in the set is a single 24x24 <path>. Assert that rather than
    // assume it: a duotone icon would silently become a blank tile, and a blank
    // tile in a list of two dozen is much harder to notice than a failed script.
    const paths = [...svg.matchAll(/<path d="([^"]+)"/g)].map((m) => m[1]);
    if (paths.length !== 1) {
      missing.push(`${slug} (${paths.length} paths)`);
      continue;
    }
    const viewBox = svg.match(/viewBox="([^"]+)"/)?.[1];
    if (viewBox !== "0 0 24 24") {
      missing.push(`${slug} (viewBox ${viewBox})`);
      continue;
    }
    hex = info.hex.toUpperCase();
    path = paths[0];
  } else {
    hex = local.hex.replace(/^#/, "").toUpperCase();
    path = local.path;
  }
  found.push({ slug, name, hex, path });
}

console.log(`found ${found.length}, missing: ${missing.join(", ") || "none"}`);
for (const f of found) console.log(`  ${f.name.padEnd(12)} #${f.hex}  ${f.path.length} chars`);

// Simple-icons paths are digits, dots, signs and letters only. If that ever
// stops being true the generated file would be syntactically valid and render
// nothing, so check rather than trust.
for (const f of found) {
  if (/["'<>\\]/.test(f.path)) throw new Error(`${f.name}: path needs escaping`);
}

const body = found
  .map(
    (f) => `  {
    name: ${JSON.stringify(f.name)},
    slug: ${JSON.stringify(f.slug)},
    hex: "#${f.hex}",
    path: "${f.path}",
  },`
  )
  .join("\n");

const out = join(root, "apps", "desktop", "src", "brands.js");
writeFileSync(
  out,
  `// GENERATED FILE -- do not edit by hand.
//
// Brand marks from Simple Icons (https://simpleicons.org), CC0 1.0 Universal,
// except LinkedIn, which Simple Icons dropped upstream and which the generator
// carries locally. Public domain or the equivalent: no attribution is required
// and none is claimed below.
// Regenerate with scripts/gen-brands.mjs after bumping the package version.
//
// Each entry is one 24x24 path, drawn in the brand's own colour. A row of grey
// silhouettes would be a wall of nothing; the colours are what let someone find
// "the blue one" in a list they are scanning rather than reading.
//
// These are trademarks of their respective owners, used only to name the
// services ifami will try to fetch from. No affiliation, no endorsement, and no
// claim that any of them endorse ifami. See NOTICE.

/**
 * Services ifami recognises, in display order.
 *
 * @type {{name: string, slug: string, hex: string, path: string}[]}
 */
export const BRANDS = [
${body}
];
`
);

console.log(`\nwrote ${out}`);
