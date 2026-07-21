<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Encrypted backup envelope

## Purpose

A device-loss recovery path that lets the user re-derive
their identity material + restore conversation context on a
fresh device, using only:

1. The 24-word BIP39 recovery phrase the user wrote down
   at signup; OR
2. K-of-N Shamir shares the user pre-distributed to
   guardians (Phase 1.21 social recovery; opt-in).

The server stores only the encrypted ciphertext + a
per-account index. The Argon2id KEK is recoverable ONLY
from the user-side phrase / Shamir shares; the server is
trustless on this path.

## Wire format

Defined in
[`crates/lakko-backup/src/payload.rs`](../../crates/lakko-backup/src/payload.rs).
Two versions in production:

| Version | Fields | Notes |
|---|---|---|
| v1 | identity_seed (64 B), contacts[] | Initial 2a1 release |
| **v2** | identity_seed, contacts[], **whole_storage_snapshot** | Phase 2a3d1 — adds the OpenMLS provider snapshot so multi-device recovery picks up MLS state too |

v2 readers parse v1 envelopes seamlessly — the
`whole_storage_snapshot` field folds to empty on a v1
input. The reverse is not symmetric: a v1 client reading a
v2 envelope sees the prefix only; the trailing
`whole_storage_snapshot` is dropped on decode.

## On-disk shape

```text
   ┌─────────────────────────────────────────────────────────┐
   │   Argon2id salt (16 B) │  IV (12 B)  │  KSF params (8)  │
   ├─────────────────────────────────────────────────────────┤
   │           ciphertext (BackupPayload proto, AEAD'd)      │
   ├─────────────────────────────────────────────────────────┤
   │                  AEAD tag (16 B)                        │
   └─────────────────────────────────────────────────────────┘
```

- Salt + KSF params are stored in the clear so a future
  parameter bump still recovers the same envelope.
- IV per-envelope random; ChaCha20-Poly1305 AEAD.
- KEK = `Argon2id(phrase, salt, mem=64MB, iter=3, par=1)`
  → 32 B AES key fed into the AEAD.

## Round-trip operations

### Build (signup / periodic baseline)

```text
client                                                 server
  │ encode BackupPayload (proto)                          │
  │ Argon2id(phrase, salt) → KEK                          │
  │ AEAD-seal(payload, KEK, IV)                           │
  │                                                       │
  │ PUT /backup/{accountIdB64}    body = envelope bytes   │
  │ ────────────────────────────────────────────────────► │
  │                              (replace prior envelope) │
```

The server overwrites any prior envelope on every
successful PUT. A future sprint adds versioning / history
if rolling back to a prior backup turns out to be a real
need.

### Tamper-evident chain (v2 envelopes — dormant; G-11 closed by ADR-0037)

[`crates/lakko-backup/src/chain.rs`](../../crates/lakko-backup/src/chain.rs)
adds an opt-in **v2 envelope shape** that carries a
plaintext `(version: u64, prev_hash: [u8; 32])` chain
header bound into the AEAD AAD. Each fresh seal:

1. Bumps `version` by 1.
2. Sets `prev_hash = BLAKE3(previous_envelope_wire)`,
   or the genesis sentinel `[0u8; 32]` for the very
   first envelope.

`verify_chain(envelopes: &[&[u8]])` walks an ordered
sequence and rejects any non-sequential version OR
broken `prev_hash` link. The header is plaintext on
wire so the verifier never needs the recovery phrase —
the version is a public count anyway, `prev_hash` is a
public commitment. The header is AAD-bound so a server
trying to edit either field hits a clean AEAD-open
failure on the next `open_v2`.

**What this defends against (in principle).** A client
that persisted `(last_known_version,
last_known_envelope_hash)` in its device-key-encrypted
local storage would catch any server-side rollback
(server returns v(N-5) instead of v(N)) on the next
restore: the returned version is too low OR the chain
audit fails against the local anchor.

