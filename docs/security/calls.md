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

The counter is 64 bits and a fresh `ParticipantKey` starts it at
a uniformly random value in `[0, 2^62)`
([ADR-0069](../adr/0069-sframe-random-counter-start.md)). The
sender-side `ParticipantKey::seal` increments the counter once per
sealed frame and never reuses a `(key, nonce)` pair **within** a
`ParticipantKey`. The nonce is the salt XOR the counter as 8
big-endian bytes in the low 8 bytes of the 12-byte nonce.

Two `ParticipantKey`s derived under one `(seed, KID)` are no longer
a caller's duty to avoid. They happen: rejoining a chat-group call
in the same MLS epoch (the seed is the chat group's exporter output
for the call id, and joining advances no epoch), a tab reload, a
reconnect. They reuse a nonce only if their counter ranges overlap,
which for `L1` and `L2` sealed frames has probability at most
`(L1 + L2) / 2^62`. The key ring's same-seed no-op stays as defence
in depth.

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

At `u64::MAX` the seal path returns `Error::CounterExhausted`
and seals nothing; from the highest possible start that is
`3 * 2^62` frames away.

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
reason codes 6..N lands gracefully on older receivers — the
call still terminates, just with the most-conservative
reason code (`HungUp`). Code 5 is `EndedForAll` (2026-10-02,
`docs/ux/calls.md` section 4.1.2): the starter of a group or
link call ended it for everyone. The value carries an intent
only; a client leaves the call only when the MLS-authenticated
sender is that call's starter, so a member who is not the
starter cannot end a call for the others by sending it. This avoids the alternative
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

