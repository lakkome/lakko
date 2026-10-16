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
RECOVER  — same as signup but keyed off the 12-word BIP39
phrase the user wrote down at signup (24 words until
ADR-0072, 2026-10-02). The phrase derives a
"recovery exportKey" used to authenticate the password
rotation. Old exportKey is invalidated by the rotation; a
fresh device-root key is derived from the new exportKey.
```

### Recovery after ADR-0072 (2026-10-02)

[ADR-0072](../adr/0072-recovery-model-simplification.md) is the record;
this is the part that bears on the auth service. Built in `services/auth`
and on web, unit-tested, not walked live; iOS and Android in progress.

- **The recovery credential** is a second OPAQUE registration per account
  (`KIND_RECOVERY`) made from the 64-byte seed of the recovery phrase. It
  authorises `/recovery/login/{start,finish}` (a session, password
  untouched) and `/password/reset/{start,finish}` (replaces the password).
- **Three words** of the phrase are checked on a device that already holds
  the phrase (`lakko_mnemonic::word_check`); on a match that device runs the
  recovery login with the seed it holds. The server stores nothing about
  the three words and cannot tell this sign-in from any other recovery
  login. The 5-miss / 15-minute budget is the client's own and is not a
  server control.
- **After a fresh identity** (ADR-0017 `mintFreshIdentity`, which the
  interface calls "Reset account keys") the recovery credential is replaced
  **inside the key reset** (next section): the standalone
  `POST /recovery/credential/{start,finish}` routes of ADR-0072 decision 9
  were deleted by ADR-0073 on 2026-10-03.
- **Removed:** the phone / email one-time code (ADR-0018 factor (d)) and the
  server-side word vault (ADR-0049 section 1). The server-issued access
  factors are another device, a passkey and TOTP.
- **Passkey vault:** an opt-in opaque blob per passkey (the 16-byte phrase
  entropy sealed under a key derived from that passkey's PRF output, which
  never reaches the server), `PUT /vault/passkey`, `POST /vault/passkey/get`,
  `DELETE /vault/passkey`, `GET /vault/passkeys`, all bearer.

### Reset account keys (ADR-0073, phase 1 built 2026-10-03)

[ADR-0073](../adr/0073-account-key-reset.md) is the record; its "What the
build changed / found" section lists the deviations. Built in `services/auth`,
`services/discovery`, `services/ds` and on web, iOS and Android; DB-gated
and unit tests green (auth 120 + 2, discovery 107, DS 85; the three client
suites); **not walked live**, no passkey ceremony executed; the
`lakko-security` pass has not run.

- **The key generation** is `accounts.identity_version` (default 1 = genesis;
  the column keeps its name, the code calls it `key_generation`). Generation
  `g` names the credentials and devices that belong to the account after its
  `(g-1)`-th reset; every session credential, the published identity bundle
  and the DeviceGroup journal record the generation they belong to, and the
  party that records it refuses a lower one.
- **`POST /account/keys/reset/start {recovery_request}`** (bearer, stateless:
  the OPAQUE registration start under `KIND_RECOVERY` for the NEW phrase's
  seed) and **`POST /account/keys/reset/finish {recovery_upload,
  new_signature_pubkey_b64}`** -> `{key_reset_attest_b64, key_generation,
  session_token, session_expires_at_ms, refresh_token, refresh_expires_at_ms}`,
  one transaction in this order: bump the generation and read it in one
  statement (`LAST_INSERT_ID`); delete the in-flight recovery-login and
  password-reset states and upsert `KIND_RECOVERY`; delete `passkey_vaults`,
  then `webauthn_credentials`, then `webauthn_states` + `webauthn_action_grants`;
  delete `device_grants` + `device_requests`; mint the `lakko-attest`
  **key-reset attestation** (177 bytes: `account_ref` = a one-way
  BLAKE3-derive-key of the account id, the DeviceGroup journal id, `g`, the
  new pubkey, `signed_at`; the shared attest key, its own domain); issue the
  caller a fresh pair at `g`. **Kept:** the password envelope and the TOTP
  secret (ADR-0073 open question 1). **No ADR-0014 action grant**, even with a
  passkey on the account. Limits: `Scope::RecoveryReplace` (10 per hour) on
  both calls, `Scope::KeyReset` (3 per day) on `finish`.
- **What the attestation buys**, stated plainly: a binding of the
  OPAQUE-authenticated account to a client-asserted key at generation `g`,
  not a proof the key is right (the ADR-0036 reading). `services/discovery`
  requires it (or a supersession proof) before the published key may change
  and deletes every KeyPackage row of the account with it; the unattested
  "heal" back to the previous key is gone (C-71). `services/ds` requires it
  before a stranger may re-root the account's DeviceGroup journal
  (`PUT /ds/group/{hex}/reset` + `key_reset_attest_b64`; the chain, registrars
  and push binding dropped; `LAKKO_DS_ATTEST_KEY`). Neither checks freshness
  (a retried upload may be days old; an applied generation cannot be applied
  again); the client bounds its own owed attestation at 30 days.
- **Residuals** (G-46): a stolen session can reset - loud, not silent, since
  every other device shows the signed-out screen within the access TTL; the
  password and TOTP survive; `DELETE /discovery/keypackage/device/{id}` is
  not under discovery's generation floor; the other discovery writes and
  storage's `PUT /backup` stay open to a superseded token for the access TTL.
  The old identity's conversation leaves survive until a contact who
  administers each group evicts them (phase 2, in code since 2026-10-04;
  G-45 for an owned, a table-less or an admin-less-contact group).

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

- Holds `accountId: bytes` (16) + `exportKey: bytes` (64) + the
  ADR-0017 `accountIdentitySeed` (64) + the ADR-0073 session pair
  (`session`: the access token and its expiry, the refresh credential
  and its expiry, the key generation; `sessionToken` /
  `sessionExpiresAtMs` are views onto it on iOS and Android).
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

## Session credentials (ADR-0073 section 3, 2026-10-03)

Every session-landing route of `auth` (`/signup/finish`, `/login/finish`,
`/recovery/login/finish`, `/password/reset/finish`, `/webauthn/auth/finish`,
`/totp/recover`, `/device/authorize/redeem`, and `/account/keys/reset/finish`)
answers the same flattened **session pair**, issued at the account's current
key generation (`crates/lakko-auth-token`, `services/auth/src/session.rs`):

- **Access token** `v2.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag>`,
  HMAC-SHA256 under the shared `LAKKO_SESSION_TOKEN_SECRET`, carried as
  `Authorization: Bearer <...>` to every protected route of `auth`
  (WebAuthn management, `/auth/genesis/attest`, `/auth/backup/head`, `/auth/tokens`,
  `/auth/login-handle`, the key reset), `discovery`, `storage` (`/backup`) and
  `gif-proxy`. `/call/jwt` is not among them since ADR-0078 section 2
  (2026-10-16): it spends an anonymous `call-join` token instead, and the
  TURN credential rides its answer (`/call/turn-credential` is gone).
  Default TTL **900 s** (`LAKKO_SESSION_TOKEN_TTL_SECS`; chosen,
  not measured). The pre-ADR-0073 four-part `v1` token is refused (flag day,
  no users).
- **Refresh credential** `r1.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag>`,
  tagged under `BLAKE3-derive-key("lakko/session-refresh/v1", secret)` so an
  access verifier refuses it by version and could never mistake its tag.
  Accepted by anonymous **`POST /auth/session/refresh {refresh_token}`** only
  (per-address limited): same generation -> a fresh pair (the credential
  slides to a fresh 30 days, `LAKKO_REFRESH_TOKEN_TTL_DAYS`); lower ->
  `401 {"error":"key-generation-superseded"}`; bad / expired / unknown /
  no account -> `401 {"error":"refresh-invalid"}` (one body, so a deleted
  account is indistinguishable from a wrong credential). No random id, no
  database row: revoked by the generation, never individually.
- **Revocation.** `auth`'s bearer middleware (`webauthn::require_session`)
  reads `accounts.identity_version` on every request and refuses a lower
  generation with the same JSON `401 key-generation-superseded` (every other
  failure stays the plain bodiless `401`). `discovery`, `storage` and
  `gif-proxy` verify statelessly, so their revocation latency is the access
  TTL; discovery's two identity writes additionally apply the floor its own
  row holds. A log-out or a per-device removal still revokes nothing
  server-side (ADR-0050 section 2.10; `removeDeviceAt` calls no auth route):
  only a key reset supersedes sessions, and it supersedes all of them.
- **Clients** hold one `SessionTokenProvider`: refresh when 120 s or less of
  the access token remain, one refresh in flight per account, callers queue;
  any bearer route answering `401` earns one refresh and one retry; a
  `key-generation-superseded` from the refresh route, an auth bearer route or
  discovery's identity writes stops every writer, shows the signed-out screen
  and runs the full forget-device wipe without publishing anything;
  `refresh-invalid` lands on the sign-in screen with nothing wiped. On web
  the provider is the `sessionFetch` wrapper every client calls; on iOS and
  Android it is a transport-seam interceptor (`AntiCensorshipTransport.perform`
  / `SessionBearerEngine`) that substitutes the live token on the wire. The
  pair is persisted where the session token always was (web IndexedDB under
  the device key, iOS Keychain, Android `EncryptedSharedPreferences`) and
  wiped with it.

The DS never sees a session credential: its journals are gated by the
ADR-0053 per-epoch proofs (and the ADR-0057 ownership statements), its
knocks by ADR-0054 anonymous tokens; push-gw likewise (anonymous tokens);
channels by owner signatures (ADR-0059 section 9).

## Call-admission JWT (Phase 3b4)

Distinct from the session token above. The session token
is the long-lived (30-day default) handle to "this account
is logged in here"; the call JWT is a 1-hour scoped
credential the client hands to the SFU per call.

### Mint round-trip

```text
client                                        services/auth
  │
  │── POST /call/jwt
  │   Authorization: Bearer <session-token>
  │   { call_id: "<32 hex>", role: "participant" }
  │                                                │
  │                          require_session       │
  │                          middleware extracts   │
  │                          account_id from the   │
  │                          session token         │
  │                                                │
  │                          mint_jwt(...)         │
  │                          - sub = account_id    │
  │                          - call_id from body   │
  │                          - role from body      │
  │                          - iat = now           │
  │                          - exp = iat + 3600    │
  │                                                │
  │◄─────────────────────────────────── 200 OK
  │     { jwt: "<JWS Compact>",
  │       expires_at_unix_secs: <iat + 3600> }
  │
  │── (later, opening a call)
  │   GET <sfu>/admit
  │   Authorization: Bearer <jwt>
  │                                                │
  │                                              SFU verifies
  │                                              the JWT against
  │                                              the public key
  │                                              cached from
  │                                              /.well-known/
  │                                              jwks.json
