<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Known gaps before public beta

This file is the **honest inventory**. Reviewers should
not re-discover these as findings; they are known and
tracked. Each entry has a target sprint / ROADMAP link.

Items here are either:

- (a) explicitly deferred to a later phase (the protocol
  shape is settled but the implementation lands later); or
- (b) known soft spots in the current implementation that
  we trade off for shipping speed, with a documented
  follow-up.

## MLS state machine

### G-1: Channel two-tier flow (>50k subscribers) not implemented

Phase 4 scope. The wire shape (PublisherGroup + sender-key)
is settled in `docs/adr/0001-mls-only.md`; nothing is
shipped against it yet.

### G-2: MLS-2 / quantum-hybrid suites pinned for later

The IETF MLS WG is still landing hybrid PQ cipher suite
identifiers. We pin classical suites today and migrate
once the draft stabilises.

### G-3: External-join proposals not wired into a UI surface

`Group::process_incoming` returns
`Incoming::ExternalJoinProposal` defensively but the
client UIs ignore it. The Phase 4 channel subscriber path
lights it up.

## Multi-device

### G-4: AccountIdentity rotation on password change has no KT-log supersession proof yet

The recover flow rotates the exportKey → rotates the
deterministic AccountIdentity. Today the old identity is
simply replaced; sibling peers will see a new pubkey on
the next handshake and TOFU it. The ADR-0007 §6 plan is
to publish a supersession proof to the KT log that
attests the old key → new key transition for ≤ 30 days,
so peers can verify the rotation without bouncing the
trust.

ROADMAP: Phase 2c (transparency log).

### G-5: Commit broadcast on `add_device_from_wire` to other pre-paired devices — **CLOSED**

**Closed (2026-06-03).** All three `runScanFlow` /
`PairingOrchestrator` implementations now broadcast the
`MemberAddition.commit` to the DeviceGroup DS journal
(`DsClient.publishGroup(group_id, commit)`) right after the
Welcome PUT, mirroring the established `removeDeviceAt`
broadcast. Other already-paired devices advance to the new
epoch when their DeviceGroup poll loop runs `processIncoming`
on the row (web `DeviceGroupSyncContext`, native lifecycle
poll). MLS requires in-order epoch processing, so the
broadcast is on the critical path (not best-effort) — a
missed commit would permanently strand a third device. For
the founder + first-pair case there is no other member, so
the row is a harmless no-op. Touchpoints:
`apps/web/src/devices/orchestrator.ts`,
`apps/ios/Lakko/Devices/PairingOrchestrator.swift`,
`apps/android/.../devices/PairingOrchestrator.kt`.

### G-6: History bundle `payload_bytes` placeholder — **cross-platform interop CLOSED; raw-ApplicationPayload-bytes aspiration still open**

`MessageLogStore` carries the projected `(text, attachments,
direction, createdAt)` shape — not the original wire
`ApplicationPayload` bytes. The history bundle round-trips
that shape as opaque JSON inside `payload_bytes`. Both
sides agree on the placeholder; the wire schema stays
proto-clean (`payload_bytes` is just bytes to the schema).

A future sprint can teach the message log to retain the
original wire bytes (smaller + lets a future "verify-replay"
step re-process every old message through the chat envelope
parser) and swap the placeholder for the real thing.

**Audited 2026-06-04 — the placeholder is currently
same-platform-only (web↔native is non-functional).** Reading
the three codecs end-to-end surfaced that the placeholder is
NOT cross-platform-interoperable today, which is broader than
the "JSON field-ordering" framing G-9 carried:

* **`HistoryMessage.message_id` field encoding diverges.** Web
  ([`history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
  sends `textEncoder.encode(m.messageId)` — the UTF-8 bytes of
  the **decimal string**. iOS
  ([`HistoryBundleCodec.swift`](../../apps/ios/Lakko/Devices/HistoryBundleCodec.swift))
  and Android
  ([`HistoryBundleCodec.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/devices/HistoryBundleCodec.kt))
  send the **8-byte big-endian u64**. Different bytes for the
  same logical id.
* **`payload_bytes` placeholder shape diverges.** Web encodes
  `PersistedMessage.messageId` as a JSON **string** (the type
  is `string`); iOS (`Int64`) and Android (`Long`) encode it
  as a JSON **number**. A native `JSONDecoder` /
  `kotlinx.serialization` decode of web's string-typed field
  throws → the message is silently dropped (`continue`); web's
  `JSON.parse` of a native number lands a non-string where the
  `PersistedMessage` contract says string, mis-driving the
  downstream `messageId` string ops.

Net: **iOS↔Android history catch-up interoperates** (matching
field encodings + order-tolerant decode); **web↔native does
not** — a cross-platform pairing falls back to "joiner starts
with no history" (no data loss, but the catch-up is a no-op).

The proper fix closes G-6 and the interop gap together: move
the placeholder encode/decode into the **shared Rust core**
(one canonical codec exposed via UniFFI + wasm-bindgen, so all
three platforms encode/decode identically and it is unit-
testable in Rust), OR ship the raw `mls_app::v1::ApplicationPayload`
proto bytes directly.

**Step 1 landed (2026-06-04) — shared-core codec.**
[`crates/lakko-mls/src/history_placeholder.rs`](../../crates/lakko-mls/src/history_placeholder.rs)
is the canonical codec: a versioned, bounded, deterministic
binary layout carrying `(message_id: u64, direction, created_at,
text, attachments[{blob_id, key, nonce, size, mime?, name?}])`.
`message_id` / `size` ride as typed `u64` (each platform renders
its own way — web decimal string, native integer); `created_at`
rides as the platform's display string verbatim. Exposed
identically through both FFI crates — `encode_history_placeholder`
/ `decode_history_placeholder` + records in
`lakko-ffi-uniffi` (Swift / Kotlin) and `encodeHistoryPlaceholder`
/ `decodeHistoryPlaceholder` + classes in `lakko-ffi-wasm` (web)
— so every platform produces and consumes the **same bytes**.
13 Rust unit tests cover round-trip, all directions,
determinism, and bounded-parse rejection (wrong version,
truncation, over-cap length, non-UTF-8, bad direction tag,
trailing bytes).

**Step 2 landed (2026-06-04) — per-platform wiring.** All three
clients now call the shared FFI codec for `payload_bytes`, and
the sibling `HistoryMessage.message_id` field converged on the
native 8-byte big-endian `u64` (web previously sent UTF-8 of a
decimal string):

* **web** ([`apps/web/src/devices/history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
  — `encodeMessagePlaceholder` / `decodeMessagePlaceholder` call
  `encodeHistoryPlaceholder` / `decodeHistoryPlaceholder`
  (wasm), base64-decoding the attachment refs to raw bytes; the
  `message_id` field is now `messageIdToBeBytes` (BE-u64).
  `tsc --noEmit` clean.
* **iOS** ([`apps/ios/Lakko/Devices/HistoryBundleCodec.swift`](../../apps/ios/Lakko/Devices/HistoryBundleCodec.swift))
  — `encodePlaceholder` / `decodePlaceholder` over the UniFFI
  `HistoryPlaceholder` record; `message_id` already BE-u64.
  Verified: `LakkoCore.xcframework` rebuilt (`just build-ios`)
  with the new FFI symbols, then `xcodebuild build -scheme Lakko
  -destination 'generic/platform=iOS Simulator'` exited 0 with
  zero errors — the app compiles + links against the rebuilt
  framework.
* **Android** ([`apps/android/.../devices/HistoryBundleCodec.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/devices/HistoryBundleCodec.kt))
  — `encodePlaceholder` / `decodePlaceholder` over the UniFFI
  `HistoryPlaceholder` data class; `message_id` already BE-u64.
  `:app:compileDebugKotlin` BUILD SUCCESSFUL.

All three feed the same Rust `encode`/`decode`, so the
`payload_bytes` bytes agree and the `message_id` field agrees —
a web↔native history catch-up now interoperates by construction.
A live cross-platform pairing roundtrip is not headlessly
runnable here; the codec-layer determinism + 13 Rust unit tests
+ web tsc + Android `compileDebugKotlin` + iOS `xcodebuild` (with
the rebuilt xcframework) are the evidence. **The cross-platform
interop bug (the G-9 concern and the web↔native break) is
closed.**