**SFrame-passthrough property the fanout preserves.** Since
ADR-0066 (2026-10-02) the SFU forwards RTP packets, not frames:
`services/sfu/src/media.rs::fanout_packets_into` walks every
non-origin peer and calls `MediaPeer::forward_packet(&packet)`,
which writes the packet with `StreamTx::write_rtp(pt, seq_no,
rtp_time, wallclock, marker, extensions, nackable,
packet.payload.clone())`. Until then it forwarded whole frames
through `MediaPeer::forward_media_data` and `Rtc::writer(..).write`;
the property is the same in both. The payload byte buffer is the
inner SFrame-encrypted payload (one packet's share of it),
forwarded **verbatim** into the destination peer's str0m state —
no inspection, no parsing, no SFrame-header peek. str0m subsequently SRTP-encrypts the
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
   group_id = b"lakko/pubgrp/v2/" || channel_id   // 16 + 16 = 32 bytes
   ```

   The domain-separation prefix prevents byte-shape
   confusion with any other channel-id-derived value
   (the exporter label, the subscriber-envelope AAD).

   **The 32-byte total is load-bearing, not incidental.**
   A PublisherGroup id is a Delivery Service journal
   routing key on the ADR-0028 OQ3 co-publisher path
   (`apps/{web,ios,android}` all hand it to
   `fetchGroup` / `publishGroup` verbatim), and
   `services/ds` accepts exactly `PSEUDONYM_LEN = 32`.
   The retired v1 prefix `b"lakko/publisher-group/v1/"`
   made the id 41 bytes → 82 hex chars → a flat 400 from
   `decode_pseudonym`, so the co-publisher flow could
   never have worked. v2 narrows the prefix to the 16
   bytes left over after the channel id, which reaches
   the DS width while keeping the derivation both
   deterministic AND invertible — `channel_id_from_group_id`
   still recovers the channel id from a `/g/` ticket that
   conveys only the `group_id`. Deriving the id by
   hashing the channel id would have hit the same width
   and destroyed that inverse. Pinned by
   `crates/lakko-mls/tests/group_id_width.rs`
   (`publisher_group_id_matches_the_ds_width`,
   `publisher_group_id_round_trips_through_channel_id`,
   `publisher_group_id_hex_matches_the_ds_path_length`).

   The prefix width is `GROUP_ID_LEN - CHANNEL_ID_LEN`
   and the literal is typed `&[u8; …]`, so a later edit
   that widens it is a compile error rather than a silent
   re-break.

   **No migration exists, by construction.** An MLS
   `group_id` is a `GroupContext` field fed into the key
   schedule and bound by every commit's confirmation tag,
   so a group persisted under the v1 id cannot be re-keyed
   to the v2 id — it is a different group. `PublisherGroup::
   load` / `create` therefore *detect* v1 state and refuse
   loudly, rather than returning `Ok(None)` and letting
   `open_or_create` mint a fresh epoch-0 group that would
   silently swap the channel's content-key seed under
   subscribers still holding the old one. The v1 state is
   left intact.
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
root).

**K-of-N witness quorum ([ADR-0039](../adr/0039-kt-log-witness-quorum.md),
2026-07-21).** The single-witness residual above — operator+witness
collusion — is now raised to a **K-of-N witness quorum**: a client
trusts an operator root only when it carries the operator signature
AND **≥ K valid co-signatures from DISTINCT keys in a
build-time-pinned witness key set of size N**
(`crates/lakko-transparency/src/witness.rs` `verify_witness_quorum`).
The count is over **distinct pinned keys satisfied, never cosigs** —
the cosig list is attacker-influenceable, so a replayed / duplicated
cosig (or a duplicate pinned key) satisfies at most the one key it is
signed by and contributes exactly one, and a single compromised
witness can never flood the list to fake a quorum. It reuses
`verify_witness_cosig` per (key × cosig), inheriting the
`(tree_root, log_size)` binding + the domain separation verbatim, and
fails closed on `threshold == 0` / `threshold > N`; N=1/K=1 is
byte-for-byte the single-witness gate. `services/transparency` fans
out to the N witnesses best-effort (`witness_client.rs`
`list_from_env`) and serves the bundle `witness_cosigs_b64:
Vec<String>` (a down / refusing witness is absent, never a stale
per-witness cosig; migration `0005` widens the cosig cache to a bundle
+ a per-witness high-water for anti-stranding auto-rejoin); the
**client** is the enforcement point and fails closed below K.
`services/witness` is unchanged — N witnesses are N independent
instances of the same binary.

Residual: operator + K COLLUDING witnesses — closed in practice ONLY
under INDEPENDENT witness operation (the N witnesses in distinct trust
domains; a deploy/ops property, not code — a single operator running
all N witnesses gains nothing against this threat) + the deferred
client-gossip / cross-check layer (the witness-trust-free split-view
defence, a separate future ADR). Three fuzz targets cover the
surfaces (`verify_consistency`, `verify_witness_cosig`,
`verify_witness_quorum`). `lakko-security`: **APPROVE-WITH-NOTES**.

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

### C-59: The iOS silent-push NSE decrypts read-only and can never write MLS state back

[ADR-0041](../adr/0041-ios-silent-push-nse-decrypt.md) brings iOS to
parity with the already-approved web (`148753b`) + Android
(`ae9a087`) silent-push slices: the
`UNNotificationServiceExtension` (NSE) renders the **real**
decrypted text of the newest message on a mutable-alert push wake
instead of a content-free "New message" banner. The NSE is a
**separate process** from the foreground app, so — like a call-side
invariant — this entry pins the properties that keep the
server + the foreground ratchet safe while a second process decrypts.

The `C-` class is the audit pack's catch-all for server-visible +
opacity-preserving properties (C-56..C-58 are KT-log + backup, not
literally calls); C-59 continues that convention.

Five sub-invariants:

1. **The seed-sharing boundary is the minimum, and it is bounded.**
   On-device MLS decrypt is impossible without the AccountIdentity
   seed (`Identity.fromSeed`, ADR-0017), so it moves into a
   **team-scoped shared Keychain access group**
   (`group.me.lakko.dev.shared`), staying
   `AfterFirstUnlockThisDeviceOnly`, no-iCloud-sync, and protected by
   the hardware-derived class key. Only the derived **64-byte phrase
   seed** is shared — never the recovery phrase — and the seed `Data`
   is zeroed after use (`resetBytes`, `NsePushDecrypt.swift`). The
   read scope widens app -> {app, first-party co-signed NSE}, not
   app -> any app (a third-party app cannot join a team's access
   group). Invariant #7's non-extractable / hardware-backed letter is
   preserved; only the process scope that may read the seed widens —
   the one invariant under tension, which is why the arc was reviewed
   before Phase 1 landed.

2. **A three-layer structural no-write guarantee makes the throwaway
   airtight.** (1) The slim `lakko-ffi-extension` FFI (Phase 2,
   `crates/lakko-ffi-extension/src/lib.rs`) exposes **no persist
   method** — only `from_seed` / `restore_storage` / `load_group` /
   `process_incoming_verifying_roles` / `parse_body_envelope`, with
   no `snapshot_storage` / compose / send / call / channel / transport
   surface. (2) The `NsePushDecryptDeps` / `NseThrowawayGroup`
   protocols (`NsePushDecrypt.swift`) expose **no write path** —
   every method is a read or a pure compute. (3)
   `NseThrowawayGroup.close()` is a documented no-op
   (`NseDecryptDeps.swift` — ARC / UniFFI `deinit` frees the handles;
   never `snapshotStorage` / `putSnapshot`). The app's DS cursor is
   never advanced (the fetch start is read-only).

3. **The G-34-class ratchet desync is avoided by construction.** The
   NSE restores a *temporary* group from the last-persisted snapshot,
   processes DS rows only to extract the newest preview, and DISCARDS
   it. Because it never writes the advanced snapshot / cursor back,
   the foreground app later restores from the **same** snapshot and
   reprocesses from its **own** append-only cursor (idempotent) — the
   publish/persist ratchet-desync hazard (ADR-0040 / the G-34
   nonce-reuse-on-crash audit) cannot arise. This is the same
   read-only-throwaway property `lakko-security` approved on web.

4. **Fail-closed (invariant #4).** A frame that will not decrypt /
   authenticate is dropped — never rendered as a plaintext guess
   (`NseThrowawayGroup.process` returns `nil`). Any missing input
   (no account match, no seed, no snapshot, a non-member group, a DS
   or decrypt failure, a control frame with no preview, or the ~22 s
   budget / `serviceExtensionTimeWillExpire`) degrades to the generic
   "New message" banner. Delivery is exactly-once (`NSLock`) and the
   NSE never crashes.

5. **Server opacity holds (invariant #2).** The NSE performs only a
   **read-only** DS group-journal fetch (the same opaque frames the
   app already fetches — a plain `URLSession` GET, no cursor
   mutation) and decrypts locally; the server learns nothing it did
   not already know when it routed the wake to the group pseudonym.
   No key material leaves the device.

The slim decrypt-only framework is Phase 2's crate split: measured
against the ~30 MB NSE ceiling, the dead-stripped linked decrypt
footprint is **~1.6 MB** (`~1.34 MB` mapped code), versus the 333 MB
full `LakkoCore.xcframework` that transitively pulls
transport / calls / SFU / channels — an order of magnitude of
headroom, so `LakkoCoreExtension.xcframework` is what the NSE links.

Invariants: #2 (opaque read-only DS fetch + local decrypt), #3 (no
`lakko-mls` change — the slim crate re-exports the audited
`process_incoming_verifying_roles` / `parse_body_envelope` path, it
does not fork it), #4 (fail-closed to the generic banner), #7 (seed
read-scope widening, letter preserved). Two Phase-3 findings are
**RESOLVED**, leaving no open residual gap: **Finding 1** — an
anti-censorship-**armed** user's NSE never makes a direct
un-tunneled DS connection (the NSE deliberately cannot carry the
transport / anti-censorship stack into its memory-starved process,
so when the user is armed it short-circuits straight to the generic
banner rather than leaking a direct connection); **Finding 2** — the
read-only fetch start is floored at the app's persisted per-group
cursor.

**`lakko-security` verdicts (APPROVE-WITH-NOTES).** Phase 1 (the App
Group / seed-sharing migration; the LOW-1 finding — a torn-copy
window in the one-time file migration — fixed via
atomic-copy-with-read-fallback, `7b18cec`); Phase 3 (the NSE
read-only-throwaway decrypt); and the earlier web sibling
(in-memory provider, zero write transactions, no `snapshot_storage`
call). Coverage: `cargo test -p lakko-ffi-extension` (incl. the full
`fromSeed -> restoreStorage -> loadGroup ->
processIncomingVerifyingRoles -> parseBodyEnvelope` round-trip
recovering plaintext) + `NsePushDecryptTests` (happy path, every
fail-closed branch, cursor math, never-persists guard) +
`SharedStorageMigration` tests. Residual: the live
APNs -> NSE -> decrypted-notification round-trip on a provisioned
device is deploy-gated.

### C-60: Push routes under an opaque, member-secret-derived id that is unlinkable to the DS `group_id`

[ADR-0042](../adr/0042-push-opaque-group-routing-id.md) changes **what 32-byte
value** the push subsystem routes on. Before it, the push subscription key, the
notify JOIN key, and the delivered payload `g` were all **identical to the
Delivery Service journal key** (`group_id`), so push-gw materialised
"device X belongs to group G" internally and Apple / Google received that same
DS-linkable identifier **in cleartext**. This entry pins the property that
replaced it.

The `C-` class is the audit pack's catch-all for server-visible +
opacity-preserving properties (C-56..C-58 are KT-log + backup, C-59 is the iOS
NSE decrypt boundary); C-60 continues that convention.

**Scope — read this first.** The claim is **narrow and exact**: push now routes
under an **opaque, member-secret-derived id that is unlinkable to the DS
`group_id`**. The claim is **NOT** "the social graph is no longer materialised on
the server". push-gw still stores device-to-routing-id rows and the providers
still receive a **stable per-group pseudonym** in cleartext; what is gone is the
ability to join that pseudonym to the group's DS identity. The residual clustering
is tracked as **G-36**.

Six sub-invariants — 1-5 from ADR-0042, 6 added by
[ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md):

1. **The routing id is algebraically independent of the `group_id`.**
   `push_gid = keyed-BLAKE3(push_routing_secret,
   "lakko/push/group-routing-id/v1")` (`crates/lakko-contacts/src/push_routing.rs`
   `derive_push_gid`), where `push_routing_secret` is **32 CSPRNG bytes** minted
   once per group and never derived from the `group_id`. The derivation is
   one-way, domain-separated, and golden-pinned (`derive_push_gid_golden`), so
   every platform computes the same id. Because the secret is independent random
   input, `push_gid` carries **no algebraic relation** to `group_id`: a
   push-gw-vs-DS cross-correlation is **impossible without the secret**, which
   neither server holds. The output is 32 bytes, so it drops into push-gw's
   existing `BINARY(32)` / hex key-space unchanged.

   **The derivation is UNCHANGED by the ADR-0044 v2 envelope, and that is a
   load-bearing property rather than an incidental one.** The `mint_epoch` v2
   added is an **ordering** field only: it never enters `derive_push_gid`
   (`derive_push_gid_ignores_the_mint_epoch`), so the golden vector, the domain
   message, and the function's whole signature are untouched. The consequence is
   the one that matters operationally: `push_gid` remains a **stable** per-group
   value with **no epoch input**, so a subscriber survives ordinary membership
   commits **without re-subscribing** and the iOS `thread-id` stays constant. Had
   the epoch been folded into the derivation, every commit would have silently
   re-keyed the routing id and every member would have had to re-subscribe — a
   far larger change than the one ADR-0044 was making. Rotation on a member
   removal (sub-invariant 6) changes the routing id by changing the **secret**,
   never by changing the derivation.

2. **The secret is distributed only inside the E2EE group.** It rides an **MLS
   application message** on the group journal as a fixed-length domain-separated
   envelope, so the DS and push-gw see only **ciphertext** (invariant #2) and the
   separator's trailing `0x00` keeps it byte-distinct from every sibling envelope
   kind (`separator_distinct_from_sibling_domains`, plus a no-cross-dispatch
   test). The codec is bounded and fail-closed — exact-length + domain-match +
   version gate, `WrongDomain` / `BadLength` / `UnknownVersion` — and is fuzzed
   (`push_routing_secret_decode`, the same threat class as `media_state_decode`,
   since the receive-loop dispatch reaches it on attacker-touchable journal bytes
   before any state is consulted).

   **AMENDED by [ADR-0044](../adr/0044-push-routing-secret-recency.md): the
   envelope is v2, 70 bytes.** It was
   `PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1 || version || secret(32)` (62 bytes);
   it is now
   `PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1 || version=0x02 || mint_epoch(8, BE u64) || secret(32)`.
   The **29 domain bytes are byte-identical** and frozen forever
   (`domain_separator_bytes_survived_the_v2_bump`) — they are the
   `incoming_envelope` dispatch prefix, so changing them would make a peer fall
   through to the chat parser; the in-payload version byte is the knob that moves,
   and `_V1` in the constant's name denotes the envelope **family**, not the
   schema version. This is a **hard flag day with no dual-accept**: a retired v1
   envelope is refused with `UnknownVersion`, because it carries no `mint_epoch`
   and so has **no position in the C-62 order** — admitting one would re-open the
   replay v2 exists to close. The decoder deliberately gates **version before
   length**, so a v1 envelope names the actual problem rather than reporting a
   truncation-looking `BadLength`. Both cross-version refusals (old client meeting
   v2 -> `BadLength`; new client meeting v1 -> `UnknownVersion`) are fail-closed to
   **defer**, never to the raw `group_id`, so invariant #4 held through the
   transition. The `IncomingEnvelope::PushRoutingSecret` dispatch variant carries
   the `mint_epoch` alongside the secret, and the fuzz target drives both the
   widened parse and the order it feeds.

3. **No client ever subscribes or notifies under the raw `group_id` (invariant
   #4).** A member that does not hold the secret **defers**: it does not subscribe
   at all, rather than falling back to the leaky DS key
   (`subscribeGroupUnderPushGid` returns `"deferred"`). There is no downgrade
   path. The functional cost is explicit and accepted — a legacy / no-role-table
   group has no owner, so nobody mints and every member defers, meaning such
   groups receive **no push at all** (G-36).

4. **The receive path is authority-gated fail-closed.** The gate is a **pure,
   host-testable decision function** on each of the three clients, fed by the MLS
   **sender leaf** (surfaced the same way the ADR-0023 author-only gate does) and
   resolved to a signature public key via the live group's `members()`, then
   compared against the ADR-0022 role table. Ranking is **owner > admin >
   member**, expressed as who may overwrite what: an **identical** secret is
   accepted from anyone (it rotates nothing, keeping the post-add redistribute
   idempotent); the **owner** may always rotate, and its branch is evaluated
   **before** anything is read about what is stored, which is what makes owner
   priority independent of envelope arrival order; an **admin** may **rotate**
   (see the ADR-0043 amendment below; ADR-0042 as shipped allowed an admin only to
   **seed** a provably-absent secret, closing the real regression where a member
   added by a non-owner admin would never receive one); a **plain member** is
   refused; an **unreadable role table** or an **unresolvable sender** is refused.
   A group with **no role table** is *accepted* (the legacy / open-group case) —
   "no role table" and "read failed" are kept as **distinct** states rather than
   one nullable, since collapsing them is exactly the bug class this gate exists
   to prevent. Without this gate any single non-owner could unilaterally rotate
   the group's routing id (the `lakko-security` MEDIUM-1 finding, fixed in
   `21554f2` and refined in `62f23ad`).

   **AMENDED by [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md)
   (ratified option (b), `935250a` / `a6ed544`): an admin may now OVERWRITE, not
   only seed.** Web renamed the verdict `reject-admin-overwrite` ->
   `accept-admin-rotate`; both natives keep a binary accept/reject and flip the
   row. Consequently the former sub-clause "a store-read failure is never allowed
   to read as a fillable hole on the admin branch" **no longer applies** and its
   Android mechanism (stripping the admin set when the secret store could not be
   read) was **deliberately deleted** — under (b) an admin may overwrite whatever
   is stored, so the rule would have refused a legitimate admin rotation on a
   transient store error. Every other property above is unchanged and still pinned
   in code. **The bound that keeps this safe: rotation NEVER MINTS** — a device
   holding no secret is a no-op, so mint authority remains **owner-only** and the
   `push_gid` can never diverge into two. The cost is a widened DoS residual
   (admin-seed -> admin-churn, **G-36** residual 3). ADR-0043's mandatory
   `lakko-security` pass for option (b) **was run: APPROVE-WITH-NOTES, no
   CRITICAL/HIGH, no invariant #2/#3/#4 break on the live path**, its one blocking
   MEDIUM (a coalescing rotation lock that folded a second removal into an
   in-flight rotation on web + iOS) fixed by adopting Android's serializing lock.

   **COMPOSED WITH (not replaced by) [ADR-0044](../adr/0044-push-routing-secret-recency.md).**
   Everything above is an **authority** question, and that was the whole of the
   gate's decision surface until ADR-0044 — it could ask *who sent this* but never
   *is this newer than what I hold*, which is what made a silent permanent rotation
   undo reachable (**G-36 residual 9**). ADR-0044 adds an **ordering** gate
   alongside it, pinned separately as **C-62**. The authority rules here are
   **unchanged** — same rank, same owner-evaluated-first branch, same fail-closed
   unreadable-role-table and unresolvable-sender refusals, same legacy
   no-role-table acceptance. The clean split: **authority decides WHO may write;
   the order decides WHICH write survives**, and the order arbitrates only between
   envelopes that already cleared authority. One consequence is a **deliberate
   narrowing of a property stated above**: a total order and *unconditional*
   owner-priority cannot both hold, so while the owner still wins whenever it mints
   at a later epoch (the normal case), at an **exact epoch tie** the secret bytes
   decide and the owner can lose to an admin. Arrival-order independence — the
   property owner-priority existed to provide — is preserved by the strictly
   stronger mechanism of a total order, which is arrival-order independent for
   *every* participant rather than only for the owner's envelope. Recorded as a
   narrowing in the same way ADR-0043's removed Android strip-admins rule was.

5. **The server is untouched and stays opaque (invariant #2).** `services/push-gw`
   is **byte-identical** across the whole arc (`git diff 51fe05d..HEAD --
   services/` is empty): it never parsed or interpreted the value's meaning, only
   its 32-byte width, so only the value's *meaning* shifted from "DS journal key"
   to "opaque routing id". The ADR-0041 background decrypt (C-59) gained a
   `push_gid -> (account, group_id)` reverse-map through a **plaintext local**
   index the background process reads; the DS fetch keeps using the **real**
   `group_id`, and the read-only-throwaway discipline of C-59 is unchanged.

6. **The id is forward-unlinkable across a member removal
   ([ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md)).** On a
   membership **shrink** — detected as a set difference over member Ed25519
   signature keys, not counts and not leaf indices, because OpenMLS reuses a
   vacated leaf for the next Add — the owner **or an admin** re-mints the secret
   and redistributes it **strictly after** the Remove commit, so the envelope is
   sealed at the post-remove epoch and RFC 9420 forward secrecy denies it to the
   evicted leaf. The old secret is read **before** the in-place overwrite so the
   retiring `push_gid` can still be named. Every remaining member then retires
   **its own** old gid (unsubscribe + delete the reverse-index row) — required,
   not hygiene: without it each rotation would leave one dead row per member,
   turning a live cluster into a live cluster **plus an archive of every previous
   one**. The evicted device's own row is deleted by **no other party** (none can),
   so from the group's side it goes **INERT**, because nothing ever notifies the
   old gid again. ~~and no client has "you were removed" handling~~ — **that clause
   stopped holding on 2026-08-29 (`8763f3e`)**: the ADR-0045 self-eviction work gave
   every client a `retire-push-subscription` teardown step, so **the evicted device
   deletes its own row**. It complements rotation and replaces nothing — it needs
   the evicted device to come online and run it, so a device that never returns
   still leaves an inert row. Rotation fires only on a
   shrink, never on an add, and **never mints**.

**Residual (G-36).** push-gw and the push providers still observe a
**co-membership cluster** — "these N device tokens share one opaque group handle"
— and can track its churn; only the linkage to the DS group identity is removed.
**Rotation renames that cluster but does not unlink it**: the token set is
identical-or-near-identical either side of a rotation, so a provider **re-links by
intersection**, and the set *difference* even **names the evicted device** — a
removal is now a *visible re-keying event* where before it was invisible. That
trade was made deliberately (it closes a live channel to an already-adversarial
evicted party at the cost of legibility to a party already watching the cluster);
closing the cluster leak itself needs **per-device-blinded routing ids**. A
malicious **admin** can now churn the whole group's routing id at will, not merely
seed a member holding none (widened by ADR-0043's option (b); bounded — foreground
polling still delivers). A legacy / no-role-table group gets no push and now also
never rotates. A crash between the persist and the teardown orphans an old gid
(inert, but permanent). `lakko-security` **APPROVE-WITH-NOTES**, no CRITICAL/HIGH
— for **both** ADR-0042 and ADR-0043; the latter's review found no invariant
#2/#3/#4 break on the live path, and its one blocking MEDIUM (a coalescing
rotation lock, web + iOS) is fixed.

### C-61: The push-routing-secret rotation is sealed at the post-Remove epoch

[ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md) rotates the
`push_routing_secret` when a member is removed, so that the evicted member's
`push_gid` goes inert (C-60 sub-invariant 6). **That property rests entirely on one
sequencing rule**, which is why it is pinned on its own line rather than left
inside C-60 — the same reason the call-key overlap rules C-8 / C-9 / C-10 are
separate entries:

> **The rotation broadcast MUST be emitted strictly AFTER the Remove commit is
> published.**

The new secret rides an **MLS application message**. Emitted at epoch N
(pre-removal), the member being evicted is still a member of that epoch and **can
open it** — learning the new secret and defeating the entire rotation, silently.
Emitted at epoch N+1 it is unreadable to the removed leaf, because RFC 9420 §12
forward secrecy is precisely the property that denies the removed leaf the
post-Remove epoch's keys.

A **second**, independent ordering rule protects the teardown: **the OLD secret
must be read BEFORE the in-place store overwrite.** The secret row is keyed
`(account, group)` and is clobbered in place, and the unsubscribe needs the OLD
`push_gid`; persist-first permanently loses the ability to name the row being
retired.

**Enforcement status.** (Line anchors are as of `a6ed544`, the commit that landed
the native mirrors; symbol names are the durable reference.)

| Rule | How enforced | Test |
|---|---|---|
| Broadcast follows the Remove commit | Structural — the call-site sequencing: web awaits `removeMemberFromGroup` before the rotate call (`Chat.tsx:2267-2271` -> `:2293-2299`); iOS awaits the publish at `ChatRepository.swift:891-894` before `:917`; Android rotates at `:842`, after the `groupsLock` block closes at `:820`. On the poll path it holds because the commit is applied by the poll before the membership diff runs. [ADR-0044](../adr/0044-push-routing-secret-recency.md) additionally moved the sequencing behind **one named seam** per client (e.g. iOS `PushGroupRouting.publishRemoveThenRotate`, whose sole production call site is `ChatRepository.removeMemberFromGroup`), so the two steps can no longer be reordered independently | **Pinned on all three clients** since `936d849` (web) / `014e84e` (iOS + Android) — six tests on web, five on iOS, six on Android |
| Old secret read before overwrite | Executed order inside the rotation/receive paths | Pinned, e.g. `apps/ios/LakkoTests/PushGroupRoutingTests.swift:1805` ("the teardown names the OLD id") and `:1680` both assert the unsubscribe targets the **old** gid |

**This entry previously read "NONE — no test pins it on any client", and that is
the state ADR-0043 shipped in.** The rule was exactly the kind a refactor can
silently break: moving the rotation call above the publish, or making the publish
non-blocking, would have passed every existing test while quietly restoring the
leak, invisibly at runtime. ADR-0043 stated the ordering "must be asserted in a
test"; the requirement went unmet and was recorded as a gap under **G-36**
(residual 6) and independently as `lakko-security` **F3** (LOW).

**How it is pinned now.** Each client asserts on the **emitted envelope's
`mint_epoch` stamp** rather than on call order — a strictly stronger check, since
it fails on any path that reaches the broadcast carrying a pre-removal epoch,
however the reordering arose — and each carries a **permanent inverted-order
companion** that runs the swap and asserts the consequence (web `the INVERTED
order would stamp the pre-removal epoch and lose the guarantee`; iOS
`testTheInvertedOrderStampsThePreRemovalEpochAndTheRotationLoses`; Android
`` `C-61 - the INVERTED order stamps PRE-removal and the rotation LOSES` ``, which
also asserts the receive gate resolves the stale replay as `ACCEPT_OWNER` over the
rotation). On both natives the discrimination was **proven by running the swap**,
not asserted: iOS temporarily added an inverted copy and recorded three failures;
Android replaced the seam with rotate-then-advance and recorded the stamp
assertion failing 11 vs 10. Both probes were reverted. The fixture is load-bearing
— at an epoch tie the bytewise-greater secret wins, so the stale secret is filled
`0xFF` (the maximum) to make the rotation lose deterministically under the swap;
the first web cut used `0xEE`, against which a random rotation secret sorts higher
roughly once in fourteen runs, so the test would occasionally have proved nothing.

**ADR-0044 also sharpened what a regression costs, which is why the tests stopped
being optional.** Under ADR-0043 alone, breaking this ordering cost the rotation's
**forward secrecy** — the envelope sealed at the pre-removal epoch, openable by the
evicted member. Under ADR-0044 it costs that **and inverts the C-62 recency
comparison**: a rotation stamped at the pre-removal epoch `N` **ties** with a stale
secret minted at `N`, the tie falls to the secret bytes, and the rotation may
simply **lose**. The guarantee degrades from "the rotation always wins" to a
**silent coin flip**. C-61 is therefore no longer a precondition of one property
but of two — C-60 sub-invariant 6 and C-62's closure of the stale-replay undo.

**AMENDED 2026-08-25 by [ADR-0045](../adr/0045-remote-commit-epoch-fork.md): the
rule is unchanged, but the place it attaches to moved.** ADR-0043 seamed the
ordering onto `removeMemberFromGroup` on all three clients, on the premise that a
removal always lands there. Under ADR-0045 it usually does **not**: in a group with
an ADR-0022 role table a removal becomes a **proposal**, which advances no epoch, so
firing the rotation at that call site would seal the new secret at an epoch **the
member being evicted is still in and can open** — the exact violation this entry
exists to prevent, reached without touching a line of rotation code, and (per the
paragraph above) also inverting the C-62 comparison so the rotation may lose the
byte tiebreak. The rotation therefore moves to **where the removal actually lands**:
the **sweep**, via the membership-**shrink** diff (a set difference over member
Ed25519 signature keys) that the poll already runs for someone else's removal. Web
drives it from `sweepNow`; Android reaches the same place from the other end, with a
removal-landed flag the rotate step consults so `publishRemoveThenRotate` skips when
nothing was removed. **This is the general form of the amendment and it is the part
worth carrying forward: any client discipline seamed onto a direct-commit call site
has to be re-seamed onto the sweep, and the ones that matter are the ones whose
correctness argument mentions an epoch.** The named-seam requirement and the
test-pinning above are unchanged; what changed is which operation the seam wraps.

Invariants: **#2** (the envelope is opaque to the DS and to push-gw either way —
this is not a server-visible property but a client sequencing one), **#4** (a
member without the current secret defers rather than downgrading; the rotation adds
no fallback path), **#5** (the forward-unlinkability this ordering buys is the
whole of C-60 sub-invariant 6).

### C-62: The push-routing secret is totally ordered by its mint epoch, and only a strictly newer envelope may displace the stored one

[ADR-0044](../adr/0044-push-routing-secret-recency.md). C-60 sub-invariant 4 gave
the receive path an **authority** gate — it can ask *who sent this*. Until this
entry there was **no ordering gate at all**: nothing anywhere could ask *is this
newer than what I hold*, so a **retry** of the current secret and a **replay** of a
superseded one were byte-indistinguishable to every client. That omission was
structural rather than a bug in one client — all three decision cores took only
authority inputs — and it made a **silent, permanent rotation undo** reachable
with no malice and no crash (`lakko-security` **F-A**, **G-36 residual 9**, which
this entry closes for the single-context case — see sub-invariant 5): a device
holding a stale secret in an un-settled `pending` row re-broadcasts it after
somebody else has rotated, every member's gate sees an authorised sender and
accepts, and the group migrates **back** onto the routing id an evicted member
still derives. With no further membership shrink there is no further rotation, so
the state does not self-heal.

This entry is pinned separately from C-60 for the same reason C-61 is: it is a
distinct rule with a distinct failure mode, and C-60's authority gate is
**unchanged** by it. **Five** sub-invariants — four about the order itself, and a
fifth about the path that consumes it, added after the arc's own review showed
that the first four are inert without it.

1. **The order is total, canonical, and defined exactly once.** Envelopes are
   ordered on the pair `(mint_epoch, secret_bytes)` — the 8-byte big-endian epoch
   first, then the 32 secret bytes compared lexicographically — and an incoming
   envelope displaces a stored one only when **strictly greater**. The order lives
   in `crates/lakko-contacts/src/push_routing.rs` as
   `push_routing_secret_is_newer(incoming_epoch, incoming_secret, stored_epoch,
   stored_secret)` (plus the `PushRoutingSecretEnvelope::supersedes` wrapper), and
   **every client routes its accept/refuse decision through it over the FFI**
   (`pushRoutingSecretIsNewer` on both uniffi and wasm); none re-implements the
   comparison. That is deliberate: three clients each hand-rolling a comparison is
   precisely the drift class that produced the finding this entry closes. Two
   properties are load-bearing and pinned — **equal is NOT newer** (what keeps a
   benign retry from reading as an update, and what makes the `accept-identical`
   fast path safe), and **wrong-length secrets fail closed to `false`** (a
   malformed incoming secret must never displace a stored one, and a corrupt
   stored secret is better left in place than overwritten on a comparison against
   a value that was never a secret). The function never panics. The **secret bytes**
   are the tiebreak because they are the only discriminator both available to every
   receiver by construction and **stable across relay** — a sender-derived tiebreak
   (leaf index, role rank) would give the same bytes a different position depending
   on who forwarded them, and a minter identity carried in the envelope is
   self-asserted. The tiebreak is the **common path, not a corner case**: under
   ADR-0043 option (b) one member removal produces **N+1 same-epoch mints** (owner
   plus N admins), so epoch alone would leave them mutually incomparable.
   Pinned by `is_newer_is_a_strict_total_order`,
   `is_newer_matches_lexicographic_wire_bytes`,
   `is_newer_rejects_a_wrong_length_secret`, `an_identical_pair_is_not_newer`,
   `a_stale_rebroadcast_never_undoes_a_rotation`,
   `convergence_is_arrival_order_independent`, and the
   `push_routing_secret_decode` fuzz target, which asserts irreflexivity on
   attacker-chosen bytes.

2. **The mint epoch is durable state, and every re-broadcast replays it
   verbatim.** `mint_epoch` is stamped **once, at mint time**, and then travels
   with that secret for the secret's whole life. Only a mint reads
   `Group::epoch()`. Every path that recomposes an envelope from an
   already-stored secret — six such sites, two per client — **must** replay the
   stored stamp, because re-stamping at send time would make a stale envelope
   arrive **newer** than the live rotation and reproduce the finding with the
   ordering gate fully in place and working as specified. This is therefore a
   **store** property, not only a wire one: the epoch is persisted per
   `(account, group)` on all three clients, alongside the secret and the
   `distributed` flag in **one atomic write**, and the pair is read through a
   single store call so it cannot tear against a concurrent receive. Legacy
   unstamped rows read at **epoch 0**, the floor of the order, so any stamped mint
   supersedes them. **Android additionally makes the rule impossible to break by
   ACCIDENT**: `composeEnvelope` takes a sealed `MintStamp` whose `Fresh` and
   `Replayed` subtypes have `internal` constructors with one producer each in
   `src/main` (the port reading the live group epoch, and the stored row). The
   claim is bounded — Kotlin `internal` is **module**-scoped and the gate, the
   repository and the chat client share the `:app` module, and `StoredSecret`'s
   constructor is public, so a wrong stamp remains *constructible*. What the
   sealed type removes is the **silent** failure mode: a bare `ULong` cannot be
   passed where a stamp is expected (a compile error), so getting it wrong
   requires a differently-named, reviewer-visible call rather than an accidental
   slip.

3. **An incoming stamp is clamped against the receiver's own group epoch, before
   authority is consulted.** An ordering rule with no upper bound hands an
   authorised writer a **strictly worse capability than the one it removes**:
   stamp `u64::MAX` and nothing can ever exceed it, so the group's routing id is
   **frozen permanently** and no later owner rotation can win. That would escalate
   **G-36 residual 3** from "a malicious admin can churn the routing id"
   (self-repairing at the owner's next rotation) to "a malicious admin can freeze
   it forever" (which does not) — trading a recoverable denial for an
   unrecoverable one. The clamp refuses any envelope whose `mint_epoch` exceeds
   the receiver's own current MLS group epoch, and is evaluated **before**
   authority precisely so that **not even the owner** can freeze the group. It
   never rejects an honest envelope, because
   `stamp <= seal epoch <= receiver's epoch at dispatch`: the minter stamps at
   mint time and the group epoch only advances afterwards, and a receiver applies
   commits up to and including the one carrying the message before dispatching its
   plaintext. A re-broadcast widens that gap but never inverts it. **This clamp is
   the client's obligation, not the core's** — `lakko-contacts` treats
   `mint_epoch` as opaque, never reading it as a time, never bounding it, never
   feeding it to `derive_push_gid`; it owns the **order** and requires only that
   one group's mints be non-decreasing. Sourcing the stamp from `Group::epoch()`
   is the **client-side binding** that makes the clamp checkable at all, since an
   MLS epoch is shared state every receiver independently observes. A free-running
   counter could be bounded by nothing. Every client reads the epoch **after** its
   serializing lock (iOS a provider closure, Android a suspending port, web inside
   the queued run), so a rotation queued behind a predecessor stamps the epoch it
   actually mints at rather than the one it was triggered at.

4. **`accept-identical` must never move the stored stamp.** The identical-bytes
   fast path is accepted from **anyone** and sits ahead of both the authority gate
   and the clamp, which is correct — it is what keeps a retry, a relay, and the
   post-add redistribute idempotent. It follows that the path may write **nothing**:
   re-persisting the *incoming* epoch there is an **epoch-inflation channel that
   reaches the exact freeze sub-invariant 3 prevents, by going around it**. A
   **plain member** — who can never clear the authority gate, and whose envelope
   would be clamped if it carried a future stamp — relays the group's **current**
   secret bytes stamped `u64::MAX`; rule 1 matches on the bytes alone, so neither
   gate runs, and every subsequent rotation *including the owner's* then loses the
   recency comparison forever. This was a **live defect in the first
   implementation**, uncovered by the web verdict tests, and is fixed on all three
   clients by preserving the **stored** stamp on that path, each with a named
   regression test that additionally proves a later owner rotation still lands.
   Generalised: **a rule that short-circuits a gate must not write any state that
   gate protects.**

5. **The gate must run against CURRENT state: its read, decision and persist are
   serialized per `(account, group)`.** This is the sub-invariant the arc's own
   review had to teach it, and it is the reason the other four are not
   self-sufficient. The gate is a **read-modify-write** — read the stored
   `(secret, mint_epoch)`, decide, persist — with suspension points between the
   read and the write. If two envelopes decide against the **same snapshot**, a
   correct total order changes nothing: the survivor is whichever task wrote last.
   **Stated generally, and this is the durable lesson: an ordering rule is a
   property of the state the deciding read observed, so a total order is only
   meaningful if the gate consuming it runs against current state.** Enforcement is
   **one serializing (never coalescing) lock per `(account, group)`**, held across
   the whole read-decide-write, and **shared** across **four** entry points —
   receive, rotate, ensure, **redistribute**. Two design points are load-bearing
   rather than incidental: the lock must be **shared**, not one per entry point
   (Android proved this by probe — with receive on its own lock the F-A and
   N-rotation cases pass while **F-E** still fails), and it must be **serializing**,
   because a coalescing lock folds a second, distinct trigger into an in-flight one
   (the ADR-0043 F1 finding). `redistribute` is included because it is the same
   read-then-write and unserialized it puts a **PRE-rotation** secret back on the
   wire mid-rotation. **Every network call taken under the lock is individually
   bounded** — web 30 s, iOS 15 s, Android 15 s — because a wedged row is not
   merely unavailable: it is the row that never applies a peer's rotation, so the
   device keeps notifying an id an evicted member still derives. That timeout is
   **fail-closed for the operation** (an expired call is treated as failed, never
   as optimistically succeeded; neither the authority gate nor the order is
   relaxed, and both still run in full inside the lock against a fresh read) and
   **fail-open for the row** (the lock is released so the queue proceeds, because
   the work queued behind it is precisely the peer rotation that retires an evicted
   member's id). **This is not the ADR-0033 fail-open**, which relaxes an
   anti-abuse *gate*; here no gate is relaxed at all. Serialization is per
   **process context**, which is the exact boundary of this sub-invariant's claim —
   see the status note below.

**What this buys.** All five sub-invariants are **built and armed on all three
clients** — `936d849` / `014e84e` (the order, the clamp, the persist-verbatim
rule, the `accept-identical` hardening) and `9a0f539` (the serializing lock), with
the cross-tab and bounded-hold follow-ups landing on top.

The path was not straight, and the record is kept because the lesson is. The
`lakko-security` review of the **first** landing returned **REQUEST CHANGES** and
**reproduced the defect by execution** on web: two envelopes driven concurrently
the way `Chat.tsx` actually drives them produced verdicts
`['accept-admin-rotate', 'accept-identical']` and a final stored secret of `0xEE`
at epoch 10 — the **stale** one (**G-36 residual 10**). Sub-invariant 4 was the
vehicle: `accept-identical` deliberately short-circuits both the clamp and the
order, which is correct for a benign retry and which sub-invariant 4 correctly
stopped from moving the stored epoch — but nothing stopped it being **reached on a
stale snapshot**, and the losing gate files the *stored* epoch it read, the stale
one. **The gate was bypassed, not defeated**: nothing in that finding said the
order was wrong, which is why this entry stands as written with sub-invariant 5
added rather than being withdrawn. The review of the fix returned **APPROVE WITH
NOTES**, no CRITICAL / HIGH; both of its actionable findings — a web lock that was
per-JS-realm rather than cross-tab, and the lock held across unbounded network
I/O — are closed.

Consequently **G-36 residual 9 closes**, and residual 7's concurrent-rotator core
closes — the N+1 same-epoch mints from one removal are totally ordered *and* now
compared against current state, so devices converge on the single maximum rather
than by scheduling order. Sharing one lock rather than several additionally closes
**residual 5's rotate-vs-receive (F-E) path**.

**The qualification on those closures is exact and is sub-invariant 5's scope: the
single PROCESS CONTEXT.** On web the cross-tab boundary is covered by a Web Lock
(`navigator.locks`) nested inside the in-process queue — but that API is
**secure-context only**, so an insecure-context origin, an old browser or an
embedded webview degrades to same-tab-only serialization. The in-process queue is
deliberately kept beneath it precisely so that state is a **degradation** rather
than a silent unserialized run. On the natives the equivalent multi-process
boundary — the ADR-0041 / C-59 NSE and background decrypt versus the app — was
**not examined** by this arc. Tracked as the re-scoped **G-36 residual 10**.

Three things stay out of scope regardless: the order **does not repair a device
already holding a stale value** (it stops a stale value spreading; it pushes
nothing), it has no bearing on residual 7's **F-D stale-role window**, which is an
authority refusal, and residuals 1 / 3 / 4 / 8 are untouched.

**One transition-window caveat on sub-invariant 2.** A legacy unstamped row reads
`mint_epoch = 0` on all three platforms — the floor, so any stamped mint supersedes
it, which is the right direction. But during the flag day **two different pre-v2
secrets both sit at 0**, so a current-versus-superseded pair among them falls to
the byte tiebreak rather than to recency — the same failure this entry exists to
prevent, once, at roughly even odds for that pair. Bounded on both sides: confined
to the transition window, and self-healing at the next rotation, which follows a
Remove commit and is therefore always stamped strictly above 0.

**Enforcement and shape.** The wire carrier is the v2 envelope (C-60
sub-invariant 2), a hard flag day with no dual-accept, since a v1 envelope carries
no `mint_epoch` and therefore has **no position in this order** — admitting one
would re-open the finding for exactly the envelopes a stale device sends. The
receive gate's rule order on every client is: identical -> **reject-future** ->
authority (unchanged) -> no-stored -> **is-newer** -> **reject-stale**. Both new
refusals **drop** the envelope entirely; neither downgrades anything. Web and
Android name every verdict individually (9 members each); iOS keeps a coarser
5-case enum, collapsing the authority accepts and refusals and carrying the
distinctions as diagnostic tags, and maps an unreadable own epoch to
`.rejectFuture` where the other two use `reject-unverifiable` — different labels,
identical fail-closed effect.

That whole rule sequence runs **inside** the sub-invariant-5 lock, against a read
taken inside it: web `withRoutingRowLock` (the in-process queue plus a nested
`withWebLock`, with `withNetworkTimeout` / `ROUTING_NETWORK_TIMEOUT_MS` bounding
each call under it) in `apps/web/src/push/group-routing.ts`; iOS `withRoutingLock`
plus `withNetworkTimeout` / `defaultNetworkCallTimeout` in
`apps/ios/Lakko/Push/PushGroupRouting.swift`; Android `routingLock` plus `bounded`
/ `NETWORK_CALL_TIMEOUT_MS` — and `boundedOrNull` on `freshMintStamp()`, for the
`ChatRepository.groupsLock` reason recorded as **G-37** — in
`apps/android/.../push/GroupPushRouting.kt`.

Invariants: **#2** (the `mint_epoch` rides inside the same E2EE MLS
application-message plaintext; the DS sees ciphertext, push-gw sees only the
derived 32-byte id and is **byte-identical unchanged**), **#3** (no
`crates/lakko-mls` edit — the clients read `Group::epoch()`, an accessor that
already existed on both FFI surfaces), **#4** (the fail-closed surface **widens**:
reject-future, reject-stale, and both cross-version refusals all drop or defer,
never downgrade; the codec stays bounded and exact-length. The one fail-**open** in
this entry is scoped to the lock's *hold time* under sub-invariant 5 and relaxes
**no gate** — every expired call is treated as failed, and both the authority gate
and the order still run in full against a fresh in-lock read), **#5** (unchanged
posture — this restores the ADR-0043 forward-unlinkability in the cases the
finding was silently revoking it, and does not touch the co-membership cluster).

