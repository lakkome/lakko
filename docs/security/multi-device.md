<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Multi-device

The multi-device story rides ADR-0007's two-tier identity
+ the DeviceGroup MLS substrate.

## Two-tier identity (ADR-0007)

| Identity | Source | Lifetime | Used as |
|---|---|---|---|
| **AccountIdentity** | Deterministic from OPAQUE exportKey via `Identity::from_seed(seed)` | Across every device on the account — same exportKey → same identity | MLS leaf in every **conversation** group |
| **DeviceIdentity** | Random per-device, cached via `DeviceIdentityStore.ensureDeviceIdentitySeed` | Per device, wiped on `forgetDevice`; survives `logout` | MLS leaf in the user's **DeviceGroup** |

Rationale: RFC 9420 §5.3 requires every leaf in a single
MLS group to have a distinct signature key. If we tried to
join the same AccountIdentity to a DeviceGroup as multiple
leaves we'd collide. Two-tier breaks the tie: the
AccountIdentity is shared across devices (one leaf per
conversation per device-via-DeviceGroup), the DeviceIdentity
is per-device (one leaf per device in the DeviceGroup).

## DeviceGroup

A per-user MLS group whose leaves are the user's own
devices. Deterministic `group_id`:

```text
   group_id = BLAKE3("lakko-device-group/v1" || account_id)
```

Why deterministic:

1. A paired device joining via a Welcome can verify that
   the embedded `group_id` matches the expected value for
   its own `account_id`. A misrouted Welcome from a
   different user's DeviceGroup is rejected at join time,
   before any state advance (`mls-state-machine.md` I-1).
2. The post-recovery flow on a fresh device knows the
   `group_id` to ask the DS for catch-up Commits before
   any other device has had a chance to send the new
   device a Welcome.

## Pairing flow

```text
   ┌─ joining device (new) ──────────────────────────────────────┐
   │                                                             │
   │  ensureDeviceIdentitySeed → seed (32 B random)              │
   │  Identity::from_seed(seed) → DeviceIdentity                 │
   │  Identity::create_key_package → KeyPackage                  │
   │                                                             │
   │  generate rendezvous pseudonym (32 B random)                │
   │  build PairingQr { account_id, key_package, rendezvous }    │
   │  render QR                                                  │
   │  poll DS /ds/mailbox/{rendezvous_hex} ──┐                   │
   └─────────────────────────────────────────┼───────────────────┘
                                             │
   ┌─ scanner device (paired) ───────────────┼───────────────────┐
   │                                         │                   │
   │  scan QR, parse PairingQr               │                   │
   │  ensureDeviceGroup → DeviceGroup        │                   │
   │  addDeviceFromWire(KeyPackage)          │                   │
   │       → (commit, welcome)               │                   │
   │  PUT /ds/mailbox/{rendezvous_hex} ──────┘                   │
   │       (publishes Welcome)                                   │
   │                                                             │
   │  derive history_bundle_secret(identity)                     │
   │  read local message log → build HistoryBundle               │
   │  seal under history_bundle_secret                           │
   │  PUT /ds/mailbox/{rendezvous_hex} (publishes Bundle)        │
   │                                                             │
   │  persistDeviceGroupSnapshot                                 │
   └─────────────────────────────────────────────────────────────┘

   ┌─ joining device (resumes the poll loop) ────────────────────┐
   │                                                             │
   │  fetched message #1 = Welcome bytes                         │
   │  DeviceGroup::join_from_welcome (verifies group_id)         │
   │  persistDeviceGroupSnapshot                                 │
   │                                                             │
   │  poll /ds/mailbox/{rendezvous_hex}?after=1 (10s soft)        │
   │  fetched message #2 = SealedBundle bytes                    │
   │  derive history_bundle_secret(identity) — same value        │
   │  open_history_bundle → project rows into MessageLogStore    │
   └─────────────────────────────────────────────────────────────┘
```

Key security properties:

- **Server sees only**: opaque rendezvous pseudonym +
  opaque Welcome bytes + opaque sealed bundle bytes. No
  knowledge of which user the slot belongs to or what's
  inside.
- **Exporter agreement**: both devices reach the same
  post-pair epoch and derive the same 32 B secret via
  `lakko/history-bundle/v1`
  (`mls-state-machine.md` I-4).
- **Misrouted Welcome**: deterministic `group_id` check at
  the joiner's `join_from_welcome` rejects any Welcome
  that wasn't minted for this account
  (`mls-state-machine.md` I-1).

## Remove-device flow (Phase 2a3c)

The "Your devices" list on every client lets the user
revoke a device's leaf via MLS Remove:

```text
   1. User taps "Remove" on a non-self row
   2. DeviceGroup::remove_device_at(leaf_index) — pre-checks
      (not own leaf, leaf exists), then OpenMLS produces a
      Remove commit
   3. Commit bytes broadcast via DsClient.publishGroup on
      the DeviceGroup's DS journal
   4. Other paired devices' DeviceGroup poll loops ingest
      the commit on their next iteration, ratchet past the
      revoked leaf
   5. The revoked device cannot decrypt anything new (no
      longer a group member); its on-disk MLS snapshot
      becomes useless — the local user can forgetDevice on
      it to wipe the orphan material
```

Invariants — see `mls-state-machine.md` I-2 and I-3.

## Cross-device sync (Phase 2a3e2)

The DeviceGroup application-message channel carries four
typed payloads:

| Payload | Wire schema | Use |
|---|---|---|
| `ReadCursor` | per-thread last-read pointer | Drives unread-count UI on sibling devices |
| `Draft` | composer text per thread | Drafts survive cross-device tab/app switches |
| `ThreadFlags` | pin / mute / archive / unread-override | UI affordances on the chat list |
| `ConversationOrder` | explicit display order | Custom thread ordering |

Wire format: `schemas/mls-app/devicegroup_v1.proto`. Each
payload carries an HLC + sender_leaf; receivers
last-write-wins by HLC for the matching keyed slot.

Per-client implementations:

- Web — `apps/web/src/devices/sync/`
- iOS — `apps/ios/Lakko/Devices/Sync/`
- Android — `apps/android/.../devices/sync/`

The cross-device runtime drains the DeviceGroup DS journal
on a background poll loop (mirror of the conversation-group
polling pattern but keyed on the deterministic
DeviceGroup `group_id`), dispatches incoming typed
payloads through `parse_devicegroup_payload`, and
publishes outgoing typed payloads via
`create_devicegroup_message`.

## History catch-up bundle (Phase 2a3d8)

Lifetime constraint addressed: MLS forward secrecy
intentionally wipes chain keys after each epoch ratchet,
so a freshly-paired device cannot re-decrypt the wire
bytes of pre-pair messages. The history bundle fills the
gap by carrying the **already-decrypted plaintext** from
the inviter's local message log, sealed under a per-pair
exporter secret.

See [`crates/lakko-mls/src/history_bundle.rs`](../../crates/lakko-mls/src/history_bundle.rs)
+ the per-client `history-bundle.{ts,swift,kt}` wrappers.

Cap: `MAX_BUNDLE_MESSAGES = 1000` total across every
group in a single bundle. Producers MUST refuse to seal
beyond the cap; receivers MUST treat an oversized open
result as an error.

`payload_bytes` per row carry an opaque JSON projection
of the runtime message shape today — see the deferred
item in [`known-gaps.md`](./known-gaps.md) for the
follow-up to switch to raw MLS application-payload bytes
once the message log starts retaining them.
