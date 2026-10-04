# Changelog

All notable changes to this project are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with
a leading `0.` until the scope boundary in `docs/SCOPE.md` stops moving, which is
not a thing a version number can express, so alpha versions are pre-release
(`-alpha.N`) and are expected to break.

Every entry here is a statement about verified behaviour. If a change is listed
under "Unreleased" it is on this branch and has passed the checks in
`CONTRIBUTING.md`; if it is listed under a released version it passed CI on the
tag. There is no third category.

## [Unreleased]

### Security

- **`quick-xml` upgraded 0.37.5 -> 0.41.0.** This closes two remote,
  unauthenticated denial-of-service advisories in the DASH manifest parser:
  - [RUSTSEC-2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195) —
    unbounded namespace-declaration allocation in `NsReader`, letting a single
    crafted start tag force large heap allocations on a remote manifest. Memory
    exhaustion.
  - [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194) —
    quadratic run time when checking a start tag for duplicate attribute names,
    letting one crafted tag pin a CPU core for minutes. CPU exhaustion.

  Both require a hostile or compromised origin serving a `.mpd`. `dash.rs` is
  exactly that code path. 0.41.0 is the first version that fixes either. No
  behavioural change to our parser; the API used here was unaffected.

  Found by the `cargo-audit` CI gate, which is the gate earning its keep.

### Fixed

- The declared `rust-version` was **1.82 and was not true**. `url` pulls in
  `idna` -> `idna_adapter` -> the `icu_*` stack, which declares 1.88. Corrected
  to 1.88, and the `msrv` CI job now reads the number from `Cargo.toml` instead
  of hardcoding it in four places -- which is how the two drifted apart in the
  first place.
- The `ifami-core stays embeddable` gate failed under `--no-default-features`
  because the loopback integration tests and one crate-level doctest imported
  the feature-gated `ReqwestClient`. Both are now gated on `network`, so a host
  that embeds the engine with its own transport inherits no test failures.
- `deny.toml` declared a top-level `[yanked]` table, which cargo-deny rejects as
  an unknown key. It is now the `yanked` field of `[advisories]`, where it
  belongs.
- `deny.toml` now evaluates the graph for `x86_64-pc-windows-msvc` rather than
  every platform. `native-tls` selects OpenSSL off Windows, so the all-platform
  graph contained a crate the ban list refused -- one that is not in anything we
  build or ship.
- CI declared no `permissions`, so `audit-check` could not write its check run
  and failed for that reason on top of the real one. CI is now `contents: read`
  by default, with `checks: write` scoped to the audit job alone.
- `deny.toml` had three further faults that only surfaced once the config
  finally parsed, all of which would have failed the next run:
  - `rustls-pki-types` was banned *and* present. It is a zero-dependency crate
    of newtypes (`Certificate`, `PrivateKey`, `ServerName`) that reqwest shares
    across its TLS backends; it performs no cryptography and is not a TLS
    implementation. Unbanned, with `ring`, `rustls`, `aws-lc-rs` and `openssl`
    still refused -- those are the boundary the ban list exists to hold.
  - `[advisories]` could not express the intended policy because cargo-deny's
    schema does not have the knobs it appears to. `unmaintained` is a scope
    selector, not a severity, and there is no way to exempt an individual crate:
    both `{ crate = "fxhash" }` and `{ crate = "fxhash@0.2.1" }` parse, are
    accepted, and then do nothing. Unmaintained checks are now off in this gate
    and said so plainly. `cargo audit` still reports them as informational on
    every run, so the signal survives; only the ability to fail on it is gone.
  - `Cargo.lock` was marked `-diff` in `.gitattributes`, hiding the one diff a
    dependency bump most needs reviewed. Removed -- including the comment that
    justified it, which argued for hiding merge conflicts in the very same breath
    as hiding the diff.
- `ifami-cli` declared `ifami-core` as a bare path dependency, which cargo-deny
  correctly classifies as an unpinned wildcard. Now carries `version = "0.1.0"`,
  which cannot drift: cargo fails the build if a path dependency stops satisfying
  its requirement.