**Still open (downgraded to a nice-to-have):** the placeholder
is a canonical *projection* `(message_id, direction, text,
created_at, attachments)`, not the raw
`mls_app::v1::ApplicationPayload` wire bytes. Carrying the raw
bytes instead would be marginally smaller and would unlock a
future "verify-replay" step (re-parse every catch-up message
through the chat-envelope parser). That is a separate,
non-blocking enhancement — the interop and byte-stability
problems are fully resolved without it.

### G-7: DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations — **CLOSED**

**Closed (2026-06-03).** The scan flow was reordered to
**persist-before-publish** on all three clients:
`add_device_from_wire` (merges the pending commit, advancing
the local view one epoch) → **persist snapshot** → publish
Welcome → broadcast commit (G-5) → best-effort history
bundle. Writing the advanced snapshot to disk before any
publish closes the window where a crash between the publishes
and the persist left the on-disk envelope an epoch behind
(which would make the next launch attempt to add the same
device twice). A crash after persist but before the publishes
leaves the on-disk state correctly advanced; the joiner
simply never receives the Welcome and retries the QR with a
fresh KeyPackage. Touchpoints: the three
`runScanFlow` / `PairingOrchestrator` files (see G-5).

## Web client

### G-8: Web has no Service Worker-side message-log mutation listener

`ActiveGroupsPanel`'s unread badge is sourced from a
one-shot `loadMessageLog(...)` on mount. Live updates from
another tab / route don't refresh the badge until the
list re-mounts. iOS / Android use `@Published` /
`StateFlow` so the in-memory store stays in sync; web
needs an IDB `change` listener (or a `BroadcastChannel`-
backed pub/sub) to match.

### G-9: Web `payload_bytes` JSON placeholder strings are NOT byte-stable — **CLOSED**

