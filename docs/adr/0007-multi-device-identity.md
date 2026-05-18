<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0007: Two-tier identity for multi-device

- **Status:** Accepted
- **Date:** 2026-05-18
- **Context:** Phase 2a3 — multi-device sync via DeviceGroup substrate

## Context

Phase 1 ships with a single deterministic `Identity` per account: every device that runs `Identity::fromSeed(exportKey)` against the OPAQUE-derived export key reproduces **byte-identical** Ed25519 + X25519 keypairs. This was the right choice for single-device flows — recovery works without server-side device state, and the `accountIdB64 → signature_public_key` map on discovery is one-to-one and trivially deterministic.

Phase 2a3 needs multi-device support. The DeviceGroup substrate (ADR-0001, [§3 of the plan](../../README.md)) is an MLS group whose **leaves are the user's own devices**. RFC 9420 §5.3 requires every leaf's `signature_key` to be unique within the group; adding two leaves with the same key fails validation. The deterministic-from-exportKey Identity collides with that requirement: a second device joining via `Identity::fromSeed(same_exportKey)` would produce the same signature key as the first leaf, and MLS rejects the Welcome.

The same friction surfaces in conversation groups once they hold multiple leaves per user (a user texting from two devices into the same chat). Each device needs its own leaf so the per-leaf private state — init key, encryption key, leaf decryption history — stays scoped to the device that owns it. Cross-device leakage of the init private bytes via a shared deterministic Identity defeats the point of per-leaf forward secrecy.

We need a multi-device-friendly identity model that:

1. Lets each device own a distinct signature key for MLS leaves.
2. Preserves the deterministic-from-exportKey property for account-level operations that must agree across devices (publishing the identity bundle on discovery, KT log entries, recovery proofs).
3. Survives recovery / forget-device on a single device without disrupting the other devices.

## Decision

**Two-tier identity model:**

```
                ┌────────────────────────────────────────┐
                │  Account-level identity                │
                │  Identity::fromSeed(exportKey)         │
                │  deterministic, shared across devices  │
                └────────────────────────────────────────┘
                              ▲
                              │ signs (Phase 2b+)
                              │ - identity bundle on discovery
                              │ - KT log inclusion proofs
                              │ - recovery / supersession proofs
                              │
        ┌─────────────────────┴────────────────────────────┐
        │                                                  │
   ┌────┴─────────────────┐                  ┌─────────────┴────────┐
   │  Device-A identity   │                  │  Device-B identity   │
   │  Identity::generate()│                  │  Identity::generate()│
   │  random per device   │                  │  random per device   │
   │  ── leaf in MLS ──   │                  │  ── leaf in MLS ──   │
   │     DeviceGroup       │                  │     DeviceGroup       │
   │     conv groups       │                  │     conv groups       │
   └──────────────────────┘                  └──────────────────────┘
```

### Account-level identity (`AccountIdentity`)

- Built via `Identity::fromSeed(exportKey)`. Already shipped in Phase 1.
- Deterministic — every device that knows the password (or recovery phrase + new password) reconstructs the same Ed25519 + X25519 keypair.
- **NOT** used as an MLS leaf in any group. Phase 1's "one leaf per user" usage of the account identity stays valid for groups created before this ADR; new groups (Phase 2b+) use a per-device leaf.
- Used for signing things the **account as a whole** asserts:
  - Identity bundle on `services/discovery` (one bundle per account).
  - Key Transparency log entries.
  - Recovery / supersession proofs on identity rotation.
  - Account-scoped ACL signatures.

### Per-device identity (`DeviceIdentity`)

- Built via `Identity::generate()`. Already shipped (used by the Phase 2a3 unit tests).
- **Random** per device — generated once on first launch of that device for that account, persisted locally encrypted under the device root key, never leaves the device.
- Used as the **MLS leaf** in:
  - The user's `DeviceGroup` (2a3a) — one leaf per device.
  - Every conversation group the user joins from Phase 2b onwards — one leaf per (account, device) pair in each conversation.
- Lifecycle:
  - Created on first signup / login / recover on the device.
  - Persisted in a new `device-identity-store` (web: IDB AES-GCM under device root key; iOS: Keychain; Android: EncryptedSharedPreferences — same pattern the existing export-key store uses).
  - Wiped on `forgetDevice`.
  - **Not** rotated on password change — the device identity is decoupled from OPAQUE.

### Binding the two

The DeviceGroup is what binds per-device identities back to the account:

