# Contributing to Ifami

Thanks for considering it. This document exists because Ifami has opinions, and
you should learn what they are before you spend a weekend on a patch we cannot
merge.

## Read the scope first

**[docs/SCOPE.md](docs/SCOPE.md) is normative.** Please read it before you write
code, not after. It defines the hard boundaries. A patch that needs to cross one
of them will not be merged, and finding that out before you build the feature
saves both of us a lot of time.

The short version:

- We do not circumvent access controls. No DRM, no paywalls, no attestation
  tokens, no browser fingerprint impersonation, no executing a site's
  JavaScript.
- If a source needs any of those, it gets a terminal error. That is the correct
  answer, not a bug to be routed around.

**YouTube does not work, and we are not going to make it work.** If your patch
is about making it work, it will not be merged.

## Setup

```sh
git clone https://github.com/toe-dot-tech/ifami
cd ifami
cargo build
```

Requires stable Rust 1.88 or newer. The toolchain, including the `clippy` and
`rustfmt` components, is pinned in `rust-toolchain.toml`, so `rustup` installs
what you need the first time you run a cargo command here. The floor is
`rust-version` in the root manifest; if the two ever disagree, the manifest is
what is true.

<details>
<summary>Windows, if the build cannot find <code>link.exe</code></summary>

The MSVC build tools are not on a normal `PATH`, so a fresh machine fails at the
link step rather than the compile step. There is a helper for that:

```powershell
. .\scripts\msvc-env.ps1
cargo build
```

It reads the environment out of your Visual Studio install via `vcvars64.bat` and
sets it for the current shell only. It deliberately does not write to your
machine environment permanently — that change outlives the project and
eventually breaks something else.

</details>

## Before you open a PR

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

These are not suggestions; they are the CI gates. Notably:

- `ifami-core` sets `missing_docs = "deny"`. Every public item needs a doc
  comment that explains *why*, not *what*.
- `ifami-core` sets `unsafe_code = "forbid"`.
- All Clippy lints are denied.

A PR that fails any of these will not be reviewed for substance.

`cargo fmt` and `cargo clippy` are the same commands CI runs. There is no local
config that makes them pass here and fail there, and if you find yourself
wanting one, the fix belongs in the pull request.

## Code standards

### `ifami-core` holds the logic

This is the load-bearing rule of the codebase. `ifami-core` is a
platform-free engine: no UI, no framework, no global state, no ambient
filesystem paths. Everything interesting happens there, which is why it is
testable without a GUI and embeddable by other people.

**`ifami-cli` contains argument parsing and output formatting. Nothing else.** If
you want to add logic there, it belongs in `ifami-core` where it can be tested
against a scripted client and a loopback fixture server. The CLI being thin is a
feature, not an oversight.

### The desktop interface owns presentation only

Same rule, other side of the seam: `apps/desktop/src` may decide what a row looks
like and may format a speed for a human, but it must not decide what a download
*is*. Anything that can be wrong in a way a user would notice — retry, resume,
container detection, rate arithmetic, what counts as a terminal error — belongs
in `ifami-core`.

The practical test: if your JavaScript would still be correct if the Rust were
replaced by something else, it is presentation. If it encodes a rule about
transfers, it is not.

There is no build step and no framework. `node serve.mjs 8787` from
`apps/desktop` is the entire toolchain. Please keep it that way: a bundler and a
framework would add a supply-chain surface to a project whose pitch is that it
does not do things you did not ask for.

Every layout number quoted in the README was **measured**, not judged. If you
change the CSS, re-measure: the invariants are that the page never scrolls
horizontally at any width, that nothing sits dead between the last row and the
statusbar, and that adding a download does not move the toolbar, the column
labels, or the heading. Assert them in the browser rather than looking at them.

### Tests are not optional

- Unit tests live next to the code, in the same module, in a `#[cfg(test)]` block.
- Anything that touches HTTP goes through the `HttpClient` trait so tests can
  substitute a scripted client. Do not open a socket in a test; bind
  `127.0.0.1` instead.
- Do not mutate global state in a test. `std::panic::set_hook`, environment
  variables, and current-directory changes are all racy under the default
  parallel test runner and have bitten us already.
- Cover the wrong case. A parser test that only feeds valid input is not a test.
- Integration tests that need a server bind `127.0.0.1` on an ephemeral port.
  Never a public interface.

### Comments explain reasoning, not mechanics

```rust
// Bad: increments i
i += 1;

// Good: the inclusive-end span `0-999` is 1000 bytes, which is the whole
// reason Content-Range and mediaRange need different parsing paths.
let len = end - start + 1;
```

If a line of code is not obvious from the code around it, the reason it is
written that way belongs in a comment. This project is judged partly on whether
a stranger can audit it.

### Naming

Spell things out. `decide_resume`, not `dr`. `invariant_violation`, not `check`.

## Pull requests

- One logical change per PR. A refactor bundled with a feature halves the
  review.
- Say what you tested and what you did not. "I only exercised the happy path" is
  useful information and we would much rather have it than find out in review.
- If you are adding a dependency, justify it. The dependency surface is a
  security boundary here, not a convenience.
- If you touch the resolver registry, the resume logic, or the state machine,
  expect closer scrutiny. Those are the parts where a subtle bug corrupts a
  user's file silently.

## Reporting bugs

Open an issue with:

- The exact URL or manifest, and the command you ran.
- Your Ifami version and Windows version.
- What you expected and what happened, including the full error text.
- Your `ifami-queue.json`, **with any URLs or filenames you need to keep private
  removed.**

If it involves network behaviour, a packet capture or the raw response headers
are worth more than a description.

## Code of conduct

[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Licence

Contributions are accepted under the Apache-2.0 licence. See [LICENSE](LICENSE).

By contributing you confirm you have the right to license the code you submit,
and that it does not require us to incorporate a GPLv3+ dependency or to bypass
an access control.