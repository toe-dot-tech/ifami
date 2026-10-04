# Security Policy

## Scope

Ifami is a local program that fetches files from the internet on your behalf.
It handles three classes of untrusted input:

1. **Manifests and playlists** — XML (DASH MPD), text (HLS playlists), and
   HTML (media pages) parsed from remote servers.
2. **Response headers and bodies** — including `Content-Length`,
   `Content-Range`, and redirect chains, all attacker-influenced.
3. **Local queue files** — user-editable JSON on disk.

If you find a way to make Ifami do something the user did not request, read a
file it should not, or write outside the destination directory, we want to know.

## What we consider in scope

- Path traversal via a manifest-supplied filename or segment URI escaping the
  destination directory.
- Unbounded memory allocation driven by a hostile `Content-Length`, a hostile
  segment count, or a hostile playlist.
- Parsers that panic, loop forever, or recurse without bound on malformed input.
- SSRF through the relay or local bridge binding a routable interface.
- Resume logic that silently produces a corrupt or truncated file and reports
  success.
- Credentials supplied for the user's own authenticated content being sent to
  any origin other than the one they belong to.

## What is out of scope

- **Circumventing access controls.** Finding a way to make Ifami decrypt DRM,
  bypass a paywall, solve an attestation challenge, or impersonate a browser's
  TLS fingerprint is not a vulnerability; it is a feature we have permanently
  declined. See [docs/SCOPE.md](docs/SCOPE.md) and
  [ADR-0005](docs/adr/0005-scope-boundary.md).
- Running Ifami against content you do not have the right to download.
- Vulnerabilities in `rustc`, `tokio`, `reqwest`, or any other upstream
  dependency with no demonstrated impact in Ifami. Report those upstream. We do
  run `cargo audit` in CI and will upgrade on report.
- Social engineering of the user into running a malicious binary.
- Findings that require an attacker who already controls the user's machine.

## Reporting

**Please do not open a public issue for a security vulnerability.**

Report privately via GitHub's **"Report a vulnerability"** button on the
Security tab of the repository, which opens a private advisory.

Please include:

- What you did, step by step.
- The source URL or manifest, if it is publicly reachable. A minimal
  reproduction manifest is far more useful than a description.
- What you expected, and what happened.
- Your Ifami version (`ifami --version`) and Windows version.

We will acknowledge within 72 hours, aim to have an assessment within 7 days,
and will keep you updated until the report is resolved. We will credit you in
the advisory unless you ask us not to.

Please give us a reasonable opportunity to ship a fix before disclosing
publicly. We will not pursue legal action over good-faith research that follows
this policy.

## Our security posture

This is what we do, and what a reviewer can verify:

- `unsafe` is `forbid`-denied in both of our crates
  (`[lints.rust] unsafe_code = "forbid"`). The dependencies we pull in are the
  risk, and `cargo deny` gates that.
- `cargo audit` and `cargo deny` run as required CI checks.
- The desktop app is code-signed, so users can verify the publisher.
- We ship **no** update mechanism that could push code to your machine. You
  update by updating your source, which is what an open-source project should do.
- We have no server, so there is no database to breach and no key to steal. The
  blast radius of compromising "our infrastructure" is empty.

## Scope boundary in code

The boundary described in [docs/SCOPE.md](docs/SCOPE.md) is enforced
structurally, not by convention:

- `ResolveError::AuthRequired` and `ResolveError::DrmProtected` are terminal.
  They are not retryable, and there is no code path that converts them into a
  different error.
- There is no decryptor, key-extraction routine, or attestation-token handler in
  the tree. A reviewer can confirm this with a grep.
- `HttpClient` is a trait seam, and the resolvers can only fetch through it. A
  resolver cannot open a raw socket.

If you find a route from a manifest to a socket that bypasses these properties,
that *is* a vulnerability, and we want it.