### C-63: A commit must never leave the group with an owner who is not a member of it

[ADR-0045](../adr/0045-remote-commit-epoch-fork.md), extending
[ADR-0022](../adr/0022-group-admin-roles.md). ADR-0022 put a group's authority in
a `RoleTable` carried in the authenticated `0xFF02` GroupContext extension, keyed
on Ed25519 **signature public keys**. A table names keys; it does not name group
members, and nothing in the codec relates the two. So the table has always been
able to say something the group cannot survive: **that the owner is a key no leaf
holds.**

**Why that is terminal rather than merely wrong.** Under ADR-0022 a role-table
change is **owner-only**, and the owner's leaf is **unremovable**. An owner
outside the group therefore cannot be replaced (nobody is authorised to rewrite
the table), cannot be removed (the rule refuses it, and there is no such leaf to
remove anyway), and cannot act (no device holds the key). The group keeps working
as a chat and can never again add a member, remove one, or change a role. There is
no recovery inside the protocol.

**This is a pre-existing hole, not a guard on a new feature — which is why it is
indexed on its own merit.** `Group::set_group_roles` has accepted a caller-built
`RoleTable` since ADR-0022 shipped, with no membership check anywhere, so an owner
could always reach this state with one ordinary commit. The rule below refuses
that commit **on every peer**, whether or not anyone ever calls the ownership-transfer
API that occasioned it.

