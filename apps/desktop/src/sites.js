// ifami desktop — what we can tell about a link from the link itself.
//
// Two questions, both answered from the URL and never from the network:
//
//   1. Is this a file? When the path ends in an extension we recognise, the
//      link already names the thing it fetches, so the button can say so.
//   2. Which service is it, and what does that service hold? A YouTube link is
//      a video, a SoundCloud link is music, a Drive link is a file.
//
// This is a hand-written table rather than a lookup service, and that is the
// whole point. Asking somebody else's server "what is this URL" hands over
// every link anyone has ever pasted into this app, which is precisely the thing
// this app does not do. Forty rows of hostnames is a small price for keeping
// them on this machine.
//
// The table is consulted before a request is made, so it never tells the user
// whether a download will succeed — it only tells them what they are asking for.
// A link we have never seen is still offered; it just gets the plain verb.

/**
 * What a service is mostly for.
 *
 * These are not categories of difficulty, they are categories of noun, because
 * that is what the button is allowed to promise: "music" is a claim about the
 * thing, not about our ability to fetch it.
 */
export const KIND = {
  /** Tracks, albums, playlists. */
  music: "music",
  /** Video, and the stills that come with it. */
  media: "media",
  /** A document, an image, an archive — something with a filename. */
  files: "files",
};

/**
 * Host → what it holds. Ordered most-specific first.
 *
 * Order is load-bearing. The match is a suffix test (`example.com` matches
 * `cdn.example.com`), so `music.youtube.com` would be swallowed by the
 * `youtube.com` row below it and every YouTube Music link would offer to
 * download a video. Anything more specific than a parent domain goes above the
 * parent.
 *
 * `slug` matches an entry in `brands.js`, so a recognised host can also be
 * answered with that service's own mark rather than a letter.
 */
const SITES = [
  // More specific before less. YouTube Music is the one that matters here: it
  // shares a domain with the video site and means the opposite thing.
  { slug: "youtube", kind: KIND.music, hosts: ["music.youtube.com"] },
  { slug: "youtube", kind: KIND.media, hosts: ["youtube.com", "youtu.be", "m.youtube.com"] },
  { slug: "spotify", kind: KIND.music, hosts: ["open.spotify.com", "spotify.com"] },
  { slug: "soundcloud", kind: KIND.music, hosts: ["soundcloud.com", "snd.sc"] },
  { slug: "bandcamp", kind: KIND.music, hosts: ["bandcamp.com"] },
  { kind: KIND.music, hosts: ["music.apple.com", "deezer.com", "tidal.com", "mixcloud.com", "audiomack.com"] },

  { slug: "tiktok", kind: KIND.media, hosts: ["tiktok.com", "vm.tiktok.com"] },
  { slug: "instagram", kind: KIND.media, hosts: ["instagram.com", "instagr.am"] },
  { slug: "facebook", kind: KIND.media, hosts: ["facebook.com", "fb.watch", "fb.com"] },
  { slug: "threads", kind: KIND.media, hosts: ["threads.net", "threads.com", "www.threads.net"] },
  { slug: "snapchat", kind: KIND.media, hosts: ["snapchat.com"] },
  { slug: "x", kind: KIND.media, hosts: ["x.com", "twitter.com", "mobile.twitter.com", "t.co"] },
  { slug: "vimeo", kind: KIND.media, hosts: ["vimeo.com", "player.vimeo.com"] },
  { slug: "twitch", kind: KIND.media, hosts: ["twitch.tv", "clips.twitch.tv"] },
  { slug: "kick", kind: KIND.media, hosts: ["kick.com"] },
  { slug: "rumble", kind: KIND.media, hosts: ["rumble.com"] },
  { slug: "dailymotion", kind: KIND.media, hosts: ["dailymotion.com", "dai.ly"] },
  { slug: "bilibili", kind: KIND.media, hosts: ["bilibili.com", "b23.tv"] },
  { slug: "odysee", kind: KIND.media, hosts: ["odysee.com", "lbry.tv"] },
  { slug: "loom", kind: KIND.media, hosts: ["loom.com", "loom.share"] },
  { slug: "reddit", kind: KIND.media, hosts: ["reddit.com", "redd.it"] },
  { slug: "pinterest", kind: KIND.media, hosts: ["pinterest.com", "pin.it"] },
  { slug: "tumblr", kind: KIND.media, hosts: ["tumblr.com"] },

  { slug: "googledrive", kind: KIND.files, hosts: ["drive.google.com", "docs.google.com"] },
  { slug: "discord", kind: KIND.files, hosts: ["discord.com", "discord.gg", "cdn.discordapp.com"] },
  { slug: "telegram", kind: KIND.files, hosts: ["t.me", "telegram.me", "telegram.org"] },
  { kind: KIND.files, hosts: ["dropbox.com", "github.com", "gitlab.com"] },

  // A posting service rather than a media host: the answer depends entirely on
  // what the post contains, so it is left to the plain verb.
  { slug: "linkedin", kind: null, hosts: ["linkedin.com", "lnkd.in"] },
];

