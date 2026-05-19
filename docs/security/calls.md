<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Call-side cryptography

Audit-readiness fixation for Phase 3a–3c (per
[`docs/ROADMAP.md`](../ROADMAP.md) §"3a–3c — calls"). This
file captures the invariants the call media-key pipeline
must preserve. The 1×1 P2P path (`CallSession`) and the
group-call path (`GroupCallSession`) share the same MLS
exporter + SFrame keying primitives — what differs is the
signaling shape and the rotation choreography.

The reviewer is asked to verify each invariant against the
cited test. Property tests intentionally exercise the full
chain end-to-end (real MLS exporter, independent `Group`
handles per party — no shared-seed shortcuts).

## Crate layout

| Module | Responsibility |
|---|---|
| [`crates/lakko-calls/src/sframe.rs`](../../crates/lakko-calls/src/sframe.rs) | `CallKey` (32-byte seed from MLS exporter) + `ParticipantKey` (per-sender HKDF-derived key + nonce salt). Per-frame seal / open with 32-bit counter. |
| [`crates/lakko-calls/src/session.rs`](../../crates/lakko-calls/src/session.rs) | `CallSession` — 1×1 state machine (`Idle → Outgoing/Ringing → Connecting → Connected → Ending → Ended`). |
| [`crates/lakko-calls/src/group_call.rs`](../../crates/lakko-calls/src/group_call.rs) | `GroupCallSession` — multi-party ledger. Owns `call_key` + `previous_call_key` (overlap), participant roster, `needs_rekey` flag. |
| [`crates/lakko-calls/src/signaling.rs`](../../crates/lakko-calls/src/signaling.rs) | `SignalingBody` wire codec (`Offer`/`Answer`/`IceCandidate`/`End` for 1×1; `GroupStart`/`GroupJoin`/`GroupLeave` for group calls). |
| [`schemas/mls-app/call_signaling_v1.proto`](../../schemas/mls-app/call_signaling_v1.proto) | Proto source of truth for the wire format. |

## Threat-model context

The SFU is **not trusted** (per [`threat-model.md`](./threat-model.md)).
It sees only:

- A short-lived JWT scoped to `(call_id, participant_account_id)`
  signed by `services/auth` — no PII, no chat-group identifier.
- Encrypted SFrame frames + the minimum forwarding metadata
  (sender leaf index, frame counter, timestamp). Frame
  bodies are AEAD-sealed under per-participant keys the
  SFU never sees.

The SFU never gets:

- The MLS exporter seed (`lakko/call/v1`).
- Any per-participant `ParticipantKey` (key bytes or nonce
  salt).
- The chat MLS group membership or epoch.
- Plaintext SDP / ICE bodies (those ride MLS for 1×1 calls;
  for group calls per-peer SDP/ICE goes participant ↔ SFU
  through LiveKit signaling — no plaintext leaks because the
  SFU never has the media keys).

## Invariants the call layer MUST preserve

### C-1: Two parties in the same MLS group at the same epoch derive byte-identical `CallKey` seeds

The seed is the output of `mls_exporter(label =
"lakko/call/v1", context = call_id, length = 32)`. RFC 9420
§8.5 guarantees the exporter is a deterministic function of
the group state at the current epoch — both sides must
reach the same 32 bytes.

This is the foundational property: a divergence here means
no participant can decrypt any other participant's frames.

Test:
[`group_call::tests::cross_party_seal_open_round_trips`](../../crates/lakko-calls/src/group_call.rs)
asserts byte equality of `session_a.call_key()` and
`session_b.call_key()` after both sides build independent
`GroupCallSession`s from their own `Group` handles.

### C-2: Cross-party seal/open works end-to-end through the MLS exporter

Stricter than C-1: not only do the seeds agree, but the
HKDF-derived per-participant key plus the AEAD seal under
that key produces a ciphertext openable by the recipient
who re-derives the sender's key from its own `Group` view.

Test: same as C-1 (the test seals on A and opens on B
through `remote_participant_key(0)`).

### C-3: Per-participant keys differ across leaves

`participant_key(leaf_index)` MUST produce distinct key
bytes AND distinct nonce salts for distinct `leaf_index`
values. Otherwise an attacker who compromised a single
participant's key could trivially impersonate any other
participant.

Test:
[`sframe::tests::participant_keys_differ_across_leaves`](../../crates/lakko-calls/src/sframe.rs)
+ [`group_call::tests::participant_keys_differ_across_leaves`](../../crates/lakko-calls/src/group_call.rs).

