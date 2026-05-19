<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — MLS state machine

Lakko wraps [OpenMLS](https://github.com/openmls/openmls) ≥
0.8 with a thin Rust facade. The facade fixes the
ciphersuite, isolates upstream API churn, and exposes a
Lakko-specific operation surface. **The facade — not OpenMLS
itself — is what this audit pass covers.** Any
state-confusion / re-key bypass / downgrade bug in the
wrapper is in scope; equivalent bugs inside OpenMLS are
out of scope (OpenMLS gets its own external audit upstream).

## Crate layout

| Module | Responsibility |
|---|---|
| [`crates/lakko-mls/src/identity.rs`](../../crates/lakko-mls/src/identity.rs) | `Identity` — signing keypair + credential + per-device storage provider. `from_seed` builds a deterministic Identity from a 32-byte seed (HKDF → Ed25519). |
| [`crates/lakko-mls/src/group.rs`](../../crates/lakko-mls/src/group.rs) | `Group` — single MLS conversation group. `create` / `join_from_welcome` / `add_member` / `remove_member` / `create_application_message` / `process_incoming` / `export_secret`. |
| [`crates/lakko-mls/src/device_group.rs`](../../crates/lakko-mls/src/device_group.rs) | `DeviceGroup` — per-user multi-device MLS group (ADR-0007). Same operations as `Group` plus a deterministic `group_id_for(account_id)`. |
| [`crates/lakko-mls/src/key_package.rs`](../../crates/lakko-mls/src/key_package.rs) | `KeyPackage` creation + wire decoding with validation. |
| [`crates/lakko-mls/src/pairing.rs`](../../crates/lakko-mls/src/pairing.rs) | Pairing-QR codec (ADR-0007 §"Add-device"). |
| [`crates/lakko-mls/src/history_bundle.rs`](../../crates/lakko-mls/src/history_bundle.rs) | History catch-up bundle exporter + AEAD seal / open (Phase 2a3d8). |
| [`crates/lakko-mls/fuzz/`](../../crates/lakko-mls/fuzz/) | `cargo fuzz` harnesses — `mls_message` (wire parser), `key_package` (wire parser). |

## Invariants the wrapper MUST preserve

These are the load-bearing properties downstream code
relies on. Each cites the test(s) that validate it.

### I-1: A Welcome targeting a different `account_id` is rejected before any state advance

DeviceGroup uses `group_id = BLAKE3("lakko-device-group/v1"
\|\| account_id)`. The receiver re-derives the expected
`group_id` from its own `account_id` and rejects any inbound
Welcome whose embedded group_id doesn't match — BEFORE
calling `MlsGroup::process_message`.

Test: `device_group::tests::join_rejects_welcome_for_a_different_account`
in [`device_group.rs`](../../crates/lakko-mls/src/device_group.rs).

### I-2: `remove_device_at(own_leaf)` is rejected

DeviceGroup remove path pre-checks the caller's
`own_leaf_index()` and refuses to commit a self-remove —
that would render the local view inconsistent on
`merge_pending_commit`. The user removes their own device
via the `forgetDevice` path, not via Remove.

Test: `device_group::tests::remove_device_at_rejects_own_leaf`.

### I-3: `remove_device_at(unknown_leaf)` is rejected before OpenMLS sees it

Pre-check against the current member list. Bails with a
clean "no such device leaf N" error instead of letting
OpenMLS surface a less specific internal failure.

Test: `device_group::tests::remove_device_at_rejects_unknown_leaf`.

### I-4: After a successful `add_device_from_wire` / `join_from_welcome` pair, both devices' exporter secrets agree

The post-pairing exporter secret (label
`lakko/history-bundle/v1`) is the symmetric AEAD key the
history catch-up bundle (Phase 2a3d8) round-trips on. If
the two views were to derive different secrets, the
joiner couldn't decrypt the scanner's bundle.

Test: `history_bundle::tests::exporter_agrees_across_paired_devices`.

### I-5: Sealing a `HistoryBundle` larger than `MAX_BUNDLE_MESSAGES = 1000` is rejected

Defensive cap on the producer side — prevents an out-of-
budget bundle from saturating the rendezvous mailbox.

Test: `history_bundle::tests::seal_refuses_oversized_bundle`.

### I-6: Opening a `SealedBundle` with a wrong key surfaces as an authentication failure, not a parse failure

The AEAD layer (`lakko-crypto::aead::open`) returns
`Error::Aead` on tag mismatch. The history-bundle open
maps that to `Error::Message`, which the caller surfaces as
"history bundle apply failed" — distinct from
"unsupported algorithm".

Test: `history_bundle::tests::seal_open_rejects_wrong_key`.

### I-7: Opening a `SealedBundle` with an unknown `algorithm` discriminator is rejected before AEAD-open is attempted

Forward-compatibility hook. Older clients reading a
future-version bundle bail with `Error::Unsupported`
instead of attempting an open that would fail with a less
informative error.

Test: `history_bundle::tests::open_rejects_unknown_algorithm`.

### I-8: `create_devicegroup_message` round-trips through `parse_devicegroup_payload` losslessly for every typed body variant (`ReadCursor` / `Draft` / `Flags` / `Order`)

Phase 2a3e wire format invariant — receivers must
reconstruct the same Rust enum the sender encoded.

Test: `device_group::tests::devicegroup_payload_each_variant_roundtrips`.

## Exporter labels in use

Every exporter label Lakko coins is namespaced under
`lakko/...` so the global RFC-9420 label registry stays
free of collisions. Current set:

| Label | Length (bytes) | Source | Used for |
|---|---|---|---|
| `lakko/history-bundle/v1` | 32 | `lakko_mls::history_bundle::exporter_secret` | Per-pairing AEAD key for the history catch-up bundle (Phase 2a3d8) |
| `lakko/call/v1` | 32 | `lakko_calls::sframe::CallKey::derive` (Phase 3a4) | Per-call SFrame seed; per-participant keys derived via HKDF over `(seed, sender_leaf_be)` with info `lakko/sframe/v1/key` and nonce salt under `lakko/sframe/v1/nonce`. See [`calls.md`](./calls.md). |

A new label requires an ADR amendment + a test +
re-running this audit pack.

## State-flow diagrams

### Conversation group lifecycle

```text
   ┌─ Identity::generate / Identity::from_seed
   │                              │
   │                              ▼
   │           Identity::create_key_package ─────► published to discovery
   │                              │
   │                              ▼
   │       Group::create  ──or──  Group::join_from_welcome
   │              │                      │
   │              ▼                      ▼
   │      ──────  Group  ──────────────────────────────────┐
   │         │   │                                          │
   │         │   ├─ add_member(KeyPackage)                  │
   │         │   │     └─► (commit, welcome)                │
   │         │   │                                          │
   │         │   ├─ remove_member(LeafNodeIndex)            │
   │         │   │     └─► commit                           │
   │         │   │                                          │
   │         │   ├─ create_application_message              │
   │         │   │     └─► encrypted bytes                  │
   │         │   │                                          │
   │         │   └─ process_incoming(bytes) ────► Incoming  │
   │         │                                              │
   │         ▼                                              │
   │      export_secret(label, ctx, n) ◄────────────────────┘
   │
   └─ snapshot_storage() / restore_storage(blob)
```

### DeviceGroup lifecycle (ADR-0007)

```text
   first device ─► create_founder(account_id)
                              │
                              ▼
                     ┌────────────────┐
                     │   DeviceGroup  │ ◄── join_from_welcome (other device)
                     └────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
    add_device_from_wire  remove_device_at  create_devicegroup_message
        │                      │                     │
        └─► (commit,welcome)   └─► commit            └─► encrypted bytes
              │                      │                     │
              ▼                      ▼                     ▼
      published on DS         broadcast on DS        DS group journal
       mailbox + journal       group journal               │
                                                           ▼
                                                process_incoming →
                                                parse_devicegroup_payload →
                                                  (ReadCursor / Draft / Flags / Order)
```

## Known soft spots — explicitly out of scope, tracked for follow-up

See [`known-gaps.md`](./known-gaps.md) for the canonical list.
The MLS-side items currently parked there:

- **Channel two-tier flow** (>50k subscribers) — sender-key
  bootstrap + content-key rotation. The wire shape lands in
  Phase 4; not in this audit pass.
- **MLS-2 / quantum hybrid suites** — pinned for later
  upgrade once the IETF lands the hybrid cipher suite
  identifiers.
- **External-join proposals** — currently returned as a
  defensive `Incoming::ExternalJoinProposal` variant but
  no UI surface drives them. Phase 4 channel-subscriber
  flow lights this up.
