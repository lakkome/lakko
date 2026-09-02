<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0001: MLS (RFC 9420) for every conversation type

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Phase 0

> **AMENDED 2026-09-02 (documentation audit).** The core decision — MLS for
> every conversation type — holds and is what shipped. **Two sub-decisions below
> did not survive contact with the implementation and were never recorded as
> changed.** Both are marked in place; the original reasoning is kept because it
> is still the reasoning, and in one case it is the reasoning that was
> *answered differently* rather than abandoned.
>
> - **Credentials: `BasicCredential`, not X.509.** See § Identity.
> - **The default ciphersuite named here does not exist in RFC 9420.** See
>   § Ciphersuites.
>
> Everything else checked in that audit — the two-tier channel model, the
> `mls_exporter("lakko/call/v1", call_id, 32)` call-key derivation, the
> DeviceGroup multi-device model, and OpenMLS as the implementation — matches
> the code.

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
- ~~`MLS_256_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`~~ **`MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`** — fast default path

> **Corrected (2026-09-02).** The X25519/ChaCha20 suite named here is **not a
> registered RFC 9420 ciphersuite** — there is no `MLS_256` X25519 suite in the
> IANA registry. The real one, and the one pinned in
> [`crates/lakko-mls/src/ciphersuite.rs`](../../crates/lakko-mls/src/ciphersuite.rs)
> as `ciphersuite::DEFAULT`, is **suite ID 3**,
> `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`. This is a naming
> error, not a security downgrade: the `128`/`256` prefix denotes the MLS
> *symmetric security tier*, not a key length — suite 3 uses a 256-bit
> ChaCha20 key and Ed25519 throughout. `FIPS` (suite ID 5) is unchanged and
> matches. `is_supported()` accepts exactly these two and rejects every other
> registered suite, so "exactly two ciphersuites" holds as decided.

**Identity** — Ed25519, ~~MLS X.509 credentials with a self-signed root~~ **MLS `BasicCredential`** + public transparency log (CONIKS-style, draft `key-transparency`).

> **Reversed in practice (recorded 2026-09-02).** `lakko-mls` uses OpenMLS's
> **`BasicCredential`** on every path
> ([`identity.rs`](../../crates/lakko-mls/src/identity.rs) — `BasicCredential::new(name)`
> for a named identity, and `BasicCredential::new(Vec::new())` — an *empty*
> credential — for the account identity). No X.509 credential, no self-signed
> root, and no certificate chain exists anywhere in the tree.
>
> **The alternative this ADR rejected is what shipped**, and the reason the
> rejection stopped applying is worth keeping rather than deleting: X.509 was
> chosen as *"the extensibility hook we need for a CONIKS-style transparency
> log without custom protocol fields."* The transparency log was built —
> `crates/lakko-transparency` + `services/transparency`, hardened by
> [ADR-0019](./0019-client-authorised-log-leaf.md),
> [ADR-0020](./0020-witness-cosigning.md),
> [ADR-0036](./0036-auth-attested-genesis.md) and
> [ADR-0039](./0039-kt-log-witness-quorum.md) — but **entirely out of band**:
> its leaf binds `(username_hash, pubkey, version)` published through
> `services/discovery`'s identity bundle, and it never reads an MLS credential.
> So the hook X.509 was carried for was never needed, and the credential field
> stayed empty rather than becoming the identity carrier.
>
> The consequence to be aware of, since it now rests on convention rather than
> on the credential: **the authenticated identity of a member is its Ed25519
> signature public key**, not anything in its credential. Every gate built
> since keys on that — [ADR-0022](./0022-group-admin-roles.md)'s role table,
> C-63's `owner_survives`, C-64's committer rank, C-68's message-author gate.
> An empty `BasicCredential` makes that unambiguous, which is a defensible
> outcome; it is simply not the one this ADR decided.

**Multi-device** — each device = a separate leaf in the user's DeviceGroup (their personal MLS group). Adding / removing a device = a commit in DeviceGroup → fan-out as Add/Remove proposals into every conversation group.

**Implementation:** [OpenMLS](https://github.com/openmls/openmls) (the only mature Rust implementation); the `lakko-mls` crate wraps it, pins ciphersuites, and isolates upstream API churn. Budget 1 FTE for upstream contributions / fork.

## Alternatives

### Signal Double Ratchet + MLS only for groups

Classic for 1×1 (forward secrecy + post-compromise security battle-tested for a decade). Downsides: two codebases, two sets of invariants, doubled audit surface, no PCS ratcheting at the 1×1 layer when the device set changes.

**Rejected:** a single protocol everywhere is significantly easier to audit and maintain; modelling 1×1 as a degenerate group gives a cleaner multi-device model.

### MLS only, no Double Ratchet, but without X.509 credentials (BasicCredential)

Minimalist. Downsides: no built-in revocation, no extensibility for a transparency log without protocol changes.

**Rejected:** X.509 credentials provide the extensibility hook we need for a CONIKS-style transparency log without custom protocol fields.

> **This rejection did not hold (2026-09-02).** `BasicCredential` is what
> shipped — see the note under § Identity. The stated downside turned out not
> to bite, because the transparency log was built beside MLS rather than
> through it: the log leaf is published via `services/discovery`'s identity
> bundle and binds `(username_hash, pubkey, version)`, so it needs no
> credential extensibility and no custom MLS protocol field. Revocation is
> likewise handled outside the credential — by MLS `Remove` (membership) and
> by the [ADR-0016](./0016-identity-supersession-transport.md) supersession
> proof plus the KT log (identity). The alternative was rejected for a cost
> that a different architecture removed, not for a cost that was paid.

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
- ~~ADR-0003 — SFU and calls~~ → [ADR-0011](./0011-rust-sfu.md) (the Rust SFU;
  [ADR-0003](./0003-livekit-fork.md) is superseded, though its SFrame +
  MLS-exporter parts — the ones this ADR depends on — carried through unchanged)
- [ADR-0029](./0029-mid-call-key-rotation.md) — the "rotation on Add/Remove with
  a 1-second overlap" named above, as built (`lakko_calls::CallKeyRing`)
- [ADR-0035](./0035-sframe-per-stream-kid-key.md) — the per-`(leaf, stream)`
  refinement of the SFrame key derived from that exporter secret
