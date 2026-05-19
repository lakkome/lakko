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
