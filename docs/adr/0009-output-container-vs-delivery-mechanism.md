# ADR-0009: Delivery mechanism and output container are different axes

- **Status:** Accepted
- **Date:** 2026-10-01
- **Decides:** `model.rs` (`Container`, `Format::is_direct`), `hls.rs`, `dash.rs`

## Context

`Container` originally conflated two unrelated questions:

1. **How is this delivered?** One range-capable file, or a playlist of segments
   walked one at a time?
2. **What are the finished bytes?** MP4, Matroska, transport stream, AAC?

The two only happen to correlate for direct URLs, where the file extension on the
URL answers both. Everywhere else the conflation produced wrong answers.

The concrete symptom: `Container::Hls` and `Container::Dash` extended to `bin`, so
an fMP4 HLS stream — which concatenates into a perfectly valid `.mp4` — landed on
disk as `clip.bin`. The README promised HLS and DASH "work well", and a user who
trusted that got a file they had to rename by hand.

The same conflation made `Format::is_direct()` wrong. It returned
`!container.is_segmented()`, so a DASH manifest whose representation is one whole
file — genuinely a single range request — was classified as fragmented.

## Decision

**Separate the axes.**

- `Format::segments.is_some()` is the test for delivery mechanism, and is what
  `Format::is_direct()` now uses. It is the one that is actually true.
- `Format::container` is the output container, and it determines the file
  extension. A resolver must set it to something real.

`Container::Hls` and `Container::Dash` survive as an explicit "not determined
yet" state. That state is real: a master playlist's variants point at variant
playlists that have not been fetched, so the container genuinely is unknown at
resolve time. It is resolved when the variant playlist is read.

**How each resolver determines the output container:**

| Source | Rule | Extension |
| --- | --- | --- |
| Direct URL | MIME type, else the path extension | as reported |
| HLS with `EXT-X-MAP` | fMP4: init segment + media segments join into a real MP4 | `mp4` |
| HLS without `EXT-X-MAP` | MPEG-2 transport stream | `ts` |
| DASH with a segment plan | ISO-BMFF fragments; MIME type if given, else MP4 | `mp4` |
| Anything genuinely unknown | `Hls`/`Dash`/`Other` | `bin` |

The rule lives in one method, `Playlist::output_container()`, because three call
sites need it and none of them can afford to disagree: resolving a media
playlist, expanding a variant playlist, and the display path.

## Consequences

**Streams land as `.mp4` and `.ts`, not `.bin`.** That is the fix.

**`.bin` is now honest rather than lazy.** It only appears when the container
really could not be determined. A user seeing `clip.bin` has been told the truth:
this tool does not know what it assembled.

**`is_direct()` is now correct.** A single-file DASH representation takes the
single range path, which is both faster and simpler than materialising a segment
plan for one URL.

**`is_segmented()` survives but is now clearly about delivery.** It is used only
where the mechanism matters; nothing uses it to pick an extension, which is how
the original bug arose.

## Alternatives considered

- **Keep one enum, add `HlsFmp4` and `HlsTs` variants.** Rejected. The
  exponential product of "which mechanism" and "which container" is the original
  mistake made explicit.
- **Name every stream by its playlist extension (`.m3u8`).** Rejected. The
  playlist is gone once the segments are joined; the file is no longer a playlist.
- **Always name DASH output `.mp4`.** Rejected. DASH carries WebM and other
  codecs, and the manifest says which. Where it does not say, `mp4` is the
  overwhelmingly likely answer, but it is a fallback, not a rule.
- **Probe the first segment's magic bytes at transfer time.** Rejected as
  over-engineering for the cases that remain, and it would mean renaming after
  the transfer rather than choosing the name up front.