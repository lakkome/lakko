<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0019: Client-authorised transparency-log leaf (G-30 #2)

- **Status:** Accepted — **built + reviewed**. Sprint 1 (rotation chain
  verifier FFI + 3-client gate-2 wiring) and Sprint 2 (genesis self-auth
  primitive + transparency server verify/reject + discovery opacity threading +
  3-client genesis mint) both landed end to end. `lakko-security`:
  **APPROVE-WITH-NOTES** — invariants #2/#4/#6 hold or are strengthened, the
  genesis-TOFU squatting residual is correctly bounded + documented; the one
  cosmetic note (a duplicate `fuzz-check` line) is fixed; cleared to merge.
  2026-06-12
- **Date:** 2026-06-12
- **Context:** The KT log (ADR-0016 supersession transport + the KT-log S1–S4
  workstream: `services/transparency`, the discovery append + reconciler, the
  3-client `verifyIdentityInLog` verify, and the S4 fresh-start `is_reset` UX)
  records each account's `(account_key -> identity_pubkey -> version)` binding
  in an operator-signed, append-only Merkle log. But the binding is **asserted
  by the discovery service**: discovery does a service-secret-authed
  `POST /transparency/append`, and the operator signs whatever it is handed. The
  client verify checks only the operator signature + Merkle inclusion + compares
  the log's latest key to what discovery served — it never proves the entry was
  **authorised by the account's own key**. This is G-30 finding 2 (the
  append-binding trust gap, `docs/security/known-gaps.md`). This ADR makes the
  binding cryptographically account-authorised.

## Threat

A malicious / compromised discovery service — or a leaked
`LAKKO_TRANSPARENCY_APPEND_SECRET` — can write an arbitrary
`(account_key, pubkey, version)` leaf: substitute a victim's key, forge a
rotation, or squat a never-used account. The operator (a separate zero-trust
tier) co-signs it because it has no way to tell an account-authorised binding
from a fabricated one. Today's client gate catches a *substituted current key*
(it diverges from the local pin and lacks a valid supersession proof, ADR-0016
fail-closed) but does NOT anchor the rotation to the log: a holder of a **leaked
old key** can mint several valid supersession proofs and present a different
successor to different peers (equivocation), and the log offers no account-bound
record to detect it.

## Decision

Make every log leaf carry an **account authorisation** that a client verifies,
so the log entry is self-certifying rather than discovery-asserted. Two cases,
split into two sprints — **both in scope**:

### Sprint 1 — rotations (reuse the built chain verifier; server untouched)

`verify_supersession_chain`
(`crates/lakko-transparency/src/supersession.rs`) already implements the full
rotation chain: operator-root signature -> no-rollback vs the peer's last-seen
root -> Merkle inclusion -> entry `username_hash` matches the pin -> version
strictly newer -> the supersession proof is signed by the pinned **old** key,
fresh, ≤30-day window, and authorises **exactly** the entry's new key. It was
built + unit-tested but never FFI-exposed or called. The supersession proof it
consumes is already minted on recover (ADR-0016) and published in the identity
bundle.