**Status: dormant, superseded for rollback by
[ADR-0037](../adr/0037-backup-head-rollback-commitment.md).**
The v2 chain is exposed on no FFI surface, and no client
persists the anchor or calls `verify_chain`, so it never
closed the rollback gap on a deployed path. The rollback
defence that actually ships is the **auth-signed
backup-head commitment** (ADR-0037): an auth-assigned,
per-account, strictly-monotone `backup_version` a
returning client refuses to go below (a HARD floor — the
client-with-state case, closed) plus a signed `signed_at`
mint timestamp a true zero-anchor fresh install bounds
with a client-side staleness window (the fresh-install
case, bounded). Both cases are now closed **in practice**;
the residual is a within-window replay of a genuine old
head for a true zero-anchor fresh install only. The v2
chain stays dormant — its distinct sequence-audit
(forensics) role is a separate concern. Tracked in
[`known-gaps.md`](./known-gaps.md) G-11 (closed in
practice) + C-58 in the invariant index.

### Recover

```text
client (NEW device — no prior state)                    server
  │ GET /backup/{accountIdB64}                            │
  │ ────────────────────────────────────────────────────► │
  │ ◄────────────────────────────── envelope ciphertext   │
  │                                                       │
  │ Argon2id(phrase, salt) → KEK                          │
  │ AEAD-open(ciphertext, KEK, IV)  → BackupPayload       │
  │                                                       │
  │ rebuild Identity from identity_seed                   │
  │ restoreStorage(whole_storage_snapshot)                │
  │ upsert contacts[] into local store                    │
```

Failure surfaces on the AEAD-open step (wrong phrase →
tag mismatch). The recover UI explicitly disambiguates
this from a network failure so the user can retry the
phrase entry.

## Per-client API

The structured FFI surface is in
[`crates/lakko-ffi-uniffi/`](../../crates/lakko-ffi-uniffi/)
+ [`crates/lakko-ffi-wasm/`](../../crates/lakko-ffi-wasm/):

- `build_backup_payload(identity_seed, contacts, whole_storage_snapshot)
   -> BackupPayload`
- `seal_backup(envelope_proto, phrase) -> wire_bytes`
- `open_backup(wire_bytes, phrase) -> BackupPayload`

Per-client wrappers:

| Platform | Wrapper |
|---|---|
| Web | [`apps/web/src/auth/backup.ts`](../../apps/web/src/auth/) |
| iOS | [`apps/ios/Lakko/Auth/BackupClient.swift`](../../apps/ios/Lakko/Auth/) |
| Android | [`apps/android/.../auth/BackupClient.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/auth/) |

## Shamir social recovery (Phase 1.21)

`crates/lakko-shamir` ships GF(256) split + combine over
the recovery phrase's entropy (256 bits before BIP39
encoding). The K-of-N split lets the user distribute
shares to guardians; recovery combines K of them on the
client side to reconstruct the phrase, then drives the
standard recover flow.

Trust model:

- Each share is encrypted to the guardian's identity key
  (HPKE base mode) before being handed off.
- The server NEVER stores plaintext shares; the user's UI
  surfaces "send this share to your guardian via Signal /
  email / paper" and the guardian's app stores the
  decrypted share locally.
- An attacker with K-1 shares learns nothing about the
  reconstructed secret (Shamir's information-theoretic
  guarantee).

## Forget-device interaction

`forgetDevice` does NOT touch the server's backup envelope
— that's deliberate. The backup is the canonical
recovery path; nuking it on a single-device wipe would
leave the user with no path back if they meant
`logout`-with-clean-slate rather than
`unrecoverable-wipe`.

The recovery-phrase entry can be optionally cached on the
device under the device-root key (so the user can
"re-verify" without re-typing). That CACHE is wiped on
`forgetDevice`; the server-side envelope stays.

## Known soft spots

See [`known-gaps.md`](./known-gaps.md). Backup-specific:

- **Backup-envelope rotation on password change** —
  the recover path rotates the password but doesn't
  re-encrypt the backup under the new exportKey-derived
  material; the next baseline write (signup-time path,
  not the recover path itself) refreshes it. Reviewers
  should verify the recover → first-baseline window
  isn't longer than they're comfortable with.
- **No tamper-evident chain across backup versions** —
  a future sprint can add a per-account sequence number
  so a rollback to an older envelope surfaces in the UI.
  Today the server's "PUT replaces" semantics is the only
  versioning.