**Closed (2026-06-04).** `encodeMessagePlaceholder`
([`apps/web/src/devices/history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
now serializes via a recursive sorted-key `canonicalJson`
helper instead of `JSON.stringify(m)` (whose key order is
insertion order, not byte-stable across JS engines — a
Chrome-scanner → Safari-joiner pairing on the same account is
a real case). Every `PersistedMessage` field is a string, so
recursive key sorting is the whole of RFC 8785 / JCS that
applies to this shape (no numeric canonicalization needed);
the doc-comment flags that a future numeric field would
require ECMAScript number canonicalization. Decode is
unaffected — `JSON.parse` is order-tolerant — so this is a
pure determinism improvement with no wire-compat change for
the web↔web path. `tsc --noEmit` clean. This matches the
canonical-encoding convention iOS already uses
(`JSONEncoder.outputFormatting = [.sortedKeys]`).

Note: this closes byte-stability of the **web-emitted** bytes;
it does NOT address cross-platform web↔native interop, which
is a deeper, separate problem tracked under G-6 below.

## Backup

### G-10: Backup envelope rotation on password change happens on the next baseline write, not the recover round-trip itself

After `/recover` rotates the OPAQUE envelope on the
server, the client's NEXT baseline upload (signup-flow
path) refreshes the backup envelope under the new
exportKey-derived material. The window between recover
and that baseline write is "old phrase still recovers";
in practice the baseline write fires within seconds.
A future sprint can fold the baseline write into the
recover round-trip so the window collapses.

### G-11: No tamper-evident chain across backup versions — **partial: client-with-state case CLOSED**

**Closed for the client-with-state case** by
[`crates/lakko-backup/src/chain.rs`](../../crates/lakko-backup/src/chain.rs):
v2 envelopes carry a plaintext `(version, prev_hash)`
chain header bound into the AEAD AAD; `verify_chain`
walks an ordered sequence and rejects any non-sequential
version OR broken `prev_hash` link. A client that
persisted `(last_known_version, last_known_envelope_hash)`
in its device-key-encrypted local storage catches any
server-side rollback on the next restore.

**Still open: fresh-install rollback.** A user who lost
all local state and is restoring from scratch with only
the recovery phrase has no way to tell whether the
server returned the latest envelope or an older one —
both verify cleanly against the same Argon2id-derived
KEK, and the chain header binds only to the *previous*
envelope, not to a global "is this the head". Closing
this requires a separate out-of-band commitment (e.g.,
a KT-log entry "latest backup envelope hash for account
X is h, signed at time t").

## Auth

### G-12: Session token validation is single-process in-memory — **CLOSED (was already addressed; text was stale)**

**Closed (2026-06-03) — verified already implemented.** The
described per-process in-memory bearer-token map no longer
exists (and a code audit confirms there is none anywhere in
`services/auth` / `services/discovery` — the only `HashMap`
is discovery's IP rate-limit buckets). Session tokens are the
**stateless** HMAC-SHA256 shape in
[`crates/lakko-auth-token`](../../crates/lakko-auth-token/src/lib.rs):
`v1.<account_id_b64>.<expires_at_ms>.<tag>`, where the tag is
HMAC-SHA256 over the prefix under a 32+-byte secret loaded
from the shared `LAKKO_SESSION_TOKEN_SECRET` env var. Both
`services/auth` (issue + `webauthn.rs` / `opaque.rs` verify)
and `services/discovery` (`auth.rs` verify) load the SAME
secret, so:

- **Any instance validates without coordination** — exactly
  G-12's ask. No DB row, no shared cache, no sticky sessions.
- **Restarts don't invalidate tokens** — the secret is
  config, not process state.

Verification is constant-time (`subtle::ConstantTimeEq`),
version-pinned (`v1`), and expiry-checked; 11 unit tests
cover round-trip, wrong-secret, tamper, expiry, malformed
shapes. The one explicit non-goal documented in the crate is
a **revocation list** (a leaked token is valid until its TTL
expires) — that lands "when the threat model expects targeted
token-theft (Phase 2+)" and is a distinct concern from the
"single-process" claim this gap raised.

### G-13: WebAuthn passkey lacks per-credential authentic-action attestation

Today the passkey assertion validates the user has the
device; it doesn't bind to the specific OPAQUE message
the request carries. A reorder attack inside the same TLS
session is implausible but not impossible — Phase 2c can
mix the OPAQUE message hash into the WebAuthn assertion
challenge.

**Design landed (2026-06-04) — [ADR-0014](../adr/0014-webauthn-action-binding.md), implementation pending.**
A spike confirmed the cryptographic binding is not a small
change: `webauthn-rs 0.5.5` (and the low-level
`webauthn-rs-core`) randomise the challenge internally with
no injection hook, and `AuthenticationState.challenge` is
`pub(crate)` — so RP-controlled `challenge = H(action_hash ‖
nonce)` requires either owning verification or a fork.
ADR-0014 chooses a **minimal vendored fork** of
`webauthn-rs-core` that adds one challenge-injection method
(`generate_challenge_authenticate_with_challenge`) while
reusing upstream's `authenticate_credential` verification
**verbatim** (no reimplementation of the dangerous
verification path), plus a new additive action-bound
`/auth/action/{start,finish}` endpoint pair. The existing
login flows stay on the safe high-level `Webauthn`. See the
ADR for the threat model, the rejected alternatives
(server-side-only binding, full hand-roll, serde-patch hack,
deprecated `txAuthSimple`), the supply-chain handling of the
fork, and the property-test plan.

## Calls

### G-17: Frame-counter exhaustion path is test-exercised — **CLOSED**

`ParticipantKey::seal` honours the RFC 9605 §4.4 32-bit
counter bound and surfaces `Error::CounterExhausted` when
exhausted; the caller is expected to trigger a fresh MLS
commit to re-key. A dedicated test that grinds 4 billion
seal ops is infeasible in CI; instead the test trio in
`crates/lakko-calls/src/sframe.rs` uses a `#[cfg(test)]`-
only `set_counter_for_test` accessor to land on the edge
in O(1):

- `counter_exhaustion_returns_typed_error` — counter at
  `u32::MAX - 2`: two seals succeed landing on
  `frame_counter = u32::MAX - 1` then `u32::MAX`; the
  third refuses with `Error::CounterExhausted` (no panic,
  no silent 33-bit wrap, no nonce re-use).
- `counter_max_frame_still_opens_on_receiver` — the last
  legal `frame_counter = u32::MAX` authenticates correctly
  on a receiver re-deriving the same `ParticipantKey`
  (no off-by-one in nonce composition at the boundary).
- `counter_exhaustion_is_idempotent` — 16 successive seal
  attempts past exhaustion all return the typed error and
  the counter stays at `u32::MAX` (never wraps to 0,
  which would re-use the very first nonce in this key's
  lifetime).

The test-only accessor is compiled out of the release
crate via `cfg(test)` so no production caller can reset
the counter and re-use an AEAD nonce.

### G-18: Overlap-window 1-second timer lives in platform code, not in the Rust core

`GroupCallSession::drop_previous_call_key` is invoked by
the platform's own timer (iOS `Task.sleep` / Android
coroutine delay / web `setTimeout`). The Rust core stays
wall-clock-free per design (testability, no time-source
dependency). Each platform's timer correctness is reviewed
alongside the WebRTC pod integration in 3a9 / 3a10
followups; a "platform forgot to drop" would extend the
overlap window indefinitely and weaken seed-boundary
forward secrecy until the next rotation.

Mitigation: the platform's overlap timer is the same
trivial-to-audit `setTimeout(drop, 1000)` shape on every
client; failure mode is local-to-that-client (an attacker
cannot cause it remotely).

**All three platforms: helpers shipped.**

- Web — [`apps/web/src/calls/overlap-timer.ts`](../../apps/web/src/calls/overlap-timer.ts):
  `OverlapTimer` class wrapping `setTimeout`.
- iOS — [`apps/ios/Lakko/Calls/OverlapTimer.swift`](../../apps/ios/Lakko/Calls/OverlapTimer.swift):
  Final class with `Task.sleep` schedule, NSLock-guarded
  state, `@Sendable` callback.
- Android — [`apps/android/app/src/main/kotlin/me/lakko/app/calls/OverlapTimer.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/calls/OverlapTimer.kt):
  `kotlinx.coroutines.delay` schedule, `Mutex`-guarded
  state, scope-bound lifetime.

All three share the same API surface (`arm(callback)`,
`cancel()`, `state`/`isArmed`, three-state machine
`disarmed`/`armed`/`fired`), the same single-shot
"newer arm replaces prior" semantics, and the same
TTL bounds (`[50 ms, 10 s]`, default 1 s per ADR-0008).
Wiring them into a live call flow lands with the SFU
integration in 3b. **G-18 is now a wiring gap, not a
"helper missing" gap** — the schedule machinery is in
place; only the call-into-the-helper at the right moment
is left.

### G-19: SFU-side JWT validation lands with 3b1c (LiveKit fork)

**Issuance side: shipped in 3b4** — crypto in
[`crates/lakko-call-jwt/`](../../crates/lakko-call-jwt/src/lib.rs),
handlers + key persistence in
[`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs).
Mints Ed25519-signed JWTs with the C-15..C-23 invariants
in [`calls.md`](./calls.md). Public key is published at
`GET /.well-known/jwks.json`. Client-side parse + verify
is exposed through the FFI crates so a Lakko client can
self-verify a JWT before presenting it.

**Validation side: still pending 3b1c (Go-side, in the
LiveKit fork)**. The Rust-side `lakko_call_jwt::verify_with_jwks`
is functional and tested + serves as the reference
implementation the Go verifier interops against, but
isn't on the load-bearing SFU path until the LiveKit fork
consumes the JWKS doc.

Operational consequence: today's `POST /call/jwt`
endpoint issues admittedly-bearer tokens but there's no
SFU host to present them to. Clients can already cache
the JWT — the call won't actually open until the SFU is
deployed in 3b2.

### G-20: Call recording is out of scope (deferred per ROADMAP 3c8)

If recording ever lands, the design is "recording client
joins as a regular participant with its own MLS leaf" — no
break to call-side invariants C-1..C-10 in
[`calls.md`](./calls.md). The recorded stream is
end-to-end encrypted just like any other consumer's view.

### G-21: Link-based call invites are single-joiner in V1

`CallInviteCapability` embeds one one-time `KeyPackage`
(see C-26 in [`calls.md`](./calls.md)). The first joiner
to call `accept_call_invite` founds the call MLS group
with the owner already added; a second joiner using the
same capability would call `Group::create` on their own
side and end up in a **separate** MLS group — both would
derive the same `CallKey` seed (same `call_id` exporter
context) but the two groups wouldn't share an epoch, so
the call wouldn't connect.

**Server-side blob storage shipped in 3c3-server** —
`services/discovery /call-invite` + `/call-invite/{blob_id}`
+ `/call-invite/{blob_id}/consume` endpoints (mirror of
the contact-invite ones from 1.18d). Capability bytes
now live server-side keyed by SHA-256(bytes); the URL
becomes `https://lakko.me/c/<blob_id>` instead of
inline-encoded.

**Owner-signed revocation shipped in 3c3-revoke** —
`DELETE /discovery/call-invite/{blob_id}` mirrors
`invite::revoke`. Cross-kind isolation via
`CALL_INVITE_REVOKE_DOMAIN_V1` (distinct from the
contact-invite revoke domain) prevents signature replay
across the two ecosystems.

**Client-side helper trio shipped in 3c3-client** —
`uploadCallInvite(capabilityBytes)`,
`fetchCallInvite(blobId)`,
`consumeCallInvite(blobId, capabilityBytes,
joinerAccountId)`, and `revokeCallInvite(blobId,
signingSeed)` on all three clients
(`apps/web/src/discovery/client.ts`,
`apps/ios/Lakko/Discovery/DiscoveryClient.swift`,
`apps/android/.../discovery/DiscoveryClient.kt`). Wire
shapes reuse the contact-invite envelopes — the server
emits byte-identical JSON for both ledgers, only the URL
path + revoke signature domain separator differ.

**URL plumbing shipped in 3c3-url-helpers** — the
`<origin>/c/<slug>` URL form + parser on three platforms.
The byte-count discrimination contract is C-30 in
[`calls.md`](./calls.md): exactly 32 bytes ⇒ pointer
(SHA-256 `blob_id`); ≥ 400 bytes ⇒ inline (censored-region
fallback that skips the server round-trip); anything else
is rejected as `slugWrongLength`. 25 JUnit cases on Android
pin the discrimination + the constants-vs-web-side drift
sentinels (`CALL_INVITE_URL_PREFIX == "/c/"`,
`CALL_INVITE_BLOB_ID_LEN == 32`,
`CALL_INVITE_INLINE_MIN_LEN == 400`).

**Composer + acceptor orchestration shipped in
3c3-{composer,acceptor}-helper** — one-call `composeAndUpload(...)`
(5 steps: mint `call_id` → mint one-time KP →
`composeCallInviteCapability` via FFI → `uploadCallInvite`
via HTTP → build URL) and `acceptFromSlug(...)` (7 steps
collapsed into an 8-variant discriminated `Result`) on web
/ iOS / Android. The acceptor's defence-in-depth
re-verify (`decodeAndVerify` runs locally AFTER the server
already verified at upload time, so a malicious server
cannot substitute bytes) and `404 → notFound` folding
(denies status-code probing of the `blob_id` space) are
pinned by the C-31 invariant. The acceptor's
`Result.accepted` exposes the 32-byte `callKeySeed` so UI
seals the joiner's first SFrame frame before the owner has
joined — no second FFI round-trip needed.

**Welcome-ferry wire path shipped in
3c3-welcome-ferry** — the joiner-side `acceptCallInvite`
flow's `welcomeForOwner` bytes now reuse the existing
`ContactRequest` envelope transport (HPKE-sealed to the
owner's identity key, routed via the
`StrangerInbox::derive(owner_account_id)` pseudonym).
Joiner-side: wraps the Welcome bytes with the 22-byte
domain separator
`CALL_WELCOME_ENVELOPE_DOMAIN_V1 = b"lakko/call-welcome/v1\x00"`
via [`lakko_contacts::call_welcome::wrap_call_welcome`](../../crates/lakko-contacts/src/call_welcome.rs)
before HPKE-sealing through the standard
`ContactRequest.compose(...)` path. Owner-side: polling
loop calls
[`try_unwrap_call_welcome(plaintext)`](../../crates/lakko-contacts/src/call_welcome.rs)
on every decrypted envelope and dispatches on a
three-way discriminator — `Some(welcome)` → drive
`Group::join_from_welcome(welcome)` and enter the call
group; `None` → render as a plain intro message
(ADR-0005 scenario A); `Err(TruncatedPayload)` → log +
drop a malformed envelope. The separator is asserted
distinct from all sibling domain prefixes (contact-invite
revoke, call-invite revoke, joiner-tag salt) by the
`separator_distinct_from_contact_invite_domains` test.
The reuse means **zero new server surface** — the
existing PoW + Privacy Pass anti-abuse gate on
`ContactRequest` ingress carries over for free, and the
server still sees only `(pseudonym → opaque ciphertext)`.
FFI exposure shipped on both wasm-bindgen
(`wrapCallWelcome` / `tryUnwrapCallWelcome` /
`callWelcomeMinPayloadLen`) and UniFFI surfaces. See
**C-32** in [`calls.md`](./calls.md) for the full
invariant set + 9 property tests in
`lakko_contacts::call_welcome::tests`.

**Platform-side wiring still pending** — the wrap /
unwrap helpers are in place but the platform polling
loops on web / iOS / Android still need to (a) call
`tryUnwrapCallWelcome` on every decrypted
`ContactRequest` they pull from the stranger inbox, (b)
dispatch a Welcome to `Group::join_from_welcome` instead
of the intro-message renderer, (c) transition the call
UI from "waiting for owner" to "in-call" once the
Welcome lands. This is straightforward glue work
deferred to the platform-UI sprint; the cryptographic
contract it depends on is fixed.

**Subsequent-joiner KP-forward wire path shipped in
3c3-kp-forward** — sibling envelope of call-welcome,
under a distinct domain separator
`CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1 = b"lakko/call-keypackage/v1\x00"`.
Subsequent joiners wrap their fresh `KeyPackage` via
[`wrap_call_kp_forward`](../../crates/lakko-contacts/src/call_kp_forward.rs)
and ferry it through the same `ContactRequest` envelope
transport to the call group's current committer. The
committer's polling loop dispatches in order:
`try_unwrap_call_welcome` → `try_unwrap_call_kp_forward` →
intro-message fallthrough. The no-cross-dispatch property
([`welcome_and_kp_forward_round_trips_do_not_cross_dispatch`](../../crates/lakko-contacts/src/call_kp_forward.rs))
guarantees no payload can match two branches at once. See
**C-33** in [`calls.md`](./calls.md) for the full
invariant set + 10 + 5 tests.

**Owner-side add-member orchestration still pending** —
the committer reads the unwrapped KP and runs MLS
`Group::add_member_from_wire(kp_bytes)` + commits, then
ferries the resulting Welcome back to the subsequent
joiner via [`wrap_call_welcome`](../../crates/lakko-contacts/src/call_welcome.rs)
(same C-32 envelope). The MLS operations + the platform-
side polling-loop wiring is straightforward glue
deferred to the platform-UI sprint; the cryptographic
contract is now fully fixed for both halves of the
multi-party handshake.

**Owner-handover protocol shipped in 3c3-handover** —
the committer-leaves scenario now has a pinned policy:
the capability's `owner_account_id` designates the
**owner as the primary committer-of-record**. When the
first joiner disconnects, the owner drives the Remove
commit for the stale leaf (OpenMLS does not permit
self-removal via `remove_members`, and a disconnected
client can't drive `leave_group_via_self_remove`
anyway). Subsequent KP-forwards then route to the owner
as the new committer. Across the leave + add commits
the MLS exporter produces a fresh CallKey seed each
epoch (forward secrecy assertion). The integration test
[`link_call_owner_handover_after_committer_leaves`](../../crates/lakko-calls/src/link_invite.rs)
exercises the full 4-phase sequence (A↔B↔C baseline →
Alice removes Bob → Dave joins via Alice → all three
at same post-handover epoch + byte-identical CallKey
seeds + pre-leave seed differs from post-handover
seed). See **C-36** in [`calls.md`](./calls.md) for the
full invariant set.

The owner-handover test surfaced + fixed a load-bearing
MLS bug: `Group::join_from_welcome` was using
`MlsGroupJoinConfig::default()` which set
`use_ratchet_tree_extension = false` on joined groups,
so a member who joined via Welcome and later committed
add (the exact owner-handover scenario) produced a
Welcome that didn't carry the ratchet tree → the new
joiner failed with `MissingRatchetTree`. The one-line
fix in `lakko-mls::Group::join_from_welcome` flips the
flag on, matching the founder's `MlsGroupCreateConfig`,
so a chain of welcome-then-add-then-welcome propagation
now survives any number of hops.

Operational impact: V1 1:1 AND multi-party (3+) link
calls have a **complete protocol-level wire path** in
all directions including the committer-leaves
handover. The full cryptographic state machine is
backed by named integration tests (C-35 happy path,
C-36 handover path). What remains for V1 is purely
platform-UI glue: composer view, `/c/<slug>` landing
page, polling-loop integration with
`dispatch_incoming_envelope`. The cryptographic
contracts the UI work depends on are all fixed and
fuzz-covered.

### G-22: Per-joiner dedup for call invites lands with multi-use orchestration

**Atomic `max_uses` enforcement shipped in 3c3-server**
via `services/discovery/src/call_invite.rs::consume` —
the handler bumps `used_count` only when the row has
remaining budget (same CASE-expression sentinel mapping
the contact-invite consume handler uses: `0 → 1`,
`u32::MAX → unlimited`, otherwise N). Exhausted budgets
surface as `410 Gone`; expired rows fold into `404 Not
Found` to deny status-code probing.

**Per-joiner dedup shipped in 3c3-multi-use** —
[`0008_call_invite_consumers.sql`](../../services/discovery/migrations/0008_call_invite_consumers.sql)
adds the `call_invite_consumers` side table mirroring
`invite_consumers` (PK on `(blob_id, joiner_tag)`,
`FOREIGN KEY ... CASCADE` on `call_invites.blob_id`).
The consume handler runs `INSERT IGNORE` into the table
inside the same transaction as the counter bump; a
repeat from the same joiner conflicts on the PK,
surfaces as `already_consumed = true` in the response,
and does NOT bump the counter.

Joiner-tag computation reuses the existing
[`lakko_contacts::invite::compute_joiner_tag`](../../crates/lakko-contacts/src/invite.rs)
function (HMAC-SHA-256 over a per-blob salt). The salt
is `SHA-256(JOINER_TAG_SALT_DOMAIN_V1 || capability_bytes)`,
so contact-invite and call-invite tags for the same
joiner don't collide even though the helper is shared —
[`lakko_contacts::call_invite::tests::joiner_tag_works_on_call_invite_bytes`](../../crates/lakko-contacts/src/call_invite.rs)
asserts this cross-kind salt isolation.

**KP-republish orchestration for multi-joiner calls
shipped in 3c3-kp-forward + 3c3-incoming-dispatcher** —
the second-and-subsequent-joiner path now has a fully
specified wire chain: joiner wraps a fresh KeyPackage
via `wrap_call_kp_forward`, ferries it through the
shared ContactRequest envelope transport to the current
committer; the committer's polling loop runs
`dispatch_incoming_envelope` which discriminates among
welcome / KP-forward / intro-message kinds; on a KP-
forward hit the committer runs
`Group::add_member_from_wire(kp)` + commits + wraps the
resulting Welcome via `wrap_call_welcome` and ferries
it back to the new joiner. The owner-handover sprint
(C-36) extended this with the policy for "what happens
when the original committer leaves" — the owner
designated in the capability becomes the new
committer-of-record. The full multi-party + handover
flow is backed by two named integration tests:
`link_call_multi_party_e2e_via_kp_forward_envelope`
(3-party rendezvous) and
`link_call_owner_handover_after_committer_leaves`
(committer-leaves + owner takeover). The dedup table
(this G-22's primary scope) composes with the KP-
forward chain cleanly: an already-consumed
`(blob_id, joiner_tag)` short-circuits to the
"already joined" UI branch without re-bootstrapping
the MLS group.

## Out-of-protocol

### G-14: Hostile-OS / hostile-browser threats

No app-level integrity attestation. We rely on the
platform's standard sandbox + the user's awareness. Phase 5
adds App Attest / Play Integrity / WebAuthn-attestation to
the signup / login round-trip.

### G-15: Side-channel + physical attacks

Out of scope for this audit pass. Mitigated by relying on
the platform's constant-time crypto primitives (`subtle`
crate, OpenMLS's vetted implementations). No hand-rolled
crypto in the codebase — the `lakko-crypto` crate is
plumbing only.

### G-16: Quantum adversaries

MLS suites pin classical curves today. PQ migration
follows the WG.

### G-26: RFC 9605 §4.3 wire frame — serializer + framed pipeline + cross-platform AAD **landed**, live RTP interop verification pending

**Status (2026-06-02):** nearly closed; only the live
cross-platform verification step remains. Progress since
the 2026-05-25 entry:

1. **Wire serializer.**
   [`crates/lakko-calls/src/header.rs`](../../crates/lakko-calls/src/header.rs)
   ships `SframeHeader::encode` / `SframeHeader::decode`
   (12 unit tests; RFC 9605 §4.3 conformant by
   construction — round trip + pinned wire layouts +
   reserved-bit refusal + truncation handling +
   bit-packing first-byte invariants for every LEN / KID
   / KLEN value in `0..=7`).
2. **Framed seal/open pipeline — DONE.**
   [`lakko_calls::sframe`](../../crates/lakko-calls/src/sframe.rs)
   now exposes `ParticipantKey::seal_framed` (peeks the
   counter, builds the header, seals with the header as
   AEAD AAD, returns `header || ciphertext`) and
   `open_framed` (decodes the header, binds the EXACT
   on-wire header bytes as AAD, checks `kid == sender_leaf`,
   opens). 7 property tests added on top of the prior
   suite. The SFU reads the KID off the wire to route by
   participant without touching the ciphertext.
3. **Cross-platform AAD contract — [ADR-0013](../adr/0013-sframe-cross-platform-aad.md).**
   The AEAD AAD is the SFrame wire header (KID + CTR), NOT
   any host RTP `additional_data`. This is the property
   that lets a frame sealed on one platform open on
   another: the host metadata is not reconstructible
   across stacks, so it cannot be in the authenticated set.
4. **All three client surfaces consume the framed path.**
   The flat C ABI (`lakko-sframe-native`:
   `lakko_sframe_seal_framed` / `_open_framed`), UniFFI
   (`ParticipantKey.seal_framed` / `open_framed`), and
   wasm-bindgen (`sealFramed` / `openFramed`) all expose
   it. The native libwebrtc C++ bridge
   (`apps/shared/sframe-bridge`) calls `seal_framed` /
   `open_framed` directly (host `additional_data`
   intentionally unbound); its 8-assertion host test
   (`just test-sframe-bridge`) proves both that a
   mismatched host AAD does NOT fail decryption and that a
   tampered header DOES. The browser side is the
   `RTCRtpScriptTransform` worker
   (`apps/web/src/calls/sframe-transform.ts` +
   `sframe-transform-worker.ts`) calling `sealFramed` /
   `openFramed` inside a module worker.

**What is still open — live cross-platform RTP verification.**
The byte-level interop is asserted by construction (the
header is platform-independent; native + web call the same
`lakko-calls` core through the same framed API) and by the
native host test. What has NOT been observed firsthand is a
real media frame sealed on one platform (e.g. a Chrome
`RTCRtpScriptTransform` sender) decrypting on another (e.g.
an iOS / Android libwebrtc receiver) over a live SFU
forwarding path. That requires two real endpoints plus the
SFU in the middle and cannot be done in a headless build
environment. The web multi-party session that drives the
worker (`apps/web/src/calls/sfu-call-session.ts` — composes
the `lakko-sfu-v1` signaling client + the call-admission JWT
+ one `RTCPeerConnection` to the SFU + `attachSenderSFrame` /
`attachReceiverSFrame` on every track + SFU-initiated
renegotiation) is now wired into the chat UI: a group chat
(>2 members) routes the call button through
`createSfuCallUi`, which drives the shared call-UI store
roster, plays remote audio, and renders per-tile remote
video, while 1×1 chats stay on the P2P path. The MSID
stream-id → source-peer correlation the video tiles need is
pinned by construction — the SFU sets the downstream MSID
stream-id to the source peer id (`add_media(..,
Some(source_peer.to_string()), ..)` in
`services/sfu/src/media.rs`), and that string is the same
`PeerId` the signaling roster carries, so
`ev.streams[0].id` maps straight onto the roster tile. The
**native** iOS + Android clients are now wired to the same
extent (SFU multi-party arc, complete): `SfuSignalingClient`
(WS + typed `lakko_sfu_proto` frames) + `SfuCallSession`
(one `RTCPeerConnection`/`PeerConnection` to the SFU,
`armSFrameEncryptors` on every local sender before the offer,
`armSFrameDecryptor` on every remote receiver, SFU-initiated
renegotiation) + the `SfuCallUi` adapter driving the shared
`CallUiState` + the chat-header trigger/presentation. Both
publish SFrame-sealed video when the call is started with the
video-call button (the local camera is captured, the video
track added before `armSFrameEncryptors` + the offer, so the
video sender is sealed alongside audio). All three platforms
therefore reach the SFU through the identical framed
`seal_framed` / `open_framed` core. What remains is only the
live cross-platform RTP verification above (a real frame
round-tripping web↔native through a deployed SFU) — a
verification step, not a missing mechanism.

**Why this isn't a security gap today.** The keying math +
the AAD set + the wire layout are all RFC 9605 conformant
and shared across platforms by construction. The residual
item is a verification step (does a live cross-stack frame
round-trip?), not a missing mechanism. The fail-closed
posture holds throughout: a frame that fails to
parse / authenticate is dropped, never forwarded as
plaintext (invariant #4), on every surface.

## Dependency advisories

The `cargo audit` baseline (RustSec advisory DB) surfaces
two open vulnerabilities + three unmaintained-crate
warnings in the workspace's transitive dependency tree.
None are in code we control; remediation depends on
upstream releases. Each entry below lists severity,
reachability analysis, and the upstream tracker the fix
is gated on.

### G-23: `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2)

> Potential Panic on Overlong Ciphertext Buffer.
> Upgrade to >= 0.0.8.

Transitive dependency chain:
`libcrux-chacha20poly1305 0.0.7 → libcrux-aead 0.0.7 →
hpke-rs-libcrux 0.6.1 → hpke-rs 0.6.1 →
openmls_rust_crypto 0.5.1 → lakko-mls`.

**Why we can't just bump it.** The fix in 0.0.8 ships with
breaking internal dependency changes (`rand ^0.10` from
`^0.9`, `libcrux-traits =0.0.7` from `=0.0.6`); a
`[patch.crates-io]` override that pins
`libcrux-chacha20poly1305 = 0.0.8` breaks the
`libcrux-aead 0.0.7` we still depend on. The cleanest fix
is upstream `hpke-rs-libcrux` cutting a release that
bumps to `libcrux-aead 0.0.8` — tracked by
[cryspen/hpke-rs](https://github.com/cryspen/hpke-rs)
issues.

**Reachability.** The vulnerable code path is HPKE
seal/open inside OpenMLS. Every byte that reaches
`libcrux-chacha20poly1305::decrypt` first passes through
OpenMLS's `MlsMessageIn` parser (length-bounded by the
TLS-codec spec — `lakko-mls-fuzz/mls_message` fuzz target
exercises this surface) and the HPKE-RS wrapper's own
length checks. We have not been able to construct an
input that reaches the panic via the OpenMLS public API.
The risk in practice is bounded to a per-request DoS on
the calling service (services run with `RUST_BACKTRACE=1`
+ tokio's panic-catching at the request boundary, so a
panic in one handler does NOT down the whole process).

**Mitigation pending upstream**: client-side
ContactRequest envelope inputs are gated by a
session-token rate limit + PoW + Privacy Pass before
they reach the OpenMLS parser. An attacker would need
to burn one PoW solve per crash attempt and tie a
session token — high enough cost that this isn't a
realistic DoS path even if reachable.

**Status**: accepted risk pending upstream
`hpke-rs-libcrux` release. Re-audit every sprint and
upgrade as soon as a compatible version lands.

### G-24: `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack

> Potential key recovery through timing sidechannels.
> No fixed upgrade is available.

Transitive dependency: `rsa 0.9.10 → sqlx-mysql 0.8.6 →
sqlx → all `services/*`.

**Why this is structural, not negligence.** The `rsa`
crate is the only pure-Rust RSA implementation that
compiles on every target sqlx supports; the Marvin-attack
mitigation requires constant-time arithmetic that the
crate maintainers explicitly call out as "we will not
land this in the current codebase." The upstream
position is "use a different RSA library if you need
side-channel resistance" — `sqlx-mysql` would have to
swap implementations, which it has not.

**Reachability.** `rsa` is reached **only** via MySQL
native-auth handshake when the server's
`mysql_native_password` authentication method requires
RSA-OAEP padding to encrypt the client password (server-
side `caching_sha2_password` cache miss path). Lakko
configures MariaDB with `caching_sha2_password` ONLY,
and the warm-cache hit path uses SHA-2 without RSA. The
RSA path is reached only on first-time connect from a
client whose password hasn't been cached server-side.
**The application never sees user passwords directly**:
authentication runs OPAQUE-PAKE (`services/auth`), which
the database connection layer is downstream of — the
MySQL connection password is the **service's** own DB
password (configured in `/etc/lakko/lakko.env`), not a
user's. A successful timing attack on this RSA exchange
would leak the service's MySQL connection password to an
attacker who already has network access to MySQL itself
— at which point the attacker has direct DB access and
the RSA leak is the wrong attack to spend a Marvin
side-channel on.

**Mitigation**: production MariaDB binds only to the
internal loopback / VPC interface; only the Lakko
services reach it. An attacker who can mount a timing
side-channel on this internal hop has already broken
the perimeter.

**Status**: accepted risk pending upstream `sqlx-mysql`
swapping to a constant-time RSA crate (no tracker —
upstream position is "no fix coming"). Re-audit if MySQL
auth surface changes.

### G-25: Three unmaintained-crate warnings

The same `cargo audit` baseline flags three
unmaintained-crate advisories. None are vulnerabilities
— maintainers have wound down support, but the code
itself works. All are transitive dependencies pinned by
the upstream tooling we depend on; remediation requires
upstream replacing them.

- **`bincode 1.3.3`** (RUSTSEC-2025-0141) — under
  `uniffi_macros 0.28.3 → uniffi 0.28.3`. Used at build
  time to serialise UDL → macro outputs. Runtime impact:
  none. Tracker: `mozilla/uniffi-rs` planning a 0.29
  release that switches to `bincode 2.x`.

- **`instant 0.1.13`** (RUSTSEC-2024-0384) — under
  `parking_lot_core 0.8.6 → parking_lot 0.11.2 →
  fluvio-wasm-timer 0.2.5 → openmls 0.8.1`. WASM-only
  timer abstraction. Runtime impact: low — only the
  WASM target compiles this. Tracker: openmls 0.9
  drops fluvio-wasm-timer for `web-time`.

- **`paste 1.0.15`** (RUSTSEC-2024-0436) — under
  `uniffi_core 0.28.3 → uniffi 0.28.3`. Compile-time
  macro helper. Runtime impact: none. Tracker: the
  `dtolnay/paste` archival happened mid-uniffi-0.28
  cycle; uniffi 0.29 is planning to drop the dep.

**Status**: accepted upstream debt. CI is configured to
warn (not fail) on unmaintained-crate advisories so
they remain visible without blocking releases.

**Cumulative posture**: zero vulnerabilities in Lakko-
authored code; two open advisories in transitive deps,
both with reachability analysis above; three
unmaintained-crate warnings pending upstream
remediation. The `cargo audit` baseline is captured
here as of `2026-05-28` (re-run delta vs `2026-05-27`:
**zero change** — same 2 vulnerabilities, same 3
unmaintained warnings, no new findings, no advisories
resolved upstream. Post-G5-6-wireup confirmation: the
`hpke 0.13` + `rand_core 0.9` deps added by ADR-0012
brought in their pure-Rust RustCrypto dependencies
[`x25519-dalek`, `aes-gcm`, `chacha20poly1305`,
`hkdf`, `hmac`, `sha2`] without introducing any new
advisory) and is re-run on every CI build; new
findings either get a G-entry of their own or are
remediated as the upstream chain catches up.

`cargo deny check` is the second tier of the supply-
chain gate (licence allowlist, advisory deduplication,
unmaintained-crate policy on top of `cargo audit`). The
binary is not currently installed on this dev host; the
`just audit` recipe invokes it during CI runs. A follow-
up sprint stands up `cargo deny` locally on the
maintainer's box so the audit pack can mirror its CI
output here.

### G-27: Channel PublisherGroup — **all three platforms wired** (G4-2-mls-publisher + G-27 iOS / Android slice); persistence + rotation still deferred

[`apps/web/src/channels/composer.ts::freshPublisherKeys`](../../apps/web/src/channels/composer.ts)
now derives the channel content-key seed from a real
MLS PublisherGroup exporter: `Group.create(identity)`
→ `exportSecret(identity, channelExporterLabel(),
channelIdBytes, 32)` →
`channelSeedFromMlsExporterOutput`. The identity is
rebuilt from the OPAQUE export key
(`wasm.Identity.fromSeed(secrets.exportKey)`) inside
the `NewChannel` route handler in
[`apps/web/src/routes/Channels.tsx`](../../apps/web/src/routes/Channels.tsx).
The wasm-pack bindings have been regenerated so
`channelExporterLabel` + `channelSeedFromMlsExporterOutput`
are exposed in the TypeScript `.d.ts`. TS typecheck +
`npm run build` are green.

**Closed — iOS + Android wired.** Both per-platform
composers now drive the same `Group::create` +
`Group::export_secret` +
`channelSeedFromMlsExporterOutput` triplet the web
composer uses:

- [`apps/ios/Lakko/Channels/ChannelsComposer.swift::freshPublisherKeys`](../../apps/ios/Lakko/Channels/ChannelsComposer.swift)
  now takes `(identity: Identity, channelIdHex:
  String)`, builds a transient `Group` via
  `Group.create(identity:)`, calls
  `group.exportSecret(identity:, label:
  channelExporterLabel(), context: channelId,
  length: 32)`, and crosses the FFI boundary via
  `LakkoCore.channelSeedFromMlsExporterOutput`. The
  caller plumbs `secrets.exportKey` from the
  `AuthRepository` session through
  `ChannelsRootScreen(exportKey:)` →
  `ChannelsListScreen` → `NewChannelScreen` → its
  `create()` action which constructs the throwaway
  `Identity.fromSeed(seed: exportKey)` for the
  duration of the call.
- [`apps/android/app/src/main/kotlin/me/lakko/app/channels/ChannelsComposer.kt::freshPublisherKeys`](../../apps/android/app/src/main/kotlin/me/lakko/app/channels/ChannelsComposer.kt)
  ships the same shape with the same lifecycle
  semantics — `Group` and `Identity` are both
  `AutoCloseable` via UniFFI's Disposable
  conformance, so the call sites use `try { ...
  } finally { group.close(); identity.close() }`
  to drop the Rust handles eagerly rather than
  waiting on the JVM GC. The Compose call site at
  `apps/android/.../ui/channels/ChannelsScreen.kt::ChannelsRootScreen(exportKey: ByteArray, ...)`
  threads the export key through the same shape
  as iOS.

`xcodebuild -sdk iphonesimulator -configuration Debug build`
and `./gradlew :app:assembleDebug` both green after
the wire-up. The audit-pack invariant C-44 ("the
channel seed crosses the FFI boundary via
`channelSeedFromMlsExporterOutput`, never via a raw
`Data` cast") now holds on every client platform
that ships a publisher composer.

**G4-2-mls-persistence — CLOSED (May 2026).**

The persistence + rotation gap closed across six
sub-sprints:

- **Step 1 (Rust core)** —
  `crates/lakko-mls/src/publisher_group.rs` ships
  `PublisherGroup` keyed by `channel_id`, with
  `load` / `create` / `open_or_create` routed
  through OpenMLS's `MlsGroup::new_with_group_id`
  and `MlsGroup::load`. Deterministic group id
  derivation: `prefix("lakko/publisher-group/v1/") ||
  channel_id`. 8 unit tests pin the load-or-create
  idempotence invariant.
- **Step 2 (FFI)** — UniFFI + wasm-bindgen
  `PublisherGroup` wrappers in `lakko-ffi-uniffi` +
  `lakko-ffi-wasm`. `publisher_group_load` /
  `publisher_group_open_or_create` free functions
  (Option<Self> work-around for UniFFI 0.28). 7
  UniFFI + 4 wasm host-side tests.
- **Step 3 (composer refactor)** — iOS + Android +
  web composers all swapped throwaway `Group.create`
  for `PublisherGroup.openOrCreate`. xcodebuild +
  `:app:assembleDebug` + `npm run build` all green.
- **Step 4 (rotation)** —
  `PublisherGroup.rotate(identity)` advances the
  MLS group epoch via `MlsGroup::self_update` +
  `merge_pending_commit`. Returns the wire-encoded
  commit for fanout to co-publishers. Both the
  Rust-core and the FFI surfaces are covered by
  `rotate_advances_epoch_and_changes_seed` /
  `rotated_seed_persists_across_reload` /
  `publisher_group_rotate_changes_seed`.
- **Step 5 (audit pack)** — see C-52 in
  `docs/security/calls.md` for the persistence +
  rotation invariants.
- **Step 6 (cross-publisher membership)** —
  `rotate_emits_commit_bytes_processable_by_co_publisher`
  proves: Alice mints PublisherGroup, adds Bob via
  Add proposal, Bob joins from welcome, Alice
  rotates, Bob processes the commit, both ends at
  the same epoch with the same exporter output.
  ADR-0010 §"PublisherGroup" cross-publisher
  seed-sync invariant satisfied.

Remaining deferred follow-ups (out of G4-2 scope):

- **G4-2-rotation-timer wave 1 DONE** (May 2026) —
  scheduled rotation policy + web orchestration.
  `crates/lakko-channels/src/rotation_policy.rs` ships
  `RotationPolicy` (24 h or 1 000 posts, both
  thresholds independently disable-able) + `RotationState`
  + pure `should_rotate(now_ms, state)` decision.
  Saturating arithmetic on the time delta tolerates
  host-clock jumps backward. 13 unit tests + UniFFI +
  wasm-bindgen FFI surface + audit-pack invariant
  **C-54** in [`calls.md`](./calls.md).
  `apps/web/src/channels/rotation-cron.ts` ships
  `runRotationOnce` (single-pass: policy check → MLS
  `PublisherGroup.rotate` → fetch subscribers → seal new
  envelopes per subscriber → atomic batch `POST
  /v1/channels/:id/rotate`) + `startRotationCron`
  (periodic poller, default 5-minute heartbeat). Empty
  batches are allowed (still bump the epoch — covers
  the "no subscribers yet" case). Server-side `POST
  /v1/channels/:id/rotate` is C-42 (atomic
  rotation); client now drives it via the new
  `rotateChannel` helper at
  `apps/web/src/channels/api.ts`.
- **G4-2-rotation-timer-ios DONE** (May 2026) —
  `apps/ios/Lakko/Channels/RotationCron.swift` ships
  the Swift orchestrator as an `actor` with `runOnce(policy,
  state, nowMs?)` + `start(policy, pollInterval,
  loadState, onError)` returning a `Task` whose
  `cancel()` stops the loop. Same shape as the web side:
  policy check via UniFFI `channelShouldRotate` →
  `PublisherGroup.rotate` → fetch subscribers → seal new
  envelopes per subscriber → atomic batch via
  `ChannelsClient.rotateChannel(...)`. New
  `RotateBatchEntry` wire type lives in
  `ChannelsClient.swift` next to the existing batch
  shapes. `xcodebuild -scheme Lakko -destination
  'platform=iOS Simulator,name=iPhone 17'` →
  BUILD SUCCEEDED.
- **G4-2-rotation-timer-android DONE** (May 2026) —
  `apps/android/app/src/main/kotlin/me/lakko/app/channels/RotationCron.kt`
  ships the Kotlin orchestrator with the same shape
  (`runOnce` suspend fun + `start(scope, policy,
  loadState, pollInterval, onError)` returning a `Job`).
  `RotateBatchEntry` @Serializable wire type +
  `ChannelsClient.rotateChannel` Ktor POST. Uses the
  UniFFI bindings (`channelShouldRotate`,
  `channelRotationStateAfterRotation`,
  `sealSubscriberSeedEnvelope`) verbatim — no Kotlin-side
  re-implementation of the policy. `./gradlew
  :app:assembleDebug` → BUILD SUCCESSFUL.

The G4-2-rotation-timer arc is now complete across all
three platforms (web + iOS + Android). All decisions
about _when_ to rotate flow through one Rust file
(`crates/lakko-channels/src/rotation_policy.rs`); each
platform's orchestrator is ~200 LOC of pure plumbing
above the same FFI surface.

The cryptographic property is preserved: the seed is
genuinely derived from the MLS exporter under the
publisher's identity, not from `OsRng`. What is
missing is the multi-device byte-identicality
property (per ADR-0010 Tier 1, two devices of the
same publisher on the same channel SHOULD produce
the byte-identical seed via shared identity +
identical `Group::create`/exporter inputs;
`Group::create` uses the identity's MLS provider
which currently treats `group_id` as random, so
that property only holds after group-id persistence

### G-28: SFrame on-device wiring — CLOSED (May 2026) via the G3-sframe-native arc (N-1..N-6)

**Status (May 2026)**: **CLOSED.** Six sub-sprints (N-1
through N-6) shipped the full native-shim stack on both
mobile platforms. The Rust SFrame algorithm is now
plumbed end-to-end through libwebrtc's
`FrameEncryptorInterface` / `FrameDecryptorInterface`
on iOS and Android, with security invariant #4 ("no
fallback to non-E2EE") enforced at five layers
(documented under C-45..C-50 in
[`docs/security/calls.md`](./calls.md)).

What landed:

- **N-1** `crates/lakko-sframe-native/` — flat C ABI
  around `lakko_calls::sframe` (11 `extern "C"`
  functions + 7 unit tests + cbindgen header emitted
  to a stable mirror at `target/lakko-include/`).
- **N-2** `apps/shared/webrtc-cpp-headers/M125/` — 13
  sha256-pinned libwebrtc M125 (branch-heads/6422)
  C++ headers + 2 Lakko-authored abseil shims +
  `fetch.sh` verify-mode + `probe.cc` compile-only
  validation harness.
- **N-3** `apps/shared/sframe-bridge/` — C++ subclass
  `LakkoFrameEncryptor` / `LakkoFrameDecryptor` of
  `webrtc::FrameEncryptorInterface` /
  `FrameDecryptorInterface` (the upstream M125
  shape). 4-test standalone round-trip suite proving
  AAD-binding, multi-sender cache, bad-seed rejection.
- **N-4** `apps/ios/Lakko/Calls/SFrameAttach.{h,mm}`
  + `Lakko-Bridging-Header.h` — Obj-C++ shim with
  boot-time `class_respondsToSelector` probe for
  `setFrameEncryptor:` / `setFrameDecryptor:`. Refuses
  to start the call when the probe goes negative.
  Swift-side `SFrameAvailability.shared` fail-closed
  gate. `cross-build-sframe-native-ios` justfile
  recipe lipo-fuses simulator slices.
- **N-5** `apps/android/lakko-sframe/` — Gradle AAR
  with Kotlin `LakkoFrameEncryptor` / `Decryptor`
  implementing `org.webrtc.FrameEncryptor` /
  `FrameDecryptor`. JNI bridge `libsframe_jni.so`
  built via `tools/build-sframe-jni.sh` against the
  cross-compiled Rust staticlib for each NDK ABI.
  Kotlin `SFrameAvailability` boot probe through
  `Class.forName + getMethod` against
  `org.webrtc.RtpSender.setFrameEncryptor`.
- **N-6** Production CallSession integration on both
  platforms — `setCallKey(seed, localLeaf)` injected
  after welcome-ferry / KP-forward dispatcher
  resolves; `attachSFrame` called between
  `addLocalAudioTrack` and SDP build so the
  encryptor is armed before libwebrtc's codec
  thread pulls bytes. Extended Rust E2E test
  `crates/lakko-sframe-native/tests/sfu_opacity_e2e.rs`
  (6 tests, all green) proving SFU opacity,
  AAD-tampering detection, wrong-key isolation,
  multi-party fan-in, cross-call isolation, counter
  monotonicity.

Validation chain (all green):

- `cargo test --workspace --all-targets` — 224+ tests
  pass including the new sfu_opacity_e2e suite.
- `xcodebuild -destination 'generic/platform=iOS
  Simulator' build` → BUILD SUCCEEDED. The full Swift
  / Obj-C++ / C++ / Rust path compiles and links
  cleanly with the lipo'd `liblakko_sframe_native.a`.
- `./gradlew :app:assembleDebug` → BUILD SUCCESSFUL.
  The full Kotlin / JNI / C++ / Rust path likewise.

**Deferred follow-ups (out of G-28 scope, tracked
separately)**:

- **N-{4,5}-validate** — host-side close-out DONE
  (May 2026). `apps/shared/sframe-bridge/test_bridge.cc`
  ships eight standalone C++17 assertions exercising
  every documented tampering mode of the SFrame
  native shim before either platform binding loads:
  single-sender round-trip, AAD-tamper, multi-sender
  per-leaf cache, bad-seed rejection, ciphertext-body
  tamper, counter monotonicity with out-of-order
  delivery, per-call key isolation (two distinct
  CallKey seeds cannot cross-decrypt), payload-size
  extremes (1B/80B/1500B/64KiB). Runner: `just
  test-sframe-bridge` locally and
  `.github/workflows/sframe-bridge.yml`
  (`workflow_dispatch` only, matrix `ubuntu-latest`
  + `macos-latest`) in CI. Pinned as audit-pack
  invariant **C-53** in
  [`docs/security/calls.md`](./calls.md).
- **N-{4,5}-validate-live** — dual-simulator /
  dual-emulator live-RTP gate (two iOS simulators
  + two Android emulators in CI running synthetic
  calls against the dockerised SFU from
  `ci/sfu-e2e/`, asserting tampered AAD →
  `kFailedToDecrypt` end-to-end). Lives in
  **G3-4-mobile** — the simulator / emulator matrix
  half of the parent **G3-4** sprint. Needs
  `macos-14` + KVM-enabled Linux runners we do not
  yet have budget for.

Build artefacts are already produced; the host-side
gate covers every code path below the platform shim,
and the dual-instance live-RTP gate is the only
remaining piece (waiting on CI-runner allocation).

---

**Historical context (preserved for audit trail)**:

The original G-28 investigation surfaced a structural
ceiling on both upstream WebRTC SDKs Lakko depends on:

- **Android `io.github.webrtc-sdk:android` 125.6422.07**
  exposes `org.webrtc.FrameEncryptor` and
  `org.webrtc.FrameDecryptor` only as Java interfaces
  with a single abstract method `long
  getNativeFrameEncryptor()` / `long
  getNativeFrameDecryptor()`. The actual `encrypt` /
  `decrypt` calls happen entirely inside the C++
  `webrtc::FrameEncryptorInterface` /
  `FrameDecryptorInterface` instance whose pointer
  that `long` returns. A pure-Kotlin override of an
  `encrypt(mediaType, ssrc, additionalData, frame)`
  method **does not exist on this SDK** — that shape
  matches the older `google-webrtc` artifact Lakko
  abandoned per G3-2 / ADR-0011's note that Google
  stopped publishing that artifact in 2021.
- **iOS stasel/WebRTC 125.0.0** — **G3-1-sframe-spike
  closed (May 2026)**: walked the
  `WebRTC.xcframework/ios-arm64/WebRTC.framework/Headers/`
  directory (92 public headers across iOS arm64, iOS
  simulator x86_64+arm64, macOS, mac-catalyst — every
  slice identical in this respect). The only public
  mention of `FrameEncryptor` / `FrameDecryptor` is the
  docstring on `RTCCryptoOptions.sframeRequireFrameEncryption`
  — a require-flag, **not** an attach-API. Neither
  `RTCRtpSender.h` nor `RTCRtpReceiver.h` exposes
  `frameEncryptor` / `frameDecryptor` /
  `frameTransformer` properties. The C++
  `webrtc::FrameEncryptorInterface` exists in upstream
  libwebrtc but stasel's Obj-C wrappers don't bridge
  it out. Same shape as Android.

**Path forward (Android, possibly iOS)**: a Rust crate
`lakko-sframe-jni` that compiles via `cxx-rs` to one
`.so` per ABI (arm64-v8a, x86_64). The `.so` exposes
a C++ subclass of `webrtc::FrameEncryptorInterface`
whose `Encrypt` / `Decrypt` thunks call back into
LakkoCore's `ParticipantKey` via the existing UniFFI
surface (a `long` pointer round-trips through the
Java `getNativeFrameEncryptor()` contract without
ever crossing back to the JVM mid-frame). This is
the canonical Insertable-Streams shape upstream
expects; it is also how Signal, Jitsi, and Matrix
do it. Estimated at ~2 sprints (cxx-rs setup +
libwebrtc header dep + Android NDK toolchain pin +
separate Gradle module for `.so` packaging).
Tracked as `G3-2-sframe-jni`.

**iOS path forward (post-spike)**: matching native-
shim shape under task **G3-1-sframe-native**. An
Objective-C++ (`.mm`) class subclasses
`webrtc::FrameEncryptorInterface` + `FrameDecryptorInterface`
from upstream libwebrtc's C++ headers (bundled
ourselves since stasel only ships Obj-C headers +
the linked binary; libwebrtc's BSD license permits
header redistribution). The shim is installed onto
`RTCRtpSender` / `RTCRtpReceiver` via the
non-public selector `setFrameEncryptor:` that the
upstream Obj-C wrapper carries but does not declare
in its public headers (the selector is hit by name
through `objc_msgSend` — same technique Signal-iOS
and Jitsi-Meet iOS use for the identical gap).
Inside `Encrypt` / `Decrypt` the shim calls into
LakkoCore's `ParticipantKey` via the existing
UniFFI surface. Estimated ~2 sprints in parallel
with G3-2-sframe-jni; the Rust side may share a
single crate (`lakko-sframe-native`) exposing a C
ABI both platforms wrap.

The native shim approach has a security caveat: the
non-public `setFrameEncryptor:` selector is not part
of stasel/WebRTC's stable API. An upstream rename or
removal would silently no-op the installation —
behaviour the audit pack must catch. Mitigation: a
boot-time `class_respondsToSelector` probe that
panics loud if the selector is missing, surfaced as
a "SFrame attach refused" log + the call MUST refuse
to start (no fallback to plain SRTP under any
circumstance, per security invariant #4 "no fallback
to non-E2EE").

**Security posture during the gap**: documented in
[`docs/security/calls.md`](./calls.md) under the
G3-1 / G3-2 partial-completion rows in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md):
the iOS + Android call paths today ride **plain
SRTP** (per-hop encryption only). They are NOT
E2EE-complete. The wire still announces SFrame
support so the SFU still forwards opaque payload,
but the absence of an actual SFrame transformer at
the codec boundary means the SFU could in principle
decode the SRTP and see plaintext Opus/AV1. ROADMAP
explicitly states this. The fix is the JNI shim
above + the iOS spike + (subsequently) MLS-exporter
seed delivery from each platform's CallSession into
the freshly-created `ParticipantKey`.
+ replication via DeviceGroup are wired).