#### The rule

`crates/lakko-mls/src/group_proposals.rs` `owner_survives(effects, members)` is the
only rule in the crate that judges a **commit** rather than a proposal. It is
evaluated against the roster the commit **produces** — pre-merge members, minus
every leaf the commit removes, plus every key it adds — because that is the roster
the new table takes effect over. It refuses:

1. a commit whose role-table rewrite names an **owner who would not be a member**
   of the resulting group; and
2. a commit that **rewrites the GroupContext without leaving a readable `0xFF02`
   table behind** — which would delete the group's authority structure rather
   than change it, silently reverting a role-enabled group to the open/legacy
   shape in which any member may add or remove.

A commit carrying no role change is always authorised by this rule.

#### Why it cannot be folded into the per-proposal gate

Both reachable paths are invisible to a single-proposal check, and for different
reasons.

- **Transfer to a non-member.** Deciding it needs the **roster**, which a rule
  about one proposal's authorisation does not consult. ADR-0022's
  `authorise` asks *may this author ask for this?* — and the answer is yes: the
  owner may rewrite the table. The table's *content* is what is wrong.
- **Transfer to a member the same commit removes.** ADR-0045 composes N intents
  into one commit, so "hand the group to Charlie" and "remove Charlie" can ride
  together. Each is **individually authorised** — the owner may rewrite the table,
  an admin may remove a plain member — so no per-proposal rule can see the
  combination. Only the post-commit roster can. This path is one ADR-0045
  **created**; path 1 predates it.

#### Enforcement, and where it is fail-closed

Three layers, and the third is the boundary:

1. **Authoring, locally.** `Group::transfer_ownership` and
   `Group::propose_ownership_transfer` both run `plan_ownership_transfer` **before
   any OpenMLS call**, so a refusal leaves the group exactly as it was — no staged
   commit, no epoch advance, nothing to roll back (ADR-0040). It refuses a
   non-owner caller, a key that is not 32 bytes, a key that is already the owner,
   and a key that is not in `members()`.
2. **Sweeping.** The ADR-0045 committer evaluates `owner_survives` over the
   authorised pending set and **excludes the role change** rather than publishing
   a commit every peer would refuse. Excluding a role change removes nobody, so it
   cannot change the answer — one pass suffices, with no fixpoint to iterate.
