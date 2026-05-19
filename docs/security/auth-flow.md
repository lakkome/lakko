<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Authentication flow

OPAQUE-PAKE for everything; WebAuthn passkey opt-in lands
in 2a2 on top.

## OPAQUE-PAKE: signup → login → recover

### Server side

[`services/auth/`](../../services/auth/) — axum HTTP service.
Stores per-account:

- `account_id` (raw 16-byte UUIDv7) — the only stable handle.
  Username + email + phone NEVER touch the server table.
- OPAQUE envelope (≈ 192 bytes for `LakkoCipherSuite`) — the
  blinded password envelope. Server can't recover plaintext
  password from it.
- Argon2id KSF params (memory + iterations + parallelism) —
  cached per-account so a future password rotation matches.

### Client side

[`crates/lakko-auth-client/`](../../crates/lakko-auth-client/)
— `RegistrationSession` (signup) + `LoginSession` (login /
recover).

```text
SIGNUP
  client                                                  server
    │ generate registration_request (blinded password)       │
    │ ─────────────────────────────────────────────────────► │
    │                                          (verify PoW)  │
    │ ◄───────────────────────── registration_response       │
    │  (build envelope locally, sign it with OPAQUE state)   │
    │ ───────────────► registration_record + username        │
    │                                       (store envelope) │
    │ ◄────────────────────────────── { account_id, token }  │
    │                                                        │
    │  client now has: account_id (16 B), exportKey (64 B)   │
    │                                                        │
    │  exportKey ─► Identity::from_seed                      │
    │  exportKey ─► HKDF ─► device root key                  │
```

```text
LOGIN
  client                                                  server
    │ generate ke1 (blinded password)                        │
    │ ─────────────────── { account_id, ke1, PoW token }  ─► │
    │                                                        │
    │ ◄────────────────────── { ke2 (server's stage 2) }     │
    │                                                        │
    │ finalize OPAQUE — derive sharedKey + exportKey         │
    │ ──────────────────────────────── { ke3, finished } ──► │
    │                                                        │
    │ ◄────────────────────────────── { token, expires_at }  │
```

```text
RECOVER  — same as signup but keyed off the 24-word BIP39
phrase the user wrote down at signup. The phrase derives a
"recovery exportKey" used to authenticate the password
rotation. Old exportKey is invalidated by the rotation; a
fresh device-root key is derived from the new exportKey.
```

## Per-client wrappers

| Platform | Client wrapper | Auth state surface |
|---|---|---|
| Web | [`apps/web/src/auth/AuthContext.tsx`](../../apps/web/src/auth/AuthContext.tsx) | Solid context: `state` (loading / anon / session) + `identitySecrets` (in-memory exportKey + token) |
| iOS | [`apps/ios/Lakko/Auth/AuthRepository.swift`](../../apps/ios/Lakko/Auth/AuthRepository.swift) | `@MainActor ObservableObject` with `@Published` `state` + `identitySecrets` |
| Android | [`apps/android/.../auth/AuthRepository.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/auth/AuthRepository.kt) | Plain class with `StateFlow<AuthState>` + `StateFlow<IdentitySecrets?>` |

## In-memory `IdentitySecrets`

The exportKey is the seed for every downstream derivation
(MLS Identity, device root key, contacts-pseudonym salt).
It MUST not survive a process restart — the user re-enters
the password to recover it. On every platform the in-memory
holder:

- Holds `accountId: bytes` (16) + `exportKey: bytes` (64) +
  `sessionToken: string` + `sessionExpiresAtMs: i64`.
- Is wiped synchronously on logout / forgetDevice.
- Is NOT serialised to any local store. The MLS provider
  snapshot persists derived state; the exportKey itself
  doesn't.

## WebAuthn passkey opt-in (Phase 2a2)

`webauthn-rs` 0.5 on the server side
([`services/auth/`](../../services/auth/)). Per-client
Credential Manager / `ASAuthorizationController` /
`navigator.credentials` integration. Passkey acts as an
**alternative second factor** for login + recover; it does
NOT replace the password (the exportKey still seeds the
identity material).

Per-account state added by passkeys:

| Field | Purpose |
|---|---|
| `passkey_credentials[]` | list of registered FIDO2 credential ids + public keys |
| `passkey_nicknames[]` | user-set labels for the registered passkeys |

The passkey assertion attests a **valid login intent**;
the OPAQUE round-trip stays in the protocol on the same
endpoint with a `passkey_assertion` field on the request.
Server validates the FIDO2 assertion + the OPAQUE message
in the same turn. See
[`crates/lakko-ffi-uniffi/src/lib.rs`](../../crates/lakko-ffi-uniffi/src/lib.rs)
and the platform-specific
`WebauthnClient` wrappers.

## Session token

A bearer token issued by `auth` on successful signup /
login / recover. Carried in the `Authorization: Bearer <…>`
header for any **protected** endpoint:

- `discovery` (OPRF lookups)
- `push-gw` (per-account subscriptions)

The DS service is anonymous — append-only journals keyed on
opaque pseudonyms; no auth check on `/ds/group/.../messages`
or `/ds/mailbox/.../messages`. PoW + Privacy Pass tokens
gate write spam.

Token shape: 32 random bytes, base64url-encoded. Validated
against an in-memory map on the auth service; future
sprints will harden against horizontal scaling.

## Password rotation invariants

When the user runs `/recover` (or a future
"change-password" flow), the exportKey rotates. This
invalidates:

1. The MLS snapshot envelope — encrypted under the OLD
   device root key (HKDF of OLD exportKey). The recover
   path explicitly deletes the snapshot row to avoid a
   stale-cipher footgun. The receiving peers re-bootstrap
   via fresh KeyPackages.
2. The AccountIdentity — deterministic from the exportKey.
   A future "stale credential" path can re-attest the old
   identity for ≤ 30 days via a KT-log supersession proof
   (ADR-0007 §6 known issue), but today the recover flow
   simply replaces the identity.
3. The on-server OPAQUE envelope — rotated as part of the
   recover round-trip.

The DeviceIdentity seed (per-device random) is **NOT**
rotated on `/recover` — it lives behind the local
DeviceIdentityStore and is independent of the password.
Only `forgetDevice` wipes it.