Sprint 1 therefore needs **no new crypto, no server change, no migration**: it
FFI-exposes `verify_supersession_chain` (UniFFI + wasm, a flat-parameter wrapper
mirroring `verify_log_inclusion`; the `last_seen_root` is reconstructed from the
client's stored last-seen `log_size`, since `check_no_rollback` reads only that)
and wires it into the client verify path. On a key **change** with the log
configured, the client runs the chain over `{ bundle supersession_proof, /root,
/latest, /proof, pinned (old_pubkey, version, last_seen_log_size) }` BEFORE
accepting the rotation, then re-pins to the authorised `(new key, new version)`.
A chain failure is fail-closed (typed refusal). When the log is unconfigured the
client keeps today's standalone gate-1 path.

**What this buys:** a rotation is now bound to a SINGLE committed log version
under a no-rollback signed root — one monotonic successor per version, plus a
permanent auditable record, with the peer re-pinning the chain-confirmed version
as its next downgrade floor. This makes the **leaked-old-key equivocation** above
**detectable** (not prevented): two distinct successors cannot both be
log-confirmed at the same version, so the second peer to fetch sees a
version-floor or inclusion failure → surfaced (`mismatch`) for an OOB
safety-number re-verify. Detection depends on at least one honest fetch + that
OOB backstop. (Honest scope: it does not defend a live-compromised *current* key
— that key can sign anything; the backstop there is likewise the OOB re-verify.)

### Sprint 2 — genesis (version 1) self-authorisation + server-side reject

The chain covers rotations (version > pinned), not the first entry. Sprint 2
adds a genesis-leaf authorisation:

- **`crates/lakko-transparency`:** `sign_leaf_genesis(seed, username_hash,
  version, pubkey)` -> an Ed25519 signature over `LogEntry::canonical_bytes`, and
  `verify_leaf_genesis(sig, leaf)` (the signature verifies against the leaf's
  **own** pubkey). FFI-exposed + fuzzed.
- **`services/transparency`:** a `leaf_auth` sidecar column (opaque); the
  `append` handler verifies the authorisation (genesis: self-sig over the leaf;
  rotation: the supersession proof against the *previous* entry's pubkey) and
  **rejects** an unauthorised append. This is defence-in-depth — the **client
  chain stays the real gate** since the server is untrusted — but it stops a
  leaked-secret attacker from polluting the log and keeps it audit-clean.
- **`services/discovery`:** a `leaf_auth` column on `identity_bundles`
  (COALESCE-preserved like `supersession_proof`), threaded through upload ->
  `append_key_change` and the reconciler.
- **Clients:** mint the genesis self-sig on the first-ever publish; thread the
  new optional wire field; verify the genesis sig on a first observation.

**Genesis authorisation model: self-signature TOFU.** The new key signs its own
genesis leaf. First-entry squatting (an attacker with the append secret writing
a version-1 leaf for a victim's account_key with the attacker's *own* key — a
self-consistent self-signature) is **detected, not prevented**: via CONIKS
self-monitoring (the owner's client audits its own entry and alarms on a foreign
version-1 key) + the existing ADR-0016 fail-closed peer gate. This matches the
system's TOFU-everywhere posture, couples to nothing, and reuses Ed25519. The
residual is documented.

**Alternative considered — auth-attested genesis:** `services/auth` (the only
party that authenticated the account at OPAQUE signup) co-signs
`(account_id -> first pubkey)`; the operator rejects an unattested genesis, so
squatting is *prevented*. Rejected for now: it couples transparency<->auth and
adds an attestation key + endpoint (the `lakko-attest` / ADR-0015 shape). It is
recorded here as the hardening to elect if first-entry squatting becomes a real
concern.

## Consequences

- **Invariant #2 (server sees only ciphertext + public artefacts) holds.** The
  supersession proof and the genesis self-sig are public artefacts signed by the
  account's *identity* key; they carry no message/chain/root/media key material.
  The server stores them opaque (the `leaf_auth` / `supersession_proof` columns
  are never parsed for key material).
- **Invariant #4 (no silent downgrade) holds.** The client chain verify is the
  real gate and is fail-closed; the server-side append reject is defence-in-depth
  on top, never a substitute.
- **Invariant #6 (identity rotation only via a KT-log supersession proof) is
  strengthened:** a rotation is now anchored to a committed, non-equivocable log
  version, not merely a point-to-point proof.
- `account_id` is a random UUIDv7 (16 bytes) — not enumerable — but public via
  `lookup`; the squatting residual above is bounded by that, not by `account_id`
  secrecy.

Gate this ADR + the Sprint-1 FFI/chain wiring through `lakko-security` before
merge (maintainer-invoked).
