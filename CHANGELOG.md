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

Nothing yet. The next three things that matter are in `README.md` under
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
  job at 1.82.

### Known gaps at this tag

- The Tauri shell: no `tauri.conf.json`, no capabilities, no icons, no command
  layer, no installer. The UI cannot download anything yet.
- No fuzz targets for the XML and M3U8 parsers, which parse attacker-influenced
  input and are the most likely place for a real vulnerability to sit.
- The loopback fixture server is plaintext, so there is no TLS test coverage.
- Three upload tests are described in the code and not yet written.