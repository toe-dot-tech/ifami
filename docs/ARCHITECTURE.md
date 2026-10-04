# Architecture

This document is a map, not a specification. Behaviour is specified in
[`SCOPE.md`](SCOPE.md), decisions are recorded in [`adr/`](adr/), and the code
is the final word on everything else.

## Crate layout

```
ifami/
├── crates/
│   ├── ifami-core/     Engine. No UI, no framework, no global state.
│   │   ├── src/
│   │   │   ├── lib.rs         Crate root, USER_AGENT, digest()
│   │   │   ├── model.rs        Media, Format, Quality, Container
│   │   │   ├── naming.rs       Filename templates + sanitisation
│   │   │   ├── error.rs        Error taxonomy incl. AuthRequired
│   │   │   ├── net/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── client.rs   HttpClient trait (injectable)
│   │   │   │   ├── reqwest.rs  Default implementation
│   │   │   │   ├── capability.rs  Accept-Ranges / Content-Range probing
│   │   │   │   └── range.rs    HTTP range arithmetic + decide_resume
│   │   │   ├── resolve/
│   │   │   │   ├── mod.rs      Resolver trait + registry + expand_segmented
│   │   │   │   ├── direct.rs   Direct media URL
│   │   │   │   ├── page.rs     HTML embed / OpenGraph extraction
│   │   │   │   ├── hls.rs      M3U8 parsing + segment plan
│   │   │   │   └── dash.rs     MPD parsing (XML)
│   │   │   └── download/
│   │   │       ├── mod.rs
│   │   │       ├── engine.rs    The transfer engine. run/run_direct/run_fragmented
│   │   │       ├── task.rs      Transfer state machine
│   │   │       ├── store.rs     Atomic, corrupt-tolerant persistence
│   │   │       └── manager.rs   Queue, pause/resume, concurrency
│   │   └── tests/
│   │       ├── common/mod.rs    Hand-rolled HTTP/1.1 origin on 127.0.0.1
│   │       └── loopback.rs      End-to-end tests over a real socket
│   ├── ifami-cli/      Thin reference client over the core
│   │   └── src/
│   │       ├── main.rs          clap parsing; nothing else
│   │       └── format.rs        bytes(), spinner_note()
│   └── (no crates; the desktop app lives under apps/)
├── docs/
│   ├── SCOPE.md
│   ├── ARCHITECTURE.md
│   └── adr/
├── deny.toml           Dependency policy: licences, bans, sources
└── .github/workflows/ CI: fmt, clippy -D warnings, audit, deny, MSRV
```

The desktop app is not in that tree because it is not a cargo workspace
member. `apps/desktop` holds the interface -- plain ES modules, CSS and HTML, no
build step -- and `apps/desktop/src-tauri` holds the Tauri shell, which is its
own cargo workspace and is excluded from the root one. Tauri pins a different
dependency graph than the engine, and sharing one lockfile between them produces
two irreconcilable resolution attempts.

The shell is not written yet: `src-tauri/src/lib.rs` is `pub fn run() {}`. The
interface beside it is complete and runs against a mock backend implementing the
same command contract the shell must expose, so the wiring is mechanical rather
than exploratory. See Status in the README for what that does and does not prove.

`engine.rs` is separate from `manager.rs` on purpose: the engine owns one file
and one writer and knows nothing about the queue, and the manager owns the
queue and knows nothing about bytes. The two only meet at `engine::run`, which
takes a borrowed task and hands back an outcome.

`expand_segmented` lives in `resolve/mod.rs` rather than in a resolver because
it is a post-processing pass over an already-resolved `Format`: it walks a
segmented format's URL and fills in the segment list. Keeping it there means a
resolver never has to care whether its output is complete.

## Layering rules

These are enforced by review, and are the reason the engine stays testable.

1. **`model`, `naming`, `error`, `range`** are pure. No I/O, no clock, no
   randomness. Everything here is unit-testable with no fixtures.
2. **`net` and `resolve`** may perform network I/O. They may not touch the
   filesystem. Every function takes an `HttpClient`, so none of them can open a
   socket that a test did not hand them.
3. **`download`** may touch the filesystem and the network. It contains the
   state machine and is where the interesting invariants live.
4. **`ifami-cli`** contains no logic of its own. If it does, that logic is in the
   wrong crate.

## The `HttpClient` seam

Every network-touching code path goes through:

```rust
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn execute(&self, req: HttpRequest) -> Result<HttpResponse, NetError>;
}
```

This exists so the entire engine is testable against an in-memory transport.
Integration tests drive a real `reqwest` client against a `127.0.0.1` fixture
server; unit tests drive a scripted client. Neither touches the public network,
and CI asserts that.

## Transfer state machine

The only part of the system with real invariant risk. States, and the only legal
transitions:

```
Queued ─────► Downloading ─────► Completed
                │    │   ▲
                │    │   └── Retrying ◄─┐
                │    └──────► Paused    │
                └───────────► Failed ───┘
```

Invariants, all asserted in debug builds and covered by tests:

- A transfer in `Downloading` owns exactly one open writer.
- `Paused` is only reachable by closing the writer and flushing, or by an error
  after flushing. A paused transfer is always resumable from a known-good offset.
- Completion means the on-disk length equals the expected total length, when the
  total is known. An unverified transfer never reaches `Completed`.
- A task on disk is authoritative for resume offset only after the store has
  been reconciled against actual file lengths. Disk wins; the store is a hint.

## Resume semantics

Resume is the reason this project is worth building carefully, so the semantics
are stated exactly:

- Resuming a partial direct download issues a `Range: bytes=<n>-` request.
- A server answering `200 OK` instead of `206 Partial Content` has ignored the
  range. We **abort and discard the partial file** rather than silently
  appending a full copy. This is the single most common corruption bug in
  download managers.
- A `Content-Range` total that disagrees with the size we already had means the
  remote object changed underneath us. We treat this as `RemoteChanged` and do
  not resume.
- If `Accept-Ranges` is absent, the source is not resumable. We say so at resolve
  time rather than discovering it after the user has waited.
- For fragmented media, resume is per segment. Completed segments are recorded
  by index in the plan file and are not re-fetched.

## Persistence

The queue is written atomically (temp file, `fsync`, rename) and is tolerant of
truncation: on load, a JSON parse failure is recovered by line-delimited
scanning for the last valid task object. A corrupt queue degrades to "some tasks
lost," never to "app won't start."

Task files are keyed by content hash where the source provides one, so the
queue survives URL changes for the same object.

## What is deliberately absent

- No global mutable state and no singletons.
- No background threads that outlive their owner.
- No `unsafe`. The core crate sets `#![forbid(unsafe_code)]`.
- No wall-clock time in the engine; elapsed-time estimates are computed in the
  client from injected timestamps, so tests are deterministic.
- No panic in any code path reachable from a user-supplied URL. All of those are
  `Result`.