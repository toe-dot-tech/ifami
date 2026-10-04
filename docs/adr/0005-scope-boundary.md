# ADR-0005: Publish the scope boundary, and treat it as a commitment

- **Status:** Accepted
- **Date:** 2026-10-01

## Context

YouTube's Terms of Service prohibit, among other things, downloading any part of
the Service except as specifically permitted, with prior written permission, or
as permitted by applicable law, and separately prohibit circumventing features
that prevent or restrict copying.

YouTube is actively narrowing access to downloadable formats. From the yt-dlp
maintainer documentation:

> YouTube is gradually enforcing the use of a "PO Token" to be able to download
> videos. Due to the nature of these tokens, yt-dlp cannot generate them and they
> must be provided externally.

The same documentation notes that OAuth login no longer works, that cookie
export is the supported path, and that using an account this way "run[s] the
risk of it being banned (temporarily or permanently)."

This is the crux of the project. The capability is not merely restricted by
policy; the primary target is being progressively and structurally locked down
by the strongest anti-abuse team in the industry, using a credential that an
open-source tool cannot generate.

## Decision

Ifami publishes an explicit, normative scope boundary in
[`docs/SCOPE.md`](../SCOPE.md), stating that it does not decrypt DRM, bypass
authentication, defeat geo-restrictions, extract keys, impersonate browser TLS
fingerprints, or execute source-site JavaScript to compute signatures.

When a source requires any of the above, Ifami reports `AuthRequired` and stops.
This is treated as a correct, final answer rather than an open bug.

The scope boundary is load-bearing in three ways:

1. **It distinguishes the product.** A general-purpose downloader is presumptively
   a circumvention tool. A downloader with a stated scope, that reports
   `AuthRequired` and stops, is not. The distinction is meaningful to a human
   reviewer and to a platform policy reviewer.
2. **It is a design input, not a disclaimer.** The engine's resolver interface
   returns `ResolveError::AuthRequired` as a first-class outcome. There is no
   code path that could be extended to bypass a gate, because none exists.
3. **It is honest.** The alternative was to ship a product whose headline feature
   is the one thing we cannot keep working, and to be surprised when it breaks.

## Consequences

- YouTube is listed in the README's known-limitations table as unsupported in
  practice, with a link to the maintainer documentation. Users learn this before
  they install rather than after.
- The addressable corpus is narrower: self-hosted media, openly-licensed media,
  and adaptive streams served without access controls. This is a real market and
  it is genuinely underserved, but it is not "everything."
- The product can be reviewed on its engineering rather than on its legal
  exposure. This is the whole point.
- If YouTube ever ships a supported, licensed export path that permits
  retrieval, that is a normal feature request and is handled as one.