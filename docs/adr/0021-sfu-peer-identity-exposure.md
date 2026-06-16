<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0021: SFU peer-identity exposure for group-call roster names

- **Status:** Accepted
- **Date:** 2026-06-15
- **Context:** Phase 3 (Rust SFU group calls)

## Context

A group call's roster needs to show each remote participant by a
human-meaningful name. The SFU is zero-trust and opaque (invariant #2):
it holds no display names and must not learn the social graph
(invariant #5). Today `services/sfu` gives each peer a
`account_pseudonym = pseudonymise(account_id)` where
`pseudonymise(id) = id[..8] + "…"` (`services/sfu/src/signaling.rs:117`),
and the clients render that short tag verbatim ("Peer" / a pseudonym
slice).

Two problems make this unusable as an *identity* the client can resolve
to a contact name:

1. **Collision-prone.** The account_id is a 16-byte UUIDv7; its first
   8 hex chars are the high bits of the millisecond timestamp, so
   prefixes cluster by account-creation time. Matching on the prefix is
   ambiguous — correctness depends on luck.
2. **Leaky.** The prefix partially discloses the raw account_id.

Even setting those aside, **there is no local map** from the tag to a
contact: `AcceptedContact` is keyed on `signaturePubkeyB64` and carries
`displayName` but **not** account_id; `PeerPinStore` (ADR-0016) is keyed
on account_id but holds an AccountIdentity-pubkey pin, not a name.

The 1×1 path is unaffected — the sole remote IS the chat's contact, so
the client already shows `entry.label` (fixed alongside this work). This
ADR is only about the **group** roster.

**Load-bearing facts (verified in the code):**

- The call-admission JWT minted by `services/auth`
  (`crates/lakko-call-jwt`, `CallJwtClaims`) carries `sub = account_id`
  (16-byte UUIDv7 hex) and is **the SFU's only authenticated peer
  input** — the SFU never sees the account-identity pubkey.
- `services/auth` (OPAQUE, no-PII) does **not** hold the
  account-identity Ed25519 pubkey; that key lives in the discovery
  identity-bundle store, and its binding to the account_id is owned by
  the KT log / discovery (ADR-0016), not auth.
- A contact's `AcceptedContact.signaturePubkeyB64` **is** that contact's
  AccountIdentity Ed25519 pubkey (a `ContactRequest` is signed by
  `Identity::from_seed(accountIdentitySeed)`, ADR-0017).
- A contact's account_id **is** known transiently at accept time (the
  contact flow resolves `@username → account_id` via discovery,
  `apps/web/src/contacts/client.ts`), but is not persisted.

**Forces:** privacy (do not leak account_id; ideally unlinkable across
calls) · correctness (collision-resistant) · **security** (spoof-proof —
a malicious participant must not make the victim's roster show a
*contact's* name for the *attacker*) · feasibility (the SFU's only
trusted input is the JWT-verified account_id).

## Decision

The SFU replaces the truncated pseudonym with a **per-call keyed peer
tag**

```text
peer_tag = BLAKE3("lakko/sfu/peer-tag/v1" ‖ call_id ‖ account_id)   // truncated, hex
```

derived over the **JWT-verified** `account_id` (`sub`) and the `call_id`.
The client resolves a roster `peer_tag` to a contact by computing the
**same** `BLAKE3("lakko/sfu/peer-tag/v1" ‖ call_id ‖ contact.account_id)`
over each of its contacts' persisted account_ids and matching: a unique
hit → that contact's `displayName`; zero or multiple hits → the short
tag hex (or "Participant N"). To make the match possible,
`AcceptedContact` gains a persisted `accountIdHex` (captured at accept
time, where the account_id is already in hand); pre-existing contacts
without it degrade to the tag until re-resolved.

The derivation is a single canonical Rust function (home: `lakko-sfu-proto`
or a small shared helper) exposed via FFI (uniffi + wasm), so the SFU and
all three clients compute byte-identical tags — golden-pinned, the same
posture as ADR's call safety number.

**Why this shape:**

- **Spoof-proof.** The SFU derives the tag from the JWT-authenticated
  `sub`, so a participant can only ever present *their own* account_id's
  tag. An attacker cannot forge a contact's tag without being that
  account (which would require that account's auth credentials).
- **Non-leaking.** One-way BLAKE3; the account_id never crosses the
  wire. The SFU computes a hash of an already-opaque identifier and
  learns no names (invariant #2 preserved).
- **Per-call unlinkable.** Keying on `call_id` yields a fresh tag each
  call, so a recurring set of peers cannot link an account across calls
  (strengthens invariant #5 vs. the current stable prefix).
- **Collision-resistant.** A full (truncated-to-N-byte) hash, not a
  timestamp-clustered UUIDv7 prefix.

## Alternatives

1. **Expose the account-identity Ed25519 pubkey; the client matches it
   against the existing `signaturePubkeyB64` (no contact-schema
   change).** Rejected: the SFU's authenticated input is the account_id,
   not the pubkey; the JWT minter (auth) does not hold the pubkey, and
   the account_id→pubkey binding is owned by the KT log / discovery.
   Exposing a *self-asserted* pubkey is spoofable (a malicious peer
   claims a contact's pubkey → the victim's roster shows the wrong
   name). Authenticating it would force auth to vouch for a binding it
   does not own — disproportionate cross-service + KT-trust work.
2. **Stable hash `BLAKE3(account_id)` (no `call_id` key).** Rejected in
   favour of the per-call tag: a stable tag is linkable across calls (a
   recurring peer set recognises the same account returning). The
   per-call variant costs nothing extra (the client already has the
   call_id) and strictly improves privacy.
3. **Client-side prefix match on the existing truncated pseudonym** (no
   SFU change). Rejected: the 8-hex-char UUIDv7 prefix is
   timestamp-clustered → ambiguous; correctness is luck. The per-call
   full hash is collision-free for the same client cost (persist
   account_id).
4. **Do nothing — keep the pseudonym.** The zero-work, privacy-maximal
   fallback (the short tag is a legitimate privacy-preserving display).
   Rejected as the chosen direction because the maintainer wants real
   names, but recorded as the do-nothing baseline.

## Consequences

- **Positive:** real contact names in the group roster; collision-free;
  per-call unlinkable; spoof-proof; the server stays opaque and the
  social graph stays unmaterialised (invariants #2 + #5 preserved, the
  latter strengthened).
- **Negative:** a contact-schema migration (persist `accountIdHex`);
  pre-existing contacts need re-resolution (or a one-time discovery
  backfill) before their names appear; a small SFU change
  (`lakko-sfu-proto` peer-snapshot field + `signaling.rs` tag
  derivation) + a new FFI derivation + 3-client resolvers. The account_id
  is now persisted on the contact — but `PeerPinStore` already persists
  account_ids, so this is not a new privacy posture.
- **Neutral:** the tag replaces the truncated pseudonym on the SFU wire
  (a `lakko-sfu-proto` schema change — keep the old field until clients
  migrate, then drop).

## Implementation status (Jun 2026)

- **Derivation + SFU wire + FFI — landed.** `lakko_sfu_proto::peer_tag`
  (BLAKE3 `derive_key("lakko/sfu/peer-tag/v1", len-framed call_id ‖
  account_id)` → 8 bytes / 16 hex, golden-pinned `2151ad585010e90b`);
  `PeerSnapshot` + `ServerFrame::PeerJoined` carry a `#[serde(default)]`
  `peer_tag`; `services/sfu` derives it at both wire sites over the
  JWT-verified `account_id`; FFI exposes `sfu_peer_tag` / `sfuPeerTag` +
  the field on the projections.
- **Web vertical — landed end-to-end.** `AcceptedContact.accountIdHex`
  captured at accept; `Chat.tsx` precomputes a `peer_tag → displayName`
  map and the resolver matches a roster entry (collision → pseudonym,
  ADR-exact). Names render in web group calls.
- **`lakko-security` — APPROVE-WITH-NOTES.** Confirmed one-way /
  per-call-unlinkable / spoof-proof (tag bound to the JWT `sub`, no
  self-assertion) / no fail-open; 64-bit collision budget adequate
  (consequence is a benign wrong-name, not an auth break). Both `low`
  findings fixed (collision-falls-back-to-pseudonym +
  `#![forbid(unsafe_code)]` on `lakko-sfu-proto`).
- **Native (iOS + Android) — landed.** The tag is plumbed through the
  native call signaling, and the resolver is active on both platforms.
  Closing the prerequisite "No persistence yet" gap (for the accepted
  list) was part of this: a per-platform `AcceptedContactsStore`
  (iOS `UserDefaults` + `Codable`; Android `SharedPreferences` + a
  `@Serializable` base64 DTO) rehydrated on the `ContactsRepository` and
  saved on every accept, and `ChatViewModel.startGroupCall` precomputes
  the `peer_tag → label` map + a `resolveDisplayName` for `SfuCallUi`
  (collision → pseudonym, matching web). **Caveat:** the native label is
  the peer's short pubkey fingerprint (`shortFingerprintLabel`), the same
  identifier the contacts UI shows — iOS/Android contacts carry **no
  user-chosen display name** (unlike web), so the full "human name in the
  roster" payoff awaits editable native contact display names (a separate
  follow-up).

## Open questions

- Domain-separation context string + truncation length (collision budget
  is trivial for ≤ a few hundred participants; pick N so the hex stays
  short but birthday-safe). **Resolved:** 8-byte / 16-hex, `lakko/sfu/peer-tag/v1`.
- Backfill existing contacts' account_ids from discovery (one pass) vs.
  lazy re-resolution on next contact interaction.
- The native resolver shows the peer's short pubkey fingerprint, not a
  human name, because iOS/Android contacts have no user-chosen display
  name. Adding editable native contact display names is the follow-up
  that makes the native roster show real names like the web.
- The web backup/restore (`lakko-backup`) does not carry `accountIdHex`,
  so a restored contact loses it until re-accepted — a `lakko-backup`
  schema follow-up if round-trip name resolution matters.
- Channels (Phase 4) broadcast rosters are out of scope.

## References

- [ADR-0011](0011-rust-sfu.md) — the Rust SFU
- [ADR-0016](0016-identity-supersession-transport.md) — account_id as the
  stable per-contact key; `PeerPinStore`
- [ADR-0017](0017-phrase-seeded-identity.md) — AccountIdentity =
  `Identity::from_seed(phrase_seed)`; the contact-request signer
- `crates/lakko-call-jwt` — `CallJwtClaims.sub = account_id` (the SFU's
  authenticated peer input)
- `services/sfu/src/signaling.rs` — the current `pseudonymise`
- Security invariants **#2** (server sees only ciphertext + opaque
  artefacts) and **#5** (social graph not materialised) — flag for
  `lakko-security` before implementation (SFU wire + identity-adjacent
  display tag)
