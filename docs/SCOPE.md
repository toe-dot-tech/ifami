# Scope and Limitations

**This document is normative.** It defines what Ifami does, what it deliberately
does not do, and where the hard boundaries are. Contributions that require
crossing one of these boundaries will not be merged. See
[ADR-0005](adr/0005-scope-boundary.md) for why this file exists.

If a feature requires any of the following, it is out of scope. Permanently.

> **One-scope test.** Does this need to decrypt, impersonate a browser, or execute
> a site's JavaScript? Then it is out of scope. Does it need none of those? Then
> it is very likely in scope, and the answer is yes.
>
> That test is not a loophole. Every out-of-scope item below fails it for a
> specific, stated reason, and the reasons are recorded as ADRs so you can
> disagree with them on their merits.

## We do not circumvent access controls

Ifami does not, and will not:

- Decrypt, unwrap, or forge DRM (EME, Widevine, PlayReady, FairPlay). We do not
  ship or link a CDM, licence-request client, or key server.
- Bypass authentication, paywalls, membership gates, or geo-restrictions.
- Extract keys from an encrypted stream, or expose them to any caller.
- Handle token-based anti-bot challenges (for example PO Tokens, attestation
  blobs, or attestation-based player gating) on behalf of a user. If a source
  requires such a token, Ifami reports `AuthRequired` and stops.
- Emit, replay, or rotate a browser TLS or HTTP2 fingerprint in order to appear
  as a different client than we are.
- Execute a source site's JavaScript to compute a signature or transform a URL.
  This is excluded deliberately: it requires shipping an unreviewed interpreter
  and unreviewed code as part of our release, which is indefensible in both a
  security review and a platform review.

## Sources we do support

Ifami targets media a user is entitled to retrieve. In scope:

- Media the user themselves hosts or owns (their own site, S3/R2 bucket, LAN).
- Media published under a licence permitting redistribution and retrieval
  (Creative Commons, public domain, openly licensed datasets).
- Direct media URLs the publisher intends to be retrievable.
- Standards-based adaptive streams on sources that serve them without
  access controls: HLS (`.m3u8`) and DASH (`.mpd`). HLS is supported including
  fMP4 (`EXT-X-MAP`) and byte-range playlists; DASH is supported for the
  `SegmentList` form. A `SegmentTemplate` manifest is refused with
  `UnsupportedFeature` rather than mis-parsed, because a wrong answer there is a
  file that is the right length and the wrong contents.
- HTML pages that name their own media, via `<video>`/`<source>` and OpenGraph
  `og:video`.

### What a stream is called on disk

We name a finished file after what its bytes are, not after how they arrived.
An fMP4 HLS playlist (`EXT-X-MAP` present) concatenates into a real `.mp4`; one
without an init segment concatenates into transport stream, `.ts`; DASH segments
are ISO-BMFF fragments and produce `.mp4`. The extension `.bin` is reserved for
the case where we genuinely cannot tell, and means exactly that. See
[ADR-0009](adr/0009-output-container-vs-delivery-mechanism.md).

### What "in scope" means for rendition choice

We do not bundle a muxer, so a video-only rendition cannot become a playable file
on its own. That is a boundary, not a blanket refusal:

- A manifest with **no audio track anywhere** is a complete presentation. We
  download the video.
- A manifest that **offers audio** marks its video-only rendition as needing a
  mux step. We do not queue that by default, because handing someone a silent
  file and calling it the video is worse than handing them the audio track, which
  does play standalone.

See [ADR-0008](adr/0008-mux-boundary.md).
- User-supplied `Authorization` headers / cookies, used at the user's explicit
  instruction, for **their own** authenticated content. Ifami never asks for
  them, never persists them to the queue file, and never transmits them anywhere
  but the origin they belong to.

## Why a source will not work

We are explicit in the UI when a source is out of scope. `AuthRequired` means
"this source gates access and we will not bypass it." That is a correct, final
answer, not a bug to be worked around. See the known-limitations table in the
README.

## No surveillance

Ifami has no accounts, no telemetry, no analytics, and no device or browser
fingerprinting. There is no user identifier of any kind.

- We do not collect hardware identifiers (machine UUID, MAC address, disk serial).
- We do not fingerprint browsers or canvases, and we do not correlate users
  across browsers, profiles, or private/incognito sessions.
- We do not require or imply login for any functionality.
- The only persisted state is your local queue, in your local directory.

This is a design constraint, not a missing feature. See
[ADR-0003](adr/0003-no-accounts-no-fingerprinting.md).

## No bundled extractors

Ifami implements standards-based retrieval only: direct URLs, HLS, DASH, and
page extraction. We do not bundle, link, vendor, or download a third-party
downloader such as yt-dlp, and none appears anywhere in the dependency graph.

Three reasons, in order of how much they matter:

1. The hosts that need an extractor are the hosts that gate playback behind
   attestation, which needs a JavaScript runtime and a spoofed TLS fingerprint.
   An extractor that worked there would have to do the thing this document
   refuses.
2. yt-dlp is GPLv3+. Bundling it would place every artefact we distribute under
   GPLv3+, changing the licence of the project without the user noticing.
3. It is unreviewable at the scale required. A security review covering every
   host a third-party extractor supports is a review that never finishes.

See [ADR-0006](adr/0006-standards-only-no-extractor-bundles.md).

## No advertising

There is no ad SDK, no ad network, no affiliate injection, and no sponsored
placement, in any build or distribution channel. Project funding is sponsorship
or donation.

## Network posture

Ifami is designed so the **user's machine** performs the transfer wherever
possible, so that we operate no media cache and store no media. The optional
relay exists only for CORS-restricted sources, is opt-in per invocation, and is
documented in full. If you do not enable it, no bytes of your media pass through
any infrastructure we control.

Every outbound request made by Ifami, in priority order:

1. To the origin serving the media. Nothing else.
2. To the local bridge, which is loopback-only (`127.0.0.1`) and never binds a
   routable interface.
3. To the relay, only if you passed `--relay`, and only to relay hosts you
   configured.