3. **On receipt — the boundary.** `Group::process_incoming_verifying_roles` ->
   `authorise_role_commit` runs `owner_survives` over the staged commit's
   proposals against the pre-merge roster and **refuses to merge** (fail-closed,
   invariant #4) rather than applying and then complaining. It runs **first**,
   ahead of the per-proposal loop, so a commit that would brick the group is
   refused before anything else about it is considered. A malicious or buggy
   committer would not run layers 1 and 2, which is exactly why layer 3 does not
   trust it.

#### What it does not do

- **It does not recover a lost owner.** Minting a transfer needs the owner's key,
  so this rule protects a group from being *handed* into an unowned state; it says
  nothing about an owner who has simply gone. That remains ADR-0022's open
  admin-quorum question.
- **It is peer-verified, not cryptographically unforgeable.** Like every ADR-0022
  and ADR-0015 gate, a colluding majority could agree among themselves to ignore
  it. That is inherent to a leaderless protocol and is the same residual those
  ADRs record.
- **It does not extend to groups with no role table.** A group with no `0xFF02`
  table has no owner to orphan, and stays on the open/legacy path ADR-0022 defined.
  `Group::create` has seeded a table since ADR-0022, so no freshly created group
  reaches it.

#### Tests

`crates/lakko-mls/tests/group_ownership_transfer.rs` (12), notably
`a_peer_refuses_a_commit_that_hands_the_group_to_a_non_member` and
`a_peer_refuses_a_commit_that_erases_the_role_table` for the boundary,
`transfer_to_a_non_member_is_refused_locally` for layer 1, and
`a_sweep_drops_a_transfer_to_someone_the_same_commit_removes` for layer 2 and the
composed-commit path. `transfer_then_the_former_owner_leaves_and_the_group_works`
walks the whole intended flow.

Invariants: **#2** (the role table is an opaque `0xFF02` GroupContext body; the DS
relays the commit as ciphertext and parses nothing — unchanged), **#3** (a
`crates/lakko-mls` state-machine change under ADR-0045; property-tested, **the
`lakko-security` pass ran 2026-08-29 (REQUEST CHANGES, seven findings, all closed);
the external crypto review is tracked once in [G-39](known-gaps.md) per
[ADR-0046](../adr/0046-external-crypto-review.md) as a release gate**), **#4** (refuse-to-merge,
never apply-then-warn), **#5** (roles stay in-group; no server ever learns them).

**Amended 2026-08-25 by the client sprints (`1de5fe7` / `1f230ab`).** Two of the
three enforcement layers acquired production callers. **Layer 2 is live for the
first time** — there was no sweep on any client until S4a, so the sweep-side
exclusion had nothing to run in. **Layer 3, the boundary, was already live**
through each client's `processIncomingVerifyingRoles` receive path and is unchanged.
**Layer 1 had no production caller anywhere**: `transferOwnership` /
`proposeOwnershipTransfer` were on both FFI surfaces and no client offered an
ownership-transfer affordance, so the authoring pre-check was reachable only from
tests. That was a **UX gap and not a weakening** — layer 3 is the boundary
precisely because it does not trust layer 1.

**Amended again 2026-09-02 (`0274fa7`).** The transfer flow shipped on all three
clients, so **all three layers now have production callers** and the sentence
"ownership transfer is built" is finally true of the product as well as of
`crates/lakko-mls`. One property of how it shipped belongs here rather than in the
UX spec, because it is what keeps layer 1 from being bypassed: the **direct** arm
(`transfer_ownership`) has no client caller and cannot acquire one by accident,
since `isOwner` and `usesProposalPath` are the same read — the role table's
presence — so a client that can transfer is by construction a client on the
proposal path. iOS established that by execution rather than by argument: driving
the direct arm advanced the epoch **and** stripped the transferor locally, so the
next role change was refused as *proposer is not the owner*.

### C-64: The committer election and the staged sweep decide from authenticated state, and no epoch advances until the journal has ordered it

[ADR-0045](../adr/0045-remote-commit-epoch-fork.md). Under ADR-0022 a membership
change was one atomic call: an owner or admin created a commit that eager-merged
before it returned. ADR-0045 splits it into **propose**, **sweep** and
**adjudicate**, and every property below is something that used to sit *inside* that
atomic step and now spans it — which is why they are pinned together rather than
one per mechanism.

**This entry was deliberately deferred and the deferral is part of its record.**
ADR-0045 argued against minting a beacon-shaped invariant while nothing emitted a
beacon and nothing drove the fold: the index describes **live properties**, and
indexing an inert mechanism would make it aspirational. It named the trigger — *the
first client that emits a beacon and folds a verdict* — and predicted that the
property worth indexing would then be **broader than the beacon alone**. Both held.
`02f0037` (web) and `1f230ab` (iOS + Android) are those clients, and what they added
around the beacon is not a liveness mechanism: a sweep that merges before the
journal has ordered its commit is the device-ahead brick ADR-0045 exists to close.

#### The five sub-invariants

**(1) No input to the committer fold is self-asserted.** The beacon payload is
**field-less by construction** — 27 bytes of domain separator plus a version byte,
and nothing else — and that is the decision, not an omission. Both inputs the fold
consumes are the **observer's own re-derivation**: the sender's rank comes from the
MLS-authenticated sender leaf resolved through `members()` and
`Group::committerRank`, and the age comes from the DS row's **server-assigned**
`created_at`, which every member reads identically on the identical row. A
self-reported timestamp would let a candidate hold the committer duty by asserting
its own freshness — a liveness claim **no peer can check** — so a later hand adding
a `sent_at` field "for diagnostics" would be a real regression, not a cosmetic one.
A beacon whose row timestamp cannot be parsed is **dropped, not stored as fresh**:
stored fresh it would make the observer defer forever to something that can never
age out.

**(2) A rank is taken at verdict time, never carried from receipt.** The observation
store persists the sender's 32-byte **Ed25519 signature key** — never a leaf index
(OpenMLS hands a vacated leaf to the next member added) and never a rank. A rank is
a **position** in the role table's canonical order, so promoting, demoting or
removing any admin renumbers every candidate sorting after it; a rank captured under
one table and compared against a rank taken under another is not stale, it is a
position in a **different ordering**, and nothing in the comparison can notice.
`None` and `0` are never interchangeable — `0` is the owner, `None` is not a
candidate — and an unreadable rank fails **closed** to defer.

**(3) A beacon is emitted only from a client that can actually sweep.** The
foreground gate is a **caller obligation the core cannot check**, and it rests on
**C-59**: the ADR-0041 background decrypt is read-only-throwaway, the slim
`lakko-ffi-extension` surface does not even export `composeCommitterBeacon`, and a
backgrounded process can be frozen at any moment. A beacon from the background would
therefore be a *lie that costs the group a takeover* — every other candidate defers
to a device that is not going to sweep, and keeps deferring for as long as the lie
continues. Each platform reads "foreground" in its own idiom and none of them
guesses: web checks `document` visibility (a missing document reads as **not**
foreground), iOS requires **both** the screen visible **and**
`applicationState == .active` because neither implies the other, and Android's
`repeatOnLifecycle(RESUMED)` scope *is* the gate while still passing the flag
through so the rule is written down in one testable place.

**(4) The sweep stages; it does not merge until the journal has ordered its commit.**
The order is **stand down, durably record, publish, read the verdict, then merge or
abandon**, and each step is load-bearing. The **stand-down** re-reads the journal
head immediately before staging and stands down on **any** unapplied row, not only a
commit — the asymmetry is deliberate, since standing down costs one retry while
staging on an unapplied commit costs a commit every peer drops. The **verdict** is
read by *applying* the rows below this device's own commit and asking whether the
epoch moved; only a commit moves it, so the question needs no content-type
discrimination, and guessing in the permissive direction would abandon a sweep that
**won** and strand the device at N with the group at N+1 holding a commit it can
never re-apply (`CannotDecryptOwnMessage`). An **unreadable verdict leaves the sweep
staged**: merging on an unread verdict is the eager shape this ADR removes, and
abandoning would discard a commit already on the wire. Rollback is per-group
(`clear_pending_commit`), never a whole-provider `restore_storage` that would roll
sibling groups back past commits they have published.

**(5) The Welcome is published only after the verdict, and a staged sweep is owed
one.** A Welcome released for a commit that then loses lets a joiner open into a
state **no member holds** — a three-party fork, strictly worse than the two-party one
this ADR closes — so the Welcome bytes and their mailbox pseudonyms are computed at
record time but sent only on the **merge** branch. And because a staged commit is
**durable** (`MlsGroupState::PendingCommit` through `write_group_state`) and freezes
that group's `propose_*` / add / remove / `self_update` via `is_operational()`, a
sweep that never reaches a verdict wedges membership **across restarts**. So every
sweep adjudicates what it staged, an owed verdict is resolved at restart through
ADR-0040's existing `sweepPendingCommits` seam rather than a second restart path, and
the owed-verdict check runs **ahead of** the acting-committer rank check — an admin
can be demoted while holding a staged sweep, and gating the resolution behind the
rank check would freeze that device permanently. Re-publishing is safe: peers refuse
a demoted author's commit, so it loses and is abandoned, which is the unwedge.

#### What this does not claim

- **It is not a leader election and does not make one.** Each candidate folds over
  its own poll-timed view, and a beacon is stale by up to one interval even when its
  sender is perfectly healthy, so **two candidates can both reach `Act`**. The
  heartbeat removes the *wait*, not the *race*. Sub-invariants 4 and 5 are what make
  the collision survivable; reading the beacon as correct-by-construction and
  dropping either of them would restore the hazard.
- **A forged or replayed beacon buys nothing but a deferral.** It carries no key
  material and authorises nothing; the worst outcome is that a device declines to
  sweep, which sub-invariant 1 bounds and which is a liveness effect. The
  confidentiality-shaped half of this entry is sub-invariant 5's Welcome rule.
- **The freshness window is met on two platforms of three.** It is derived as
  `2 × interval + worst-case poll`, and the web client has **no periodic poll** —
  mount, a user action and the Refresh button drive it — so a web observer can see a
  beacon well after the window it was sized for and a takeover lags. Both natives
  long-poll and meet it. Stated here because it is a property of the deployment, not
  of the mechanism, and a reader should not infer the window holds everywhere.
- **The constants are chosen, not measured.** `COMMITTER_BEACON_INTERVAL_MS` (60 s)
  and `COMMITTER_BEACON_FRESHNESS_WINDOW_MS` (150 s) rest on a budget argument —
  journal litter and how finely a candidate's activity schedule is exposed — and the
  core says so. Nothing interactive waits on a beacon.

#### Where it lives

Core: `crates/lakko-contacts/src/committer_beacon.rs` (the envelope,
`acting_committer_verdict`, `should_emit_beacon`, the constants; 33 in-module tests
and the `committer_beacon_decode` fuzz target, which drives the **fold** from the
same attacker-controlled bytes and asserts that a `None` own-rank is always
`NotACandidate` and that a `Defer` never names a rank at or above the observer's
own) and `crates/lakko-mls/src/group.rs` (`stage_pending_proposals` /
`merge_staged_sweep` / `abandon_staged_sweep`, plus `group_roles::committer_rank`).
Clients: `apps/web/src/chat/{sweep,committer-beacon}.ts`,
`apps/ios/Lakko/Chat/{CommitterSweep,CommitterBeacon}.swift`,
`apps/android/.../chat/{CommitterSweep,CommitterBeacon}.kt`, each expressed over
injected ports so the whole orchestration runs in a plain host test environment, and
each carrying probes whose **discrimination was measured by running them and
recording the failure** — hoisting the Welcome above the verdict, merging instead of
abandoning on a loss, putting the owed-verdict check back behind the rank check,
deleting the foreground gate, and carrying a rank over from receipt.

Invariants: **#1** and **#5** (the beacon payload is field-less — no identity, no
timestamp, no server-visible artifact of any kind; the election is in-group and no
server learns who commits), **#2** (both the beacon and the sweep commit ride the DS
as opaque ciphertext; the only server contribution is the row's `created_at`
timestamp, which is metadata the DS already assigns to every row), **#3** (the staged
commit lifecycle is a `crates/lakko-mls` state-machine change under ADR-0045;
property-tested; the **`lakko-security` pass ran 2026-08-29 (REQUEST CHANGES, seven
findings, all closed) and the external crypto review is tracked once in
[G-39](known-gaps.md) per [ADR-0046](../adr/0046-external-crypto-review.md) as a
release gate**), **#4** (every new decision point fails closed toward the cheap outcome — an
unreadable verdict defers, an unreadable journal leaves the sweep staged, an
unclassifiable row stands the sweep down, and a device that cannot establish it is a
candidate does not claim to be one).

### C-65: At most one role-table change may be in flight in a group at a time

