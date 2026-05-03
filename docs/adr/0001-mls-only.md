<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0001: MLS (RFC 9420) for every conversation type

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Phase 0

## Context

Lakko must provide E2EE by default for:

- 1×1 chats
- Group chats up to ~50,000 participants
- Channels with millions of subscribers
- Audio / video calls (P2P, group, broadcasts, audio rooms)

We need a single crypto protocol with formal verification, industry support, and efficient group operations.

## Decision

**MLS (RFC 9420)** as the single E2EE protocol for everything:

- 1×1 = degenerate MLS group of 2 (no separate protocols like X3DH / Double Ratchet)
- Groups ≤ 50,000 — vanilla MLS, ratchet tree O(log n)
- Channels > 50,000 — two-tier: vanilla MLS PublisherGroup (≤500 owners + moderators) + sender-key broadcast to subscribers via HPKE envelopes
- Calls — media keys via `mls_exporter("lakko/call/v1", call_id, 32)` → SFrame (RFC 9605); rotation on Add/Remove with a 1-second overlap

**Ciphersuites:**

- `MLS_256_DHKEMP384_AES256GCM_SHA384_P384` — FIPS-friendly path
- `MLS_256_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` — fast default path

**Identity** — Ed25519, MLS X.509 credentials with a self-signed root + public transparency log (CONIKS-style, draft `key-transparency`).

**Multi-device** — each device = a separate leaf in the user's DeviceGroup (their personal MLS group). Adding / removing a device = a commit in DeviceGroup → fan-out as Add/Remove proposals into every conversation group.

**Implementation:** [OpenMLS](https://github.com/openmls/openmls) (the only mature Rust implementation); the `lakko-mls` crate wraps it, pins ciphersuites, and isolates upstream API churn. Budget 1 FTE for upstream contributions / fork.

## Alternatives

### Signal Double Ratchet + MLS only for groups

Classic for 1×1 (forward secrecy + post-compromise security battle-tested for a decade). Downsides: two codebases, two sets of invariants, doubled audit surface, no PCS ratcheting at the 1×1 layer when the device set changes.

**Rejected:** a single protocol everywhere is significantly easier to audit and maintain; modelling 1×1 as a degenerate group gives a cleaner multi-device model.

### MLS only, no Double Ratchet, but without X.509 credentials (BasicCredential)

Minimalist. Downsides: no built-in revocation, no extensibility for a transparency log without protocol changes.

**Rejected:** X.509 credentials provide the extensibility hook we need for a CONIKS-style transparency log without custom protocol fields.

### A custom protocol based on academic work

Design freedom. Downsides: years of formal verification, no interop, full ownership of any crypto bug.

**Rejected:** we do not roll our own crypto protocols. An IETF standard is better.

## Consequences

### Positive

- One protocol for 1×1 / groups / channels / calls → one audit, one state machine, one interop toolchain
- IETF standard RFC 9420 → industry-wide interop is possible (with other MLS implementations: mlspp, libmls, …)
- Efficient group operations (O(log n) Add/Remove) up to 50k participants
- A ready-made Rust implementation (OpenMLS) saves ~2 person-years

### Negative

- Channels > 50k subscribers are **not covered** by vanilla MLS → we need a two-tier setup with sender-key broadcast → loss of backward secrecy for channel content (documented publicly)
- OpenMLS is pre-1.0 → breaking changes are possible; upstream contributions are required
- The MLS state machine is more complex than Double Ratchet for 1×1 → demands more rigorous testing
- watchOS cannot hold full MLS state for many groups (see ADR-0002)

### Neutral

- 1×1 ratchet has medium cost (vs Double Ratchet) — acceptable on any 2026+ hardware
- KeyPackages need to be rotated regularly (TTL 30 days) — standard MLS behaviour

## Open questions

- Migration strategy to the full MLS `mls-large` draft once it is standardised — affects the channel scheme
- Federation between Lakko instances — later, not in Phase 0-2
- Post-quantum — MLS extensions for ML-KEM are being worked on at IETF; track and migrate once stable

## References

- [RFC 9420 — The Messaging Layer Security (MLS) Protocol](https://www.rfc-editor.org/rfc/rfc9420)
- [RFC 9605 — Secure Frame (SFrame)](https://www.rfc-editor.org/rfc/rfc9605)
- [draft-ietf-keytrans-architecture (Key Transparency)](https://datatracker.ietf.org/doc/draft-ietf-keytrans-architecture/)
- [OpenMLS](https://github.com/openmls/openmls)
- ADR-0002 — client architecture
- ADR-0003 — SFU and calls