/**
 * Extensions that make a link self-describing.
 *
 * The point of this list is not completeness, it is precision: every entry is
 * an extension that essentially only appears on a link to a file. That is why
 * compressed archives, installers, subtitles and containers are here while
 * `.php` and `.aspx` are not — those are web pages that happen to have a file
 * extension, and calling them "a file" would be a lie in the other direction.
 */
const FILE_EXT = new Set([
  // video
  "mp4", "m4v", "mkv", "webm", "mov", "avi", "flv", "wmv", "mpg", "mpeg", "m2ts", "ts",
  // streamed manifests
  "m3u8", "mpd",
  // audio
  "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma",
  // images
  "jpg", "jpeg", "png", "gif", "webp", "avif", "svg", "bmp", "tiff", "heic",
  // documents
  "pdf", "epub", "mobi", "rtf", "txt", "csv", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt",
  // subtitles
  "srt", "vtt", "ass", "ssa", "sub",
  // archives and installers
  "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", "exe", "msi", "deb", "rpm", "apk",
]);

/** Hostname of `href`, lowercased, without a port. `""` when it is not a URL. */
function hostname(href) {
  try {
    return new URL(href).hostname.toLowerCase();
  } catch {
    return "";
  }
}

/**
 * The site a link belongs to, or `null` if we do not recognise it.
 *
 * @param {string} href
 * @returns {{slug: string | null, kind: string | null} | null}
 */
export function siteForUrl(href) {
  const host = hostname(href);
  if (!host) return null;
  for (const site of SITES) {
    for (const h of site.hosts) {
      // Exact, or a subdomain of it. Nothing else: `notyoutube.com` must not
      // match `youtube.com`, so the dot is part of the test.
      if (host === h || host.endsWith(`.${h}`)) {
        return { slug: site.slug ?? null, kind: site.kind ?? null };
      }
    }
  }
  return null;
}

/**
 * Whether the link names a file outright, judged from the path alone.
 *
 * No request is made to find out. `Content-Type` and `Content-Disposition`
 * would answer this more reliably, but asking the origin is the one thing this
 * bar must not do before the user has decided: the whole promise of the app is
 * that nothing is fetched, let alone inspected, until you press the button.
 *
 * @param {string} href
 * @returns {boolean}
 */
export function isDirectFile(href) {
  try {
    const { pathname } = new URL(href);
    const last = pathname.slice(pathname.lastIndexOf("/") + 1);
    const dot = last.lastIndexOf(".");
    // No dot, or a dot that opens the segment (".gitignore" is a name, not an
    // extension), means there is nothing to read.
    if (dot <= 0 || dot === last.length - 1) return false;
    return FILE_EXT.has(last.slice(dot + 1).toLowerCase());
  } catch {
    return false;
  }
}

/**
 * The brand slug for a link, for drawing that service's own mark.
 *
 * Separate from `siteForUrl` so that the logo question stays "do we know who
 * this is" while the button question stays "do we know what it holds".
 *
 * @param {string} href
 * @returns {string | null}
 */
export function brandForUrl(href) {
  return siteForUrl(href)?.slug ?? null;
}
