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

### G-5: Commit broadcast on `add_device_from_wire` to other pre-paired devices

Today's `runScanFlow` PUTs the Welcome to the joiner's
rendezvous mailbox but does NOT broadcast the commit to
the DeviceGroup DS journal for the other already-paired
devices. The first-pair case (founder + first joiner) is
unaffected — there's no other device to advance — but a
third pairing wouldn't ratchet the second device forward
until that device's `add_device_from_wire` happens to
serialise after.

Mitigation: each device's DeviceGroup poll loop already
processes commits from the journal; the post-add commit
just needs to be published. Marked `TODO(2a3b-followup)`
in every per-client `runScanFlow`.

### G-6: History bundle `payload_bytes` use a JSON placeholder, not raw MLS application-payload bytes

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

### G-7: DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations

The current flow is `add_device_from_wire` → publish
Welcome → publish bundle → persist snapshot. If the
process crashes between the publish and the persist, the
in-memory state has advanced but the on-disk envelope is
one epoch behind. On next launch, the load + a fresh
`addDeviceFromWire` would attempt to add the same device
twice. The DS journal's epoch check rejects the duplicate
gracefully, but the local snapshot would benefit from a
"persist before publish" reordering.

## Web client

### G-8: Web has no Service Worker-side message-log mutation listener

`ActiveGroupsPanel`'s unread badge is sourced from a
one-shot `loadMessageLog(...)` on mount. Live updates from
another tab / route don't refresh the badge until the
list re-mounts. iOS / Android use `@Published` /
`StateFlow` so the in-memory store stays in sync; web
needs an IDB `change` listener (or a `BroadcastChannel`-
backed pub/sub) to match.

### G-9: Web `payload_bytes` JSON placeholder strings are NOT byte-stable

JSON.stringify field ordering is not guaranteed across
JS engines. Cross-engine pairings (e.g. Chrome → Safari)
should still decode correctly via JSON.parse (which is
forgiving of field ordering), but a future sprint can
switch to a canonical encoding (RFC 8785 / JCS) for
byte-stable wire representations.

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

### G-11: No tamper-evident chain across backup versions

The server's "PUT replaces" semantics is the only
versioning. A maintainer-coerced server could roll the
envelope back to a prior known-good one. Phase 1.21 (or
later) can add a per-account monotonic sequence number +
client-side reject of older versions.

## Auth

### G-12: Session token validation is single-process in-memory

The auth service's bearer-token map is per-process.
Horizontal scaling (or a service restart) invalidates
every outstanding token. Phase 2c can add a signed-token
shape (JWT-ish but with our own crypto) so any auth
instance validates without coordination.

### G-13: WebAuthn passkey lacks per-credential authentic-action attestation

Today the passkey assertion validates the user has the
device; it doesn't bind to the specific OPAQUE message
the request carries. A reorder attack inside the same TLS
session is implausible but not impossible — Phase 2c can
mix the OPAQUE message hash into the WebAuthn assertion
challenge.

## Calls

### G-17: Frame-counter exhaustion path is documented but not test-exercised

`ParticipantKey::seal` honours the RFC 9605 §4.4 32-bit
counter bound and surfaces `Error::CounterExhausted` when
exhausted; the caller is expected to trigger a fresh MLS
commit to re-key. A dedicated test that drives the counter
to 2^32 is infeasible in CI (≈ 4 billion seal ops). The
property is reviewed by inspection;
`crates/lakko-calls/src/sframe.rs::MAX_FRAME_COUNTER` is
the canonical bound.

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

**Multi-joiner orchestration still pending** —
publishing subsequent joiners' KPs back to the owner for
add-member commits requires a separate "join-pending"
side table + a notification path from the discovery
service to the auth-flow on the owner side. Tracked as
the 3c3-multi-use follow-up sprint.

Operational impact: V1 link calls work for the 1:1
"share a link, person clicks, both join the call" UX
with the now-shipped server-side blob storage acting as
the durable indirection (URL no longer carries the
capability bytes). Multi-party link calls still fall
back to in-chat group calls (`GroupCallScope::InChat`).

### G-22: Per-joiner dedup for call invites lands with multi-use orchestration

**Atomic `max_uses` enforcement shipped in 3c3-server**
via `services/discovery/src/call_invite.rs::consume` —
the handler bumps `used_count` only when the row has
remaining budget (same CASE-expression sentinel mapping
the contact-invite consume handler uses: `0 → 1`,
`u32::MAX → unlimited`, otherwise N). Exhausted budgets
surface as `410 Gone`; expired rows fold into `404 Not
Found` to deny status-code probing.

**Per-joiner dedup still pending** — V1 doesn't have a
`call_invite_consumers` side table mirroring
`invite_consumers` (PK on `(blob_id, joiner_tag)`,
HMAC-blinded). Repeat consumes from the same account
count against the budget like distinct joiners do.
Acceptable for V1 because the call-invite lifecycle is
much shorter than a contact invite (~minutes vs ~days);
the dedup lands with 3c3-multi-use alongside the
KP-republish orchestration.

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