> **Generalised by [C-66](#c-66-a-group-must-never-lose-the-ability-to-change-its-own-membership).**
> This entry is the first instance of a rule the `lakko-security` review of ADR-0045
> later found to be open-ended: *a sweep over an authorised pending set must always be
> buildable*. C-65's ceiling is named by the RFC and can therefore be enumerated; the
> general case cannot, which is why C-66 backs the enumeration with an exclusion search.
> Nothing in C-65 changes — its propose-time guard and its sweep-side bound are the
> first two of the fifteen conditions C-66 enumerates.

[ADR-0045](../adr/0045-remote-commit-epoch-fork.md), extending
[ADR-0022](../adr/0022-group-admin-roles.md). ADR-0022 carries a group's `RoleTable` in
the authenticated `0xFF02` GroupContext extension, so **changing a role means rewriting
the GroupContext**, which on the wire is a `GroupContextExtensions` proposal. RFC 9420
admits **at most one** of those per commit. Under ADR-0022 that ceiling was invisible: a
role change was one atomic direct commit, so a second toggle read a table the first had
already committed. ADR-0045 turned the change into a **proposal**, and a proposal changes
nothing until it is swept — so two toggles taken a moment apart both read the **same**
committed table, each proposes a table carrying only its own change, and both sit in the
pending set at once.

**Why that is a membership wedge and not a lost toggle.** OpenMLS enforces the RFC's
ceiling as a **hard validation failure**, not a silent selection:
`validate_group_context_extensions_proposal` returns `TooManyGCEProposals` on the second
`GroupContextExtensions` proposal (`openmls-0.8.1`,
`src/group/public_group/validation.rs`), reached from `CommitBuilder::build`
(`src/group/mls_group/commit_builder.rs`). So a pending set holding two role changes
**cannot be swept at all**. The proposal store empties only when a commit is *merged*,
and `refuse_if_proposals_pending` closes every direct-commit escape (add, remove,
role-change, external-join approve, `self_update`) while anything is pending — so a
committer holding both has **no way out**: its membership operations are dead.

**Two faces, and the reassuring one is the lucky outcome.** Both proposals travel the
group journal, so the ordinary case is that both reach **every** candidate committer, and
then the whole group's membership is wedged — escapable only if some committer's view
happens to hold just one. The **asymmetric** case is milder: one committer holds a single
role change, sweeps, the merge empties everyone's store, and the other change is dropped
group-wide with no error anywhere. ADR-0045 originally recorded only that second face.
Chat is unaffected in both, because `create_application_message` parks the pending set
around the encrypt call.

**Neither serialization nor the client can repair it after the fact.** Serializing the two
toggles cannot help, because there is nothing for the second read to observe — the first
proposal has changed no committed state. Recomputing "the committed table as amended by our
own pending proposal" is not available either: `ProposalKind::RoleChange` carries no
payload (the core's `classify` discards the extensions), so a client cannot read back what
it proposed, cannot withdraw a proposal, and — in the symmetric case — cannot even sweep
past it. The repair therefore has to be a **refusal before the second proposal exists**,
which is why the rule lives in the core rather than in three clients.

#### The rule, in two halves

1. **Propose time — refuse.** `Group::refuse_if_role_change_pending` counts pending
   proposals classifying as `ProposalKind::RoleChange` and refuses a new one if any exist.
   It is called from `propose_group_roles` **last**, after authority
   (`authorise_own_proposal`) and validity (`refuse_unenforceable_role_table`), so a caller
   handed an unenforceable table learns *that* rather than "not now" — availability is the
   least informative refusal, so it goes at the back.
2. **Sweep time — bound.** `sweep_keeps` keeps **at most one** role change per sweep, via a
   `role_change_taken` flag, using the same exclusion-not-refusal mechanism its existing
   `role_change_survives` clause uses. Which of the two survives is the proposal store's own
   order and is arbitrary; the one dropped is dropped from every member's store when the
   commit merges, so its author must re-propose, and the caller learns which survived from
   `ProposalCommit::swept`. The predicate is **stateful**, so each pass over the pending set
   needs its own flag — the emptiness prediction and the real filter must not share one, or
   the prediction would spend the budget the filter needs.

**The second half is not made redundant by the first, and this is the part a later hand
will get wrong.** The propose-time guard cannot see a state that arises **legitimately**. A
device that publishes a role-change proposal and then loses its own record of it before
persisting — the ADR-0040 crash window — proposes a second one in good faith, and every
*peer* then holds two. Without the sweep-side bound those peers can never sweep again. The
guard keeps a well-behaved device from building the set; the bound keeps a set that reached
it anyway from wedging the group.

#### Deliberately narrower than the direct-commit guard

`refuse_if_role_change_pending` counts **only** role changes, where
`refuse_if_proposals_pending` counts everything. That asymmetry is the rule, not an
oversight: a pending add or remove composes into the same sweep perfectly well, and the
exclusive resource is the single `GroupContextExtensions` slot. Widening this guard to "no
role change while anything is pending" would make the core **stricter than the protocol
requires** and would block a legitimate toggle behind an unrelated membership request.

Two consequences fall out rather than being restated:

- **Ownership transfer collides in both directions.** `Group::propose_ownership_transfer`
  is the same owner-only role-table rewrite under another name, and it reaches the guard by
  delegating to `propose_group_roles` — so a pending transfer refuses a promote or demote,
  and a pending promote or demote refuses a transfer. One rule, one place.
- **A group with no role table is untouched.** Such a group has no proposal path at all
  (every `propose_*` refuses without a table, and `gate_incoming_proposal` stores nothing),
  so its pending set is permanently empty and the guard is a counted-zero no-op.
  `DeviceGroup`, `PublisherGroup`, call groups and every other `Group::create_with_id`
  group keep their direct commits unchanged.

#### What it does not do

- **It is not a confidentiality or integrity property.** Nothing here protects a secret or
  authenticates an actor: the guard converts a wedge (and, in the lucky case, a silent
  loss) into a synchronous, specific refusal. It is indexed because the state it prevents
  is one the group **cannot recover from inside the protocol** in the symmetric case — the
  same reason C-63 is indexed — not because an attacker gains anything by reaching it.
- **It does not tell the second proposer what it collided with.** The refusal names the
  count and the reason; it does not identify the pending change or its author. A client
  wanting to pre-empt the refusal rather than surface it reads `pending_proposals` and looks
  for a `ProposalKind::RoleChange`, which is the same question the guard asks.
- **It is a local pre-check plus a committer-side bound, not a peer-verified gate.** Unlike
  C-63's `owner_survives`, no peer refuses a *commit* on this rule — it never needs to,
  because a commit carrying two role changes cannot be built in the first place, and
  OpenMLS's own receive-side validation (`staged_commit.rs`) would refuse one anyway.
- **It does not remove the need to re-propose.** Whichever change loses — to the guard at
  propose time, or to the bound at sweep time — its author must ask again. Under ADR-0045's
  no-deadline model the wait is one sweep in the ordinary case (the owner is rank 0 and
  sweeps immediately) and a real wait only when no candidate committer is running the app.

#### Tests

`crates/lakko-mls/tests/group_proposals.rs`:
`a_second_role_change_is_refused_while_one_is_pending` (the guard, including that the
refusal message names *what* was refused and that it is a timing refusal rather than an
authority one, that nothing was published, and that the surviving change still lands);
`control_two_role_changes_in_one_pending_set_cannot_both_be_swept` — the **control**, which
reaches the two-role-change state the guard makes unreachable (by rolling the proposer's
provider back between the two proposals) and asserts a sweep over it still lands, carries
exactly one role change, and advances the epoch;
`a_pending_add_or_remove_does_not_block_a_role_change` (the narrowness, with all three
intents riding one commit); `ownership_transfer_and_a_role_change_exclude_each_other` (both
directions); and `a_group_with_no_role_table_is_untouched_by_the_role_change_guard`, which
also asserts the refusal such a group *does* get names the pre-existing no-role-table
reason and not this one. 33 tests in the binary, all green.

Invariants: **#2** (unchanged — the role table is an opaque `0xFF02` GroupContext body and
the DS parses nothing; this rule adds no wire format and no server surface), **#3** (a
`crates/lakko-mls` state-machine change under ADR-0045 — a new refusal on the propose path
and a stateful predicate on the sweep filter; property-tested, **the `lakko-security` pass
ran 2026-08-29 (REQUEST CHANGES, seven findings, all closed); the external crypto review is
tracked once in [G-39](known-gaps.md) per
[ADR-0046](../adr/0046-external-crypto-review.md) as a release gate**), **#4** (fail-closed: the second propose is
refused rather than published, and the sweep excludes rather than building a commit
OpenMLS would reject), **#5** (unchanged — roles stay in-group).

**Client status (2026-08-28, closed 2026-09-01).** Enforcement is complete in the core and
holds for every caller on every platform. What was outstanding was **copy, not
enforcement**: web and iOS carried narrow client-side pre-checks (`hasPendingRoleChange`,
each filtering on the role-change kind specifically, so neither is stricter than the core
rule) and surfaced a sentence; Android had no pre-check and surfaced the core's
`Error::Group` diagnostic. **Closed in `8018e8b`,** and the fix was wider than the gap
described: the drift was three-way rather than one-way — web rendered the spec's string,
iOS threw its own near-variant prose, Android rendered the diagnostic raw — so mapping
core prose to fixed copy by substring would have rebuilt the same drift class. The core now
raises `Error::MembershipRefused { reason, detail }` (`crates/lakko-mls/src/refusal.rs`)
whose whole `Display` is `detail`, the sentence it always carried, with a typed
`RefusalReason` travelling beside it as the contract; each client selects copy from the
type, and Android gained the pre-check as well. The refusal string is specified in
[`docs/ux/proposal-based-membership.md`](../ux/proposal-based-membership.md) §6.

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
| JWT `(account_id, call_id, role, expires_at)` | ✅ (validate via cached JWKS) | ✅ (mint via `POST /call/jwt`) | Claims shape per ADR-0008, which ADR-0011 superseded for the SFU choice; the shape itself survives in `crates/lakko-call-jwt`. C-15..C-23 |
| Ed25519 signing seed for call JWTs | ❌ | ❌ (stays on `services/auth` host) | Persisted in `call_jwt_signing_key.seed`. Public half published at `/.well-known/jwks.json` |
| Plaintext SDP / ICE | ❌ | ✅ | 1x1 rides MLS; group rides the Rust SFU's own signaling channel (ADR-0011), which never sees keys |

## Known soft spots — out of scope, tracked

See [`known-gaps.md`](./known-gaps.md) — call-side gaps:

- **G-17** — frame-counter exhaustion path is documented but
  not exercised by a dedicated test (running 2^32 seal
  operations is infeasible in CI).
- **G-18** — **CLOSED (2026-07,
  [ADR-0029](../adr/0029-mid-call-key-rotation.md)).** The overlap keyring moved
  into the Rust core as `lakko_calls::CallKeyRing` and is driven from all three
  clients on a mid-call MLS epoch advance. Only the live rekey over a **deployed**
  SFU is deferred, on the same runtime gate as G-26.
- **G-19** — **CLOSED.** The LiveKit fork was retired by
  [ADR-0011](../adr/0011-rust-sfu.md), so there is no Go verifier and never will be;
  the load-bearing path is the Rust SFU's own admission gate, which verifies the JWT
  through `lakko_call_jwt::verify_with_jwks` **before** the WebSocket upgrade and
  takes the peer's `call_id` and `account_id` from the verified claims rather than
  from the client (`services/sfu/src/signaling.rs`).
- **G-20** — call-recording is out of scope (deferred per
  ROADMAP 3c8). If it ever lands, the recording client
  joins as a regular participant with its own MLS leaf, so
  no break to C-1..C-10.

## MLS group-membership rules (C-66..C-69)

**These four are in scope, not soft spots.** They sit here because the `C-` class is
the catch-all, and until 2026-09-02 they were appended directly below the
"out of scope" heading above with no heading of their own — which read as though the
core of what [G-39](./known-gaps.md) asks an external reviewer to examine had been
waived. They are group-membership rules from
[ADR-0045](../adr/0045-remote-commit-epoch-fork.md), minted by the `lakko-security`
review of that arc, and they are the primary review surface.

### C-66: A group must never lose the ability to change its own membership

[ADR-0045](../adr/0045-remote-commit-epoch-fork.md), from the `lakko-security` review
of that arc. **C-65 is the first instance of this rule; this entry is the rule.** Both
say the same thing about two different doors into one room, and they are indexed
separately only because C-65's door is a named RFC ceiling while this one is open-ended
by construction.

**The class.** A role-table group administers itself: a request becomes a proposal, and
one owner-or-admin sweeps the pending set into a commit. Two properties keep that loop
alive, and neither is a confidentiality or integrity property — they are indexed for the
same reason C-63 is, because the state they prevent is one **the group cannot recover
from inside the protocol**. There is no operator to call, no key escrow, no
administrative override: a group that cannot change its membership is a group whose
composition is frozen for the lifetime of its data, while chat keeps working so nothing
announces it.

