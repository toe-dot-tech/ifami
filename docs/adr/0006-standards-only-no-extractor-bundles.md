# ADR-0006: Standards-only retrieval, no bundled extractors

- **Status:** Accepted
- **Date:** 2026-10-01
- **Decides:** `docs/SCOPE.md` ("Sources we do support"), `NOTICE`

## Context

Media downloaders in the open-source ecosystem fall into two groups. The first
is a set of site-specific *extractors* — yt-dlp is the canonical example — that
know the request sequence, the player parameters, and the signature scheme for a
particular host. The second is standards-based retrieval: fetch the URL the
origin actually serves, follow HLS or DASH if the origin publishes them, and
range-request the bytes.

The first group is dramatically more capable. It is also, for a tool that wants
to be installed on an engineer's machine and audited by a platform review,
categorically unusable.

Three independent problems:

1. **Licence.** yt-dlp and its predecessors are GPLv3+. Linking or redistributing
   them inside our installer would place the distributed application under
   GPLv3+. The project is Apache-2.0. Bundling them would change the licence of
   everything we ship, and the choice of extractor would be invisible to a user
   who downloaded "ifami".

2. **Unreviewable code at scale.** yt-dlp is on the order of 10^5 lines of
   per-host logic. It is competently written and individually reasonable, but a
   security review that has to cover every host is a security review that never
   finishes. We would be asking a store reviewer, an employer, and every user to
   take that on faith. Our own surface is small enough to read.

3. **What it would have to do anyway.** The hosts that need an extractor are the
   hosts that gate playback behind attestation, which requires a JavaScript
   runtime and a browser TLS/HTTP2 fingerprint. We refuse that in
   [ADR-0005](0005-scope-boundary.md). An extractor that *did* work on those
   hosts would have to be doing the thing we refuse to do, which means its
   capability and our scope boundary are in direct conflict: the tool would be
   exactly as capable as its willingness to circumvent access controls.

Point 3 is the decisive one. The capability gap between the two approaches is
not incidental. It follows from the scope boundary.

## Decision

Ifami implements standards-based retrieval only:

- Direct media URLs, with HTTP range resumption.
- HLS (`.m3u8`), including fMP4 with `EXT-X-MAP` and byte-range playlists.
- DASH (`.mpd`), `SegmentList` forms.
- HTML page extraction for `<video>`/`<source>` and OpenGraph video.

We do not bundle, link, vendor, or ship a downloader of any kind. We do not
download one at runtime and execute it. `deny.toml` denies copyleft licences and
a CI job asserts that no GPL extraction tool appears anywhere in the dependency
graph.

Sites that need an extractor are out of scope, and the README names YouTube as
broken in practice rather than leaving a user to discover it.

## Consequences

**We lose the sources people most want.** This is the whole cost, and it is
severe. Someone looking for a YouTube downloader will not use Ifami, and we say
so in the first screen of the README.

**We gain hosts we would not otherwise have.** Direct URLs, self-hosted media,
openly licensed datasets, and standards-based adaptive streams are the cases
where a hand-written, auditable implementation is genuinely sufficient. Those
cases also have almost no competition, because every competitor is optimising for
the sites we declined.

**The scope boundary becomes structural.** With no extractor, there is no place
for a per-host attestation handler to be added "just for" one site. The
limitation is a consequence of the architecture, not a policy promise that
depends on discipline.

**Licence is stable.** Apache-2.0 in, Apache-2.0 out, unless someone adds a
copyleft dependency and `deny.toml` fails CI.

## Alternatives considered

- **Bundle yt-dlp as a binary, GPL the whole app.** Rejected. It changes the
  licence of every artefact we distribute for a capability we would then be
  obliged to defend in a security review.
- **Bundle yt-dlp, GPL it, and accept it.** Rejected. Same reason, and it would
  also have forced the scope boundary to be enforced inconsistently.
- **Shell out to a yt-dlp the user installs separately.** Rejected. It is the
  same capability with an extra process boundary and no better audit story; the
  user gets no supported path and we get no credit.
- **Implement attestation handling ourselves.** Rejected under
  [ADR-0005](0005-scope-boundary.md). It needs a JS runtime and a spoofed TLS
  fingerprint, and it is the specific thing the project exists not to do.
- **Depend on a Rust-native extractor.** Rejected. The same scope conflict as
  yt-dlp, and the licence status of that ecosystem is less settled.