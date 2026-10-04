<!--
  The shape of this template is the shape of a review.

  Most pull requests are rejected for one of three reasons: they cross the scope
  boundary without noticing, they cannot be told apart from a larger change, or
  nobody can find out what was actually tested. Those are the three questions at
  the top, in the order that catches them earliest.
-->

## What this changes

<!-- One or two sentences. If the title does not say it, say it here. -->

## Why

<!--
  The problem, not the solution. A diff already shows the solution.

  If this touches the resolver registry, the resume logic, or the queue state
  machine, say explicitly what could go wrong for a user whose file is silently
  corrupted. Those three are where a subtle bug is expensive.
-->

## How this was tested

<!--
  Be specific: `cargo test -p ifami-core resume`, not "tests pass".

  If you only exercised the happy path, say so. That is useful information and we
  would much rather have it here than discover it in review.
-->

## What was **not** tested

<!--
  Required. "Platforms I did not check", "the case I knew was broken and left
  alone", "I only ran the unit tests and not the loopback ones".
-->

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] I read [`docs/SCOPE.md`](../docs/SCOPE.md) and this does not cross any boundary in it
- [ ] This change needs no new dependency, **or** I justified it in the description
      (the dependency surface is a security boundary here, not a convenience)
- [ ] `ifami-cli` still contains no logic beyond argument parsing and formatting
- [ ] Any public item in `ifami-core` has a doc comment that explains *why*
- [ ] I added or updated an ADR if this changes a decision, rather than only the code
- [ ] I updated `CHANGELOG.md` under `[Unreleased]`

<!--
  If any box cannot be ticked, that is fine and normal -- leave it unticked and say
  why underneath. An honest partial PR is far more useful than one that claims
  completeness it does not have.
-->