```

### Crypto

- **Signing alg**: EdDSA over Ed25519 (RFC 8037).
- **Signing key**: persisted in `call_jwt_signing_key.seed`
  (migration 0007). Single-row table; generated on first
  boot via `INSERT IGNORE` (race-safe across concurrent
  startups).
- **Public key**: published at `GET /.well-known/jwks.json`
  as an OKP/Ed25519 JWK per RFC 8037. SFU fetches at
  startup + refreshes every 24 h per ADR-0008.
- **`kid`**: `hex(BLAKE3(public_key_bytes)[..8])` — 16
  hex chars; stable across restarts, bumps only on
  rotation.
- **Wire shape**: JWS Compact `header.payload.signature`
  (RFC 7515 §3.1), hand-rolled — no `jsonwebtoken`
  dep. Sign/verify ride `lakko_crypto::signing`.

### Claim set

```text
iss     "lakko-auth"                  // pinned constant
aud     "lakko-sfu"                   // pinned constant
sub     <account_id_hex>              // 16-byte UUIDv7, lower hex
call_id <call_id_hex>                 // 16-byte random, lower hex
role    "participant" | "publisher" | "subscriber"
iat     <unix seconds>
exp     <iat + 3600>                  // 1 hour
```

`account_id` is NEVER taken from the request body — the
middleware stamps it into request extensions from the
session token. The body carries only `call_id` (caller's
choice) and `role` (defaults to `participant`).

The SFU is **assumed-compromised** (per
[`threat-model.md`](./threat-model.md)). The JWT
signature is the only proof of admission; a leaked JWT
is bounded by the 1-hour `exp`.

See [`calls.md`](./calls.md) §"Call-admission JWT
invariants" for the full C-15..C-23 invariant list +
cited tests.

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
