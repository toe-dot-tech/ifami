<div align="center">

# Ifami

**A private, local-first media fetcher for media you have the right to download.**

No accounts. No ads. No telemetry. No fingerprinting. Your bytes go from the
origin to your disk and nowhere else.

[![CI](https://github.com/toe-dot-tech/ifami/actions/workflows/ci.yml/badge.svg)](https://github.com/toe-dot-tech/ifami/actions/workflows/ci.yml)
[![Licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/msrv-1.88-blue.svg)](Cargo.toml)
[![YouTube does not work](https://img.shields.io/badge/YouTube-broken-critical.svg)](#read-this-first-what-ifami-will-not-do-for-you)

[Apache-2.0](LICENSE) · Windows-first · [Scope & limitations](docs/SCOPE.md) ·
[Architecture](docs/ARCHITECTURE.md) · [Decisions](docs/adr/README.md) ·
[Changelog](CHANGELOG.md)

</div>

---

## Read this first: what Ifami will not do for you

Ifami is not a downloader for any site you can paste a link to. That is a
deliberate product decision, made for legal and architectural reasons, and it is
the single most important thing to understand before you spend five minutes
trying to get it to work on something.

**YouTube does not work. At all. Not partially.**

This is the honest headline limitation, and we put it here rather than burying it:

| Source | Status | Why |
| --- | --- | --- |
| **YouTube** | **Broken. Unusable.** | YouTube gates playback behind attestation tokens tied to a specific browser's JavaScript engine and TLS/HTTP2 fingerprint. A correct extraction requires impersonating a browser at the transport layer and executing the site's own JavaScript. We do neither. See [ADR-0005](docs/adr/0005-scope-boundary.md) and [ADR-0006](docs/adr/0006-standards-only-no-extractor-bundles.md). |
| Facebook / Instagram / TikTok / X / Reddit video | Not supported | Gated behind session cookies and/or attestation. We do not bypass auth. |
| DRM-protected anything (Widevine, FairPlay, PlayReady) | Refused by design | See [ADR-0005](docs/adr/0005-scope-boundary.md). This is a correct final answer, not a bug. |
| Vimeo, Dailymotion, and similar public embeds | Partial | Works only when the publisher serves an unencrypted manifest. Gated embeds return `AuthRequired`. |
| **Direct `.mp4` / `.mov` / `.webm` URLs** | **Works well** | The primary, fully-supported case. |
| **HLS (`.m3u8`) and DASH (`.mpd`)** on open sources | **Works well** | Primary, fully-supported case. Includes fMP4 (`EXT-X-MAP`) and byte-range playlists. |
| Your own site / S3 / R2 / LAN server | Works well | First-class supported case. |
| Anything paywalled, geo-restricted, or membership-gated | Refused by design | Bypassing these is out of scope permanently. |

If you need a tool that downloads from YouTube, this is the wrong tool and we
would rather you know that in the first ten seconds than after an hour of
troubleshooting.

**What we ask of you:** use Ifami for media you own, media published under a
licence that permits retrieval, or a direct link from a site that intends for the
file to be retrievable. Everything else, we decline on purpose.

---

## Why this exists

Every general-purpose media downloader eventually acquires the same two
properties, and both of them are disqualifying for a tool you install on your
own machine:

1. **A user identifier.** To monetise, to rate-limit, or to "prevent abuse", the
   vendor fingerprints your machine or requires a login. Once you have a user ID,
   you have a data subject, and you have a privacy policy.
2. **A server in the middle.** Bytes are relayed, media is cached, and your
   viewing habits belong to somebody other than you.

Ifami is built to be useful *because* it has neither property. It is a local
program that fetches files. If you delete our accounts (there are none), close
our servers (there are none), we cannot tell that you ever existed.

We would rather be boringly correct on a narrow scope than impressively capable
on a scope we would have to lie about.

---

## Design commitments

These are the things a reviewer can hold us to. Each has an ADR behind it.

| Commitment | Consequence | ADR |
| --- | --- | --- |
| **No user identifier exists** | No machine UUID, no MAC, no disk serial, no browser/canvas fingerprint, no device ID. Nothing to correlate because nothing is collected. | [0003](docs/adr/0003-no-accounts-no-fingerprinting.md) |
| **No accounts, ever** | No login for any function. No "7 downloads a day". No rate limit that needs enforcement infrastructure. | [0003](docs/adr/0003-no-accounts-no-fingerprinting.md) |
| **No ads, any channel** | No ad SDK, no ad network, no affiliate injection, no sponsored placement. We looked at ad monetization and it is structurally impossible: Google AdSense bans it in desktop apps, and Chrome's Web Store bans it in extensions. | [0004](docs/adr/0004-no-browser-extension.md) |
| **No browser extension** | The Chrome Web Store forbids unauthorized media downloading and single-purpose launcher extensions. Building one would mean a takedown on day one. | [0004](docs/adr/0004-no-browser-extension.md) |
| **No DRM circumvention** | `DrmProtected` is a first-class terminal error with no retry path and no bypass path in the codebase. | [0005](docs/adr/0005-scope-boundary.md) |
| **No telemetry** | No crash reporter, no analytics, no usage pings. The only persisted state is your local queue file. | [0003](docs/adr/0003-no-accounts-no-fingerprinting.md) |
| **Resume actually works** | Verified against the file on disk, not against a hopeful byte counter. If the server cannot honour the range, we restart cleanly rather than corrupt the output. | — |
| **Bounded dependency surface** | `cargo deny` is a CI gate: licences, bans, and allowed sources. `unsafe` is `forbid`-denied in our own crates. Missing docs are denied. Clippy runs with `-D warnings`. | — |
| **No bundled extractors** | No yt-dlp, no ffmpeg, no downloader of any kind in the tree or the dependency graph. Asserted by CI, not just documented. | [0006](docs/adr/0006-standards-only-no-extractor-bundles.md) |
| **Loopback-only test fixtures** | Integration tests bind `127.0.0.1` and assert it. A routable listener in a test suite would be a security regression. | [0007](docs/adr/0007-loopback-fixture-server.md) |
| **Resume is proven, not asserted** | Tests cut a transfer mid-stream and compare the resumed file byte-for-byte against the source. They also fail if the client silently restarts instead of resuming. | [0007](docs/adr/0007-loopback-fixture-server.md) |
| **Video-only media is not blanket-refused** | A manifest with no audio track downloads. A manifest that offers audio is not handed a silent file as if it were the video. | [0008](docs/adr/0008-mux-boundary.md) |
| **Streams get real extensions** | An fMP4 HLS stream lands as `.mp4`, a transport-stream one as `.ts`, not as `.bin`. `.bin` now means "we genuinely could not tell". | [0009](docs/adr/0009-output-container-vs-delivery-mechanism.md) |
| **The scope boundary lives in code** | Not just in a policy doc. `ResolveError::AuthRequired` and `DrmProtected` have no `retryable` path. You can grep for the bypass and find nothing. | [0005](docs/adr/0005-scope-boundary.md) |

---

## Status

**Pre-release. Alpha. The engine is verified. The desktop app is not finished and
cannot download anything yet.**

Being specific here costs nothing and saves someone an evening, so here is
exactly what works and what does not.

### Verified

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` all pass. **283 tests, zero failures**: 259 unit,
  16 loopback integration, 5 CLI, 3 doc tests.
- The engine (`ifami-core`) is complete against the scope above: direct files,
  range resume, HLS and DASH including fMP4 and byte-range playlists, the queue
  state machine, and atomic persistence.
- **Resume is proven, not claimed.** The integration tests cut a transfer
  mid-stream, resume it, and compare the finished file byte-for-byte against the
  source. They also fail if the client silently restarts instead of resuming,
  which is the failure mode that produces a corrupt file with no error.
- The reference CLI (`ifami-cli`) works.
- CI runs seven jobs on every push: build, format, Clippy, tests and a release
  build; "core stays embeddable"; `cargo-audit`; `cargo-deny`; licence policy;
  MSRV; and a secret scan. Six of the seven are policy gates rather than tests.
  All seven have caught a real defect already — see
  [CHANGELOG](CHANGELOG.md#unreleased).

### Not finished, or not verified

- **The desktop app cannot download anything.** The interface is complete as an
  interface and runs against a **mock backend** in a browser. The Tauri shell —
  the command layer, the config, the capabilities, the icons, the installer —
  is not written. `src-tauri/src/lib.rs` is currently `pub fn run() {}`. This is
  the single largest piece of remaining work and the reason there is no binary to
  hand you.
- **The UI has never been run inside Tauri.** Every layout and behaviour number
  quoted anywhere in this repository was measured in a browser against the mock.
  That is a real limitation of the evidence, not a caveat about the code.
- **No TLS test coverage.** The loopback fixture server is plaintext, so the TLS
  path is exercised by nothing.
- **No fuzzing.** The XML and M3U8 parsers have no fuzz targets. They parse
  attacker-influenced input and that judgement has already been vindicated the
  hard way: `cargo-audit` found two remote unauthenticated denial-of-service
  advisories in `quick-xml`, the DASH parser's dependency, and they are fixed
  as of this snapshot. That covers *known* advisories in our dependencies. It
  does not cover the bugs nobody has published yet, which is what fuzzing is
  for, and on a project whose entire claim is "we do not do anything you did not
  ask for" an unfuzzed parser is the wrong thing to leave. This is the highest
  value remaining gap.
- **Three upload tests are described in comments and not yet written.**

The next three things that matter, in order: the Tauri command layer, fuzz
targets for the manifest parsers, then TLS in the fixture server.

---

## The desktop app

The interface is built and documented here because it is a real part of the
project, and because the reason it is not finished is a single missing seam.

**It runs.** `apps/desktop` is plain ES modules, CSS and HTML with no framework
and no build step, so it is served statically and driven against a mock backend
that implements the exact command contract the real shell will expose. Every
layout claim below was measured in that harness, at twenty-two window sizes from
1440×1200 down to 400×520.

- **The welcome screen stays on screen while downloads run.** Above 870px of
  window height the full home screen is shown, including the ring of 25
  supported-site marks; below it the ring is dropped and the list takes the space.
  The download list is guaranteed three rows and its height depends on the window
  and nothing else, so adding a download never moves the toolbar, the column
  labels, or the heading. Measured across three sequential adds at fourteen
  window sizes: zero movement.
- **No horizontal scroll at any size**, and no dead space between the last row
  and the statusbar. Both asserted, not eyeballed.
- **Three elevations, border-based, one accent colour, 8px grid, no shadows.**
- **A speed test drawn as a trading chart**, two lanes, in kbps as well as Mbps,
  switching at 999.5 kbps. There is no target URL field, deliberately: it is
  measuring your connection to a CDN, not a file you chose.
- **Keyboard-first queue.** Expand, collapse, select, pause/resume, details,
  remove — and focus returns to the control that opened each overlay.

What is missing is `src-tauri`: the command layer that forwards
`add_url`, `pause_task`, `resume_task`, `remove_task`, `speed_test` and the rest
into `ifami-core`, plus the clipboard read that is currently done in JavaScript
and belongs in Rust. The UI already speaks the exact contract those commands
have to implement, so wiring it is mechanical rather than exploratory.

---

## Architecture

```
crates/ifami-core     Platform-free engine. No UI, no framework, no global state.
                      Resolvers -> MediaPlan -> TransferEngine -> Manager (queue).
crates/ifami-cli      A thin reference client. Argument parsing and formatting ONLY.
                      No download logic lives here. (That is the point.)
apps/desktop          Tauri v2 Windows app. Owns presentation, talks to ifami-core.
  src/                The interface. Framework-free, no build step.
  src-tauri/          The shell. Its own cargo workspace; see below.
```

The split is load-bearing. `ifami-core` has no idea a GUI exists, which is what
makes the transfer logic testable with a scripted HTTP client and a loopback
fixture server, and what makes a third-party embed possible.

The CLI contains **no download logic beyond argument parsing and output
formatting**. If you find yourself wanting to put logic there, it belongs in
`ifami-core` where it can be tested.

`apps/desktop/src-tauri` is a **separate cargo workspace**, excluded from the
engine's. Tauri pins a different dependency graph than the engine does, and
sharing one lockfile between them produces two irreconcilable resolution
attempts. The cost is that `cargo test --workspace` at the root does not cover
the shell; the cost of not doing it is that the root build stops resolving.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design, and
[`docs/adr/`](docs/adr/README.md) for the nine decisions and their reasoning.

### Why Tauri, not Electron

The engine is Rust either way, so Tauri gives a native WebView against a
Rust core with a small install footprint. Electron would bundle a second
JavaScript runtime and a second Chromium for a program whose entire claim is
"we do not do anything you did not ask for".

---

## Building

Requires a stable Rust toolchain (1.88 or newer; see `rust-version` in
`Cargo.toml`, which is the authority). The pinned toolchain is in
`rust-toolchain.toml`; `rustup` will read it.

```sh
git clone https://github.com/toe-dot-tech/ifami
cd ifami
cargo build --release
```

To check exactly what CI checks:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

<details>
<summary>On Windows, if <code>cargo</code> cannot find <code>link.exe</code></summary>

The MSVC build tools have to be on the path before you build. There is a helper:

```powershell
. .\scripts\msvc-env.ps1
cargo build --release
```

It reads the environment from your Visual Studio install via `vcvars64.bat` and
sets it for the current shell. Nothing is written to your machine's environment
variables permanently, because a permanent change is the kind of thing that
breaks a different project later.
</details>

Run the CLI:

```sh
# Queue one or more links and wait for them to finish.
cargo run -p ifami-cli -- get https://example.invalid/clip.mp4

# See what a link offers without downloading anything.
cargo run -p ifami-cli -- info https://example.invalid/clip.m3u8

# Inspect and steer the queue.
cargo run -p ifami-cli -- list
cargo run -p ifami-cli -- pause <ID>          # or pause all with no id
cargo run -p ifami-cli -- resume <ID>

# Versions, resolved paths, and a restatement of the scope boundary.
cargo run -p ifami-cli -- doctor
```

Exit codes are stable: `0` success, `1` ordinary failure, `2` the request
contradicted the task's state. `ifami --help` documents every flag.

`ifami-core` denies `missing_docs` and `unsafe`, and treats all Clippy lints as
errors. If a build passes locally, it should pass in CI.

---

## Contributing

Read [docs/SCOPE.md](docs/SCOPE.md) before you write code. It is normative: a
patch that needs to cross one of its boundaries will not be merged, and it is
much cheaper for both of us if you find that out before you build the feature.

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

Report vulnerabilities privately. See [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Ifami does not bundle or link yt-dlp or any other extractor. This is a
deliberate licensing decision — those are GPLv3+ and shipping them inside our
installer would make the distributed application GPLv3+.