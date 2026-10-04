# ADR-0007: Integration tests run against a loopback HTTP origin

- **Status:** Accepted
- **Date:** 2026-10-01
- **Decides:** `crates/ifami-core/tests/`, `CONTRIBUTING.md`

## Context

The engine's entire value proposition is in the parts that unit tests with a
scripted `HttpClient` cannot reach:

- Does a `206` actually get parsed the way RFC 9110 says?
- When a server drops the connection halfway, does the error surface at the right
  point, and are the bytes already written still on disk?
- Does a `200` reply to a ranged request get caught, or does it silently produce
  a double-length file?
- Does anything bind a routable interface?

None of that is exercised by handing a resolver a `RawResponse` in memory. The
framing, the streaming, the mid-stream error, and the socket are all stubbed out
by such a test, and those are precisely the layers where the bugs live.

The obvious fix is to hit real URLs. That fails immediately: the tests would be
flaky, slow, dependent on the public internet, and would require handling content
we have deliberately refused to support.

## Decision

Integration tests run against a hand-rolled HTTP/1.1 origin in
`crates/ifami-core/tests/common/mod.rs`, bound to `127.0.0.1` on an ephemeral
port, driven through the production `ReqwestClient`.

Properties the fixture guarantees:

- **Loopback only.** `assert_loopback` panics if the bound address is not a
  loopback address. A routable listener in a test suite is a security
  regression, not a convenience.
- **No public network.** Every URL in an integration test is built from
  `Fixture::url`, which can only ever produce a loopback address.
- **Scriptable failure modes.** A route can advertise `Accept-Ranges` and then
  ignore `Range`, or declare a full `Content-Length` and send half the body
  before dropping the connection. Those two are the failure modes that corrupt
  user files, and they are the ones worth a test.
- **A request log.** Tests assert on which requests were made, so "did it resume
  or silently restart?" is answerable rather than assumed.

The server is hand-written rather than taken from a framework. That is the part
that looks like needless effort. The reason is that a framework would own the
exact behaviour under test: a bug in our framing assumptions would hide behind
the framework's correct implementation, and the test would pass. ~300 lines of
`tokio::net` is cheaper than that ambiguity.

## Consequences

**Resume is genuinely tested.** `a_transfer_cut_mid_stream_resumes_to_the_correct_bytes`
asserts the final file is byte-identical to the source *and* that some request
asked for a non-zero offset, so a client that quietly restarted from zero cannot
pass by accident.

**Origin-ignores-Range is genuinely tested.** The `.part` file is pre-seeded with
the wrong bytes and the final output is compared byte-for-byte, which is the only
assertion that distinguishes a restart from a concatenation.

**TLS is not covered.** The fixture is plaintext, because a test certificate
authority is a supply-chain dependency we would rather not take on, and a
self-signed-cert test proves less than it appears to. `native-tls`/Schannel is
exercised by any real use, and its absence here is a known gap rather than an
oversight.

**One process per test binary.** Each integration test file is its own binary and
binds its own ephemeral port. There is no shared global server, so tests cannot
interfere and `cargo test` stays parallel-safe.

**The fixture is test-only.** It lives in `tests/`, not in the library, so it
cannot be reached by anything we ship.