1. A device's `DeviceIdentity.signature_public_key` is published as a leaf in the user's `DeviceGroup`.
2. The `DeviceGroup` itself sits under a deterministic `group_id = BLAKE3("lakko-device-group/v1" || account_id)` (2a3a).
3. The account-level identity's published bundle on `services/discovery` carries the `DeviceGroup.group_id` plus the current `DeviceGroup` epoch's exporter-derived "account fingerprint". Peers verify that an inbound conversation-group leaf's signature key is a current member of the sender's DeviceGroup — that is the "this leaf belongs to this account" check (Phase 2b's per-device-leaf conversation support fills this in).

## Alternatives

### Alt-1: Stay with single deterministic identity, no per-device leaves

Pros: smaller code change, no new key-storage layer per platform.

Cons:
- MLS rejects two leaves with the same signature key — DeviceGroup can never have more than one device, defeating the whole 2a3 effort.
- Cross-device leak of leaf-private state (every device reconstructs the same init private bytes from the same exportKey) defeats per-leaf forward secrecy.
- No way to surgically remove one device without rotating exportKey — `forgetDevice` becomes "rotate password globally".

Rejected: cannot support multi-device at all.

### Alt-2: Per-device identity deterministic from `(exportKey, device-uuid)`

Each device generates a random `device-uuid` on first launch, derives its DeviceIdentity via `HKDF(exportKey, "lakko/device-identity/v1", device-uuid)`.

Pros: still recoverable from server-stored `device-uuid` list — no per-device random key to back up.
Cons:
- `device-uuid` is per-device random, so the recovery scenario is no better than Alt-3 (we still need to back up *something* per device, just a UUID instead of a private key).
- HKDF of a shared root (`exportKey`) keeps the cross-device-leakage threat partially alive — compromise of `exportKey` still gives an attacker every device's leaf private key (now via re-derivation rather than direct readout, but practically identical).
- More moving parts than Alt-3 with no real win.

Rejected: doesn't actually decouple device leaks from account leaks.

### Alt-3: Per-device random `Identity::generate()` (chosen)

See Decision above.

Trade-offs:
- A device whose local storage is wiped (no `forgetDevice` first) can't recover its prior leaf — that is OK, the user re-pairs the device via the QR add-device flow (2a3b) which lands a *new* leaf.
- Per-device private keys do NOT appear in the encrypted backup envelope (2a1) — they are local-only by design. Adding them would create the same cross-device-leak surface that Alt-1 has.

## Consequences

- **Positive:**
  - DeviceGroup leaves are cryptographically distinct → multi-device actually works.
  - Compromise of one device's local storage scopes leaf private keys to that device — the others are untouched.
  - `forgetDevice` surgically removes a single leaf via MLS `Remove` without rotating the account.
  - Recovery flow stays simple: recover from phrase → re-derive AccountIdentity (deterministic) → land on a new device with a freshly-generated DeviceIdentity → re-pair via add-device or run as a single-device DeviceGroup again.

- **Negative:**
  - New per-platform `device-identity-store` layer (small — same shape as `export-key-store` from 2a2c2). Three platform implementations.
  - Backups don't carry DeviceIdentity keys — a device that wipes its local storage (without `forgetDevice` first) is unrecoverable. The user must re-pair via QR; this is by design (per-device forward secrecy) and matches Signal / Element / Matrix.
  - Existing Phase 1 conversation groups built with the deterministic AccountIdentity as the leaf stay valid forever, but never gain multi-device leaves. Phase 2b+ groups use per-device leaves from the start. This is acceptable — Phase 1 was single-device by design; the upgrade path for a Phase 1 user is "log in on a second device, accept the offered DeviceGroup pairing, future conversations are multi-device-aware."

- **Neutral:**
  - The discovery identity bundle continues to publish AccountIdentity public keys (Phase 1 contract preserved). The DeviceGroup epoch + group_id fields it carries are additive — old peers ignore them.

## Open questions

- **Per-device-leaf conversation groups (Phase 2b)** — how peers verify a leaf belongs to the sender's DeviceGroup. Sketch: every conversation group's `epoch_authenticator` exporter binds the per-device leaf to the sender's published DeviceGroup epoch; peers cross-check on every Commit. ADR-0008 will pin this once the wire shape lands.
- **Multi-account on one device** — out of scope here, but the per-platform store layer is keyed by `accountIdB64` so the data model already tolerates it.
- **PRF-extension WebAuthn** — orthogonal: if landed (2a2c3?) the export key gains a passkey-derived alternative path. Doesn't affect DeviceIdentity.

## References

- ADR-0001 — MLS for everything
- ADR-0002 — Rust core + native UI
- Phase 2a3 sub-sprints in `docs/ROADMAP.md`
- RFC 9420 §5.3 — leaf node uniqueness
- Signal Protocol's per-device "device ID" model
- Matrix Olm/Megolm's per-device identity keys
