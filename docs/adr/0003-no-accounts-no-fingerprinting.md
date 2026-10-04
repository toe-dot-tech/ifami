# ADR-0003: No accounts, no telemetry, no fingerprinting

- **Status:** Accepted
- **Date:** 2026-10-01

## Context

An earlier design proposed:

- 7 free downloads per day, after which an account is required.
- Unlimited downloads once signed up.
- Identification of users by machine UUID and browser fingerprint, so that a user
  cannot evade the limit by using another browser, a proxy, or a private window.

This was evaluated on correctness, policy, and strategy. It fails all three.

**It does not work.** A machine UUID (for example `Win32_ComputerSystemProduct`),
a MAC-derived identifier, and a browser fingerprint are all spoofable, and all
unstable across OS reinstall, hardware replacement, virtual-machine cloning, and
ordinary multi-device use. The limit fails *open*: it penalises users who never
intended to evade anything, while providing no obstacle to anyone who does.

**It breaks platform policy.**

- Chrome Web Store Limited Use prohibits using user data for personalised
  advertising and prohibits sharing user data with advertising platforms, data
  brokers, or information resellers.
- Microsoft Store policy 10.10.1 requires respecting the user's advertising-ID
  setting. A cross-context hardware identifier cannot respect a per-user toggle.
- Microsoft Store policy 10.5.2 and 10.5.3 require express opt-in consent, and an
  in-product mechanism to withdraw it, before publishing personal information
  off-device. A permanent identifier for users who never consented has neither.

**It is strategically incoherent for an open-source project.** In a public
repository, fingerprinting is not a moat, it is documentation. The source of the
identifier scheme is public, so it can be trivially forked away, and every user
who reads the README learns precisely how they are tracked. It also creates a
standing de-anonymisation liability: a breach of a database mapping hardware
identifiers to browsing and download history would let a third party
de-anonymise the entire user base.

**It contradicts the product's own claim.** Ifami's differentiator against the
existing downloader sites is that it does not profile you. Shipping a hardware
identifier while marketing "no account needed" would make that claim false.

## Decision

Ifami has:

- No accounts and no login.
- No user identifier of any kind, stored or derived.
- No hardware collection: no machine UUID, no MAC, no disk serial, no BIOS data.
- No browser fingerprinting and no cross-browser or cross-context correlation.
- No telemetry and no analytics. Errors are not reported to us.
- No rate limit tied to identity. The only backpressure is the concurrency
  limit the user controls.
- No advertising.

## Consequences

- Nothing to protect, and nothing to lose, in a breach. This is the single
  largest reduction in expected cost available to this project.
- The product cannot monetise by converting anonymous users into identified
  users, because it cannot identify them. Accepted.
- Positioning becomes coherent: privacy-first is the product, not a privacy
  policy attached to a growth funnel.
- Any future request to "add analytics" is a rejection of this ADR and requires
  a new one, argued on its merits. That is the intent.