### C-4: Three-party full-mesh seal/open holds

Every participant in an N-party call must be able to derive
every other participant's `ParticipantKey` from the shared
`CallKey` seed + the other's `leaf_index`. The SFU
forwarding path depends on the receiver re-deriving the
sender's key from the leaf index carried in the SFrame
header.

Test:
[`group_call::tests::three_party_full_mesh_seal_open`](../../crates/lakko-calls/src/group_call.rs)
— A, B, C each seal one frame; the other two open via the
sender's leaf index.

### C-5: Frame counter is strictly monotonic per sender per epoch

RFC 9605 §4.4 pins the counter to 32 bits. The sender-side
`ParticipantKey::seal` increments the counter once per
sealed frame and never reuses a `(key, nonce)` pair. After
counter exhaustion (2^32 frames ≈ 4 billion at any frame
rate this is years of continuous calling, but the bound is
honoured) the seal path returns `Error::CounterExhausted`
and the caller MUST trigger a fresh MLS commit on the
keying group → fresh CallKey → fresh ParticipantKey →
counter resets.

Test:
[`sframe::tests::counter_increments_strictly`](../../crates/lakko-calls/src/sframe.rs)
covers the monotonic increment;
[`sframe.rs`](../../crates/lakko-calls/src/sframe.rs)
documents the exhaustion path. A dedicated exhaustion test
is gap [G-17](./known-gaps.md#g-17) — too expensive to run
inside `cargo test`.

### C-6: AEAD AAD-binding holds across the cross-party path

The SFrame header bytes form the AAD in production. A
receiver attempting to open a frame with the wrong AAD MUST
get an authentication failure. This stops a header-tamper
attacker from re-routing or replaying frames.

Tests:
- [`sframe::tests::wrong_aad_rejects_open`](../../crates/lakko-calls/src/sframe.rs)
  — primitive level.
- [`group_call::tests::cross_party_wrong_aad_rejects`](../../crates/lakko-calls/src/group_call.rs)
  — full cross-party path.

### C-7: Wrong frame counter on the receive path is rejected

The receiver opens with the explicit counter the SFrame
header carries. A counter mismatch yields a distinct nonce,
which the AEAD authentication rejects.

Test:
[`sframe::tests::wrong_counter_rejects_open`](../../crates/lakko-calls/src/sframe.rs).

### C-8: MLS epoch advance rotates the `CallKey` seed

A commit on the keying group (Add / Remove of a participant
or a periodic forced rotation) advances the MLS epoch; the
next `refresh_call_key` re-derives the exporter and gets a
different 32-byte seed. This is the protocol-level forward
secrecy boundary for media: after a Remove, the removed
device cannot decrypt frames sealed under the post-commit
seed even if it kept its own pre-commit `ParticipantKey`.

Tests:
- [`group_call::tests::refresh_call_key_picks_up_new_epoch`](../../crates/lakko-calls/src/group_call.rs)
  — the new seed differs from the old.
- [`group_call::tests::cross_party_round_trip_survives_epoch_advance`](../../crates/lakko-calls/src/group_call.rs)
  — fresh frames decode under the new seed; frames sealed
  under the OLD seed cease to decode once the overlap
  window closes.

### C-9: The 1-second overlap window absorbs in-flight frames and ONLY in-flight frames

`refresh_call_key` stashes the prior `CallKey` into
`previous_call_key`; `try_open_with_overlap` tries the
current seed first, falls back to the previous seed on AEAD
rejection. The platform invokes `drop_previous_call_key`
when its own 1-second timer fires; after that the previous
seed is gone and frames sealed under it cease to decode.
The crate stays wall-clock-free — the timer lives in the
platform layer.

Tests:
- [`group_call::tests::overlap_window_accepts_stale_frame_until_dropped`](../../crates/lakko-calls/src/group_call.rs)
  — stale frame decodes during overlap, fresh frame decodes
  always, stale stops decoding after drop.
- [`group_call::tests::no_overlap_window_collapses_to_current_only`](../../crates/lakko-calls/src/group_call.rs)
  — outside an overlap window the fallback path is skipped;
  tampered frames reject with a `"no overlap window active"`
  hint so platform-side debug logs can differentiate "we
  missed a rotation" from "the wire is corrupted".
- [`group_call::tests::drop_previous_call_key_is_idempotent`](../../crates/lakko-calls/src/group_call.rs).

### C-10: Only the most-recent previous epoch is retained in the overlap slot

Chained rotation contract — a single `previous_call_key`
slot. Two rotations in rapid succession retain only the
most-recent previous seed; an older one is displaced. The
contract assumes ≤ 1 commit per second per call, which
matches the SFU's commit debouncing. Without this bound the
overlap-window memory footprint would grow with rotation
frequency.

Test:
[`group_call::tests::chained_rotations_retain_only_most_recent_previous`](../../crates/lakko-calls/src/group_call.rs)
— epoch-0 frame decodes during epoch-1 overlap; after
epoch-2 rotation displaces epoch-1, the epoch-0 frame no
longer decodes.

### C-11: The 1×1 call signaling dispatcher does NOT cross-feed group-control bodies into the state machine

`CallSession::apply_remote_signaling` silently drops
`GroupCallStart` / `GroupCallJoin` / `GroupCallLeave`
bodies — those route through `GroupCallSession::apply_remote_signaling`
instead. The discriminator `SignalingBody::is_group_control()`
is the canonical client-side dispatcher gate.

A bug where a group-control body advances the 1×1 state
machine would let a stranger in the chat MLS group (e.g. a
chat member who hasn't joined the 1×1 call) inject events
that nudge an unrelated 1×1 call's state machine. The
silent-drop closes that surface.

Test:
[`signaling::tests::is_group_control_false_for_one_on_one_bodies`](../../crates/lakko-calls/src/signaling.rs)
covers the discriminator;
[`group_call::tests::apply_remote_signaling_ignores_one_to_one_bodies`](../../crates/lakko-calls/src/group_call.rs)
covers the inverse (1×1 body into a group dispatcher is
also dropped silently).

### C-12: Wrong-`call_id` signaling bodies are dropped silently

A `GroupCallJoin` whose `call_id` doesn't match the active
`GroupCallSession` returns `SignalingOutcome::Ignored` and
leaves the ledger untouched. This stops a stranger in the
chat group from poisoning the active call's roster by
publishing a stale or fabricated `call_id`.

Test:
[`group_call::tests::apply_remote_signaling_ignores_wrong_call_id`](../../crates/lakko-calls/src/group_call.rs).

### C-13: `EndReason` parser folds unknown discriminator values to `HungUp`

Forward-compatibility hook. A future schema bump adding
reason codes 5..N lands gracefully on older receivers — the
call still terminates, just with the most-conservative
reason code (`HungUp`). This avoids the alternative
behaviour of a panic / decode error tearing the call down
mid-renegotiation.

Test:
[`signaling::tests::unknown_end_reason_folds_to_hung_up`](../../crates/lakko-calls/src/signaling.rs).

### C-14: `call_id` length is strictly enforced at parse time

Every `SignalingBody` variant carries a 16-byte `call_id`.
The parser rejects any other length with
`Error::Crypto("call_id length: expected 16, got N")` —
NOT a panic / index-out-of-bounds. This stops a
malformed-payload attacker from crashing the receiver.

Test:
[`signaling::tests::wrong_call_id_length_rejected`](../../crates/lakko-calls/src/signaling.rs).

## Call-admission JWT invariants (Phase 3b4)

The SFU admits a participant only after validating a JWT
signed by `services/auth`. The Rust-side issuance shipped
in 3b4 (forward-progress, since the SFU-side verifier in
Go lives in 3b1c). Module:
[`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs).
Storage: `services/auth/migrations/0007_call_jwt_signing.sql`
(single-row `call_jwt_signing_key` table holding the
32-byte Ed25519 seed).

Wire shape — JWS Compact:

```text
header.payload.signature

  header  = base64url({"alg": "EdDSA", "typ": "JWT", "kid": <16-hex>})
  payload = base64url({"iss": "lakko-auth",
                       "aud": "lakko-sfu",
                       "sub": <account_id_hex>,
                       "call_id": <call_id_hex>,
                       "role": "participant" | "publisher" | "subscriber",
                       "iat": <unix-seconds>,
                       "exp": <iat + 3600>})
  signature = base64url(Ed25519(header || "." || payload))
```

Public key is published anonymously at
`GET /.well-known/jwks.json` per RFC 8037 (OKP/Ed25519
JWK). The SFU process fetches the JWKS doc at startup +
refreshes every 24 h per ADR-0008.

### C-15: Call JWTs are signed by an Ed25519 key persisted across restarts

The signing seed lives in `call_jwt_signing_key.seed` (32
bytes). `load_or_init_call_signing_key` reads the existing
row OR inserts a freshly-generated seed on first boot via
`INSERT IGNORE`. Concurrent first-boots race-safely converge
on a single row (the CHECK `id = 1` constraint pins the
table to one row).

Surfacing the public key as a JWK lets the SFU verifier
fetch + cache it without any handshake with the auth
service beyond a single GET on a static URL.

Test:
[`call_jwt::tests::mint_then_verify_round_trips`](../../services/auth/src/call_jwt.rs)
covers the sign-then-verify happy path; the persistence
side is exercised by the bootstrap test fixture spinning
a fresh `AppState`.

### C-16: A JWT with the wrong `iss` or `aud` is rejected

`iss = "lakko-auth"` and `aud = "lakko-sfu"` are pinned
constants (in code: `JWT_ISSUER` / `JWT_AUDIENCE`). The
verifier checks both after the signature. This stops a
JWT minted for some other audience (e.g. a future
`lakko-edge` service) from being replayed at the SFU.

Tests:
[`call_jwt::tests::wrong_issuer_rejects`](../../services/auth/src/call_jwt.rs)
+ [`call_jwt::tests::wrong_audience_rejects`](../../services/auth/src/call_jwt.rs).

### C-17: A JWT past its `exp` is rejected

`exp = iat + 3600` (1 hour, per ADR-0008 §"Auth token
format"). The verifier compares `now_secs` injected by
the caller (the SFU verifier, in production) against
`claims.exp` and rejects on `>=`. This bounds the damage
from a leaked JWT to a 1-hour window.

The injection lets tests drive the clock without depending
on `SystemTime::now`.

Test:
[`call_jwt::tests::expired_jwt_rejects`](../../services/auth/src/call_jwt.rs)
— verifies both `now == exp` and `now == exp + 1` reject.

### C-18: A JWT signed by a different key is rejected, even if the `kid` matches

Ed25519 verification is the load-bearing check — `kid`
exists for key discovery (the SFU picks the matching JWK
from the JWKS doc) but is not a security boundary by
itself. The test exercises the realistic attacker shape:
swap the JWKS doc to advertise the attacker's public key
under the legitimate `kid`, mint a JWT with that key. The
verifier rejects because the **signature** doesn't match
the **expected** public key the verifier was constructed
with.

In production this maps to: the SFU process fetches the
JWKS doc directly from `services/auth` over TLS; an
attacker who can swap the JWKS doc has already MITM'd the
auth-service-to-SFU link, which is a separate failure mode
that TLS terminates upstream of this check.

Test:
[`call_jwt::tests::wrong_signing_key_rejects`](../../services/auth/src/call_jwt.rs).

### C-19: A JWT with a tampered header / payload is rejected

The signature is computed over the exact bytes
`header_b64 || "." || payload_b64`. Any single-byte
mutation in either segment changes the signing input;
Ed25519 verification rejects. This is the canonical JWS
tamper-evident property.

Test:
[`call_jwt::tests::tampered_payload_rejects`](../../services/auth/src/call_jwt.rs)
mutates the last base64url character in the payload
segment and asserts the verifier rejects with one of
`{Signature, Format, Encode}` — the specific error type
depends on whether the mutated payload still decodes to
valid JSON.

### C-20: `kid` is a deterministic, stable function of the public key

`kid = hex(BLAKE3(public_key_bytes)[..8])` (16 hex
characters). Two derivations from the same seed produce
the same `kid`; different seeds produce different `kid`s.
This lets the JWKS-cache-refresh logic on the SFU side
detect a key rotation by `kid` inequality rather than
needing to compare full public-key bytes.

Test:
[`call_jwt::tests::kid_is_stable_per_keypair`](../../services/auth/src/call_jwt.rs).

### C-21: `POST /call/jwt` requires a valid session token

Bearer-auth via the same `webauthn::require_session`
middleware passkey routes use. A request without
`Authorization: Bearer <session-token>` (or with an
expired / forged one) is rejected at the middleware
layer with 401 — the handler never sees the call_id /
role. This stops an unauthenticated caller from minting
JWTs for arbitrary `account_id`s.

The middleware stamps the authenticated `account_id`
into request extensions; the handler reads from there
rather than trusting any field in the request body.
The request body carries only `call_id` and `role` —
`account_id` is NEVER supplied by the caller.

The middleware itself is shared with
`services/discovery` (Phase 1.5b) and reviewed there;
the audit cross-link is in [`auth-flow.md`](./auth-flow.md).

## Exporter labels (call-side)

| Label | Length | Context | Source | Used for |
|---|---|---|---|---|
| `lakko/call/v1` | 32 bytes | 16-byte `call_id` | `lakko_mls::Group::export_secret` | Per-call SFrame seed (`CallKey`) |

HKDF info strings (downstream of the exporter, not exporter
labels themselves):

| HKDF info | Source | Used for |
|---|---|---|
| `lakko/sframe/v1/key` | `lakko_calls::sframe` | Per-participant 32-byte AEAD key (HKDF over `call_key + sender_leaf_be`) |
| `lakko/sframe/v1/nonce` | `lakko_calls::sframe` | Per-participant 12-byte nonce salt; per-frame nonce = `salt XOR counter_in_low_4_bytes` (RFC 8439 §2.4 / RFC 9605 §4.4) |

## State-flow diagram — group-call key lifecycle

```text
   MLS group       ┌─ Add / Remove commit
   (chat or call)  │   ├─► epoch advances
        │          │   └─► local Group::process_incoming
        ▼          │
   ┌────────────┐  │
   │ CallKey    │◄─┘
   │ (32 B seed)│       refresh_call_key:
   └────────────┘         current → previous
        │                 export → current
        │                 needs_rekey = true
        │
        │ HKDF-SHA-256
        │ info = "lakko/sframe/v1/key"
        │      || sender_leaf_be
        ▼
   ┌──────────────────┐
   │ ParticipantKey   │   send: seal(aad, plaintext)
   │ (key+salt+ctr)   │      → (counter, ciphertext)
   └──────────────────┘   recv: try_open_with_overlap(...)
        │                       ├─► current path
        │                       └─► previous path (overlap)
        │
        ▼
   AEAD seal/open
   AES-128-GCM (planned 3a5)
   ChaCha20-Poly1305 (default — current)
```

After a rotation, the receive-side overlap window holds the
previous `CallKey` so in-flight frames sealed under the OLD
seed still decode. Platform overlap timer (~1 s) fires →
`drop_previous_call_key()` → seed-boundary forward secrecy
fully in effect.

## What the SFU sees vs what the client owns

| Material | SFU | Client | Notes |
|---|---|---|---|
| MLS exporter seed | ❌ | ✅ | Stays in `GroupCallSession::call_key` |
| Previous-epoch seed (overlap) | ❌ | ✅ | Stays in `GroupCallSession::previous_call_key`; dropped after platform 1 s timer |
| Per-participant key bytes | ❌ | ✅ (own + every remote leaf) | HKDF-derived on-the-fly per frame |
| Per-participant nonce salt | ❌ | ✅ | Same derivation chain |
| SFrame header (sender leaf + counter + timestamp) | ✅ | ✅ | Forwarding metadata only |
| Encrypted frame body | ✅ (opaque) | ✅ (decoded) | AEAD-sealed |
| JWT `(account_id, call_id, role, expires_at)` | ✅ (validate via cached JWKS) | ✅ (mint via `POST /call/jwt`) | Per ADR-0008 + C-15..C-21 |
| Ed25519 signing seed for call JWTs | ❌ | ❌ (stays on `services/auth` host) | Persisted in `call_jwt_signing_key.seed`. Public half published at `/.well-known/jwks.json` |
| Plaintext SDP / ICE | ❌ | ✅ | 1×1 rides MLS; group rides LiveKit-signaling channel which never sees keys |

## Known soft spots — out of scope, tracked

See [`known-gaps.md`](./known-gaps.md) — call-side gaps:

- **G-17** — frame-counter exhaustion path is documented but
  not exercised by a dedicated test (running 2^32 seal
  operations is infeasible in CI).
- **G-18** — overlap-window 1-second timer lives in the
  platform layer (iOS / Android / Web), not in the Rust
  core. Each platform's timer correctness is reviewed
  alongside the WebRTC pod integration (3a9 / 3a10
  followups).
- **G-19** — the SFU JWT validation surface (3b1c) is not
  yet implemented; this audit covers the client-side
  contract only. The server-side validation lands with the
  LiveKit fork.
- **G-20** — call-recording is out of scope (deferred per
  ROADMAP 3c8). If it ever lands, the recording client
  joins as a regular participant with its own MLS leaf, so
  no break to C-1..C-10.
