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

**Client wiring (G3-{1,2}-call-key, Jun 2026).** The mobile
clients now derive this seed in the chat-call path. iOS
`ChatRepository.deriveCallKey(groupIdHex:callId:)` and the
Android `ChatRepository.deriveCallKey` both call
`Group.export_secret(identity, "lakko/call/v1", callId, 32)`
+ `Group.own_leaf_index()` on the live MLS group handle the
repository owns, and `ChatViewModel` arms
`CallSession.setCallKey(seed, localLeaf)` **before** the
session dials or accepts (covering the outgoing + incoming
paths). A failed derivation (not bootstrapped / group
missing / epoch skew) leaves the seed unset, so
`attachSFrame` refuses the call per security invariant #4 —
the fail-closed path. Before this wiring `setCallKey` was
never called from the app, so every chat-initiated call
refused; the keying plumbing was the missing link, not a
violation. The live two-device seed-match is asserted by
construction (both derive from the same exporter label +
context + epoch); an on-device cross-platform call test is
a deferred follow-up.

### C-2: Cross-party seal/open works end-to-end through the MLS exporter

Stricter than C-1: not only do the seeds agree, but the
HKDF-derived per-participant key plus the AEAD seal under
that key produces a ciphertext openable by the recipient
who re-derives the sender's key from its own `Group` view.

Test: same as C-1 (the test seals on A and opens on B
through `remote_participant_key(0)`).

### C-3: Per-participant keys differ across leaves

