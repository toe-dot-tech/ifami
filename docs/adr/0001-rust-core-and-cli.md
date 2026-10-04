# ADR-0001: A shared Rust core, UI-free, with a CLI

- **Status:** Accepted
- **Date:** 2026-10-01

## Context

The product needs an engine that can be driven from a desktop app, from a CLI,
and eventually from a browser. The engine does I/O-heavy, stateful work:
resumable transfers, concurrency control, on-disk state.

Two failure modes were on the table:

1. Duplicating the engine per client (e.g. a JS engine in the web tier and a
   separate implementation for the desktop tier). Divergence is guaranteed, and
   bugs will exist in exactly one tier.
2. Coupling the engine to a UI framework, which makes the engine untestable
   without a window.

## Decision

The engine is a standalone Rust library crate, `ifami-core`, with:

- No UI dependency of any kind.
- `HttpClient` as a trait, so tests can drive the engine without a socket.
- A public `download_one` entry point that takes an injected client, so every
  behaviour is testable deterministically.
- A thin `ifami-cli` binary over the same API. The CLI is the reference client
  and the way the engine is exercised in CI.

## Consequences

- The engine is compiled and tested on every platform CI runs, independent of
  whether a UI exists for that platform.
- The engine can be reused by a Tauri desktop app, a CLI, and (via a future
  `wasm32` target with an injected fetch-based client) a browser build, without
  a rewrite.
- Async (`tokio`) throughout. This costs some complexity in tests, which are
  fully async as a result. Accepted: the alternative (thread-per-transfer)
  makes pause/resume and concurrency far harder to get right.
- Optional dependencies are deliberately few. The `reqwest` implementation is
  behind a `network` feature so a downstream embedder can supply its own client
  and inherit the core without an HTTP stack.