- CI pinned cargo-deny through `EmbarkStudios/cargo-deny-action@v2`, which
  supplied 0.16.0 while a local `cargo install` gave 0.20.2. The two disagree
  about the config *schema*, so a policy gate could break on a tool upgrade
  rather than on a policy change. Both jobs now install a pinned
  `cargo-deny@0.20.2` -- the version this config was validated against -- which
  also removes about three minutes of compiling the tool from each run.
- The `msrv` job interpolated `steps.floor.outputs.msrv` into its own job `name`,
  which is not a permitted context there. The effect was not a failed step but a
  *rejected workflow*: the run completed as a failure having created zero jobs,
  with no annotation and nothing in the log to explain why. The name is now
  static and the version goes to the step summary. A required status check whose
  name changes with the manifest would also break branch protection on the next
  MSRV bump, demanding a check that no longer exists.

The next three things that matter are in `README.md` under
[Status](README.md#status).

## [0.1.0-alpha.1] - 2026-10-04

The first public snapshot. Nothing here is finished, and the README says so in
the same words rather than in a footnote.

### Engine (`ifami-core`)

- Resolver registry that tries **every** matching resolver rather than the first,
  so a source that is not supported fails with an accurate reason instead of an
  arbitrary one.
- Transfer engine for direct files, with HTTP range resume verified against the
  bytes on disk rather than a stored hint. A server that answers a ranged request
  with `200 OK` causes a clean restart, not a corrupt file.
- HLS and DASH manifests, including fMP4 (`EXT-X-MAP`) and byte-range playlists.
  Segmented transfers write to `.part` and are renamed only once complete.
- `TaskSnapshot`: a stable, camelCase wire type for the UI, with `TransferKind`
  deliberately distinct from `MediaKind` so "delivered as fragments" and "is a
  video" cannot be confused by a serialiser.
- Per-task rate marks (`MARK_INTERVAL_MS`, capped at `MAX_MARKS`), transfer
  seams recorded on resume, and a bounded event log, so the UI can draw a speed
  history instead of guessing one.
- Atomic store writes, and corruption recovery that surfaces the failure rather
  than silently starting a new queue.

### CLI (`ifami-cli`)

- `get`, `info`, `list`, `pause`, `resume`, `doctor`. Stable exit codes: `0`
  success, `1` ordinary failure, `2` the request contradicted the task's state.
- Contains argument parsing and output formatting. Nothing else. That is enforced
  by review and asserted in `CONTRIBUTING.md`, not by accident.

### Desktop app (`apps/desktop`)

The interface is complete as an interface and **runs against a mock backend**. The
Tauri command layer is not written, so nothing is wired to `ifami-core` yet. This
is the largest single piece of remaining work.

- Light-mode, border-based elevation, one accent colour, 8px grid, three
  elevation levels, no shadows and no horizontal scroll at any window size.
- The welcome screen stays on screen while downloads run. Above 870px of window
  height the full home screen is shown, including the ring of 25 supported-site
  marks; below that the ring is dropped and the download list takes the space.
  The list is guaranteed three rows and its height depends on the window and
  nothing else, so adding a download never moves anything above it.
- A speed test drawn as a trading chart with two lanes, in kbps as well as Mbps,
  switching at 999.5 kbps. No target URL field, by design.
- Keyboard-first queue: expand, collapse, row selection, pause/resume, details,
  remove, and focus returned to the control that opened each overlay.

### Tests

- **283 passing** at this tag: 259 unit, 16 loopback integration, 5 CLI, 3 doc
  tests. `cargo fmt --check` and `cargo clippy --workspace --all-targets
  -- -D warnings` are clean.
- Integration tests bind `127.0.0.1` only, and assert that they have.

### Policy and process

- Architecture decision records 0001–0009, `docs/SCOPE.md` as a normative
  boundary, and a CI gate that fails if a UI framework or a linkable extractor
  appears in the graph.
- `cargo-deny` for licences, bans and sources; `cargo-audit`; gitleaks; an MSRV
  job at the `rust-version` declared in the manifest.

### Known gaps at this tag

- The Tauri shell: no `tauri.conf.json`, no capabilities, no icons, no command
  layer, no installer. The UI cannot download anything yet.
- No fuzz targets for the XML and M3U8 parsers, which parse attacker-influenced
  input and are the most likely place for a real vulnerability to sit.
- The loopback fixture server is plaintext, so there is no TLS test coverage.
- Three upload tests are described in the code and not yet written.