`participant_key(kid)` MUST produce distinct key bytes AND
distinct nonce salts for distinct `sender_leaf` values (the
KID's high bits, `KID = (sender_leaf << 8) | stream_index`).
Otherwise an attacker who compromised a single participant's
key could trivially impersonate any other participant. (The
`stream_index` low byte of the KID additionally separates a
single participant's concurrent streams — audio, camera,
screen-share — into disjoint key spaces; see C-5 /
[ADR-0035](../adr/0035-sframe-per-stream-kid-key.md).)

Test:
[`sframe::tests::participant_keys_differ_across_leaves`](../../crates/lakko-calls/src/sframe.rs)
+ [`group_call::tests::participant_keys_differ_across_leaves`](../../crates/lakko-calls/src/group_call.rs).

### C-4: Three-party full-mesh seal/open holds

Every participant in an N-party call must be able to derive
every other participant's `ParticipantKey` from the shared
`CallKey` seed + the KID carried in the SFrame header
(`KID = (sender_leaf << 8) | stream_index`). The SFU
forwarding path depends on the receiver re-deriving the
sender's per-stream key straight from the KID the header
carries — the SFU forwards that header verbatim and never
derives a key (ADR-0035).

Test:
[`group_call::tests::three_party_full_mesh_seal_open`](../../crates/lakko-calls/src/group_call.rs)
— A, B, C each seal one frame; the other two open via the
sender's leaf index.

### C-5: Frame counter is strictly monotonic per (sender, stream) per epoch — no cross-stream `(key, nonce)` reuse

RFC 9605 §4.4 pins the counter to 32 bits. The sender-side
`ParticipantKey::seal` increments the counter once per
sealed frame and never reuses a `(key, nonce)` pair
**within** a `ParticipantKey`.

A single monotonic counter is only sufficient when each
`(key, nonce_salt)` pair is used by exactly one stream. A
participant who sends more than one media stream shares one
MLS leaf, so the discriminator that separates their streams
must ride in the SFrame header KID — the **only** per-stream
signal both endpoints agree on through the zero-trust SFU,
which forwards the RFC 9605 header verbatim and cannot read
the media. Two intermediate designs fell short:

- **Original (pre-ADR-0034):** the KID was `= sender_leaf`
  alone, so a participant's audio + video derived the **same**
  `(key, nonce_salt)` and their **independent** counters (each
  from 0) sealed different plaintexts under an identical
  `(key, nonce)` pair — ChaCha20 keystream reuse + Poly1305 tag
  forgery, a HIGH break for an SFU group video call.
- **ADR-0034 (superseded discriminator):** folded a 1-byte
  Audio/Video **media-type** tag into the derivation. This
  closed the audio-vs-video pair but **re-collided two video
  streams** from one leaf (camera + screen-share both map to
  `Video`), and a local media-type/track-kind cannot tell two
  same-kind streams apart, nor can a transceiver MID (it
  differs on the sender and subscriber sides of the SFU).

The key is now derived per-`(leaf, stream)` via the header
KID ([ADR-0035](../adr/0035-sframe-per-stream-kid-key.md)):
`KID = (sender_leaf << 8) | stream_index` (`stream_index` a
per-sender `u8` ordinal — audio 0, camera 1, screen-share 2,
…), and `CallKey::participant_key(kid)` folds the **FULL KID**
into **both** HKDF `info` strings. Distinct KID ⇒ distinct
`(key, nonce_salt)` space automatically, so **every**
concurrent stream of a leaf keys into its own space — the
reuse is closed for **any** number of streams per leaf, not
just audio + one video. The receiver reads the KID off each
frame's header and derives `participant_key(KID)` with **no**
local media context; roster consumers recover
`sender_leaf = KID >> 8` (`SframeHeader::leaf_of`). The
`SframeHeader` codec and the AAD are unchanged — RFC 9605 §4.3
already encodes an arbitrary `u64` KID in an extended form, so
the header is still the AAD (see C-6 /
[ADR-0013](../adr/0013-sframe-cross-platform-aad.md)).

After counter exhaustion (2^32 frames ≈ 4 billion at any
frame rate this is years of continuous calling, but the
bound is honoured) the seal path returns
`Error::CounterExhausted` and the caller MUST trigger a
fresh MLS commit on the keying group → fresh CallKey →
fresh ParticipantKey → counter resets.

Tests:
[`sframe::tests::counter_increments_strictly`](../../crates/lakko-calls/src/sframe.rs)
covers the monotonic increment;
[`sframe::tests::three_streams_of_one_leaf_get_distinct_keys_and_nonces`](../../crates/lakko-calls/src/sframe.rs)
proves three streams from one leaf (audio + camera + screen-share)
derive pairwise-distinct keys AND distinct nonce salts and
never seal the same plaintext at the same counter to the same
ciphertext (no cross-stream reuse — including the two-video-stream
pair ADR-0034 missed);
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
signed by `services/auth`. The Rust-side issuance + the
client-side verifier shipped in 3b4 (forward-progress,
since the SFU-side verifier in Go lives in 3b1c).

Crypto + wire shape lives in the shared crate
[`crates/lakko-call-jwt/`](../../crates/lakko-call-jwt/src/lib.rs)
so issuance side (auth service), client side (FFI), and
the Go-side SFU verifier (3b1c, reimplemented against the
same spec) all speak one shape and can't drift. The auth
service-side HTTP handlers + keypair persistence live in
[`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs);
storage in `services/auth/migrations/0007_call_jwt_signing.sql`
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

Tests:
- [`lakko_call_jwt::tests::mint_then_verify_round_trips`](../../crates/lakko-call-jwt/src/lib.rs)
  — sign-then-verify happy path at the primitive layer.
- [`call_jwt::tests::auth_side_issuance_round_trips`](../../services/auth/src/call_jwt.rs)
  — server-side smoke that the claim shape the HTTP
  handler emits round-trips cleanly through the shared
  crate.
- [`call_jwt::tests::issue_call_jwt_happy_path_round_trips`](../../services/auth/src/call_jwt.rs)
  — full HTTP integration test: `POST /call/jwt` with a
  valid session-token bearer header through the real
  axum router (middleware → handler → JSON), then
  `verify_jwt` on the response under the same signing
  key.

### C-16: A JWT with the wrong `iss` or `aud` is rejected

`iss = "lakko-auth"` and `aud = "lakko-sfu"` are pinned
constants (in code: `JWT_ISSUER` / `JWT_AUDIENCE`). The
verifier checks both after the signature. This stops a
JWT minted for some other audience (e.g. a future
`lakko-edge` service) from being replayed at the SFU.

Tests:
[`lakko_call_jwt::tests::wrong_issuer_rejects`](../../crates/lakko-call-jwt/src/lib.rs)
+ [`lakko_call_jwt::tests::wrong_audience_rejects`](../../crates/lakko-call-jwt/src/lib.rs).

### C-17: A JWT past its `exp` is rejected

`exp = iat + 3600` (1 hour, per ADR-0008 §"Auth token
format"). The verifier compares `now_secs` injected by
the caller (the SFU verifier, in production) against
`claims.exp` and rejects on `>=`. This bounds the damage
from a leaked JWT to a 1-hour window.

The injection lets tests drive the clock without depending
on `SystemTime::now`.

Test:
[`lakko_call_jwt::tests::expired_jwt_rejects`](../../crates/lakko-call-jwt/src/lib.rs)
— verifies `now == exp + TTL` rejects.

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
[`lakko_call_jwt::tests::wrong_signing_key_rejects`](../../crates/lakko-call-jwt/src/lib.rs).

### C-19: A JWT with a tampered header / payload is rejected

The signature is computed over the exact bytes
`header_b64 || "." || payload_b64`. Any single-byte
mutation in either segment changes the signing input;
Ed25519 verification rejects. This is the canonical JWS
tamper-evident property.

Test:
[`lakko_call_jwt::tests::tampered_payload_rejects`](../../crates/lakko-call-jwt/src/lib.rs)
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

Tests:
[`lakko_call_jwt::tests::kid_is_stable_per_keypair`](../../crates/lakko-call-jwt/src/lib.rs)
covers stability + distinctness;
[`lakko_call_jwt::tests::wrong_kid_rejects`](../../crates/lakko-call-jwt/src/lib.rs)
covers the verifier's `kid` discrimination.

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

Tests (HTTP integration, drives the real axum router):
- [`call_jwt::tests::issue_call_jwt_without_bearer_rejects_401`](../../services/auth/src/call_jwt.rs)
  — request with no `Authorization` header → 401.
- [`call_jwt::tests::issue_call_jwt_with_wrong_secret_rejects_401`](../../services/auth/src/call_jwt.rs)
  — bearer token minted under a DIFFERENT HMAC secret →
  401 (attacker who got the wire shape right but doesn't
  have the secret).
- [`call_jwt::tests::issue_call_jwt_with_malformed_call_id_rejects_400`](../../services/auth/src/call_jwt.rs)
  — bearer passes but `call_id` is the wrong length /
  non-hex → 400.
- [`call_jwt::tests::issue_call_jwt_happy_path_round_trips`](../../services/auth/src/call_jwt.rs)
  asserts the `sub` field is taken from the bearer
  token's account_id, never from the request body.

### C-22: `verify_with_jwks` resolves the correct key by `kid` across a multi-key JWKS doc

During key rotation the JWKS document carries BOTH the
old + new keys for the overlap window (so JWTs minted
under either key still validate). The verifier picks the
JWK whose `kid` matches the JWT header; a JWT signed by
the old key opens against the old JWK, a JWT signed by
the new key opens against the new JWK, simultaneously.
A JWT whose `kid` matches no JWK in the doc rejects
with a clear `Format("no JWK matches header.kid")` so the
SFU's logs differentiate "key rotation in progress, our
JWKS cache is stale" from "this JWT is forged".

Tests:
- [`lakko_call_jwt::tests::verify_with_jwks_resolves_by_kid`](../../crates/lakko-call-jwt/src/lib.rs)
  — multi-key happy path.
- [`lakko_call_jwt::tests::verify_with_jwks_rejects_unknown_kid`](../../crates/lakko-call-jwt/src/lib.rs)
  — no-match-on-kid rejection.
- [`lakko_call_jwt::tests::jwks_round_trips_through_json`](../../crates/lakko-call-jwt/src/lib.rs)
  — JWKS doc round-trips cleanly through JSON.
- [`call_jwt::tests::jwks_endpoint_full_chain`](../../services/auth/src/call_jwt.rs)
  — HTTP integration: mint via `POST /call/jwt`, fetch
  the JWKS via `GET /.well-known/jwks.json`, verify the
  minted JWT against the fetched doc through
  `verify_with_jwks`. Same path a real SFU process takes.

### C-23: `Jwk::to_verifying_key` rejects non-OKP / non-Ed25519 JWKs

The JWK parser checks `kty == "OKP"`, `crv == "Ed25519"`,
`alg == "EdDSA"` before decoding the `x` field. A JWKS doc
that an attacker — or a confused upstream — populated with
an RSA / EC P-256 / EC P-384 key under our `kid` would
get rejected at parse time, before reaching the signature
verify step. Defence-in-depth against algorithm-confusion
attacks (RFC 8037 §6 attack class).

Test:
[`lakko_call_jwt::tests::jwk_to_verifying_key_rejects_wrong_kty`](../../crates/lakko-call-jwt/src/lib.rs).

## Link-based call-invite invariants (Phase 3c3)

A second capability class lives alongside the contact-invite
capability from ADR-0005: the **call-invite capability**
that lets an owner mint a `https://lakko.me/c/<slug>` link
which a single joiner can use to bootstrap a fresh MLS
group call. The wire codec lives in
[`crates/lakko-contacts/src/call_invite.rs`](../../crates/lakko-contacts/src/call_invite.rs);
the joiner-side MLS orchestration in
[`crates/lakko-calls/src/link_invite.rs`](../../crates/lakko-calls/src/link_invite.rs).

The capability embeds a **one-time `KeyPackage`** the owner
generated specifically for this invite, alongside a
16-byte `call_id`, `expiry_ms`, media flags, and a label.
The wire bytes are Ed25519-signed over the owner's
long-term identity key.

Wire flow:

```text
   owner Alice                          joiner Bob
      │                                    │
      │ — mints KP + call_id               │
      │ — composes CallInviteCapability    │
      │ — publishes https://…/c/<b64>      │
      │ ─────────────── link OOB ────────► │
      │                                    │ — decode_and_verify
      │                                    │ — accept_call_invite
      │                                    │      (new MLS group with
      │                                    │       Bob founder, Alice
      │                                    │       added via embedded KP;
      │                                    │       GroupCallSession with
      │                                    │       scope = LinkBased)
      │ ◄── Welcome ──────────────────────│
      │                                    │
      │ — join_from_welcome                │
      │ — GroupCallSession::create         │
      │     with same call_id              │
      │                                    │
      │ both sides derive matching         │
      │ CallKey seed (C-1 holds)           │
```

### C-24: A `CallInviteCapability` with a tampered payload is rejected

The wire format is `schema_version || owner_pubkey ||
owner_account_id || call_id || expiry_ms || max_uses ||
media_flags || key_package_len || key_package_bytes ||
label_len || label || Ed25519_signature`. The signature
covers everything before the trailing 64 bytes. Flipping a
single byte in any field — including deep inside the
embedded `KeyPackage` — breaks the signature, and the
decoder refuses with `InvalidEncoding("call-invite
signature invalid")`.

Test:
[`lakko_contacts::call_invite::tests::tampered_payload_rejected`](../../crates/lakko-contacts/src/call_invite.rs)
flips a byte inside the KP region; AEAD-equivalent
authentication rejects.

### C-25: Splicing a different owner pubkey into a signed capability is rejected

An attacker takes Alice's signed capability, swaps Bob's
Ed25519 pubkey into the `owner_signature_pubkey` field,
ships the result. The signature was computed against
Alice's pubkey + the rest of the payload; verifying against
Bob's pubkey rejects. The capability's identity is bound
to the signature, not just the embedded pubkey field.

Test:
[`lakko_contacts::call_invite::tests::cross_signing_key_rejected`](../../crates/lakko-contacts/src/call_invite.rs).

### C-26: Schema-version + bounds enforced at parse time

The first byte must equal `1`; future schema bumps land as
`2`, etc. The KP length is bounded to
[`MAX_KEY_PACKAGE_LEN = 4 KiB`](../../crates/lakko-contacts/src/call_invite.rs)
and the label to `MAX_LABEL_LEN = 64`. Empty KP is
rejected at compose AND decode. Oversize KP / label
rejected at compose; oversize-on-the-wire rejected at
decode (defence-in-depth against an attacker who skipped
the compose step). This bounds parser memory + caps URL
length.

Tests:
[`lakko_contacts::call_invite::tests::unsupported_schema_version_rejected`](../../crates/lakko-contacts/src/call_invite.rs),
[`…::empty_key_package_rejected_at_compose`](../../crates/lakko-contacts/src/call_invite.rs),
[`…::oversize_key_package_rejected_at_compose`](../../crates/lakko-contacts/src/call_invite.rs),
[`…::oversize_label_rejected_at_compose`](../../crates/lakko-contacts/src/call_invite.rs),
[`…::truncated_wire_rejected`](../../crates/lakko-contacts/src/call_invite.rs).

### C-27: Cross-party `CallKey` seeds agree across the bootstrap path

The joiner-side `accept_call_invite` builds a fresh MLS
group with the joiner as founder + the owner already
added via the embedded KP, then derives a
`GroupCallSession` keyed by `capability.call_id`. The
owner runs `Group::join_from_welcome` on the Welcome
bytes the joiner ships back + builds her own
`GroupCallSession` against the same `call_id`. Both sides
land at the same MLS epoch with the same exporter, so the
32-byte `CallKey` seed is byte-identical.

This is C-1 (cross-party seed agreement) re-asserted
specifically across the link-based bootstrap path —
proves the invite-codec → MLS-group → exporter chain
preserves the cross-party invariant.

**Owner-signed revocation (3c3-revoke).** The capability
owner can call `DELETE /discovery/call-invite/{blob_id}`
with a body carrying `signed_at_ms` + Ed25519 signature
over `call_revoke_canonical_bytes(blob_id, signed_at_ms)`.
Server verifies against the embedded
`owner_signature_pubkey` recovered from the stored
capability; on success the row is deleted and the FK
CASCADE on `call_invite_consumers` reaps the dedup
side-table rows for free. Cross-kind isolation:
`CALL_INVITE_REVOKE_DOMAIN_V1` is distinct from
`REVOKE_DOMAIN_V1` so a contact-invite revoke signature
can't replay against a call-invite of the same `blob_id`.
[`lakko_contacts::call_invite::tests::call_revoke_signature_does_not_validate_under_contact_invite_domain`](../../crates/lakko-contacts/src/call_invite.rs)
pins the invariant.

Tests:
[`lakko_calls::link_invite::tests::accept_invite_round_trips_via_welcome`](../../crates/lakko-calls/src/link_invite.rs)
asserts byte-identical seeds + a successful seal/open
between Bob and Alice.
[`…::accept_invite_with_corrupted_kp_rejects`](../../crates/lakko-calls/src/link_invite.rs)
shows the MLS layer rejects when the embedded KP is
internally tampered (the capability signature stays
valid; the MLS validate step catches it).
[`…::joiner_is_founder_at_leaf_0`](../../crates/lakko-calls/src/link_invite.rs)
asserts the "first accepted joiner founds the group"
contract: joiner at leaf 0, owner at leaf 1 post-Welcome.

### C-28: Owner fingerprint enables OOB anti-phishing verification

The capability exposes `owner_fingerprint()` — a
71-character ASCII Signal-style safety number derived
from the owner's long-term Ed25519 pubkey (same format
joiner UIs already use for contact verification). The
joiner-side UI shows this **before** the Accept button
becomes active; the human compares the digits against
what the owner shared out-of-band (a Signal chat, a
business card, a phone call). Mismatch → the link is from
someone other than who the human expected, typically a
link-substitution attack where an attacker swapped a real
`/c/<slug>` URL for one pointing at their own account.

Properties:

- **Deterministic per pubkey** — same owner always
  produces the same fingerprint regardless of `call_id`,
  expiry, media flags, or label. So the human only learns
  one fingerprint per contact, not per invite.
- **Format match** — 12 groups of 5 digits separated by
  spaces, identical to
  [`crate::safety::safety_number`](../../crates/lakko-contacts/src/safety.rs)'s
  contract. UI surfaces don't need a second renderer.
- **Pure function of the embedded pubkey** — no
  external lookup, no network round-trip. Joiner can
  display the fingerprint before consuming the
  capability's KP.

The protection holds only when the human actually
compares the digits. Skipping the check is a UX
regression but not a cryptographic break — the SFU still
rejects the call if the joiner's MLS group epoch doesn't
match the owner's. The fingerprint is a **defence-in-depth
UX layer**, not a load-bearing crypto invariant.

Tests:
- [`lakko_contacts::call_invite::tests::owner_fingerprint_is_deterministic_per_pubkey`](../../crates/lakko-contacts/src/call_invite.rs)
  — same owner, different capability fields → same
  fingerprint.
- [`…::different_owners_have_different_fingerprints`](../../crates/lakko-contacts/src/call_invite.rs)
  — distinct pubkeys → distinct fingerprints (overwhelmingly
  likely; BLAKE3 collision-resistance is the formal
  argument).
- [`…::owner_fingerprint_matches_safety_number_format`](../../crates/lakko-contacts/src/call_invite.rs)
  — 71 chars, 12 × 5-digit groups, space separator,
  ASCII digits only.

The same `owner_fingerprint()` method exists on the
**contact-invite** capability
[`lakko_contacts::InviteCapability`](../../crates/lakko-contacts/src/invite.rs).
It defends the `/i/<slug>` contact-link substitution
attack with identical UX: joiner UI shows the digits
before "Add contact". Critically, both invite kinds
produce **byte-identical fingerprints** for the same
owner pubkey (both reuse `safety::safety_number(pk, pk)`),
so a human only needs to learn **one** fingerprint per
contact across both invite ecosystems —
[`lakko_contacts::invite::tests::owner_fingerprint_matches_across_invite_kinds`](../../crates/lakko-contacts/src/invite.rs)
asserts this cross-invite-kind compatibility explicitly,
plus the three parallel deterministic / distinct / format
tests in
[`lakko_contacts::invite::tests::owner_fingerprint_*`](../../crates/lakko-contacts/src/invite.rs).

### C-29: Server-side call-invite ledger handler invariants

The server-side handler module
[`services/discovery/src/call_invite.rs`](../../services/discovery/src/call_invite.rs)
exposes four endpoints — `POST /discovery/call-invite`
(upload), `GET /discovery/call-invite/{blob_id}` (fetch),
`POST /discovery/call-invite/{blob_id}/consume`, `DELETE
/discovery/call-invite/{blob_id}` (revoke). Five
constants-vs-migration drift sentinels and the helper-side
pure-function contracts are pinned by the
3c3-server-tests module:

- **`blob_id_for(capability_bytes)`** is plain SHA-256
  (no domain separator) — deterministic, length-32,
  collision-resistant under SHA-256's security argument.
  The cross-table FK CASCADE on
  `call_invite_consumers.blob_id` reaps dedup rows when the
  parent row is deleted; the lack of a domain separator on
  the id is allowed because the dedup tables are
  partitioned by table, not by id-namespace.
- **`MAX_CAPABILITY_BYTES = 8192`** matches the
  `VARBINARY(8192)` width in
  [`0007_call_invites.sql`](../../services/discovery/migrations/0007_call_invites.sql).
  The 3c3 wire format tops out around 4308 bytes with a
  4 KiB embedded KP; the ~2× headroom covers future
  schema extensions (e.g. a `transparency_proof`
  parallel to the ADR-0005 plan).
- **`MAX_USES_UNLIMITED = u32::MAX`** is the
  unlimited-budget sentinel the SQL CASE expression in
  `consume` binds directly into the atomic UPDATE.
  Constant drift would silently change unlimited-budget
  semantics.
- **`within_replay_window`** is symmetric across
  `±CALL_REVOKE_REPLAY_WINDOW_MS = 10 min` with inclusive
  boundaries, and `saturating_sub` semantics on small
  `now_ms` so an early-boot revoke signed against an
  almost-zero clock still folds into the lower bound.
- **`decode_and_verify` server-side round-trip** — the
  upload handler's signature gate is exercised end-to-end
  through `lakko_contacts::call_invite::compose` → handler-
  level `decode_and_verify` → trailing-byte tamper
  rejection. A future change to `lakko-contacts` that
  loosens signature checking would surface here, not just
  on the client side.

Tests: 10 cases in
[`services::discovery::call_invite::tests`](../../services/discovery/src/call_invite.rs)
— `blob_id_is_sha256_of_capability_bytes`,
`same_bytes_yield_same_blob_id`,
`different_bytes_yield_different_blob_id`,
`blob_id_layout_is_plain_sha256_no_domain_separator`,
`max_capability_bytes_matches_column_width`,
`blob_id_len_matches_column_width`,
`call_invites_migration_indexes`,
`call_invite_consumers_migration_shape`,
`max_uses_unlimited_matches_u32_max`,
`within_replay_window_is_symmetric`,
`within_replay_window_saturates_on_small_now`,
`server_side_decode_and_verify_round_trip`.
lakko-svc-discovery test count: 28 → 38.

### C-30: `/c/<slug>` URL plumbing — discrimination contract

The owner-shared URL has the shape
`<origin>/c/<base64url>`. The joiner-side dispatcher
discriminates pointer (server-backed) vs. inline (censored-
region fallback) by **decoded byte count**:

- exactly 32 bytes ⇒ pointer (SHA-256 `blob_id`); joiner
  fetches the capability via
  `DiscoveryClient.fetchCallInvite(blobId)` →
  `decodeAndVerify` → `consumeCallInvite`.
- ≥ 400 bytes ⇒ inline; joiner runs
  `decodeAndVerify` directly on the URL bytes, no server
  round-trip. The 400-byte floor is below the 3c3 wire-
  format minimum with a 256-byte KP (~500 bytes) but above
  the 32-byte pointer ceiling; the gap is wide enough that
  no legal 3c3 wire-format payload can be mistaken for a
  pointer.
- anything else (e.g. 100 bytes) is **invalid input** —
  the parser rejects with a typed `slugWrongLength` error
  the UI surfaces as "this link is corrupted".

The `/c/` path prefix contrasts with contact-invite's
`/i/` so the platform-level deep-link dispatcher (web
router / iOS universal-link / Android intent filter)
branches on the path segment without parsing the slug.

Tests (parity across web typecheck-gate + Android JUnit;
iOS XCTest infra not yet wired): 25 JUnit cases in
[`apps/android/.../calls/CallInviteUrlTest.kt`](../../apps/android/app/src/test/kotlin/me/lakko/app/calls/CallInviteUrlTest.kt)
cover pointer-URL round-trip, inline-URL round-trip,
`parseCallInviteSlug` discrimination (pointer / inline /
dead-zone / malformed base64), form-side enum wire-value
sentinels, human-visible renderer sentinel branches, and
the constants-vs-web-side drift sentinels
(`CALL_INVITE_URL_PREFIX == "/c/"`,
`CALL_INVITE_BLOB_ID_LEN == 32`,
`CALL_INVITE_LABEL_MAX_LEN == 64`,
`CALL_INVITE_INLINE_MIN_LEN == 400`).

### C-31: Composer + acceptor pipelines — single-call surface contract

Phase 3c3-{composer,acceptor}-helper exposes a
single-call API on each client for the two ends of the
link-based call-invite flow. The pipelines collapse 5–7
discrete steps each into one async call so the UI can
branch on the discriminated result without re-implementing
the dispatch — and so a future security regression in any
step surfaces in a tight, named property test rather than
as a silent UX issue.

**Composer (owner side, 5 steps)**:

1. Mint a fresh 16-byte random `call_id` via the
   platform CSPRNG (`crypto.getRandomValues` on web,
   `SecRandomCopyBytes` on iOS, `SecureRandom` on Android).
2. Mint a one-time MLS `KeyPackage` via
   `identity.createKeyPackage()`. The capability embeds it;
   the joiner consumes it to bootstrap a fresh call MLS
   group.
3. `composeCallInviteCapability(...)` via FFI — produces
   wire bytes Ed25519-signed under the owner's identity
   key.
4. `DiscoveryClient.uploadCallInvite(capabilityBytes)` —
   the server verifies the signature server-side
   (defence-in-depth against a buggy client) and content-
   addresses by SHA-256.
5. Build the `<origin>/c/<slug>` URL via
   `composePointerCallInviteUrl(blobId)`.

Output (identical across the three platforms):
`{ blobId, url, callId, capabilityBytes }`. Each field is
independent — UI typically surfaces `url` in the share
sheet, persists `blobId` for the future revoke flow, and
keeps `callId` around so a subsequent call-admission JWT
mint (C-15..C-23) binds to the same id.

**Acceptor (joiner side, 7 steps + 8 result variants)**:

1. `parseCallInviteSlug(slug)` discriminates pointer vs.
   inline per C-30.
2. (pointer branch) `DiscoveryClient.fetchCallInvite(blobId)`
   — bytes or 404.
3. `CallInviteCapability.decodeAndVerify(bytes)` — local
   Ed25519 signature check. This re-runs the same verify
   the upload handler already executed; defence-in-depth
   against a malicious server serving substituted bytes.
4. `capability.isValidNow(nowMs)` — expiry filter.
5. (pointer branch)
   `DiscoveryClient.consumeCallInvite(blobId,
   capabilityBytes, joinerAccountId)` — atomic counter
   bump + per-joiner dedup via the `joiner_tag`.
6. `acceptCallInvite(capability, joinerIdentity)` — MLS
   bootstrap (joiner founds group + owner added via the
   embedded KP).
7. Return the eight-variant `Result` to the UI.

Behavioural invariants the acceptor pins (parity on three
platforms):

- **Local re-verify defence**: `decodeAndVerify` runs
  AFTER the server already verified at upload time. A
  malicious server cannot serve substituted bytes — the
  joiner-side verify catches the tamper before `consume`
  / `accept`. Surfaces as `signatureFailure` variant.
- **404 folding**: a `DiscoveryHttpError(404)` from
  `fetchCallInvite` collapses into the `notFound` variant
  — the joiner cannot distinguish "never-issued" from
  "revoked" from "expired". This is by design and denies
  status-code probing of the `blob_id` space.
- **`alreadyJoined` short-circuit**: when the server's
  per-joiner dedup table already has a `(blob_id,
  joiner_tag)` row (joiner clicked the link twice), the
  acceptor returns `alreadyJoined` without re-bootstrapping
  the MLS group — the UI opens the existing call group
  instead of minting a fresh one (which would create a
  duplicate Welcome the owner couldn't sensibly resolve).
- **Inline-fallback skips fetch + consume**: an inline
  slug has no server-side row to bump; the acceptor
  surfaces this asymmetry as `accepted` with
  `capabilityBytes` populated but `welcomeForOwner` still
  requiring out-of-band ferrying.
- **`callKeySeed` exposed in the result**: 32-byte SFrame
  seed surfaced directly so the UI can seal the joiner's
  first frame via `sframeDeriveParticipantKey(seed, leaf)`
  before the owner has joined — no second FFI round-trip
  needed.

Tests: typecheck gates (web `npx tsc --noEmit`, Android
`./gradlew :app:compileDebugKotlin` in CI, iOS `xcodebuild`
in CI) cover the structural correctness; the discriminated
`Result` variants are pinned by their pattern-match
exhaustiveness in the platform code. End-to-end Rust-side
coverage of the joiner-side MLS bootstrap lives in
[`lakko_calls::link_invite::tests::accept_invite_round_trips_via_welcome`](../../crates/lakko-calls/src/link_invite.rs)
and the three sibling integration tests.

### C-32: Call-Welcome envelope reuses ContactRequest transport with domain separation

The joiner-side `acceptCallInvite` MLS bootstrap returns a
`welcomeForOwner` byte string the owner needs to drive
`Group::join_from_welcome` and enter the call group.
Phase 3c3-welcome-ferry ferries these bytes over the
**existing** stranger-inbox `ContactRequest` envelope path
rather than standing up a dedicated
`/discovery/call-welcome` endpoint. The reuse is
load-bearing on three already-pinned contracts:

- **Confidentiality** — the outer `ContactRequest`
  envelope is HPKE-sealed to
  `capability.owner_x25519_pubkey` (the same key bound to
  the owner's identity in the KT log). The server sees
  only `(stranger_inbox_pseudonym → opaque ciphertext)`.
  This is the existing contract pinned by C-12 / I-3 /
  the [`crate::envelope`](../../crates/lakko-contacts/src/envelope.rs)
  sealed-box tests.
- **Authenticity** — the envelope's Ed25519 signature is
  over the joiner's identity key, so the owner can pin
  exactly which joiner sent the Welcome. Existing
  `ContactRequest` invariant.
- **Anti-abuse** — PoW + Privacy Pass on every
  `ContactRequest` ingress already rate-limits cold
  senders. The Welcome-ferry inherits the same DoS gate
  without code duplication.

The piece **this invariant** adds is a 22-byte ASCII
domain separator
[`CALL_WELCOME_ENVELOPE_DOMAIN_V1 = b"lakko/call-welcome/v1\x00"`](../../crates/lakko-contacts/src/call_welcome.rs)
that the joiner prepends to its `welcome_for_owner` bytes
**inside** the HPKE-sealed plaintext (so the server never
sees it). The owner-side polling loop discriminates on a
clean byte-prefix test:

```text
plaintext_payload = CALL_WELCOME_ENVELOPE_DOMAIN_V1 || welcome_bytes
                  └────── 22 bytes, ASCII ──────┘
                  "lakko/call-welcome/v1\x00"
```

Contract:

- **`wrap_call_welcome(welcome_bytes) -> Vec<u8>`** is
  infallible byte-for-byte concatenation — surface here if
  a future refactor accidentally re-encodes / pads /
  truncates the welcome (the `wrap_preserves_arbitrary_welcome_bytes_verbatim`
  test exercises 2 KiB of cyclic bytes round-tripping).
- **`try_unwrap_call_welcome(plaintext) -> Option<&[u8]>`**
  is a three-way discriminator:
  - `Ok(Some(payload))` — prefix matched AND payload ≥
    32 bytes (the `CALL_WELCOME_MIN_PAYLOAD_LEN` floor,
    well below the practical OpenMLS Welcome size of
    ~200 bytes — anything smaller is hostile / buggy).
    Caller drives `Group::join_from_welcome(payload)`.
  - `Ok(None)` — prefix did not match. Caller renders
    the plaintext as a plain intro message (ADR-0005
    scenario A — "add me as a contact"). The trailing
    `\x00` byte in the prefix ensures a UTF-8 intro
    message can never start with the separator.
  - `Err(CallWelcomeError::TruncatedPayload { min, got })`
    — prefix matched but payload < 32 bytes. Caller logs
    + drops the envelope.

**Cross-kind isolation** is asserted by the
`separator_distinct_from_contact_invite_domains` test —
the 22-byte `lakko/call-welcome/v1\x00` separator MUST NOT
collide with `lakko-invite-revoke/v1\x00`,
`lakko-call-invite-revoke/v1\x00`, or
`JOINER_TAG_SALT_DOMAIN_V1`. The naming pattern (`lakko/`
prefix for in-envelope domains vs. `lakko-` for canonical-
bytes signing domains) is a deliberate stylistic cue but
the test is what actually pins distinctness.

Tests (9 cases in
[`lakko_contacts::call_welcome::tests`](../../crates/lakko-contacts/src/call_welcome.rs)):
`wrap_then_unwrap_round_trips`,
`unwrap_returns_none_for_intro_message_plaintext`,
`unwrap_returns_none_for_partial_prefix_match`,
`unwrap_rejects_truncated_payload`,
`unwrap_accepts_exactly_min_payload_len`,
`empty_plaintext_returns_none_not_error`,
`domain_separator_is_22_bytes_and_ends_with_null`,
`separator_distinct_from_contact_invite_domains`,
`wrap_preserves_arbitrary_welcome_bytes_verbatim`. lakko-
contacts test count: 98 → 107.

FFI exposure: wasm-bindgen `wrapCallWelcome(welcomeBytes)`
+ `tryUnwrapCallWelcome(plaintext)` +
`callWelcomeMinPayloadLen()`; UniFFI
`wrap_call_welcome(welcome_bytes)` /
`try_unwrap_call_welcome(plaintext)` /
`call_welcome_min_payload_len()`. lakko-ffi-uniffi test
count: 67 → 71 (4 new cases — round-trip, intro-message
fallthrough, truncated-payload rejection, floor-constant
pin). Negative paths on the wasm side stay UniFFI-only
because `JsError::new` panics on the native target the
`cargo test` harness runs on.

### C-33: Call KeyPackage-forward envelope for subsequent joiners

Multi-party (3+ participant) link calls need a way for
**subsequent** joiners (the 3rd, 4th, … participants in a
group call) to enter an already-established call MLS
group. The first joiner founds the group + adds the owner
via the embedded KP (see C-26); subsequent joiners cannot
repeat that flow without creating disjoint MLS groups that
share a `CallKey` seed but not an epoch — splitting the
call.

The correct flow flips the topology: the subsequent joiner
ferries its own fresh `KeyPackage` to the call group's
current committer (the first joiner — owner handover lands
later), who runs an MLS `Add` proposal + commit and
ferries a Welcome back via C-32. This invariant pins the
**joiner→committer** half of that handshake; the
**committer→joiner** half reuses C-32 unchanged.

The KP-forward envelope is a sibling of the call-welcome
envelope from C-32 — same outer transport
(`ContactRequest` HPKE-sealed to the committer's identity
key + routed via the committer's stranger inbox), same
three pinned contracts (confidentiality, authenticity,
anti-abuse), only the inner domain separator differs:

```text
plaintext_payload = CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1 || kp_bytes
                   └────── 25 bytes, ASCII ──────┘
                   "lakko/call-keypackage/v1\x00"
```

Constants + helpers in
[`lakko_contacts::call_kp_forward`](../../crates/lakko-contacts/src/call_kp_forward.rs):

- **`CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1`** — 25 bytes,
  trailing `\x00` makes the prefix
  UTF-8-text-prefix-impossible. Length is intentionally
  distinct from the 22-byte
  `CALL_WELCOME_ENVELOPE_DOMAIN_V1` as a defence-in-depth
  cue, though the actual dispatch is a byte-prefix match.
- **`CALL_KP_FORWARD_MIN_PAYLOAD_LEN = 256`** — floor for
  the truncation check. OpenMLS KPs are 400–1024 bytes
  depending on credential + signature size; 256 is well
  below the practical minimum, so a truncated payload
  surfaces as a typed error rather than a confused
  `Some(too_short_kp)` that the MLS layer would later
  reject opaquely.
- **`wrap_call_kp_forward(kp_bytes)`** — infallible
  byte-for-byte prefix + payload concatenation.
- **`try_unwrap_call_kp_forward(plaintext)`** — three-way
  discriminator with the same shape as
  `try_unwrap_call_welcome` (reuses
  [`CallWelcomeError`](../../crates/lakko-contacts/src/call_welcome.rs)
  to keep the FFI surface compact — both envelopes have a
  single failure mode).

**No-cross-dispatch guarantee** (the strongest invariant
this module pins): a wrapped call-welcome envelope MUST
surface as `None` (NOT `Some(welcome_bytes_misparsed_as_kp)`)
when fed to `try_unwrap_call_kp_forward`, and vice versa.
The
[`welcome_and_kp_forward_round_trips_do_not_cross_dispatch`](../../crates/lakko-contacts/src/call_kp_forward.rs)
test asserts this in both directions. The owner-side
polling loop's dispatch order (welcome first, KP-forward
second, intro-message fallthrough last) is therefore
unambiguous — no payload can match two branches at once.

Tests (10 cases in
[`lakko_contacts::call_kp_forward::tests`](../../crates/lakko-contacts/src/call_kp_forward.rs)):
`wrap_then_unwrap_round_trips`,
`unwrap_returns_none_for_intro_message_plaintext`,
`unwrap_returns_none_for_call_welcome_envelope`,
`unwrap_rejects_truncated_payload`,
`unwrap_accepts_exactly_min_payload_len`,
`empty_plaintext_returns_none_not_error`,
`domain_separator_is_25_bytes_and_ends_with_null`,
`separator_distinct_from_sibling_domains`,
`wrap_preserves_arbitrary_kp_bytes_verbatim`,
`welcome_and_kp_forward_round_trips_do_not_cross_dispatch`.
lakko-contacts test count: 107 → 117.

FFI exposure: wasm-bindgen
`wrapCallKeyPackage(kpBytes)` /
`tryUnwrapCallKeyPackage(plaintext)` /
`callKeyPackageMinPayloadLen()`; UniFFI
`wrap_call_keypackage(kp_bytes)` /
`try_unwrap_call_keypackage(plaintext)` /
`call_keypackage_min_payload_len()`. lakko-ffi-uniffi test
count: 71 → 76 (5 new cases — round-trip, intro-message
fallthrough, cross-dispatch guard from welcome envelope,
truncated-payload rejection, floor-constant pin).

### C-34: Owner-side incoming-envelope dispatcher pins dispatch order

The owner's stranger-inbox polling loop receives one HPKE-
decrypted plaintext at a time and has to route each to the
right handler: `Group::join_from_welcome` for first-joiner
Welcomes (C-32), `Group::add_member_from_wire` for
subsequent-joiner KeyPackages (C-33), or the ordinary intro-
message renderer (ADR-0005 scenario A). Doing this dispatch
manually across web / iOS / Android opens three separate
opportunities for a drift bug to misroute a payload —
e.g. running the KP-forward unwrap first and then falling
through to the welcome unwrap (which would still produce
the right answer for today's envelope set but could not
absorb a future third kind without re-aligning all three
platforms in lock-step).

Phase 3c3-incoming-dispatcher pins the dispatch order in
**one** place — the
[`dispatch_incoming_envelope`](../../crates/lakko-contacts/src/incoming_envelope.rs)
free function. The contract:

1. `try_unwrap_call_welcome` (the C-32 first-joiner branch)
2. `try_unwrap_call_kp_forward` (the C-33 subsequent-joiner
   branch)
3. **Intro-message fallthrough** — surfaces the original
   plaintext verbatim (including the degenerate
   empty-bytes case, which is a valid empty intro message).

Returns a discriminated `IncomingEnvelope<'a>` enum with
zero-copy borrow semantics — the dispatcher itself never
allocates; the caller decides whether to clone bytes when
forwarding them to `Group::join_from_welcome` or
`Group::add_member_from_wire`.

**Load-bearing properties** the test suite pins:

- Each call-side envelope routes to its specific variant
  (`dispatches_call_welcome_envelope`,
  `dispatches_call_kp_forward_envelope`).
- A plain intro message — even when empty — falls through
  to the `IntroMessage` variant, NOT to a truncated-call
  error (`dispatches_intro_message_for_plain_text`,
  `dispatches_intro_message_for_empty_plaintext`).
- A truncated call envelope (domain separator matched,
  payload below the kind's floor) surfaces as
  `Err(CallWelcomeError::TruncatedPayload)` — caller logs
  + drops the envelope rather than mistaking it for an
  intro message (`truncated_welcome_envelope_surfaces_as_error`,
  `truncated_kp_forward_envelope_surfaces_as_error`).
- The dispatcher is **zero-copy** on both the call-welcome
  branch and the intro-message branch — `slice.as_ptr()`
  equality between the returned slice and the appropriate
  offset of the input is asserted by
  `dispatch_is_zero_copy_on_intro_path` and
  `dispatch_is_zero_copy_on_call_welcome_path` (and stays
  unsafe-free under the crate-wide
  `#![forbid(unsafe_code)]`).

Tests (9 cases in
[`lakko_contacts::incoming_envelope::tests`](../../crates/lakko-contacts/src/incoming_envelope.rs)).
lakko-contacts test count: 117 → 126.

FFI exposure: wasm-bindgen
`dispatchIncomingEnvelope(plaintext) -> IncomingEnvelope`
(class with `kind: IncomingEnvelopeKind` getter +
`payload: Uint8Array` getter; `IncomingEnvelopeKind`
exported as a numeric enum with stable values `0`
(`CallWelcome`), `1` (`CallKeyPackage`), `2`
(`IntroMessage`) matching the dispatch order); UniFFI
`dispatch_incoming_envelope(plaintext) ->
FfiIncomingEnvelope` (record with `kind:
FfiIncomingEnvelopeKind` + `payload: Vec<u8>`). The
owned-bytes shape at the FFI boundary frees the platform
caller from Rust borrow-lifetime concerns. lakko-ffi-uniffi
test count: 76 → 81 (5 new cases — all four dispatch
branches + a truncated-welcome rejection sentinel).

### C-35: End-to-end link-call wire chain holds across both 1×1 and multi-party paths

C-24..C-34 each pin **one** invariant of the link-call
flow. C-35 is the contract that the **full chain holds
together**: starting from the owner composing a
capability and ending with all participants deriving
byte-identical SFrame seeds at the same epoch, every
property test along the way agrees with every other. A
silent break at any seam (composer ↔ acceptor ↔ welcome-
ferry ↔ dispatcher ↔ MLS) surfaces as a single named
failure rather than as a UX regression a human has to
trace back through three platforms.

Two integration tests in
[`lakko_calls::link_invite::tests`](../../crates/lakko-calls/src/link_invite.rs)
back this contract — both pure Rust, no HTTP / FFI /
network mocks:

**`link_call_full_e2e_via_welcome_envelope`** —
1×1 link call. Walks the seven-step chain a production
deployment would drive for the simplest case:

  1. Owner composes the capability (C-24..C-26).
  2. Joiner runs `accept_call_invite` (C-27 sets up the
     call-key seed agreement).
  3. Joiner wraps `welcomeForOwner` via
     `wrap_call_welcome` (C-32).
  4. Owner runs `dispatch_incoming_envelope` on the
     wrapped plaintext — asserts the `CallWelcome`
     variant fires (C-34).
  5. Owner runs `Group::join_from_welcome` on the
     unwrapped payload.
  6. Both sides build `GroupCallSession`s — call-key
     seeds MUST agree byte-for-byte (the full C-27
     contract from the dispatcher's output down).
  7. Bidirectional SFrame round-trip exercises the media
     path (C-2 + C-3 hold across the full bootstrap).

**`link_call_multi_party_e2e_via_kp_forward_envelope`** —
3-participant link call. Walks the subsequent-joiner KP-
forward chain a production deployment would drive when a
third party joins an already-established 2-party group:

  1. Set up the A↔B 1×1 base group via the same flow
     (capability `max_uses = u32::MAX` so the row stays
     consumable).
  2. Charlie mints a fresh KP + wraps via
     `wrap_call_kp_forward` (C-33).
  3. Bob (current committer) runs
     `dispatch_incoming_envelope` on the wrapped
     plaintext — asserts the `CallKeyPackage` variant
     fires (C-34's discrimination contract holds for
     KP-forward as well as Welcome).
  4. Bob runs `Group::add_member_from_wire(kp)` on the
     existing call group — produces (commit, welcome).
  5. Bob wraps the Welcome via `wrap_call_welcome` to
     ferry it back to Charlie (the KP-forward and
     welcome envelopes compose).
  6. Charlie's polling loop dispatches the wrapped
     welcome → `CallWelcome` → `Group::join_from_welcome`
     → lands at leaf 2.
  7. Alice processes Bob's commit (broadcast side-output
     of step 4) → her group state advances to the same
     new epoch.
  8. All three participants build `GroupCallSession`s →
     **all three CallKey seeds agree byte-for-byte**.

The multi-party test is the strongest property the audit
pack pins: it proves that **the cryptographic state
machine holds together across a 3-party rendezvous over
the wrapped-envelope transport**, not just at each
individual seam. Any future regression to a single
helper, the dispatcher's order, or the MLS-side
`add_member_from_wire` would surface here as a single
named failure that names the specific phase it broke at.

Also covered as part of the same module:
- `dispatcher_treats_plain_intro_message_as_intro_message`
  — sanity that a free-text intro doesn't mis-dispatch
  to a call variant when the chain is exercised end-to-
  end.
- `dispatcher_rejects_truncated_welcome_in_full_chain` —
  the truncation rejection path holds end-to-end, not
  just at the unwrap-helper level.

lakko-calls test count: 72 → 77 (5 new E2E + sentinel
cases).

### C-36: Owner-handover preserves call-key chain across mid-call committer leave

The multi-party E2E in C-35 covers the happy path where
the first joiner (Bob) remains the committer for the
call's lifetime. C-36 pins what happens when **the
committer leaves mid-call**: the capability's
`owner_account_id` designates the owner as the
**primary committer-of-record**, so when Bob
disconnects, Alice (the owner) takes over by emitting
the Remove commit for Bob's stale leaf and is then the
member that subsequent joiners' KP-forwards route to.

The contract:

  1. Owner is **always a member** of the call group by
     construction — added via her embedded KP when the
     first joiner accepted (C-26 + C-27 set up).
  2. If the current committer disconnects, the **owner
     drives the Remove commit** to evict the stale
     leaf (OpenMLS does not permit self-removal via
     `remove_members`, and a disconnected client can't
     drive `leave_group_via_self_remove` anyway —
     someone else must commit the leave).
  3. After the Remove commit, subsequent KP-forward
     envelopes (C-33) flow to the **owner** as the new
     committer; she runs `add_member_from_wire` +
     ferries the resulting Welcome (C-32) back to the
     new joiner.
  4. Across the leave + add commits the MLS exporter
     produces a **fresh CallKey seed each epoch** — the
     pre-leave seed is no longer derivable post-handover
     (forward secrecy across the leave + add commits).

The integration test
[`link_call_owner_handover_after_committer_leaves`](../../crates/lakko-calls/src/link_invite.rs)
exercises the full sequence:

  - Set up A↔B↔C via the multi-party flow (C-35 baseline).
  - Alice removes Bob's leaf (owner-driven kick — Bob
    has disconnected); Charlie's view advances.
  - Dave arrives with a KP-forward envelope; the
    dispatcher (C-34) routes it to Alice.
  - Alice runs `add_member_from_wire(dave_kp)` +
    broadcasts the commit to Charlie; Alice ferries the
    Welcome to Dave.
  - Assertions: epoch advances past the leave; A, C, D
    all at the same post-handover epoch; all three
    derive byte-identical CallKey seeds; the pre-leave
    seed Bob held differs from the post-handover seed
    (forward secrecy assertion).

The test surfaced a load-bearing **MLS bug** in
`lakko-mls::Group::join_from_welcome`: it was using
`MlsGroupJoinConfig::default()` which sets
`use_ratchet_tree_extension = false`. Members who joined
via Welcome had a group state without the ratchet tree
extension flag, so when one of them later acted as the
committer (the exact owner-handover scenario), their
freshly-emitted Welcome did not carry the embedded
ratchet tree → the new joiner's
`join_from_welcome` failed with `MissingRatchetTree`.
The fix flips the join-config flag on, matching the
`MlsGroupCreateConfig` setting the founder uses; the
two now stay in lock-step so a chain of welcome-then-
add-then-welcome propagation survives any number of
hops. This is documented inline in
`lakko-mls::Group::join_from_welcome` and is a one-line
change with workspace-wide test coverage backing it.

lakko-calls test count: 77 → 78.

### C-37: SFrame keying math + AEAD pipeline pinned

Phase 3a5's SFrame primitive
([`lakko_calls::sframe`](../../crates/lakko-calls/src/sframe.rs))
implements **the keying half of RFC 9605** — the layers
between the MLS exporter and the per-frame AEAD operation
— end-to-end. The wire-level frame header serialization
(the RFC 9605 §4.3 `Config|KID|CTR` layout; see C-38) was
out of scope for this primitive and tracked as G-26; it
has since landed in `lakko_calls::header`, §4.3-conformant.

**Pinned invariants** the test suite asserts:

1. **CallKey derivation via MLS exporter.** Both
   participants on the same call epoch agree on the same
   32-byte seed:

   ```
   CallKey.seed = mls_exporter("lakko/call/v1", call_id, 32)
   ```

   The label is a frozen ASCII string
   (`SFRAME_EXPORT_LABEL`); any change is a breaking
   protocol bump for every existing call surface. Test:
   `call_key_derives_32_bytes`.

2. **ParticipantKey derivation via HKDF.** A receiver
   re-derives a sender's ParticipantKey from the shared
   CallKey + the sender's MLS leaf index:

   ```
   participant_key  = HKDF-SHA-256(call_key_seed, info = "lakko/sframe/v1/key"   || sender_leaf_be, len = 32)
   nonce_salt       = HKDF-SHA-256(call_key_seed, info = "lakko/sframe/v1/nonce" || sender_leaf_be, len = 12)
   ```

   The two `info` strings carry the SFrame role of the
   bytes so an attacker who confuses contexts (key-as-
   nonce, nonce-as-key) hits a distinct HKDF context.
   Test: `participant_keys_differ_across_leaves`.

3. **Nonce composition per RFC 9605 §4.4.** The 12-byte
   AEAD nonce is built as `salt XOR (frame_counter as u32
   BE in the low 4 bytes)` — the leading 8 bytes are
   per-participant entropy from the salt, the trailing 4
   bytes XOR in the counter. Same construction every
   compliant SFrame implementation uses; deterministic
   across implementations. Inline test:
   `participant_key_round_trips` exercises seal→open
   across an A↔B pair re-deriving from the same seed.

4. **AEAD ciphersuite ChaCha20-Poly1305** (RFC 9605
   §4.5.3 — cipher suite identifier 3 in the RFC's IANA
   registry). The current implementation is keyed only on
   this suite; FIPS-track AES-256-GCM (cipher suite 4)
   would land as a separate `Algorithm` enum variant
   without changing the surrounding keying math.

5. **Counter exhaustion is a typed error, not a silent
   nonce reuse.** The per-participant counter is `u32`,
   pinned to `MAX_FRAME_COUNTER = u32::MAX`. Sealing the
   2³² nd frame returns `Error::CounterExhausted`; the
   caller is expected to trigger a fresh MLS commit
   (which advances the group epoch → fresh CallKey via
   the exporter → fresh ParticipantKey → counter resets
   to 0). At 30 fps video this is ~1.7 years per
   participant per epoch; in practice every membership
   change resets it long before the ceiling is reached.
   Test: a property test exercising the bump → exhaust
   path lands with the wire-header sprint (currently the
   error path is reached by reading the counter directly,
   not by burning 2³² seals in a unit test).

6. **AAD authentication holds.** The seal/open AEAD path
   binds an arbitrary AAD byte string into the tag; a
   receiver that opens with a different AAD MUST surface
   a `Crypto` error rather than a wrong plaintext. Test:
   `wrong_aad_rejects_open`. Mismatched counter (which
   composes into a different nonce) also rejects: test
   `wrong_counter_rejects_open`.

7. **Diagnostic fingerprint via BLAKE3.** The
   `call_key_fingerprint(&CallKey) -> [u8; 32]` helper
   produces a deterministic non-secret identifier of the
   call key for "matching SAS" UI affordances. Same hash
   on both peers when they hold the same seed. Test:
   `fingerprint_is_stable`.

The full pipeline (MLS exporter → CallKey → ParticipantKey
→ AEAD) is exercised end-to-end inside the link-call
integration tests
(`link_call_full_e2e_via_welcome_envelope`,
`link_call_multi_party_e2e_via_kp_forward_envelope`,
`link_call_owner_handover_after_committer_leaves`) which
all assert byte-identical CallKey seed agreement after
each MLS state transition — the keying math contract
holds across single-joiner, multi-party, and committer-
leave-handover scenarios.

### C-38: RFC 9605 §4.3 SFrame wire-header round-trip pinned

The companion of C-37. SFrame's keying math (C-37) lives in
[`lakko_calls::sframe`]; the wire-header layout — the
RFC 9605 §4.3 `X|KKK|Y|CCC` config byte + inline `KID<=7` /
`CTR<=7` packing Lakko prepends to each AEAD ciphertext — lives in
[`lakko_calls::header`](../../crates/lakko-calls/src/header.rs).
**Update (2026-07):** the header is RFC 9605 §4.3-conformant. An
earlier revision used a Lakko-custom `R|LEN|X|KID` layout that a
2026-06-30 finding flagged as non-conformant; the §4.3 rewrite
landed and is **proven byte-for-byte against RFC 9605 Appendix C.1's
published vectors** (`header.rs::rfc_9605_appendix_c1_header_vectors`),
so `(0,0)` is now the single byte `00` exactly as the RFC specifies.
Since the header IS the AEAD AAD (ADR-0013) and it is now
spec-conformant, a Lakko frame is interoperable with any other RFC
9605 stack (browser-native, mlspp, Cisco libsframe) sharing the
key + nonce derivation; only a live cross-platform / external-stack
RTP round-trip over a deployed SFU remains (G-26). This invariant
pins the §4.3 wire shape, which is what guarantees the five Lakko
stacks agree byte-for-byte (C-55).

**Pinned invariants** the test suite asserts:

1. **First-byte bit packing follows RFC 9605 §4.3**
   (network bit order, MSB first) — `X | KKK | Y | CCC`,
   inline KID/CTR for values `<= 7`:

   ```
   Bit:   7 6 5 4 3 2 1 0
   Field: X K K K Y C C C
   ```

   - **X** = extended-KID flag (0 → KID inline in bits 6-4;
     1 → KID length minus 1 in bits 6-4, KID bytes follow).
   - **KKK** = KID (when X=0, range 0..=7) or KID-length-1
     (when X=1).
   - **Y** = extended-CTR flag (0 → CTR inline in bits 2-0;
     1 → CTR length minus 1 in bits 2-0, CTR bytes follow).
   - **CCC** = CTR (when Y=0, range 0..=7) or CTR-length-1
     (when Y=1).

   Tests: `first_byte_bit_packing_pinned` walks inline KID /
   inline CTR / extended-KID KLEN / extended-CTR CLEN ∈
   0..=7 and asserts the packed byte; `rfc_9605_appendix_c1_header_vectors`
   pins the RFC 9605 Appendix C.1 published vectors verbatim.

2. **Round-trip stability across the full numeric range.**
   `SframeHeader { kid, ctr }.encode().pipe(decode)` is
   the identity for every `(kid, ctr) ∈ [0, u64::MAX]²`
   the tests exercise — basic-inline, extended-KID,
   extended-CTR, both extended, and the maximum
   `(u64::MAX, u64::MAX)` boundary. Tests:
   `round_trip_zero_zero`, `round_trip_basic_kid_basic_ctr`,
   `round_trip_basic_kid_extended_ctr`,
   `round_trip_extended_kid`.

3. **Concrete wire layouts pinned as constants.** Six
   spec-derived byte sequences are pinned literally in
   `pinned_wire_layouts` so a refactor that shifts a
   bit-boundary lights up loudly:

   - `(kid=0, ctr=0)` → `[0x00]`
   - `(kid=5, ctr=0x42)` → `[0x58, 0x42]`
   - `(kid=0, ctr=0x0100)` → `[0x09, 0x01, 0x00]`
   - `(kid=8, ctr=0)` → `[0x80, 0x08]`
   - `(kid=0x0100, ctr=0)` → `[0x90, 0x01, 0x00]`
   - `(kid=u64::MAX, ctr=u64::MAX)` → 17 bytes with
     first-byte `0xFF` and 16 trailing `0xFF`s.

4. **Truncation handling.** Three rejection paths are
   typed: `HeaderError::Truncated { needed, had }` carries
   the byte budget the decoder expected vs. observed.
   Tests: `decode_rejects_empty_buffer`,
   `decode_rejects_truncated_ctr`,
   `decode_rejects_truncated_extended_kid`.

5. **`MAX_HEADER_LEN = 17`** is a public constant — every
   call site that pre-allocates a scratch buffer for an
   SFrame frame uses this rather than re-deriving it.
   Sanity test inside `encoded_length_predictable`
   confirms the bound holds for the worst-case
   `(u64::MAX, u64::MAX)` input.

The wire-header module is **standalone** for now —
[`crate::sframe::ParticipantKey::seal`] still returns the
typed `(u32 counter, Vec<u8> ciphertext)` tuple instead of
prepending `SframeHeader::encode()` to its output. The
integration step lands in `S-3-B-routing-media` (when the
SFU needs to read KID bytes off the wire to route by
participant without decrypting). At that point the
`SframeHeader { kid, ctr }` becomes the natural arg/return
shape of `seal` / `open`, and the typed-tuple form
becomes deprecated.

### C-39: SFU signaling layer enforces zero-payload-inspection + cross-call isolation

[`services/sfu`](../../services/sfu) — the Rust SFU
crate per [ADR-0011](../adr/0011-rust-sfu.md) — has now
shipped its **signaling half** (S-3-B-routing-signaling
+ S-3-B-tests in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md)
Group B). The media half (str0m-backed RTP fanout) is
gated on the WebRTC pod sprint and stays open as
`S-3-B-routing-media`. This invariant set pins what the
signaling layer already guarantees, independently of
whether real RTP is being forwarded.

**Pinned invariants** the test suite (32 unit + 9
integration) asserts:

1. **Admission requires a verified `lakko-call-jwt`.**
   The WS-upgrade handler verifies the inbound token
   against the JWKS cache pulled from
   `auth.lakko.me/.well-known/jwks.json`. Failures —
   bad signature, unknown kid, expired token — return
   HTTP 401 and increment `lakko_sfu_peers_rejected_total`
   **before** the WS upgrade completes. Tests:
   `ws_admission_with_bad_jwt_is_rejected`,
   `verify_rejects_expired`, `verify_rejects_unknown_kid`,
   `verify_rejects_tampered_signature`.

2. **The SFU never inspects the `signal-relay` payload.**
   The `payload_b64` field of `ClientFrame::SignalRelay`
   is forwarded byte-identical from sender to recipient(s).
   No JSON decode, no SDP parse, no SFrame header peek.
   This is the wire-level expression of the "SFU is
   assumed compromised" threat-model line in
   [`threat-model.md`](./threat-model.md) — the audit-pack
   property that lets clients exchange MLS Welcome
   ferries, SFrame key envelopes, and any future
   Lakko-side blob without the SFU operator gaining a
   meaningful surface. Test:
   `signal_relay_unicast_only_reaches_target` asserts
   `payload_b64 == "AAAA"` survives the round-trip
   byte-exact.

3. **Cross-call isolation is structural, not policy —
   across BOTH the signaling AND the media plane.**
   *Signaling:* peer A in call-X cannot receive
   `peer-joined` / `peer-left` / `signal-relay` frames
   originating in call-Y. The router's broadcast filter is
   keyed on the `call_id` field of the inbound JWT — a peer
   that attempts to send to a peer in a different call gets
   `ErrorCode::UnknownPeer` back; the SFU does not
   "helpfully" cross-route by account id even when the same
   account is in both calls. Test: `cross_call_isolation`.

   *Media (RTP):* the media plane is the same — a frame from
   a publisher in call-X is forwarded ONLY to peers in
   call-X, never to peers in call-Y, even though they share
   the one process-wide `MediaRouter`. The binding `call_id`
   is the **server-authoritative** JWT claim (set on the
   `MediaPeer` from `Peer::call_id` at the peer's first SDP
   offer — `media.rs::set_call_id`, never client-supplied
   SDP), and both `fanout_media_data` (RTP forwarding) and
   `propagate_new_tracks_in` (downstream-track opening) skip
   any non-origin peer whose `call_id` differs from the
   origin's; the renegotiation path is transitively scoped
   because it only acts on already-opened (same-call)
   `TrackOut` entries. Fail-closed: an origin with no bound
   call forwards to nobody. (This media-plane enforcement
   was added as a security fix — before it, the fanout
   walked every router peer with no `call_id` filter, so a
   peer admitted to any call received the SFrame-encrypted
   media of all concurrent calls; SFrame kept the payload
   confidential, but the isolation invariant — a metadata /
   DoS surface — was violated.) Tests:
   `media::tests::fanout_never_crosses_calls` +
   `fanout_unbound_origin_forwards_to_nobody` (unit) +
   `fanout_never_crosses_calls_over_the_live_sfu`
   (live e2e). `lakko-security`: APPROVE.

4. **Account ids are pseudonymised in every outbound
   frame and every log line.** The `account_pseudonym`
   field on `ServerFrame::PeerJoined` is the first 8 hex
   characters of the JWT `sub`. The full account id never
   crosses the SFU's outbound surface; tracing log lines
   from `services/sfu/src/signaling.rs` use the same
   8-char truncation. Test: `pseudonymise_truncates`
   (unit) + assertions inside
   `two_peers_in_same_call_see_each_other` (integration).

5. **Broadcast respects sender exclusion.** A `signal-
   relay` with `to = RelayTarget::Call` reaches every
   peer in the call **except the sender**. Symmetric for
   `peer-joined` / `peer-left` — the joining / leaving
   peer never receives their own announcement. Test:
   `signal_relay_broadcast_skips_sender` (integration) +
   `join_broadcast_reaches_existing_peers_only` (unit).

6. **Backpressure is local, never global.** A single
   slow recipient's bounded mpsc filling does NOT stall
   broadcast fanout to the other peers — the router
   logs the drop, increments the relevant metric, and
   continues with the remaining recipients. Test inside
   `router::tests::cross_call_isolation` exercises the
   per-recipient delivery loop; the bounded mpsc
   capacity is asserted at compile time
   (`PEER_OUTBOUND_CAPACITY ∈ [64, 1024]`).

7. **Peer departure is observed by the rest of the
   call.** A WS-disconnect (clean Close frame or
   transport error) triggers a `ServerFrame::PeerLeft`
   broadcast to every remaining peer in the same call
   before the SFU drops the peer's registry entry.
   Test: `disconnect_propagates_peer_left` (integration).

The signaling layer is a properly verified contract.
The SFU's media-layer responsibilities — RTP track
forwarding and the `TracksIn`/`TracksOut` mapping that
drives SDP renegotiation when a peer joins — have now
**also** landed (S-3-B-routing-media steps 1–5 +
S-3-B-tracks steps 1–3b). Simulcast layer selection per
receiver and TWCC aggregation remain deferred follow-ups.

**SFrame-passthrough property the existing fanout
preserves.** `services/sfu/src/media.rs::fanout_media_data`
walks every non-origin peer and calls
`MediaPeer::forward_media_data(&data)`, which is a thin
wrapper over `Rtc::writer(mid).match_params(params).write(
pt, network_time, time, data.data.clone())`. The
`data.data` byte buffer is the inner SFrame-encrypted
payload, forwarded **verbatim** into the destination
peer's str0m state — no inspection, no parsing, no
SFrame-header peek. str0m subsequently SRTP-encrypts the
buffer for the per-peer transport hop, but the inner
SFrame ciphertext stays opaque from origin client to
destination client. The variant-typed `MediaForward`
outcome (`Sent` / `NoWriter` / `NoCodecMatch` /
`WriteFailed`) keeps observability counters
disambiguated — a future metrics layer will count each
skip reason separately so the audit-time question "is
the SFU dropping all my media or refusing it for a
codec reason?" is answerable without reading bytes.
Unit-tested in `media::tests::{forward_to_peer_without_writer_returns_no_writer,
fanout_with_single_peer_is_noop, fanout_skips_origin_peer,
fanout_empty_pending_is_noop}`.

**The audit loop on this property is now closed on the
integration-test side.**
`services/sfu/tests/synthetic_client_e2e.rs::two_clients_cross_fanout_forwards_media_verbatim`
(S-3-B-tracks step 3b) drives two real `str0m::Rtc` peers
through full DTLS + ICE against the production
`spawn_media_task`: a publisher A and a subscriber B both
reach `Event::Connected`; the SFU-initiated renegotiation
(`drive_renegotiations` → `ServerFrame::SdpOffer`) opens
B's downstream track; A then writes a distinctive 80-byte
payload via `Rtc::writer(mid).write(...)`, and the test
asserts the **exact same bytes** surface as an
`Event::MediaData` at B. A pass proves the SFU received
A's RTP, resolved B's downstream mid via the `tracks_out`
map, and forwarded the opaque payload verbatim — without
inspecting, parsing, or peeking the SFrame header.

**Fanout fairness (Jun 2026).** `fanout_media_data` now
returns a `FanoutOutcome { sent, no_writer, no_codec_match,
write_failed }` tally — one increment per `(frame ×
destination peer)` — and mirrors it into the
`lakko_sfu_media_frames_forwarded_total` +
`lakko_sfu_media_frames_dropped_total{reason}` Prometheus
counters, so a starved subscriber (a spike in `no_writer`)
is observable rather than silently dropped. The fairness
invariant — for one origin frame in an N-peer call, every
non-origin peer is visited exactly once, the origin never —
is pinned by `fanout_visits_every_non_origin_peer_for_three_plus`
(asserts `attempted() == N - 1` for N ∈ {3,4,5}) +
`fanout_tally_is_per_frame_times_non_origin_peers`. The
**still-deferred** work is simulcast-layer selection on the
downstream offer (RID / dependency-descriptor extraction +
per-receiver layer choice under bandwidth estimation) — a
wire-touching feature that needs simulcast-capable test
clients; it does not change the verbatim-forwarding property
this section pins.

## Channel-broadcast invariants (Phase 4)

These invariants pin the contract between
[`crates/lakko-channels`](../../crates/lakko-channels) (the
client-side broadcast crypto per ADR-0010) and
[`services/channels`](../../services/channels) (the
backend that stores opaque ciphertext + relays it between
publisher and subscribers). The C-numbering continues
from the call-side invariants above; the modules are
distinct (channels are a broadcast surface, not a call
surface), but the audit-pack convention is one source of
truth per crypto subsystem.

### C-40: services/channels publisher path treats every byte as opaque

The publisher endpoints —
`POST /v1/channels` (registration),
`POST /v1/channels/:channel_id/posts` (publish),
`GET  /v1/channels/:channel_id/posts?after=&limit=`
(cursor-paginated fetch) — **never** decode the
[`ChannelPost`](../../crates/lakko-channels/src/post.rs)
they carry. No signature verify, no AEAD open, no
ciphertext-shape inspection.

**Pinned invariants** the test suite asserts:

1. **The `lakko-channels` Rust crate is NOT in the
   production dep graph.** `services/channels/Cargo.toml`
   pulls `lakko-channels.workspace = true` **only as a
   `dev-dependency`** — used by the integration test for
   one compile-time cross-check on
   `SEED_ENVELOPE_LEN`, never linked into the release
   binary. The audit-pack property "the server cannot
   parse a ChannelPost even if it tried" is therefore
   structural, not policy-bound.

2. **Wire-level byte-identical passthrough.** Integration
   test `payload_round_trips_byte_identical` publishes
   all 256 distinct byte values (0x00..0xFF), fetches via
   the cursor endpoint, base64-decodes the returned
   `payload_b64`, and asserts a bit-exact equality. Any
   future refactor that inserts a transformation (
   compression, normalisation, "helpful" re-encoding)
   trips this test loudly.

3. **Per-post body cap is length-only, not
   content-based.** `LAKKO_CHANNELS_MAX_POST_BYTES`
   (default 17 MiB — leaves ~1 MiB SFrame/AEAD overhead
   over the `lakko-channels` 16 MiB plaintext ceiling)
   is enforced at the gate before any storage write. A
   post body that exceeds the cap is rejected as
   `post-too-large` without the server ever touching
   the bytes.

4. **Cursor-only fetch — no body indexing, no
   content-based search.** `?after=<cursor>` is the only
   query knob; the server has no notion of "search
   posts by content" because content is opaque to it.
   Subscribers do their own client-side indexing after
   decryption.

### C-41: services/channels subscribe handshake opacity holds

The subscribe endpoints —
`POST /v1/channels/:id/subscribers` (register pubkey),
`GET  /v1/channels/:id/subscribers` (list for publisher
fan-out),
`POST /v1/channels/:id/subscribers/:pubkey/envelope`
(publisher delivers a per-subscriber HPKE-sealed seed
envelope),
`GET  /v1/channels/:id/subscribers/:pubkey/envelope`
(subscriber pulls latest) — match the same opacity
contract as the publisher path. The server never
performs a Diffie-Hellman operation against a registered
`subscriber_pubkey`; it only matches identities in the
registry.

**Pinned invariants:**

1. **Envelope length is structurally pinned.**
   `services/channels/src/api.rs::EXPECTED_ENVELOPE_LEN
   == lakko_channels::SEED_ENVELOPE_LEN` is asserted
   compile-time inside `tests/integration_api.rs`. A
   bump in either crate without the matching bump in
   the other fails the build.

2. **Wrong-length envelope rejected at the gate.**
   `deliver_envelope_with_wrong_length_is_400` —
   a 79-byte body against the 80-byte target returns
   400 `bad-envelope-len` without touching the
   envelope store.

3. **Per-subscriber wire round-trip is byte-identical.**
   `subscribe_then_list_then_deliver_then_fetch`
   delivers bytes 0x00..0x4F, decodes the
   base64-wrapped fetch response, and asserts bit-exact
   equality. Same wire-level opacity proof as C-40 but
   for the subscriber path.

4. **Unsubscribed pubkey cannot receive an envelope.**
   `deliver_to_unsubscribed_pubkey_is_404` —
   `POST .../subscribers/<unregistered>/envelope`
   returns 404 `unknown-subscriber` without storing the
   envelope. Stops a misbehaving publisher from
   silently spraying envelopes against arbitrary
   pubkeys.

5. **`SubscriberId` is the subscriber's X25519 public
   key, used only as an identifier — never as ECDH
   input on the server side.** The server's storage
   surface (`storage.rs`) is the only place
   `SubscriberId` flows through; no path constructs
   a `lakko_crypto::agreement::PublicKey` from it. The
   `subscriber_pubkey` ends up at the publisher's
   client where `lakko_channels::subscribe::seal_seed`
   does the actual sealing.

### C-42: rotation is atomic — either every envelope lands or none does

`POST /v1/channels/:id/rotate` bumps the channel's
`current_epoch` **and** replaces the per-subscriber
envelope for every entry in the batch under a single
write lock. The implementation pre-validates every
entry's length + registration before committing any
mutation; on any failure, the epoch counter stays put
and the previous envelopes survive.

**Pinned invariants:**

1. **Two-pass commit.** `storage.rs::rotate`'s first
   loop validates every batch entry's envelope length;
   the second loop walks the registered-subscriber set
   under the write lock. Mutation runs only after
   both passes complete. Source-level enforcement,
   not just test-checked.

2. **No torn rotations.**
   `rotate_aborts_on_wrong_envelope_length` —
   a one-element batch with a 79-byte envelope (off by
   one) returns 400 `bad-envelope-len`; the epoch
   counter remains 0 and the previous envelope is
   unchanged. Same property covered by
   `rotate_rejects_unsubscribed_recipient` — one
   unsubscribed pubkey in the batch aborts the whole
   commit.

3. **Empty batch still advances the epoch.**
   `rotate_empty_batch_still_bumps_epoch` — a scheduled
   tick on a quiet channel must still increment the
   counter so a future subscriber that comes online
   sees "stale state, refresh me" via
   `GET /v1/channels/:id/epoch`. ADR-0010 §"Epoch
   boundaries" lists this as the expected behaviour.

4. **`current_epoch` is monotone + saturating.** The
   counter is a `u32` with
   `saturating_add(1)` on bump — at 1 rotation/second
   (a pathological worst case) the counter takes ~136
   years to wrap, well past the cryptographic lifetime
   of any reasonable identity key.

5. **Genesis epoch is 0.** `genesis_epoch_is_zero`
   pins the starting value so the
   `lakko_channels::EpochId::GENESIS = 0` constant on
   the client side matches the server's view from the
   first byte of a channel's life.

6. **Batched fanout preserves atomicity (ADR-0030).** For
   the >50k-subscriber scale-out, rotation splits into a
   chunked, resumable **stage** phase
   (`POST /v1/channels/:id/rotate/stage`,
   `ChannelStore::stage_envelopes`) followed by a single
   atomic **commit**
   (`POST /v1/channels/:id/rotate/commit`,
   `ChannelStore::commit_rotation`). C-42 survives as
   "the commit is atomic" rather than "the one giant
   request is atomic":
   - **Staged ≠ served.** Staged envelopes live in a
     separate table (`channel_staged_envelopes`) / map that
     the read path (`fetch_envelope`) never consults — only
     `commit_rotation` promotes them — so the prepare phase
     is invisible to subscribers and no torn rotation is
     observable mid-stage
     (`staged_envelopes_are_not_served_before_commit`,
     `staged_envelope_is_not_served_before_commit`).
   - **Monotone target epoch.** Both stage and commit refuse
     unless `target_epoch == current_epoch + 1`
     (`StorageError::StaleTargetEpoch` → 409), so a replayed
     or out-of-order phase call cannot roll the epoch back
     or skip (`stage_rejects_wrong_target_epoch`,
     `commit_rejects_wrong_target_epoch`,
     `stage_with_wrong_target_epoch_is_409`,
     `commit_with_wrong_target_epoch_is_409`).
   - **Atomic commit.** The in-memory impl promotes +
     bumps + clears under a single write lock; the SQL impl
     does it in one `FOR UPDATE` transaction. An abandoned
     prepare leaves the live epoch + envelopes untouched
     (`abandoned_prepare_leaves_epoch_and_envelopes_untouched`).
   - **Idempotent + resumable.** Stage upserts keyed on
     `(channel, target_epoch, subscriber)`, so a dropped
     client re-sends missing chunks
     (`restaging_is_idempotent_and_merges_chunks`); the
     chunk is bounded (`MAX_STAGE_CHUNK_ENTRIES`, over-cap →
     413 `chunk-too-large`, `stage_chunk_over_cap_is_413`).
   - **Partial coverage (ADR-0030 OQ3).** A registered
     subscriber with no staged row keeps their prior-epoch
     envelope; a staged row whose subscriber was pruned
     mid-prepare is skipped, not an abort
     (`commit_partial_coverage_keeps_prior_envelope`).
   - **Opacity (invariant #2).** The staging table is
     ciphertext-only, exactly like `channel_envelopes`; the
     server never seals, opens, or parses a seed.

### C-43: FFI subscriber-side surface preserves the in-Rust opacity contract

The Phase 4b subscriber path is reachable from Swift /
Kotlin / TypeScript through
`crates/lakko-ffi-uniffi/src/channels.rs` and
`crates/lakko-ffi-wasm/src/channels.rs` (sprint G4-3c).
Five symbols cross the boundary:

| FFI surface | Maps to |
|---|---|
| `ChannelSubscriberKeypair.generate()` / `.fromSecret(bytes)` | `lakko_crypto::agreement::SecretKey::{generate, from_bytes}` |
| `ChannelSubscriberKeypair.secret()` / `.public()` | `SecretKey::to_bytes` + `SecretKey::public_key().to_bytes()` |
| `sealSubscriberSeedEnvelope(subPub, channelId, epoch, seed)` | `lakko_channels::subscribe::seal_seed` |
| `openSubscriberSeedEnvelope(kp, channelId, epoch, wire)` | `lakko_channels::subscribe::open_seed` |
| `channelSeedEnvelopeLen()` | `lakko_channels::SEED_ENVELOPE_LEN = 80` |

**Pinned invariants:**

1. **`ChannelSubscriberKeypair` is deliberately not
   `Debug`.** Same rule as `ChannelSigningKey` (C-37
   support invariant): a Debug impl would render the
   32-byte X25519 secret into panic backtraces. UniFFI
   tests use a hand-rolled `match` over the Result
   variants instead of `unwrap_err()` — see
   `crates/lakko-ffi-uniffi/src/channels.rs::tests::
   subscriber_keypair_from_short_secret_errors`.

2. **The seed crosses the FFI boundary only through
   `copy_to_bytes_for_export`.** A new public method on
   `ContentKeySeed` (`crates/lakko-channels/src/seed.rs`)
   is the **single** path that turns the in-Rust
   `SecretBytes<32>` handle into an owned `[u8; 32]` the
   FFI layer can hand back. Grep for `for_export` to
   audit every in-Rust use site — there is exactly one:
   the two FFI shims. The `_for_export` suffix mirrors
   `from_bytes_for_test`'s "non-default-handle" naming
   convention.

3. **Envelope length advertised matches the on-wire
   length.** `channelSeedEnvelopeLen()` always returns
   `80` (32-byte ephemeral pub + 32-byte sealed seed +
   16-byte AEAD tag). Both FFI shim tests
   (`seed_envelope_seal_open_round_trip{,_wasm_host}`)
   assert `wire.len() == channelSeedEnvelopeLen()` on
   every seal — a mismatch in the underlying constant
   would fail compilation through the
   `crates/lakko-channels::SEED_ENVELOPE_LEN` re-export.

4. **End-to-end seal → open → decrypt is covered on
   both FFI surfaces.** Tests
   `full_subscriber_e2e_publisher_to_subscriber` (UniFFI
   host) and `full_subscriber_e2e_wasm_host` (wasm host)
   drive the whole Phase-4b receive pipeline:
   `compose → seal seed → open envelope → decode →
   verifyAndDecrypt`. A regression in any of the five
   FFI entry points or the underlying
   `seal_seed` / `open_seed` crypto fails one of these
   two tests before binding regeneration.

5. **Wrong-recipient / wrong-epoch rejected at the FFI
   boundary, same error variant as in Rust.**
   `seed_envelope_wrong_recipient_fails` and
   `seed_envelope_wrong_epoch_fails` confirm that the
   underlying `ChannelError::AeadOpen` surfaces as
   `FfiError::Channels(_)` on Swift / Kotlin — callers
   branch on the variant, never on the message string.

### C-44: Production channel seed comes from the MLS exporter; the demo path is grep-clean

Sprint G4-mls-seed splits the FFI seed-source surface
into two explicitly-named functions so an auditor can
trivially separate production from demo paths:

| FFI surface | Source | Audit class |
|---|---|---|
| `channelSeedFromMlsExporterOutput(bytes)` | Caller-supplied 32 bytes from `Group.exportSecret(identity, channelExporterLabel(), channel_id, 32)` | **Production** |
| `newChannelSeedForTest()` | `getrandom(32)` | **Demo / single-publisher** |

**Pinned invariants:**

1. **Label is the wire contract.**
   `channelExporterLabel()` returns the ASCII string
   `"lakko/channel/v1"` (matches
   `lakko_channels::EXPORTER_LABEL_V1` byte-for-byte).
   A change to this label silently breaks every
   previously-derived seed — pin tests on both the
   UniFFI side
   (`channel_exporter_label_pinned`) and the wasm side
   (`channel_exporter_label_pinned_wasm_host`) assert
   the literal so any future schema bump that needs to
   re-key surfaces as `_v2` with the `_v1` label frozen
   for back-compat.

2. **Demo path is grep-clean.** Every non-MLS seed
   source on the FFI boundary carries `for_test` in its
   name. The only such entry today is
   `newChannelSeedForTest()`; an auditor running
   `rg "newChannelSeedForTest\|for_test" apps/` gets a
   complete list of demo call sites. Production code
   uses `channelSeedFromMlsExporterOutput` and is
   grep-greppable as such.

3. **End-to-end coverage on both FFI surfaces.**
   `mls_exporter_output_full_compose_round_trip`
   (UniFFI host) and
   `mls_exporter_output_full_compose_round_trip_wasm_host`
   (wasm host) drive the full pipeline: pretend the MLS
   exporter yielded 32 bytes → push through the
   production helper → use the result as the seed for
   `composeChannelPost` → `verifyAndDecryptChannelPost`
   round-trips the plaintext. A regression in either the
   helper or the underlying `ContentKeySeed::
   from_exporter_output` fails one of these two tests
   before binding regeneration.

4. **Length validation only — no domain separator at
   the FFI helper.** The helper is a `ContentKeySeed::
   from_exporter_output` wrapper that validates 32
   bytes and returns them. The actual domain separation
   lives in the MLS exporter (`label = "lakko/channel/v1"`,
   `context = channel_id`) and the in-crate constant
   `lakko_channels::seed::EXPORTER_LABEL_V1`. Two MLS
   groups invoking `exportSecret` with the same label
   and the same channel_id produce different output
   because the MLS exporter ratchet is keyed off each
   group's epoch secret; the seed at the channels layer
   is therefore the right cryptographic unit of
   isolation.

5. **`copy_to_bytes_for_export` is the single egress
   path.** The helper returns the seed as raw bytes for
   the FFI caller (Keychain / Keystore / IndexedDB-
   encrypted storage on the platform side). The audit
   grep marker `for_export` continues to enumerate every
   in-Rust use site that crosses the secret-bytes
   boundary — two today: this helper and the subscriber-
   side `openSubscriberSeedEnvelope`. Listed in C-43 #2.

## SFrame native-shim invariants (G3-sframe-native — closes G-28)

The native-shim stack lands the per-frame AEAD seal /
open on iOS + Android by subclassing libwebrtc's M125
`webrtc::FrameEncryptorInterface` / `FrameDecryptorInterface`
and trampolining through a flat C ABI into
`lakko_calls::sframe`. Six sub-sprints (N-1..N-6)
shipped six invariants:

### C-45: lakko-sframe-native C ABI is the only path into the AEAD seal from non-Rust callers

`crates/lakko-sframe-native` (N-1) is the bottom of
the stack — every iOS Obj-C++ and Android JNI call
into the SFrame seal / open path goes through this
crate's 11 `extern "C"` functions. No alternative
back-channel exists:

1. The iOS Obj-C++ shim
   (`apps/ios/Lakko/Calls/SFrameAttach.mm`) imports
   only the cbindgen-generated
   `lakko_sframe_native.h` header (via the stable
   mirror at `target/lakko-include/`); it never
   reaches into `lakko-calls` directly.
2. The Android JNI shim
   (`apps/android/lakko-sframe/src/main/cpp/sframe_jni.cc`)
   does the same — its sole Rust-side dependency is
   the cross-compiled
   `liblakko_sframe_native.a` archive linked into
   `libsframe_jni.so`.
3. The C++ N-3 bridge layer
   (`apps/shared/sframe-bridge/sframe-bridge.cc`)
   is the only consumer of the C ABI from C++;
   neither iOS nor Android side bypasses it.

This ABI surface is what audit tooling greps for; a
hypothetical alternative seal path in the iOS /
Android binaries would be visible as a missing
symbol against this header.

### C-46: Bad seed length is refused at every layer, no silent downgrade

The 32-byte requirement on the MLS exporter output
(`Group::export_secret("lakko/call/v1", call_id, 32)`)
is enforced symmetrically at five layers, each of
which refuses to proceed (returns null /
`BadSeedLength` / `false`) on a shorter or longer
input:

1. **Rust core**: `lakko_calls::sframe::CallKey::from_seed`
   takes `[u8; 32]`, so length is a compile-time
   constraint at the type system level.
2. **C ABI**: `lakko_sframe_call_key_new(seed_ptr,
   seed_len, ...)` checks `seed_len == CALL_KEY_LEN`
   before constructing.
3. **C++ bridge**: `LakkoFrameEncryptor::Create` /
   `LakkoFrameDecryptor::Create` return `nullptr`
   when the C ABI returned non-OK.
4. **iOS Obj-C++**:
   `+[LakkoSFrameAttach attachEncryptorToSender:
   seed:localLeaf:]` returns
   `LakkoSFrameAttachStatusBadSeedLength` on
   `seed.length != 32`, before the C++ side is
   even called.
5. **Android Kotlin**:
   `LakkoFrameEncryptor.create(seed, localLeaf)`
   returns `null` on `seed.size != 32`.

The path of least resistance through any layer
under bad input is a refused call, never a silently-
unencrypted media stream.

### C-47: Boot-time selector probe enforces invariant #4 (no fallback to non-E2EE)

Both platform shims run a one-shot capability probe
before the first call surface renders:

- **iOS** (`SFrameAttach.mm`): atomic
  `class_respondsToSelector` against
  `RTCRtpSender` / `RTCRtpReceiver` for the non-
  public selectors `setFrameEncryptor:` /
  `setFrameDecryptor:`. Cached tri-state result
  (`0 = unknown`, `1 = yes`, `2 = no`) in
  `g_probe_result`.
- **Android** (`SFrameAvailability.kt`):
  `Class.forName + getMethod` against
  `org.webrtc.RtpSender.setFrameEncryptor` /
  `RtpReceiver.setFrameDecryptor`. Cached
  `AtomicReference<Boolean?>`.

Both probes are **fail-closed**: a not-yet-run or
negative result returns `false` from
`isCallingAvailable`. The Swift `SFrameAvailability.shared`
+ Kotlin `SFrameAvailability` singletons run the
probe at the very top of `LakkoApp.init` (iOS) /
`MainActivity.onCreate` (Android) so the first
call-UI render already sees the correct value.

`CallSession.attachSFrame` re-checks
`isCallingAvailable` immediately before each call
start and aborts via `endLocal(reason: .error)` on
`false`. A milestone bump in either upstream WebRTC
SDK that renames or removes the selectors / methods
makes the probe go negative; the call UI surfaces
"calling unavailable on this build" rather than
silently shipping unencrypted media.

### C-48: SFrame attach happens between addLocalAudioTrack and SDP build

The `armSFrameEncryptors(seed, localLeaf)` call
runs inside the call-start path AFTER
`peer.addLocalAudioTrack(...)` captures the freshly-
created `RtpSender` and BEFORE `composeAndShipOffer`
/ `composeAndShipAnswer` builds the SDP. This is a
hard ordering invariant:

- BEFORE add-track: there is no sender to attach to;
  `armSFrameEncryptors` silently no-ops because the
  internal `localAudioSender` / `localVideoSender`
  are nil.
- AFTER SDP-build: libwebrtc's codec thread can begin
  pulling encoder bytes the moment the sender exists
  + the SDP exchange has handed it a peer. An
  attach that races against the first codec poll
  would tee plaintext to libwebrtc's encoder before
  the encryptor is installed — a window of
  unencrypted egress that audit invariant #4
  forbids.

`CallSession.dial` and `CallSession.acceptCall` on
both platforms call `attachSFrame` exactly between
those two points, and refuse-to-start (`endLocal`)
on `false` return.

### C-49: Per-sender ParticipantKey cache is bounded by leaf count + thread-safe under multi-sender fan-in

`LakkoFrameDecryptor` (N-3) holds an
`unordered_map<uint32_t, lakko_sframe_participant_key_t*>`
keyed by the `sender_leaf` extracted from each
SFrame wire header. First frame from a new leaf
triggers one HKDF derivation via
`lakko_sframe_participant_key_new`; subsequent
frames hit the cache. The map is guarded by
`std::mutex cache_mutex_`:

- **Lookup-or-derive** races safely: two threads
  arriving on the same fresh `sender_leaf`
  concurrently both derive a `ParticipantKey`; the
  loser of the `emplace` race frees its copy via
  `lakko_sframe_participant_key_free` and returns
  the winner's pointer. Derive happens **outside**
  the mutex so N parallel first-frame arrivals
  don't serialise on a contended lock.
- **Destructor drain**: the map is drained in
  `~LakkoFrameDecryptor()` without holding the
  mutex (no other thread can observe an object in
  its destructor under refcount discipline; the
  refcount drop is the synchronisation edge).
- **Cardinality**: bounded by the number of
  distinct leaves the receiver sees. In an MLS-
  group call this is `O(participants)`, well below
  any DoS threshold (MLS group caps are at
  ≤ 50 000 in vanilla mode; broadcast scenarios
  use the two-tier channel path which doesn't go
  through this code).

Test 4 in `sfu_opacity_e2e.rs`
(`multi_party_fanin_through_per_sender_cache`)
proves the cardinality bound: three encryptors
(leaves 10/20/30) feed one decryptor; after round 1
the cache holds exactly 3 entries; round 2 adds
none.

### C-50: Refcount machinery balances every AddRef with exactly one Release, no leak no double-free

The C++ N-3 bridge implements `rtc::RefCountInterface`
manually (no upstream
`rtc::FinalRefCountedObject<T>` — that header is
not in the vendored M125 set, see C-45). Discipline:

- **Construct**: `Create()` returns a raw pointer
  with `refcount_ = 0`. First `AddRef()` bumps to 1.
- **Platform shim**: iOS Obj-C++
  `LakkoSFrameEncryptorWrapper` holds the raw
  pointer behind an `objc_setAssociatedObject`
  strong retain; its `dealloc` fires
  `_cppEncryptor->Release()`. Android Kotlin
  `LakkoFrameEncryptor` holds a `nativePtr: Long`
  field; its `release()` (idempotent) calls
  `nativeRelease` which fires `Release()`.
- **libwebrtc takes ownership**:
  `[sender setFrameEncryptor:enc]` (iOS) or
  `sender.setFrameEncryptor(LakkoFrameEncryptor)`
  (Android) hands the pointer to libwebrtc, which
  internally wraps it in
  `rtc::scoped_refptr<webrtc::FrameEncryptorInterface>`.
  That wrap-up does its own `AddRef()` — refcount
  goes to 2 (ours + libwebrtc's).
- **Detach**: `setFrameEncryptor:nil` (or
  `setFrameEncryptor(null)`) on the platform side
  drops libwebrtc's `scoped_refptr` — refcount goes
  back to 1. Our wrapper's `dealloc` /
  `release()` then drops to 0 and runs
  `delete this`.

`CallSession.close` / `WebRtcPeerConnection.close`
on both platforms releases attached encryptors /
decryptors BEFORE peer-disposal so libwebrtc's
internal Release runs while our wrapper is still
holding its retain — no use-after-free window.

Memory-ordering: `AddRef` uses
`std::memory_order_relaxed`; `Release` uses
`std::memory_order_acq_rel` so the happens-before
edge from any prior `AddRef` synchronises with the
final `delete`. Same posture as upstream
`rtc::RefCountedNonVirtual`.

## PublisherGroup persistence + rotation (G4-2-mls-persistence)

### C-52: PublisherGroup MLS state is persisted by deterministic channel-id, exporter output stable across reloads, rotation advances the epoch

`crates/lakko-mls/src/publisher_group.rs` ships
`PublisherGroup` — a thin wrapper around the standard
[`Group`] with two invariants channels publishers rely on:

1. **Persistence keyed by channel id**. The MLS group
   id is deterministically derived from the 16-byte
   `channel_id`:

   ```
   group_id = b"lakko/publisher-group/v1/" || channel_id   // 25 + 16 = 41 bytes
   ```

   The domain-separation prefix prevents byte-shape
   confusion with any other channel-id-derived value
   (the exporter label, the subscriber-envelope AAD).
   `PublisherGroup::open_or_create(identity, channel_id)`
   resolves to the same persisted group across process
   restarts — `MlsGroup::load(provider.storage(),
   group_id)` reads the OpenMLS snapshot from
   SQLCipher. Subsequent calls return the same handle's
   exporter output for the current epoch, so subscribers
   who received a sealed seed envelope under a prior
   epoch continue to decrypt new posts.

   Pre-G4-2-mls-persistence, every composer call built a
   throwaway `Group::create` with a random group_id —
   the exporter output reset on every app restart,
   subscribers' sealed seed envelopes silently stopped
   matching, and the channel broke at the first reopen.

2. **Rotation advances the MLS epoch, exporter output
   changes**. `PublisherGroup::rotate(identity)` issues
   an MLS self-update commit (`MlsGroup::self_update` +
   `merge_pending_commit`), advancing the group epoch by
   one. The exporter at the new epoch returns a
   different 32-byte output for the same `(label,
   context, length)` triple — that is the rotation
   primitive ADR-0010 §"Rotation" requires
   (scheduled-rotation timer 24 h / N posts, plus
   on-demand for compromise response).

   The rotation is **durable** — `rotated_seed_persists
   _across_reload` proves that after `rotate` + drop, a
   fresh `PublisherGroup::load` surfaces the post-
   rotation seed (not epoch 0).

   `rotate` returns the wire-encoded commit bytes so the
   publisher can broadcast them to every other
   PublisherGroup member. `rotate_emits_commit_bytes_
   processable_by_co_publisher` proves the cross-
   publisher seed-sync invariant: Alice mints
   PublisherGroup, adds Bob via Add proposal, Bob joins
   from welcome, Alice rotates, Bob processes the
   commit, both ends at the same epoch with the same
   exporter output.

3. **FFI surface preserves the invariant across iOS /
   Android / web**. UniFFI `PublisherGroup` +
   `publisher_group_open_or_create` +
   `publisher_group_load` (Swift / Kotlin) and
   wasm-bindgen `PublisherGroup.openOrCreate` /
   `.load` / `.rotate` (TypeScript) wrap the same Rust
   handle behind a `Mutex<>`. Composer-side tests on
   all three platforms exercise the idempotence
   invariant: two consecutive `open_or_create` calls
   for the same identity + channel id return byte-
   identical seeds.

Tested in 11 Rust-core tests
(`crates/lakko-mls/src/publisher_group.rs` —
including `open_or_create_is_idempotent`,
`rotate_advances_epoch_and_changes_seed`,
`rotated_seed_persists_across_reload`,
`rotate_emits_commit_bytes_processable_by_co_publisher`)
+ 7 UniFFI + 4 wasm host-side tests.

## TURN-credential invariants (G3-{1,2}-turn)

### C-51: TURN credential follows the coturn `static-auth-secret` REST scheme; bound to a per-call expiry

`POST /call/turn-credential` (services/auth) mints a
short-lived credential the WebRTC `RTCIceServer.urls`
authenticates against, using the convention
draft-uberti-rtcweb-turn-rest-00 (also known as the
"TURN REST API") pins:

```
username = `<unix_expiry_secs>:<account_id_hex>`
password = base64(HMAC-SHA1(static_auth_secret, username))
```

Where `static_auth_secret` is loaded once from
`LAKKO_TURN_SHARED_SECRET` at auth-service startup and
MUST match the same value coturn's config file pins
under `static-auth-secret`. Rotation = service restart;
no in-process key migration path.

The credential expires at the embedded `unix_expiry_secs`
— coturn refuses any STUN allocation past that timestamp
even if the password recomputes correctly. Default
expiry is `now + 600` (`TURN_CRED_TTL_SECS`) — long
enough to comfortably outlast ICE gathering on every
tested mobile network (≤ 30 s p99), short enough that a
leaked credential is useless for any post-call attack.

Fail-soft posture (audit-relevant): when the auth
service has `LAKKO_TURN_URIS` / `LAKKO_TURN_SHARED_SECRET`
unset the endpoint returns 503 with body
`{"error":"turn_not_configured"}`, and both platform
clients (`apps/ios/Lakko/Calls/TurnCredentialClient.swift`
+ `apps/android/.../calls/TurnCredentialClient.kt`)
swallow that into an empty TURN-server list. The call
still attempts to dial; only symmetric-NAT peers will
fail to connect. This is a deliberate trade-off — the
alternative is refusing every call when the auth
service hiccups, which is much worse UX than a
degraded-NAT-traversal call.

Why HMAC-SHA1 (not SHA-256): the coturn protocol pins
it, and we cannot override. The HMAC use case is
unaffected by the SHA-1 collision-attack family
(SHAttered, etc., target plaintext-digest construction,
not keyed-MAC). NIST SP 800-131A continues to allow
HMAC-SHA1 in authentication roles for the same
reason.

Tested in `services/auth/src/turn.rs` (5 unit tests:
shape-matches-coturn-rest-api, distinct-accounts-distinct-
passwords, same-account-different-time-distinct-password,
saturating-expiry-does-not-overflow,
from-env-rejects-short-secret).

## SFrame native-shim host-side validation (N-{4,5}-validate)

### C-53: Host-side bridge round-trip exercises every documented tampering mode of the SFrame native shim before either platform binding is loaded

The iOS Obj-C++ shim (`apps/ios/Lakko/Calls/SFrameAttach.{h,mm}`)
and the Android JNI shim
(`apps/android/lakko-sframe/src/main/cpp/sframe_jni.cc`) are
~99% wire-trampoline code — they cast pointers, marshal byte
slices, and hand control off. Every cryptographic decision
the call surface makes lives one layer down, in:

  * `crates/lakko-sframe-native` (N-1) — flat C ABI over
    `lakko_calls::sframe::{CallKey, ParticipantKey}`.
  * `apps/shared/sframe-bridge/sframe-bridge.{h,cc}` (N-3) —
    the C++ subclass of `webrtc::FrameEncryptorInterface` +
    `webrtc::FrameDecryptorInterface` that the platform shims
    actually pass to libwebrtc's `setFrameEncryptor:` /
    `RtpSender.setFrameEncryptor(...)`.

`apps/shared/sframe-bridge/test_bridge.cc` builds and runs as
a standalone C++17 binary (no libwebrtc runtime, no gtest)
against the same bridge code the platform shims attach. Eight
assertions, all green on every push that touches the bridge
or the Rust crate it links:

  1. **Single-sender round-trip** — encrypt + decrypt produces
     byte-identical plaintext.
  2. **AAD binding** — tampered `additional_data` returns
     `kFailedToDecrypt` (NOT `kOk`, NOT `kRecoverable`).
  3. **Multi-sender into one receiver** — three concurrent
     senders into one decryptor each get their own
     `ParticipantKey` cache entry, every round-trip succeeds.
  4. **Bad seed length** — both `LakkoFrameEncryptor::Create`
     and `LakkoFrameDecryptor::Create` return `nullptr` for
     seeds shorter than 32 bytes (fail-closed; the platform
     shim must refuse to install a `nullptr` encryptor per
     security invariant #4 — no fallback to non-E2EE).
  5. **Ciphertext-body tamper** — flipping a single bit in the
     AEAD-protected body (between the SFrame wire header and
     the trailing 16-byte tag) returns `kFailedToDecrypt`. Together
     with #2 this exercises both halves of the AEAD's
     authenticated set.
  6. **Counter monotonicity + out-of-order delivery** — 1000
     frames sealed in order, decrypted in a deterministic
     shuffle, every plaintext recovered correctly. SFrame
     counters live in the wire header, not in receiver state;
     no monotonic-order assumption.
  7. **Per-call key isolation** — two `CallKey` instances
     seeded with different bytes cannot cross-decrypt. Mirrors
     the per-call domain-separation MLS exporter labels
     guarantee at the layer above.
  8. **Payload-size extremes** — 1 B / 80 B / 1500 B / 64 KiB
     all round-trip; rules out a silent-truncation regression
     in `GetMaxCiphertextByteSize` or
     `GetMaxPlaintextByteSize`.

The runner is `just test-sframe-bridge` locally and
`.github/workflows/sframe-bridge.yml` in CI
(`workflow_dispatch`-only, matrix on `ubuntu-latest` +
`macos-latest`). Quota-conservation mode means no nightly
cron yet; flip to `schedule:` when a paid tier lands.

**Scope boundary.** This invariant does NOT cover live RTP
between two iOS simulators or two Android emulators inside CI
— that end-to-end gate is the remit of **G3-4-mobile**, the
Phase 3 closeout sprint that wires `xcodebuild test -sdk
iphonesimulator` and Gradle `connectedAndroidTest` against
the dockerised SFU compose stack from `ci/sfu-e2e/`. The Obj-C++
shim's selector trampoline (`objc_msgSend` →
`setFrameEncryptor:`) and the JNI shim's
`Java_me_lakko_sframe_LakkoFrameEncryptor_nativeCreate`
plumbing are compile-gated by `xcodebuild build` + `./gradlew
assembleDebug` today; the runtime install path is exercised
by the `SFrameAvailability.bootProbe` reflection check on
both platforms before any call entry point renders (security
invariant #4: refuse-to-start if `RtpSender.setFrameEncryptor`
disappears from a libwebrtc bump).

Tested in `apps/shared/sframe-bridge/test_bridge.cc` (eight
host-side assertions, listed above) + boot probe at
`apps/ios/Lakko/Calls/SFrameAvailability.swift` +
`apps/android/lakko-sframe/src/main/kotlin/me/lakko/sframe/
SFrameAvailability.kt` (run at app start; fail-closed).

## Channel rotation policy (G4-2-rotation-timer)

### C-54: Scheduled rotation cadence is governed by a pure policy, identical across all three platforms, with clock-skew-tolerant arithmetic

ADR-0010 §"PublisherGroup" pins the rotation cadence as
"every 24 h or N posts, whichever lands first." The
**policy** that decides _when_ to rotate lives in one
place — `crates/lakko-channels/src/rotation_policy.rs` —
and is exposed verbatim to all three platforms via UniFFI
+ wasm-bindgen.

Three sub-invariants:

1. **Either trigger fires independently.**
   `RotationPolicy::should_rotate(now_ms, state)` returns
   `true` iff either:
   - `interval_ms > 0 && now_ms.saturating_sub(state.last_rotation_ms) >= interval_ms`, or
   - `max_posts > 0 && state.posts_since_rotation >= max_posts`.

   Setting either threshold to `0` disables the
   corresponding trigger; setting both to `0` disables
   scheduled rotation entirely (membership-change-driven
   rotations still happen on every MLS commit).

2. **Saturating arithmetic on the time delta keeps a host
   clock jump backward from forcing a spurious rotation.**
   A NTP correction, manual timezone change on Android,
   suspend / resume on iOS, or fork-bombed `setTimeout`
   on web can all surface as `now_ms < state.last_rotation_ms`.
   `saturating_sub` clamps the delta to `0` for those
   cases, so the time trigger stays silent until the
   clock catches up — no rotation storm.

3. **The orchestration layer is platform-specific but
   the decision is single-sourced.** The web side ships
   `apps/web/src/channels/rotation-cron.ts` with
   `runRotationOnce` (single-pass: policy check → MLS
   rotate → fetch subscribers → seal per subscriber →
   atomic batch `POST /v1/channels/:id/rotate`) and
   `startRotationCron` (periodic poller, default
   5-minute heartbeat). iOS / Android orchestration
   ports are explicitly deferred — same Rust core
   policy via UniFFI; same shape of fetch + seal + post
   batch. Subscriber fanout is bound to the
   **post-rotation server epoch** via
   `sealSubscriberSeedEnvelope(pub, channel_id,
   new_server_epoch, new_seed)` so envelopes the
   subscribers fetch on `GET /v1/channels/:id/subscribers/:pk/envelope`
   land at the right epoch slot.

Tested in 13 Rust-core unit tests
(`crates/lakko-channels/src/rotation_policy.rs`) covering:
genesis state, exact-boundary time trigger, exact-count
count trigger, either-alone-sufficient, clock-skew
tolerance (no spurious rotation on backward jump),
zero-interval disables time trigger, zero-max-posts
disables count trigger, both-zero disables scheduled
rotation entirely, `after_post` saturates at `u32::MAX`,
`after_rotation` resets the post counter, genesis state is
zeroed, custom-cadence policy round-trip. FFI mirrors at
`crates/lakko-ffi-uniffi/src/lib.rs` (Swift / Kotlin) +
`crates/lakko-ffi-wasm/src/channels.rs` (TypeScript).
Web orchestration `tsc --noEmit` clean at
`apps/web/src/channels/rotation-cron.ts`.

### C-55: SFU multi-party media is SFrame-sealed end-to-end on every client; the AEAD AAD is the cross-platform wire header, not host RTP metadata

The SFU multi-party call arc wires three independent clients
(web, iOS, Android) to the Rust SFU, and the property that
makes their media interoperable while keeping the SFU blind
([invariant #2](../../CLAUDE.md)) is a single keying contract,
[ADR-0013](../adr/0013-sframe-cross-platform-aad.md): the
SFrame AEAD **additional authenticated data is the SFrame
wire header bytes (KID + CTR) — the RFC 9605 §4.3 layout
(see C-38) — never any host-stack RTP
`additional_data`**. Host RTP metadata is not reconstructible
across libwebrtc (native) and `RTCRtpScriptTransform` (web),
so binding it would make a frame sealed on one platform fail
to open on another; binding only the wire header — which every
platform produces identically — is what lets a frame cross
stacks. (The header is now RFC 9605 §4.3-conformant, so a
conformant external stack sharing the key + nonce derivation
also produces the same header; only a live deployed-SFU RTP
round-trip remains, G-26.) This is the same `lakko_calls::sframe`
`seal_framed` / `open_framed` core pinned by C-37 / C-38;
C-55 records that all three SFU clients now reach it.

Four sub-invariants:

1. **One framed core, three call sites.** The browser worker
   (`apps/web/src/calls/sframe-transform.ts` +
   `sframe-transform-worker.ts`, via wasm-bindgen
   `sealFramed` / `openFramed`) and the native C++ bridge
   (`apps/shared/sframe-bridge`, via the
   `lakko-sframe-native` C ABI `lakko_sframe_seal_framed` /
   `_open_framed`) call byte-for-byte the same Rust pipeline.
   No platform re-implements the AEAD, the header, or the
   AAD choice.

2. **Every sender is armed before the offer; every receiver
   on arrival.** The native `SfuCallSession` (iOS
   `SfuCallSession.swift` + Android `SfuCallSession.kt`)
   calls `armSFrameEncryptors` over **all** local senders
   (audio + video) BEFORE `createOffer`, and
   `armSFrameDecryptor` on **every** remote receiver in the
   `onRemoteReceiver` delegate (audio + video). A video
   call adds the camera track before the arm so the video
   sender is sealed alongside audio — the SFU never sees a
   plaintext frame on either track. A failed arm aborts the
   call (fail-closed, invariant #4).

3. **The SFU routes by the on-wire KID without touching the
   ciphertext.** `open_framed` checks `kid == sender_leaf`
   and binds the exact on-wire header; the SFU reads the KID
   to fan out by participant (`services/sfu/src/media.rs`
   pins the downstream MSID stream-id to the source `PeerId`)
   but never decodes the body — the SFrame ciphertext is
   opaque on the forwarding path (C-39).

4. **Fail-closed on every surface.** A frame that fails to
   parse or authenticate is dropped, never forwarded as
   plaintext — the web worker, the native bridge, and the
   `lakko-calls` core all hold this.

**Verification status.** The byte-level interop is asserted
by construction (one shared core + a platform-independent
header) and by the native host test
(`apps/shared/sframe-bridge/test_bridge.cc`, C-53: a
mismatched host AAD does NOT fail decryption, a tampered
header DOES). What is NOT yet observed firsthand is a live
media frame sealed on one platform decrypting on another over
a deployed SFU forwarding path — that needs two real
endpoints plus the SFU and cannot run in a headless build.
This residual item is tracked as [G-26](known-gaps.md#g-26)
and is a verification step, not a missing mechanism.

### C-56: Every new KT-log binding is account-authorised (not discovery-asserted)

G-30 #2 ([ADR-0019](../adr/0019-client-authorised-log-leaf.md))
closes the append-binding trust gap
([G-30](known-gaps.md#g-30) finding 2): a new transparency-log
`(account_key → pubkey)` binding is no longer merely asserted
by the discovery service, it carries a cryptographic
**account authorisation** that the operator verifies and a
client re-checks.

Four sub-invariants:

1. **Rotations are chain-verified against the log.**
   `verify_supersession_chain`
   (`crates/lakko-transparency/src/supersession.rs`) is
   FFI-exposed (UniFFI + wasm) and wired into all three
   clients' gate 2 (`verifyRotationChain`). On a key change the
   rotation is bound to a SINGLE committed log version under a
   no-rollback signed root (operator-root sig → no-rollback →
   Merkle inclusion → `username_hash` match → strictly-newer
   version → old-key supersession proof → leaf-pubkey == proof
   key). Gate 1 (the ADR-0016 fail-closed supersession gate)
   is untouched and remains the only hard-block on a key
   change; the chain lives in gate 2 (a contradiction surfaces
   as `mismatch`, only `rollback` hard-fails). This makes
   leaked-old-key equivocation **detectable** (two distinct
   successors cannot both be log-confirmed at one version).

2. **Genesis / reset bindings carry a self-signature.**
   `sign_leaf_genesis` / `verify_leaf_genesis`
   (`leaf_auth.rs`, domain `lakko/transparency/leaf-genesis/v1`,
   version-independent) authorise a first entry or an
   `is_reset` mint by signing `(account_key, pubkey)` with the
   leaf's OWN key. All three clients mint it on a new-key
   publish and verify a served genesis self-sig against the
   served key on a first observation.

3. **The operator REJECTS an unauthorised append.** The
   transparency `append` handler verifies the `leaf_auth`
   under the append lock (genesis: self-sig; rotation: the
   supersession proof against the previous entry's pubkey) and
   returns 403 on an invalid one;
   `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` (default on) also
   refuses an absent one. Defence-in-depth — the client chain
   stays the real gate (the server is untrusted) — but a
   leaked append secret can no longer fabricate a keyless leaf.

4. **Discovery treats `leaf_auth` as OPAQUE
   ([invariant #2](../../CLAUDE.md)).** It stores + COALESCE-
   preserves + forwards the bytes, and NEVER decodes, parses,
   or verifies them (length-cap is storage-bounding only).

Residual: genesis is TOFU — a self-sig does not PREVENT
first-entry squatting (an attacker with the append secret can
self-sign their own key for a victim's never-used
account_key); it is detected via CONIKS self-monitoring + the
ADR-0016 fail-closed peer gate. The auth-attested-genesis
hardening (which prevents squatting) is the elective
alternative recorded in ADR-0019. Two new fuzz targets cover
the new verify surfaces (`verify_supersession_chain`,
`verify_leaf_genesis`). `lakko-security`: **APPROVE-WITH-NOTES**.

### C-57: An operator root is trusted only with an independent witness co-signature (split-view resistance)

G-30 #1 ([ADR-0020](../adr/0020-witness-cosigning.md)) closes the
operator-equivocation gap ([G-30](known-gaps.md#g-30) finding 1):
the operator alone signs each KT-log root, so a malicious operator
could sign two divergent roots and show each to a different victim
(a split view). An independent **witness** now co-signs each root,
and a client trusts a root only with BOTH signatures.

Four sub-invariants:

1. **The witness co-signs only an append-only extension.** Before
   co-signing a new root the witness verifies a Merkle consistency
   proof (RFC 6962 §2.1.2, `crates/lakko-transparency/src/merkle.rs`
   `verify_consistency` — reconstructs both roots) that it
   append-only-extends the witness's last-co-signed root. Two
   divergent forks cannot both extend one root, so the operator
   cannot get a co-signature on a fork — the honest witness refuses
   (`services/witness` `POST /witness/cosign` → 409 `NotConsistent`,
   proven live in `recovery_kt_e2e.rs` step 7).

2. **Clients fail closed on a missing / invalid co-signature.** All
   three clients run `witnessGateRefuses` immediately after the
   no-rollback gate in `verifyIdentityInLog` + `verifyRotationChain`:
   a root without a valid co-signature from the build-time-pinned
   witness key → `mismatch` (invariant #4). No pinned witness key →
   operator-only transition posture (unconfigured fallback).

3. **The co-signature is a verify-only public artefact.** A 105-byte
   blob (`version ‖ tree_root(32) ‖ log_size(8 BE) ‖ sig(64)`, domain
   `lakko/transparency/witness/v1`, distinct from the operator root
   sig); the FFI is verify-only (`verify_witness_cosig` — signing
   stays server-side). `services/transparency` co-signs-on-read (fresh
   head-root sig, cached cosig keyed on `(tree_root, log_size)`,
   re-obtained only on growth; witness unreachable → serve `None`,
   never a stale cosig).

4. **The witness materialises no accounts ([invariant #5](../../CLAUDE.md)).**
   It persists only its last-co-signed `(tree_root, log_size)`
   high-water — never any account, username, or pubkey. The cosig +
   the consistency proof are public artefacts over public roots
   ([invariant #2](../../CLAUDE.md)).

Strengthens invariant #4 (the root gate now needs operator AND
witness) + invariant #6 (a rotation is anchored to a non-forkable
root). Residual: operator+witness collusion (a K-of-N witness
threshold is a config slot for later) + independence-as-deploy-
property (the witness must run in a different trust domain). Two
fuzz targets cover the new surfaces (`verify_consistency`,
`verify_witness_cosig`). `lakko-security`: **APPROVE-WITH-NOTES**.

### C-58: A backup rollback is detected by an auth-signed backup-head commitment

G-11 ([ADR-0037](../adr/0037-backup-head-rollback-commitment.md))
closes the backup-rollback gap: an active attacker who controls the
storage tier could hand a restoring client an *older* backup blob,
which opens cleanly under the same recovery phrase, so nothing a
client ran authenticated "this blob is the current head". An
auth-signed **backup head** minted on every upload and verified on
every restore now does.

Five sub-invariants:

1. **The head is a domain-separated, fixed-layout commitment.**
   `BackupHead` (`crates/lakko-attest/src/backup_head.rs`) is a
   129-byte Ed25519 blob binding
   `(account_id, backup_version, envelope_hash, signed_at)`, domain
   `lakko/backup-head/v1`. A shared-key three-way domain-crossing
   test proves it is cryptographically non-interchangeable with the
   `device-attest/v1` and `genesis-attest/v1` kinds even under the
   ONE attest signing key it reuses (ADR-0037 §3).

2. **A signature proves authentic, not current — two mechanisms
   bridge the gap.** (a) The **`backup_version`** is an
   auth-assigned, per-account, strictly-monotone counter, minted at
   `POST /auth/backup/head` (bearer-authed + STATEFUL — the
   `backup_head_versions` table, migration `0015`, read-bumped in
   one transaction). It is never client-supplied, so a regression is
   structurally impossible; a **returning client** refuses any head
   below a device-key-encrypted `last_known_version` floor (HARD, no
   window). (b) The signed **`signed_at`** mint timestamp (Unix
   millis) lets a **zero-anchor fresh install** — which has no floor
   — fall back to a client-side staleness window (default 30 days).

3. **The head + envelope swap atomically.** The head rides as a
   `0xB1` container-tag prefix of the single storage blob
   (`crates/lakko-backup/src/head_frame.rs` `frame_backup_blob` /
   `parse_backup_blob`, byte-0 discriminator), so one atomic `PUT`
   swaps head and envelope together — a separate head object could
   not be swapped atomically over HTTP and would leave a torn window
   that bricks a legitimate restore. `services/storage` is untouched
   (invariant #2 — the tag + head are more opaque bytes).

4. **The restore gate is a three-case fail-open/closed policy.**
   `verify_backup_head` (full: signature + `account_id` +
   `envelope_hash` + freshness) / `verify_backup_head_binding`
   (binding only) are FFI-exposed with a discriminable
   `BackupHeadStale` vs `Backup` / `invalid:` error; the three
   clients (`head-gate.ts` / `BackupHeadGate.swift` / `HeadGate.kt`,
   12 gate tests each) apply: **absent** (byte 0 ≠ `0xB1`, legacy /
   stripped) → **fail-open**; **invalid** (bad signature / account /
   envelope-hash, or a framed-but-truncated blob) → **hard
   fail-closed**; **stale** (below floor or over window) →
   **fail-closed with an explicit user-acknowledged override**.

5. **The absent-head fail-open is scoped to rollback-detection.**
   It is the deliberate availability exception (mirroring the
   ADR-0033 throttle) so a legacy / pre-rollout blob still restores;
   crucially it is on the rollback-detection layer only — the
   envelope is AEAD-sealed under the recovery phrase regardless of
   the head, so invariant #4 (no silent downgrade to non-E2EE) is
   untouched (confidentiality + content integrity never depend on
   the head).

Invariants: #1 (the head carries only `account_id`, a `BLAKE3` of
ciphertext, a monotone integer, a timestamp; the counter table
stores only `(account_id, version)` — no PII), #2 (storage + auth
both opaque to key material — auth persists only a counter), #4
(fail-closed, scoped as above). Residual: a within-window replay of
a genuine old head is uncaught for a **true zero-anchor fresh
install only** (any client that ever persisted a floor is
hard-protected); the tightening path (auth serving a trusted
current-version marker on restore) is declined for single-fetch
simplicity, and the dormant v2 chain stays out of scope. Two fuzz
targets cover the new attacker surfaces
(`lakko-attest/backup_head_open` — the head bytes are a prefix of a
storage blob a hostile tier crafts; `lakko-backup/backup_blob_parse`
— the container framing). Closes G-11 in practice.

**Post-landing `lakko-security` pass (APPROVE-WITH-NOTES).**
No CRITICAL / HIGH and no invariant-#4 break on the live path;
the F1–F5b hardenings from it tighten the gate. **F4** (all
three clients): sub-invariant 4's **absent** case no longer
unconditionally fail-opens — an absent head with a persisted
floor now compares `BLAKE3(served envelope)` against the
floor's `envelope_hash` and proceeds only on a byte-identical
match, else routes to the user-acknowledged stale-override, so
the fail-open is scoped to a **true zero-anchor fresh install**
(a returning client is hard-protected against a head-stripping
downgrade — closing the gap between the shipped code and this
entry's residual). **F5a**: a present-but-corrupt version floor
(undecryptable / tampered) routes to stale-override, never a
silent demote to the weaker fresh-window path. **F2**: the
restore gate is now mandatory (the ungated no-verification
restore path was removed). **F1**: a loud warning on
verification against an unpinned (JWKS-served) attest key.
**F3** (Android): the `parseBackupBlob` catch widened to
`Throwable` (a clean hard-fail). **F5b** (auth): a per-account,
**fail-open** rate-limit on `POST /auth/backup/head`
(`backup_head_mint_limits`, migration `0016`), shipped
DEFAULT-OFF (`LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` default `0`).

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
| JWT `(account_id, call_id, role, expires_at)` | ✅ (validate via cached JWKS) | ✅ (mint via `POST /call/jwt`) | Per ADR-0008 + C-15..C-23 |
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