**(1) The owner's response is admissible at any pending-set occupancy.** The cap
(`MAX_PENDING_PROPOSALS`) refused *every* proposal once full, including the owner's own
`Remove` and `RoleChange`; `refuse_if_proposals_pending` closes every direct-commit
escape; and nothing in the crate discards a proposal. So a member with add authority
could fill the set and thereby take away the owner's ability to remove them, demote
them, or clear the set — with the only exit, sweeping, **committing the flooder's
payload**, after which they refill it. `Group::pending_capacity` therefore admits a
small reserve past the cap to a proposal `group_proposals::is_owner_response`
recognises: authored by the **owner**, kind `Remove` or `RoleChange`. The same function
is consumed by `authorise_own_proposal` and by the peers' `gate_incoming_proposal`, and
that shared use is load-bearing rather than tidy — if the two disagreed, the owner would
publish a response its peers refuse to retain and then sweep a commit they answer with
`InvalidCommit(MissingProposal)`.

**It grants nothing.** `authorise` still runs, unchanged, afterwards; the reserve only
decides *when the set is too full to accept a request*, never *who may make one*.
Authorship is the MLS-authenticated sender leaf resolved against the pre-merge roster,
so it cannot be claimed by anyone else, and `Add` is deliberately **not** a response, so
the owner cannot inflate the group's own bound through the one kind a flood is made of.

**The mis-triage is part of the record, because the reasoning will recur.** This was
first dismissed as strictly weaker than a capability the role already holds — an admin
can simply remove everyone. That is wrong, and the error is a general one:
**capabilities compose, and the composition is not the sum of its parts.** Removing
members is loud, attributable and repairable, because the owner demotes the admin and
adds the members back. Filling the pending set removes the **means of repair**, which is
a strictly stronger capability reached through a strictly weaker-looking act. Any
argument of the form *"they could already do something worse"* has to check whether the
worse thing leaves the response intact.

A per-sender sub-cap was considered and rejected: it bounds one principal's share while
the cap stays global, so several colluding admins still reach it. It delivers a
probability where the property needed is *never*.

**(2) A sweep over an authorised pending set must always be buildable.** If
`CommitBuilder::build` hard-fails, the sweep is not a lost round: the proposal store
empties only on a **merged** commit, the direct paths are refused while proposals pend,
and nothing discards — so the failure is permanent. `create_application_message` parks
the pending set, so **chat keeps working over a membership that can never change
again**, which is what makes this invisible rather than merely bad.

**One proposal is enough, and one of its causes is the passage of time.** An `Add`
naming someone already in the group, or two admins independently inviting the same
person — which is exactly the concurrent composition ADR-0045 exists to make safe —
fails with `DuplicateSignatureKey`. Fifteen conditions are enumerated in `sweep_keeps`
across three families: the single `GroupContextExtensions` slot (C-65), key uniqueness
across signature, encryption and init keys, and the added leaf's admissibility to *this*
group (capabilities, required capabilities, leaf extensions, credential types, and
**KeyPackage lifetime**). That last one settles the argument on its own: ADR-0045
decision 2 gives a request **no deadline**, so a proposal need only wait long enough and
the group freezes with nobody having done anything at all.

**The enumeration cannot be the whole answer, and the fallback is the actual
invariant.** Fifteen conditions is a mirror of a rule set this crate does not own; an
openmls upgrade adds a sixteenth and the freeze returns. So when the build still
refuses, `narrow_sweep_by_exclusion` offers each candidate in turn and keeps it only if
the selection still builds, then re-offers the rejects once. **It does not need to know
why a proposal is uncommittable — only which one**, which is what makes it robust
against rules nobody has written yet. It is bounded by `2 x candidates + 1` builds and
terminates structurally: two flat loops over a finite list, no fixpoint, no recursion.
When every candidate is uncommittable the result is **no commit**, never an
epoch-advancing empty one, and the store is left as it was — which is not a wedge,
because the next committable proposal clears it.

Three properties of the search that a later hand will otherwise get wrong. **Additive
selection, not leave-one-out**: leave-one-out is cheaper for exactly one offender and
needs a fresh round per offender, and when the cause is unknown the single-offender case
is precisely the one that cannot be assumed. **The second pass is load-bearing**: a
`Remove` and the re-`Add` of the same signature key are only committable **together**,
and one pass drops half the pair and silently evicts the member. **Each trial selection
re-asks `owner_survives`**: dropping an `Add` can strand a role change naming that person
owner, which openmls — knowing nothing of role tables — builds happily and every peer
then refuses, trading a local freeze for a fork.

**The search is deterministic per device and deliberately not device-independent.**
*Can I commit this* is a question about the committer, which the live case that first
exercised the fallback proves: an admin proposing its **own** departure wedges its own
sweep with `CannotRemoveSelf`, while any other candidate commits it fine. Two committers
may therefore stage different subsets of one pending set; ADR-0045's journal ordering
already arbitrates that, the loser abandons, and because a merged commit clears the whole
store a proposal dropped by either search is dropped everywhere and its author
re-proposes.

**What this does NOT do.** It does not identify *why* a proposal was excluded —
`CreateCommitError` carries no `ProposalRef`, leaf index or signature key on any variant,
so a client learns only that a request was not carried (by diffing `pending_proposals`
against `ProposalCommit::swept`) and never the reason; naming one would mean inventing
it. It does not enforce two conditions: whether **every existing member's** capabilities
accept a joiner's credential type (openmls exposes a member's credential but not its
capabilities, and it is a property of the group rather than of the proposal), and the
commit path's own encryption-key uniqueness (generated inside `build`, invisible to a
proposal filter). It does not recover a group already wedged by a build rule this
enumeration and this search both fail to route around — the search is a strong general
answer, not a proof.

**Tests** (`crates/lakko-mls/tests/group_proposals.rs` and the in-crate
`proposal_gate_tests`): `a_flood_of_admin_adds_never_takes_away_the_owners_response` and
`the_reserve_is_the_owners_alone_and_only_for_a_response` for (1);
`the_pending_set_is_still_bounded_by_the_sum_of_the_two_caps` for the bound;
`a_single_add_of_an_existing_member_cannot_wedge_the_sweep`,
`a_capability_deficient_add_cannot_wedge_the_sweep` and
`an_add_whose_key_package_has_expired_cannot_wedge_the_sweep` for the enumeration;
`the_fallback_rescues_a_wedge_the_enumeration_cannot_see`,
`a_pair_that_is_only_committable_together_survives_the_wrong_store_order`,
`the_exclusion_search_never_leaves_the_group_without_an_owner` and
`a_wholly_uncommittable_set_yields_no_commit_and_no_wedge` for the search;
`an_ordinary_sweep_costs_exactly_one_build` for the good path. Invariants **#2**
(unchanged — no wire format, no server surface), **#3** (a `crates/lakko-mls`
state-machine change under ADR-0045; property-tested, **the external crypto review
tracked once in [G-39](known-gaps.md) per
[ADR-0046](../adr/0046-external-crypto-review.md) as a release gate**), **#4**
(fail-closed in the direction that matters: an undecidable proposal is
dropped, because wrongly keeping one freezes the group permanently while wrongly
dropping one costs a re-request), **#5** (unchanged).

### C-67: Durable bookkeeping must never claim more progress than actually landed

[ADR-0045](../adr/0045-remote-commit-epoch-fork.md), from the `lakko-security` review of
that arc, and the receive-side counterpart to
[ADR-0040](../adr/0040-durable-pending-commit-outbox.md)'s committer-side rule. Two
findings, one rule: a durable marker that says *this is done* while the thing it names is
not done is worse than no marker, because every recovery path branches on it and each one
then declines to help.

**(1) No membership commit may be acked while an artifact it authorised is still
unpublished.** ADR-0045 obligation 5 says the Welcome is published only after the
verdict, which is an **ordering** rule and is correct. What it did not say is that the
committer must **record that the verdict settled, before attempting the fan-out**. A
Welcome publish that failed *after* the commit had won and merged was unrecoverable in
the ordinary case: the next tail sweep finds no staged sweep (it merged) and no pending
proposals (the merge consumed them); the restart path's owed-verdict resolver finds
nothing owed for the same reason; and the legacy re-publish loop skips the entry. The
added member sits in every roster with every subsequent message encrypted to their leaf,
and can never construct group state — a **ghost member**, repairable only by another
admin removing and re-adding them.

**The general rule is worth more than the fix: the merge is not the end of the sweep's
obligations.** An artifact still owed after the epoch-advancing step needs its **own
durable name**, because the state every recovery path keys on is gone by the time it is
owed. The two-state entry lifecycle (verdict-owed → acked) becomes three
(verdict-owed → welcomes-owed → acked), with the middle marker written between the merge
and the fan-out and read by a new resolver that re-publishes **without re-adjudicating**
— the verdict is already in.

Two properties of that marker. **Pre-verdict ordering is guarded twice**: it is written
strictly after the merge and its persist, so it *is* the durable statement "the verdict
said WON" and a pre-verdict restart finds a staged sweep and no marker; and the resolver
additionally refuses whenever a staged sweep exists, **before reading the store at all**,
so even a marker present out of order cannot put a Welcome on the wire ahead of a
verdict. **A failed marker write is deliberately non-fatal**: the verdict is already in,
so publishing is correct with or without it, and aborting the fan-out on a bookkeeping
failure would *manufacture* the ghost member it exists to prevent.

Idempotency is established rather than assumed: `StagedWelcome::into_group` deletes the
init KeyPackage, so a duplicate arriving after a successful join cannot be staged, and
every client's mailbox handler already catches and skips a failed `joinFromWelcome`.

**(2) The journal cursor may lag the durable MLS state, but must never lead it.** ADR-0045
made a receiver's **durable proposal store** a precondition for applying a later
by-reference commit — `group_proposals.rs` relies on that deliberately as a boundary,
which is right for a *refused* proposal and fatal for a *lost* one. Three facts composed:
the receive-path snapshot write was best-effort and swallowed failures; the cursor lives
in a **different store that no transaction can span** (two IndexedDB databases on web,
separate files on both natives); and the poll advanced the cursor before processing the
row. One swallowed write, or a crash between the two, and the device restores an MLS
state without the proposal and a cursor past it. The commit referencing it throws
`MissingProposal`, is logged and skipped, **the cursor advances past that too**, and the
device sits at epoch N forever while the group is at N+1, with every later message
undecryptable. It is still a member, so C-59's membership accessor cannot see it;
recovery is being removed and re-added.

**The asymmetry is the whole rule, and it is why ordering suffices where atomicity is
impossible.** Cursor **behind** the state is benign and self-healing: rows are re-fetched,
already-applied ones are rejected as wrong-epoch and skipped exactly as the poll already
skips them, an already-stored proposal is de-duplicated by `ProposalRef`, and the cursor
catches up. Cursor **ahead** is permanent. So: persist fail-closed first, advance second,
hold the cursor when the write does not land. Merging the two stores was considered and
rejected — impossible on the natives at any price, expensive on web, and it would buy
only *equality* where lagging is already safe.

**A row that changed durable state is any row handed to the core, not any row that
applied cleanly.** `process_message` decrypts and ratchets **before** the role gate can
refuse a commit or the receipt gate can drop a proposal, so a throw is no evidence of a
clean state. Echo-suppressed rows never reach the core and are correctly excluded.

**The infinite-retry trap is avoided because the two failures are distinguishable**, and
they are different operations at different points in the batch: a receive throw is
per-row and skipped as before with the cursor advancing, while a persist throw is
device-wide and holds. So a permanently undecryptable row can never wedge the poll. A
persist that never succeeds does stall the cursor, and that is the correct side: the same
failure already fail-closes the send path, so such a device is already non-functional for
writes, and advancing would be the strand.

The ADR-0045 sweep's verdict apply advances the same cursor over rows it applied and
obeys the same rule through its own fail-closed persist port, distinct from the
merge-path persist because the two want opposite things from a failure.

**What this does NOT do.** It does not make the two stores atomic, so the state may be
*ahead* of the cursor after a crash — deliberately, that being the benign direction. It
leaves one degraded corner: a persist fails, the debounced message-log write lands with
the old cursor and the new rows, then the process dies, and on restart the rolled-back
MLS state re-decrypts those rows and appends them again — a duplicate timeline row,
cosmetic, and strictly better than the permanent strand it replaces. And it is a client
discipline, not a core-enforced fact: `crates/lakko-mls` cannot see either store.

