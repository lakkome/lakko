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
(RFC 9605 §4.3 compact `Config|KID|CTR` layout) is
explicitly out of scope for the current implementation
and tracked as G-26.

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
