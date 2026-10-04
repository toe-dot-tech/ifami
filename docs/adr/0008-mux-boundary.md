# ADR-0008: Video-only renditions are refused only when audio exists

- **Status:** Accepted
- **Date:** 2026-10-01
- **Decides:** `crates/ifami-core/src/resolve/dash.rs`, `Manager::add_url`

## Context

A DASH manifest and a multi-variant HLS master can offer a *video-only*
rendition: video with no audio track. It is impossible to produce a playable
single file from that rendition without the audio, which would mean muxing.

Two ways to handle it:

1. Mark every video-only rendition as `requires_mux` unconditionally.
2. Mark it as `requires_mux` only when the manifest *also* offers audio.

Approach 1 is what the code did, and it was wrong. It refused to download
**any** video-only DASH manifest — including a screen recording, a silent film,
or a clip whose audio was never published. Those are complete, legitimate
presentations. We would have been refusing the exact media we support best, in
exchange for a rule that is easier to write down.

Approach 2 asks the question that actually matters: is there an audio track
that this rendition is failing to include?

## Decision

`requires_mux` is `kind == VideoOnly && manifest contains an audio adaptation
set`.

The answer is a property of the whole manifest, not of one representation, so it
is computed once in the resolver before the per-representation loop. Deciding it
inside the loop cannot work: the loop is what produces the representations whose
presence the decision depends on.

`Manager::add_url` queues `media.best_standalone()`. When audio exists, that is
the audio rendition — an `.m4a` plays standalone, so the user gets the audio
track rather than nothing at all, and `ifami info` reports the video rendition
with the note that it needs muxing. When audio does not exist, that is the video
rendition, and it downloads.

## Consequences

**A video-only manifest downloads.** This is the fix. `a_video_only_manifest_is_downloadable`
pins it.

**A silent file is never presented as the whole video.** When audio exists, the
video rendition is still marked `requires_mux` and is not queued by default.
`a_video_rendition_needs_muxing_when_an_audio_track_exists` pins that too.

**We do not bundle a muxer.** That is the honest consequence, and it is the same
reason we do not bundle an extractor ([ADR-0006](0006-standards-only-no-extractor-bundles.md)).
`ffmpeg` would add hundreds of megabytes, a GPL licensing question, and an
unreviewable external process to a tool whose claim is that it does exactly what
was asked and nothing else.

**The alternative is available to the user.** `ifami info` shows the video
rendition and its size. A user who wants it can fetch it by URL with any tool.
We decline to make that the default rather than pretending it is the whole thing.

## Alternatives considered

- **Queue video-only and warn.** Rejected. Users do not read warnings before
  closing the file, and a silent video presented as a download is worse than an
  audio-only download that plays.
- **Bundle `ffmpeg` to mux.** Rejected. Size, licence, and reviewability; see
  above.
- **Keep approach 1 and document the limitation.** Rejected. It refused working
  media, and the limitation was in the implementation rather than in the format.