# ADR-0002: Windows-first, desktop-first, and no web tier in v1

- **Status:** Accepted
- **Date:** 2026-10-01

## Context

Three delivery surfaces were proposed: a web app, a Windows desktop app, and a
Chrome extension. Windows was recommended as the main product.

The following constraints were established by reading platform policy directly:

- **AdSense is not permitted in a software application.** Ad placement policy:
  "Publishers are not permitted to distribute Google ads or AdSense for search
  boxes through software applications including, but not limited to toolbars,
  browser extensions, and desktop applications."
- **Chrome Web Store forbids enabling unauthorised download of copyrighted
  media**, and separately deprioritises video downloaders from its featured
  programme, so an approved extension gets no organic discovery.
- **Chrome Web Store forbids an extension whose single purpose is launching
  another app**, which was the original intent for the extension.
- **Microsoft Store policy 10.10.1** requires that the product respect the
  advertising-ID setting the user has selected. A product with no advertising
  surface satisfies this trivially.

The web tier was also the only tier that created a reason for Ifami to operate a
media cache, and therefore the only tier that created meaningful server-side
liability. It was the most expensive tier to build and the least defensible.

## Decision

v1 is Windows desktop only, built on Tauri, on top of `ifami-core`.

There is no web tier in v1. There is no hosted service. Ifami does not operate
a media cache at any point in v1.

Distribution for v1 is a signed installer. Microsoft Store submission is
deferred, because store certification requires a signed binary chained to the
Microsoft Trusted Root Program (policy 10.2.9) and a demo account for
server-backed features (10.3), and because a Store listing is a poor fit for a
project whose first users are technically capable of running a signed CLI.

## Consequences

- The download manager, which is the genuinely valuable and genuinely
  policy-clean part of this product, gets full attention and has no policy
  surface at all.
- No revenue depends on ad inventory. Funding is sponsorship or donation. This
  is a real constraint and is stated plainly in the README rather than papered
  over.
- We lose the "paste a link in a browser tab" acquisition path. Accepted: that
  path requires operating a service, and the service is the liability.
- A CLI remains available for users who do not want a GUI, and is the CI
  reference client.