**Tests:** `apps/web/src/chat/receive-durability.test.ts`,
`apps/ios/LakkoTests/ReceiveDurabilityTests.swift`,
`apps/android/.../chat/ReceiveDurabilityTest.kt` for (2), each reproducing the strand
before the fix and pinning that no interleaving can leave the cursor ahead; the
welcomes-owed path in each platform's committer-sweep suite for (1). Invariants **#1**
/ **#2** (unchanged — durable markers are local bookkeeping; nothing new reaches the
server), **#3** (no `lakko-mls` change; this is entirely client discipline over the
audited receive path), **#4** (fail-closed: the persist aborts rather than the cursor
advancing, and a marker-write failure never aborts a publish the verdict already
authorised), **#5** (unchanged).

### C-68: An author is a signature key resolved at receipt, never a leaf index

[ADR-0023](../adr/0023-message-edit-delete-reactions.md), corrected by the
`lakko-security` review of [ADR-0045](../adr/0045-remote-commit-epoch-fork.md). ADR-0023
gates an edit or a delete on the op's sender being the target message's original author —
the one rule that stops a member rewriting or erasing somebody else's words. All three
clients implemented that comparison on a raw **MLS leaf index**, and OpenMLS hands a
**vacated leaf to the next member added**.

**ADR-0045 is what made it cheap rather than what made it possible.** A remove and an add
compose into **one commit**, so the whole sequence lands in a single poll batch: remove
Bob, add Dave, and Dave — now holding Bob's old leaf — edits and deletes Bob's history,
and every client applies it. No crash, no race, no malice from the server, no modified
client. On both natives it was **permanent**, because the leaf was persisted with the
message; on web it was session-scoped only because the author was not persisted at all,
which was a Phase-2 deferral that happened to narrow the window rather than a defence.

**The same shape, twice more.** Reactions were attributed to the sender leaf and toggled
on the `(reactor, emoji)` pair, so a newcomer at a vacated leaf could un-react on the
departed member's behalf — and all three clients independently measured that the damage
is worse than a mis-toggle: removing the last reactor prunes the set, so the chip
**disappears entirely**. Read-receipt watermarks are a per-sender map never reset on a
membership change, so a newcomer **inherits** the departed member's watermark and the UI
reports messages as read by someone who never saw them.

**The fix is not "compare keys". It is "compare keys resolved at receipt, in the epoch
the row arrived".** That second half is load-bearing and each client pins it with a
permanent contrast test: a batch carrying the removal *and* the add would resolve an
epoch-N message against the N+1 roster, which **is** the impersonation, reintroduced
under a key comparison that looks correct. The resolution therefore happens per message,
inside the batch loop, one line after the receive call returns — never in a view model,
never hoisted out of the loop for speed. There are **two** receive paths that build
timeline events (the poll and the ADR-0045 sweep's verdict apply) and both resolve
identically; on iOS the event builder takes the group and resolves internally, so there is
no resolver a call site can get wrong.

**A torn roster read resolves to nothing and the gate refuses.** So does a stored message
that carries no author. There is no fallback of the form *this row is outgoing, so assume
our current key* — it would invent an author the store never recorded.

**The wider point, which is why this is indexed rather than filed as a bug.** Every module
built after ADR-0023 keys on the signature public key, and several name leaf reuse in a
comment as the reason: the ADR-0022 role table, ADR-0045's `committer_rank` and beacon
observation store, the ADR-0042/0044 push-routing authority gate, ADR-0043's
membership-shrink diff, ADR-0027's external-join set. The convention is right and is held
everywhere it was applied. **ADR-0023 predates it and was never revisited when the
convention formed** — conventions do not retroactively fix code written before them, and
nothing in the repository was looking for the gap. What remains leaf-keyed anywhere is
either epoch-scoped by construction (a `Remove` proposal's target, an SFrame KID whose
key space rotates with the epoch) or a transient intent inside one epoch.

**What this does NOT do.** It does not authenticate the author cryptographically at
render time — the binding is made once, at receipt, from the MLS-authenticated sender
leaf, and stored; a client that corrupts its own store corrupts its own attribution. It
does not repair history stored under the old shape: a pre-existing row decodes with no
author and the gate **refuses for everyone**, including the genuine author, which is the
safe direction and is asserted rather than assumed on all three clients. And it does not
make reactions authenticated — any member may react; the key only ensures a reaction is
attributed to, and togglable by, the member who cast it.

**Tests:** `apps/web/src/chat/author-identity.test.ts`,
`apps/ios/LakkoTests/AuthorIdentityTests.swift`,
`apps/android/.../chat/AuthorIdentityTest.kt` — each reproducing the impersonation before
the fix with edit and delete asserted **separately**, each carrying the resolve-late
contrast, and each pinning that a pre-existing stored row fails closed rather than
misattributing. Invariants **#1** / **#2** (unchanged — the signature key is already
public and already in the MLS snapshot beside the message store; nothing new reaches the
server), **#3** (no `lakko-mls` change; the core already surfaced the sender leaf and the
roster), **#4** (fail-closed at every unknown: a torn roster, an unresolvable sender, an
authorless stored row), **#5** (unchanged).

### C-69: A discipline attached to a direct-commit call site must be re-attached to the sweep

[ADR-0045](../adr/0045-remote-commit-epoch-fork.md). Stated first as an observation
during the client sprints, promoted to an invariant when S7 produced the **second**
instance — and the second one's failure mode is silent, which is what makes the pattern
worth a number rather than a paragraph.

**Why the pattern exists.** ADR-0022 made a membership change one atomic call: an
owner or admin created a commit that eager-merged before it returned, so anything that
had to happen *because* membership changed could be written on the line after. ADR-0045
splits that into propose / sweep / adjudicate, and the sweep may run on a **different
device** from the one that asked. Every discipline seamed onto the old call site is
therefore now attached to the wrong end of the operation — it fires when the request is
*made*, not when it *lands* — and the compiler cannot see the difference, because the
call site still exists and still returns.

**The tell is in the discipline's own correctness argument: if it mentions an epoch, or
mentions "after the commit", it is a candidate.** Both known instances do.

**Instance 1 — the push-routing rotation (C-61).** ADR-0043 requires the rotation to
follow the `Remove` commit, so the new secret is sealed at an epoch the evicted member
can no longer open. Seamed onto `removeMemberFromGroup`, that held. Under ADR-0045 a
removal usually becomes a **proposal**, which advances no epoch — so rotating there
would seal the new secret at an epoch the target is **still in and can still open**,
silently revoking C-61's forward-secrecy half without touching a line of rotation code,
and under ADR-0044 it would additionally stamp the secret at the pre-removal epoch where
it **ties** with the stale one and can lose the byte tiebreak. Moved to the sweep, via
the membership-shrink diff the poll already runs.

**Instance 2 — the Welcome rendezvous (S7).** ADR-0027's approve produced a Commit and a
Welcome together, so the approving admin wrapped a `WelcomeRendezvous` and posted it on
the spot. Under S7 the approve produces only a **proposal**; the Welcome is produced by
whichever device sweeps, which is not necessarily the approver and **cannot tell which
adds came from a `/g/` link** — origin-blindness being the property that makes S7 sound.
So the sweep posts a rendezvous for **every** added key, alongside the mailbox fan-out.
Left unmoved, the failure is silent in both directions: the approving admin sees its
proposal published and swept, every member sees the joiner in the roster, and the joiner
polls a journal that will never carry its envelope — **approved into a group that already
holds them as a member, and unable to enter it**.

**The rule, stated so it can be checked rather than admired.** When a membership
operation moves from a direct commit to a proposal, every side effect attached to the old
call site must be re-examined against three questions: *does it need the post-commit
epoch* (then it belongs at the sweep); *does it need to happen exactly once per landed
change* (then it belongs at the sweep, because a proposal may be excluded and re-proposed);
and *does it need to know something about the request that the sweep cannot see* (then
either the sweep must do it origin-blind for everything, as S7 does, or the information
must be carried — and carrying it is usually a leak, which is why S7 does not).

**What this does NOT do.** It does not enumerate the disciplines — that is the point; a
list would go stale the next time one is added, and the tell above is what makes a
reviewer able to find them. It does not apply to a group with **no role table**, which
keeps direct commits and where every seam is still correct where it sits. And it is not
enforced by anything: no test can assert that a discipline nobody has written yet is
attached to the right end. It is a review rule, indexed so that it is read.

**Where the two instances live:** C-61's rotation at each client's sweep
(`apps/web/src/routes/Chat.tsx`, `apps/ios/.../ChatRepository.swift`,
`apps/android/.../ChatRepository.kt`), pinned by each platform's C-61 ordering test; the
rendezvous fan-out in each platform's committer-sweep module, pinned by a
rendezvous-for-**every**-add test that a flag-threaded implementation could not pass.
Invariants **#2** (unchanged — no wire format change; the rendezvous is a pre-existing
envelope kind), **#3** (no `lakko-mls` change; this is client seam placement), **#4**
(unchanged), **#5** (S7's fan-out puts a raw signature key on the journal where an
ordinary add puts a hash on a mailbox — recorded in ADR-0045 rather than here, since it
is a property of the fan-out and not of the rule).

**Amended by [ADR-0053](../adr/0053-ds-journal-authorisation.md) (2026-09-27): the rule
stands, instance 2's carrier changed.** The rendezvous went to the group journal because
the journal was the only place a non-member could read. ADR-0053 closes the journal to
non-members, so the sweep now delivers the Welcome for every added key to the **mailbox**
named by that key - the same path a direct add uses - and writes no
`WelcomeRendezvous` to a journal at all. The origin-blind fan-out survives unchanged: the
sweep still cannot tell which adds came from a `/g/` link and still delivers for every
one. The #5 note above is retired with it - a mailbox Welcome puts a hash on a mailbox, as
an ordinary add does. The co-publisher approve (ADR-0028 OQ3) follows: its Welcome goes
to the joiner's account mailbox wrapped as `KIND_WELCOME`, on all three clients. Pinned by
each platform's committer-sweep tests, rewritten from rendezvous-for-every-add to
mailbox-only.

## Conversation kind (C-70)

### C-70: A conversation's kind is fixed at creation, and a one-to-one keeps its two people

[ADR-0062](../adr/0062-conversation-kind.md). **The rule.** Whether a conversation
is a one-to-one or a group is decided once, by its creator, and written into the
MLS GroupContext as the `0xFF03` extension (`version 0x01 ‖ kind 0x01/0x02`), which
`Group::create` also names in `RequiredCapabilities`. No commit may change or drop
it. In a one-to-one the set of people (accounts, not leaves) never changes after
founding: a commit may admit the second person at founding and nothing more, and
may never remove a person. A person's own devices still join and retire
(ADR-0050). **Every member's device refuses otherwise**; this is a protocol rule,
not a UI promise.

**Why it is indexed.** Before ADR-0062 nothing in the group said which kind it
was, so each client guessed from the people count and a local flag. A
two-person group read as a one-to-one on everyone but its creator, and nothing
stopped any single device from adding a third person to a one-to-one, which
every other member's device then accepted.

**Where it is enforced.** `crates/lakko-mls`:

- `join_from_welcome` refuses a Welcome whose GroupContext carries the `0xFF02`
  role table but no readable `0xFF03` (plain `Error::Group`), so no device ever
  holds a conversation it cannot classify;
- `group_kind_unchanged` at the peer commit gate and `proposal_keeps_group_kind`
  at the receipt gate and in the sweep refuse a `GroupContextExtensions` rewrite
  that changes or drops the kind (`GroupKindChanged`); the four role rewrites
  build their extension list through one helper, `role_rewrite_extensions`, that
  carries it verbatim;
- `one_to_one_holds` at the peer commit gate, in the sweep
  (`selection_is_admissible`) and on the direct `add_members` / `remove_member`
  paths: in a one-to-one the pre-merge accounts must be a subset of the
  post-commit accounts, there must be at most two, and no `GroupContextExtensions`
  rewrite is admitted (`OneToOneMembershipFixed` / `OneToOneHasNoRoles`);
- `one_to_one_allows` at the proposer and the receipt gate refuses the
  individual proposal early; it is the early answer, `one_to_one_holds` is the
  authority, because two harmless proposals (removing an account's two leaves)
  can remove a person together;
- an external join into a one-to-one is refused on approve and surfaced with
  `retained: false`.

Pinned by the `group_kind.rs` unit tests, the `one_to_one` integration binary
(including a commit built without the local checks and refused by the other
member) and the `group_kind_decode` fuzz target.

**What this does NOT do.** It does not let a person leave a one-to-one: deleting
it stays local, and the other side is not told. It does not apply to
`PublisherGroup` or `DeviceGroup`, which carry no kind. It does not protect
against a member that never runs these checks at all being the only device in the
conversation; it protects every member that does. Invariants **#3** (a new
GroupContext extension, a commit-wide rule and a join condition; external review
under G-39) and **#4** (every new decision point refuses); **#2** and **#5**
untouched.
