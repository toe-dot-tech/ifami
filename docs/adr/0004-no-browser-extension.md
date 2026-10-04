# ADR-0004: The browser extension is out of scope

- **Status:** Accepted
- **Date:** 2026-10-01

## Context

A Chrome extension was planned, primarily to test on real pages before
publishing to the Chrome Web Store.

Chrome Web Store Program Policy states, verbatim:

> Do not facilitate unauthorized access to content on websites, such as
> circumventing paywalls or login restrictions.
>
> Do not encourage, facilitate, or enable the unauthorized access, download, or
> streaming of copyrighted content or media.

A general-purpose "download any link" extension falls squarely inside the
prohibited category. That is independent of intent, and independent of whether
the extension is monetised.

Two further policies remove the workarounds we would have reached for:

> **Minimum Functionality.** Do not post an extension with a single purpose of
> installing or launching another app, theme, webpage, or extension.

This forbids the original design, in which the extension's job was to hand a
URL to the desktop app.

> **Manifest V3.** the full functionality of an extension must be easily
> discernible from its submitted code ... [remote resources] must not contain
> any logic.

This forbids shipping the resolver as a remote script, which was the other
plausible design.

And from the featured-programme policy:

> [non-compliant but not explicitly banned products] such as VPN extensions and
> **video downloaders** ... are currently not featured in the Chrome Web Store.

So even if it were approved, it would receive no organic discovery. The only
realistic acquisition path for such an extension is you already having installed
it.

Additionally, MV3 is hostile to this workload generally. Manifest V3 removed
remote code and most blocking APIs, and content-script injection on the pages we
would need is subject to an expanding list of host-permission restrictions.
Even setting policy aside, the engineering cost is high and the payoff is
structurally limited.

## Decision

No browser extension. Not in v1, and not planned.

## Consequences

- Ifami does not interact with pages in the user's browser. It takes a URL.
- Page-derived metadata for self-hosted and openly-licensed media is instead
  obtained by fetching the page from our own process, which is permitted and
  which works without an extension.
- We forgo the "right-click and download" flow. Accepted.
- Nothing in Ifami requires a browser extension, a native messaging host, or
  Chrome-specific tooling, which removes an entire class of install-failure and
  review-risk problems.