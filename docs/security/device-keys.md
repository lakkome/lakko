<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Device key derivation chain

Every piece of at-rest material on the device sits behind a
hardware-backed wrap key. This document enumerates the
material, the wrap key it sits behind, and the on-disk
shape (envelope format + IV + tag).

## The chain at a glance

```text
   OPAQUE exportKey (64 B, in-memory only, never on disk)
          │
          │  HKDF-SHA-256
          ▼
   Device root key (32 B AES-GCM, non-extractable / Keystore / Secure Enclave)
          │
          │  AES-GCM (per-file IV + 16-B tag)
          ▼
   [ MLS provider snapshot ]        — every group's MLS state
   [ DeviceGroup snapshot ]         — multi-device MLS group state
   [ Persistent message log ]       — plaintext timeline (Phase 2a3f1)
   [ DeviceIdentity seed cache ]    — random per-device seed (ADR-0007)
   [ Export-key cache ]             — sealed exportKey + token (rehydrate path)
   [ Recovery phrase cache ]        — sealed BIP39 phrase if the user opted in
   [ Session token cache ]          — bearer token from auth
```

Plaintext is only ever in RAM (and only for the duration of
the operation that needs it). The unwrapped exportKey is
released as soon as the user's session ends; the device
root key is unwrapped lazily per operation that needs it.

## Per-platform anchors

### iOS / iPadOS / macOS

**Wrap key**: `SymmetricKey` in the Keychain under the
service `me.lakko.<purpose>-wrap.v1`, account `wrap-key`,
attr `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`.
The system manages a Secure-Enclave-bound key on devices
that have one; for those that don't (older / Mac Catalyst)
the key still has TEE-equivalent protection.

| Material | Wrap key alias | On-disk location |
|---|---|---|
| MLS provider snapshot | `me.lakko.mls-snapshot-wrap.v1` | `ApplicationSupport/lakko-mls-snapshots/<accountIdB64>.bin` |
| DeviceGroup snapshot | `me.lakko.device-group-snapshot-wrap.v1` | `ApplicationSupport/lakko-device-group-snapshots/<accountIdB64>.bin` |
| Message log (Phase 2a3f1) | `me.lakko.message-log-wrap.v1` | `ApplicationSupport/lakko-message-logs/<accountIdB64>.bin` |
| Export-key cache | (Keychain item, no separate wrap key) | Keychain `me.lakko.export-key.v1` |
| DeviceIdentity seed | (Keychain item, no separate wrap key) | Keychain `me.lakko.device-identity.v1` |
| Recovery phrase | (Keychain item, no separate wrap key) | Keychain `me.lakko.recovery-phrase.v1` |
| Session token | (Keychain item) | Keychain `me.lakko.session-token.v1` |

`URLResourceValues.isExcludedFromBackup = true` is set on
every snapshot directory + envelope file so iCloud backup
does NOT carry MLS state across devices (auto-restore
would diverge the recipient's view from every other
device's view the moment any of them commits).

### Android

**Wrap key**: AES-256-GCM key in the Android Keystore alias
`me.lakko.<purpose>-wrap.v1`. Block mode GCM, no padding,
no user-auth requirement, no StrongBox binding by default
(StrongBox's per-second AES throughput throttles
high-traffic stores like the message log — TEE protection
is the right trade-off).

| Material | Wrap key alias | On-disk location |
|---|---|---|
| MLS provider snapshot | `me.lakko.mls-snapshot-wrap.v1` | `noBackupFilesDir/lakko-mls-snapshots/<accountIdB64>.bin` |
| DeviceGroup snapshot | `me.lakko.device-group-snapshot-wrap.v1` | `noBackupFilesDir/lakko-device-group-snapshots/<accountIdB64>.bin` |
| Message log (Phase 2a3f1) | `me.lakko.message-log-wrap.v1` | `noBackupFilesDir/lakko-message-logs/<accountIdB64>.bin` |
| Device root key cache | `me.lakko.device-key-wrap.v1` | `noBackupFilesDir/lakko-device-keys/<accountIdB64>.bin` |
| Export-key cache | (`EncryptedSharedPreferences`, MasterKey-wrapped) | `lakko-export-key` SharedPreferences |
| DeviceIdentity seed | (`EncryptedSharedPreferences`) | `lakko-device-identity` SharedPreferences |
| Recovery phrase | (`EncryptedSharedPreferences`) | `lakko-recovery-phrase` SharedPreferences |
| Session token | (`EncryptedSharedPreferences`) | `lakko-session-token` SharedPreferences |

`noBackupFilesDir` ensures Google-cloud-backup does NOT
carry the material across devices (same divergence
rationale as iCloud).

### Web

**Wrap key**: non-extractable AES-256-GCM `CryptoKey` in
IndexedDB, derived from the OPAQUE exportKey via
`SubtleCrypto.deriveKey` + a per-account salt. The key
material never leaves the WebCrypto sandbox — there is no
`exportKey` step.

| Material | Wrap key | IDB store / row |
|---|---|---|
| MLS provider snapshot + known-group-ids | non-extractable device-root `CryptoKey` | `lakko-mls` / `snapshots` / `keyPath: accountId` |
| Message log (Phase 2a3f1) | same `CryptoKey` | `lakko-message-log` / `logs` / `keyPath: accountId` |
| Device root key cache | (the CryptoKey itself lives in IDB as a non-extractable blob) | `lakko-device-key` / `keys` / `keyPath: accountId` |
| DeviceGroup snapshot | same `CryptoKey` | `lakko-device-group-snapshots` / `snapshots` / `keyPath: accountId` |
| DeviceIdentity seed | same `CryptoKey` | `lakko-device-identity` / `seeds` / `keyPath: accountId` |
| Export-key cache | same `CryptoKey` | `lakko-export-key` / `keys` |
| Recovery phrase | same `CryptoKey` | `lakko-recovery-phrase` / `phrases` |
| Session token | same `CryptoKey` | `lakko-session-token` / `tokens` |

WebAuthn passkey credentials live in the user agent's
credential store — outside Lakko's reach. Lakko stores
only the credential id + nickname, never the public-key
metadata.

## On-disk envelope shape (all platforms)

```text
   ┌───────────────┬────────────────────────────────────┐
   │   12-byte IV  │         ciphertext + 16-byte tag   │
   └───────────────┴────────────────────────────────────┘
```

Per-call random IV; never re-used under the same wrap key.
The 16-byte trailing AEAD tag is mandatory and is the only
authentication signal — a tampered envelope surfaces as a
GCM-tag mismatch on open.

## Forget-device wipe contract

`forgetDevice` MUST clear every per-account at-rest
material listed above. Each platform's `AuthRepository`
+ `ChatRepository.forgetDevice` walks the list:

- iOS: `apps/ios/Lakko/Chat/ChatRepository.swift::forgetDevice`
  + `apps/ios/Lakko/Auth/AuthRepository.swift::forgetDevice`
- Android: `apps/android/.../chat/ChatRepository.kt::forgetDevice`
  + `apps/android/.../auth/AuthRepository.kt::forgetDevice`
- Web: `apps/web/src/auth/AuthContext.tsx::forgetDevice`

The Keystore / Keychain wrap keys themselves are LEFT
intact — they're shared across accounts (per-installation,
not per-account). Rotating them would invalidate every
surviving envelope.
