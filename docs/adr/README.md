# Architecture Decision Records

Nine decisions, in the order they were made. Each one records a choice that was
not obvious, along with what it cost, because a decision record without the cost
is a press release.

The ADR process itself follows [Michael Nygard's template](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions):
numbered, immutable, and superseded rather than edited. If you disagree with one,
write the tenth.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-rust-core-and-cli.md) | A Rust core with a thin CLI over it | Accepted |
| [0002](0002-windows-first-no-web-tier.md) | Windows desktop first; no web tier | Accepted |
| [0003](0003-no-accounts-no-fingerprinting.md) | No accounts and no fingerprinting, permanently | Accepted |
| [0004](0004-no-browser-extension.md) | No browser extension | Accepted |
| [0005](0005-scope-boundary.md) | The scope boundary: what we decline to fetch | Accepted |
| [0006](0006-standards-only-no-extractor-bundles.md) | Standards only; no bundled extractor binaries | Accepted |
| [0007](0007-loopback-fixture-server.md) | A loopback fixture server and the `HttpClient` seam | Accepted |
| [0008](0008-mux-boundary.md) | Where muxing is allowed, and where it is not | Accepted |
| [0009](0009-output-container-vs-delivery-mechanism.md) | Output container vs delivery mechanism | Accepted |

## Writing one

Copy `0001-rust-core-and-cli.md` as the template. Keep it short; the ones above
are a page each, and that is a deliberate constraint — an ADR nobody reads has
failed at the only job it has.

Four things it must contain:

1. **The context.** What was true at the time that forced a choice. Not what we
   believe now; the pressure that produced the decision.
2. **The decision.** One sentence, in the active voice. "We will…".
3. **The consequences.** Including the ones we dislike. Every ADR here has at
   least one, and those are the ones worth reading.
4. **The alternatives.** What was rejected, and specifically why. A record with
   no rejected alternatives records a fait accompli.

Then:

- Name it `NNNN-kebab-title.md`, using the next number. Never reuse a number.
- Add a row to the table above.
- Add it to `CHANGELOG.md` under `[Unreleased]`.

## Status values

- **Proposed** — written, under discussion, not yet in force.
- **Accepted** — in force. Code follows it.
- **Superseded by NNNN** — kept for the reasoning. Never deleted; a decision that
  was reversed without explanation is how a codebase stops being auditable.

Changing an accepted ADR in place is not allowed. If it turns out to be wrong,
that is what the next number is for.