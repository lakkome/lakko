// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * TypeScript-side mirror of the JSON shapes consumed and produced
 * by `lakko-svc-auth`.
 *
 * The Rust originals live in `services/auth/src/{opaque,pow}.rs`
 * and `crates/lakko-auth-client/src/wire.rs`. We mirror them by
 * hand here for two reasons: the Rust definitions are
 * `pub(crate)`, and TypeScript on the web side has no way to
 * import Rust types without a heavy proto pipeline. When a field
 * is added on the server, mirror it here in the same commit.
 *
 * Conventions:
 *   * Every byte string crosses the wire base64-encoded
 *     (standard alphabet, with padding) — same as the server.
 *   * Snake_case field names in the JSON match the server's
 *     `serde` defaults; we keep the same on the TypeScript side
 *     so we can pass these objects straight to `JSON.stringify`.
 */

import type { SessionPairWire } from "./session-pair";

// ----------------------------------------------------------------------------
// Proof of work (services/auth/src/pow.rs) - signup start only
// ----------------------------------------------------------------------------

/** Body of `POST /pow/challenge` (auth, signup only). The endpoint takes
 *  no input. */
export interface PowChallengeResponse {
  /** base64(seed) — 16 random bytes the server will recognise on
   *  the matching expensive request. */
  seed: string;
  /** Number of leading zero bits required on
   *  `SHA-256(seed || nonce || solution)`. */
  effort_bits: number;
  /** Seconds until the challenge row expires (consumed-on-use). */
  expires_in: number;
}

/** Embedded `pow` object. Since ADR-0054 S4 only `/signup/start`
 *  carries one. All three fields are base64-encoded. */
export interface PowProofPayload {
  seed: string;
  nonce: string;
  solution: string;
}

// ----------------------------------------------------------------------------
// Signup (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/** `POST /signup/start` request body. */
export interface SignupStartRequest {
  pow: PowProofPayload;
  /** base64(account_id) — raw 16-byte UUIDv7 the client picked. */
  account_id: string;
  /** base64(RegistrationRequest) from the password OPAQUE flow. */
  password_request: string;
  /** Optional base64(RegistrationRequest) from the BIP39 recovery
   *  flow. Recommended on signup; client SHOULD always enrol a
   *  recovery mnemonic. Omitted entirely when absent. */
  recovery_request?: string;
}

/** `POST /signup/start` response body. */
export interface SignupStartResponse {
  password_response: string;
  recovery_response?: string;
}

/** `POST /signup/finish` request body. */
export interface SignupFinishRequest {
  account_id: string;
  password_upload: string;
  recovery_upload?: string;
}

/** `POST /signup/finish` response body. ADR-0065: every session landing
 *  carries the session pair (`SessionPairWire`) flattened in. */
export interface SignupFinishResponse extends SessionPairWire {
  ok: boolean;
  /** The fresh-auth grant (the password was typed just now). */
  fresh_grant?: string;
  fresh_grant_expires_at_ms?: number;
}

// ----------------------------------------------------------------------------
// Login (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/** Shared by `/login/start` and `/recovery/login/start`. */
export interface LoginStartRequest {
  /** ADR-0054 S5: the login handle, base64url (no pad) of the 32 bytes
   *  `loginHandle(username)` returns. Never an account id. */
  handle: string;
  /** base64(CredentialRequest). */
  request: string;
}

/** Shared by `/login/start` and `/recovery/login/start`. */
export interface LoginStartResponse {
  /** UUIDv7 (string form) the server assigned to this login attempt. */
  session_id: string;
  /** base64(CredentialResponse). */
  response: string;
}

/** Shared by `/login/finish` and `/recovery/login/finish`. */
export interface LoginFinishRequest {
  session_id: string;
  /** base64(CredentialFinalization). */
  finalize: string;
  /** ADR-0066 section 0.4: this device's id (URL-safe base64, no padding,
   *  32 bytes: its conversation leaf signature key), when the device
   *  identity already exists. Absent = the session names no device and is
   *  bound afterwards (`POST /auth/session/bind-device`). */
  device_id_b64?: string;
}

/** Shared by `/login/finish` and `/recovery/login/finish`. */
export interface LoginFinishResponse extends SessionPairWire {
  ok: boolean;
  /** base64(account_id) — echoed back so the client can pin it. */
  account_id: string;
  /** ADR-0065 (amended 2026-10-06): the fresh-auth grant, an opaque
   *  string. `/login/finish` (the PASSWORD login), `/webauthn/auth/finish`
   *  and `/signup/finish` return one; the recovery login does not. Read
   *  through `freshGrantOf`. */
  fresh_grant?: string;
  /** Absolute expiry of `fresh_grant`, unix milliseconds. */
  fresh_grant_expires_at_ms?: number;
}

// ----------------------------------------------------------------------------
// Password reset (services/auth/src/opaque.rs — 1.16c3c3a)
// ----------------------------------------------------------------------------

/**
 * `POST /password/reset/start` request body. The user is rotating
 * their password by proving they know the BIP39 recovery
 * mnemonic; both OPAQUE flows (recovery login + new password
 * registration) start in this single round trip.
 */
export interface PasswordResetStartRequest {
  /** ADR-0054 S5: the login handle, base64url (no pad) of the 32 bytes
   *  `loginHandle(username)` returns. Never an account id. */
  handle: string;
  /** base64(CredentialRequest) from `OpaqueLogin.startRecovery(seed)`. */
  recovery_request: string;
  /** base64(RegistrationRequest) from `OpaqueRegister.startPassword(newPassword)`. */
  new_password_request: string;
}

/** `POST /password/reset/start` response body. */
export interface PasswordResetStartResponse {
  /** UUIDv7 — used to bind the matching `/password/reset/finish`. */
  session_id: string;
  /** base64(CredentialResponse) — feed to `OpaqueLogin.finish`. */
  recovery_response: string;
  /** base64(RegistrationResponse) — feed to `OpaqueRegister.finish`. */
  new_password_response: string;
}

/** `POST /password/reset/finish` request body. */
export interface PasswordResetFinishRequest {
  session_id: string;
  /** base64(CredentialFinalization) — proves recovery ownership. */
  recovery_finalize: string;
  /** base64(RegistrationUpload) — the new password envelope. */
  new_password_upload: string;
  /** ADR-0066 section 0.4, as on `LoginFinishRequest`. */
  device_id_b64?: string;
}

/** `POST /password/reset/finish` response body. A password reset does NOT
 *  bump the key generation (ADR-0065). */
export interface PasswordResetFinishResponse extends SessionPairWire {
  ok: boolean;
  /** base64(account_id) — echoed back, same shape as `LoginFinishResponse`. */
  account_id: string;
}

// ----------------------------------------------------------------------------
// Device authorization (services/auth/src/device_authorize.rs — ADR-0018
// factor (a), "another-device" recovery)
// ----------------------------------------------------------------------------

/** `POST /device/authorize/start` response body. */
export interface DeviceAuthorizeStartResponse {
  /** URL-safe base64 (no pad) of the RAW 32-byte single-use grant
   *  token — the only time it leaves the server (the server stores
   *  only its SHA-256). The existing device seals it to the fresh
   *  device over the E2EE pairing channel. */
  grant_token_b64: string;
}

/** `POST /device/authorize/redeem` request body. Anonymous (the
 *  fresh device has no token yet) (no proof of work since ADR-0054 S4). */
export interface DeviceAuthorizeRedeemRequest {
  /** URL-safe base64 (no pad) of the raw 32-byte grant token
   *  received over the rendezvous channel. */
  grant_token_b64: string;
  /** ADR-0066 section 0.4, as on `LoginFinishRequest`. */
  device_id_b64?: string;
}

/** `POST /device/authorize/redeem` response body — the account id plus the
 *  session pair every login flow returns. */
export interface DeviceAuthorizeRedeemResponse extends SessionPairWire {
  /** URL-safe base64 (no pad) of the 16-byte account id: where a fresh
   *  device first learns it (ADR-0056). */
  account_id_b64: string;
}

// ----------------------------------------------------------------------------
// TOTP (services/auth/src/totp.rs — ADR-0018 factor (c),
// authenticator-code recovery)
// ----------------------------------------------------------------------------

/** `POST /totp/enroll/start` response body — the provisioning
 *  material the authenticator app consumes. The raw secret leaves
 *  the server exactly once, here. */
export interface TotpEnrollStartResponse {
  /** `otpauth://totp/...` provisioning URI for the QR code. */
  otpauth_uri: string;
  /** RFC 4648 base32 (no padding) of the raw secret, for manual
   *  entry into the authenticator app. */
  secret_b32: string;
}

/** `POST /totp/enroll/finish` request body. Bearer-auth. */
export interface TotpEnrollFinishRequest {
  /** The first 6-digit code the authenticator produced, proving it
   *  holds the secret. */
  code: string;
}

/** `POST /totp/recover` request body. Anonymous (the caller lost
 *  their device) (no proof of work since ADR-0054 S4). */
export interface TotpRecoverRequest {
  /** ADR-0054 S5: the login handle, base64url (no pad) of the 32 bytes
   *  `loginHandle(username)` returns. Never an account id. */
  handle: string;
  /** Current 6-digit authenticator code. */
  code: string;
  /** ADR-0066 section 0.4, as on `LoginFinishRequest`. */
  device_id_b64?: string;
}

/** `POST /totp/recover` response body — the account id plus the session
 *  pair every other login flow returns. */
export interface TotpRecoverResponse extends SessionPairWire {
  /** URL-safe base64 (no pad) of the 16-byte account id: where a fresh
   *  device first learns it (ADR-0056). */
  account_id_b64: string;
}

// ----------------------------------------------------------------------------
// Session refresh + the key reset (ADR-0065; services/auth/src/session.rs and
// services/auth/src/key_reset.rs)
// ----------------------------------------------------------------------------

/** `POST /auth/session/refresh` request body. Anonymous: the refresh
 *  credential is the authorisation. */
export interface SessionRefreshRequest {
  /** The `r1.` refresh credential. */
  refresh_token: string;
}

/** `POST /auth/session/refresh` response body: a new pair (new access token,
 *  new sliding refresh credential). `401 {"error":"key-generation-superseded"}`
 *  when the keys were reset elsewhere; `401 {"error":"refresh-invalid"}` for a
 *  bad, expired or unknown credential. */
export type SessionRefreshResponse = SessionPairWire;

/** `POST /account/keys/reset/start` request body. Bearer. Built exactly as
 *  signup builds its recovery half, from the NEW phrase seed. */
export interface KeyResetStartRequest {
  /** The fresh-auth grant (`fresh-auth.ts`). Without a usable one the
   *  server answers `403 {"error":"fresh-auth-required"}`. */
  fresh_grant: string;
  /** base64(RegistrationRequest) from `OpaqueRegister.startRecovery(newSeed)`. */
  recovery_request: string;
}

/** `POST /account/keys/reset/start` response body. */
export interface KeyResetStartResponse {
  /** base64(RegistrationResponse) — feed to `OpaqueRegister.finish`. */
  recovery_response: string;
}

/** `POST /account/keys/reset/finish` request body. Bearer. One transaction
 *  server-side: bumps the generation, replaces the recovery credential,
 *  deletes every passkey, vault, action grant, device grant and device
 *  request of the account, mints the attestation and issues the new pair. */
export interface KeyResetFinishRequest {
  /** The same grant the start carried; a successful finish spends every
   *  grant of the old generation. */
  fresh_grant: string;
  /** base64(RegistrationUpload) for the new recovery seed. */
  recovery_upload: string;
  /** URL-safe base64 (no pad) of the NEW account identity's 32-byte Ed25519
   *  signature public key: the key the reset installs, bound into the
   *  attestation. */
  new_signature_pubkey_b64: string;
}

/** `POST /account/keys/reset/finish` response body: the attestation plus the
 *  session pair at the new generation (the caller's old pair is superseded
 *  too; install this one at once). */
export interface KeyResetFinishResponse extends SessionPairWire {
  /** URL-safe base64 (no pad) of the 177-byte key-reset attestation the
   *  client carries to discovery (the identity upload) and to the DS (the
   *  DeviceGroup journal reset). */
  key_reset_attest_b64: string;
}

/** `POST /account/keys/rotate/finish` request body (ADR-0066 section 3.4):
 *  the reset's, plus the proof and the devices to revoke. */
export interface KeyRotateFinishRequest extends KeyResetFinishRequest {
  /** URL-safe base64 (no pad) of the 145-byte supersession proof signed by
   *  the OLD account key for the new one. */
  supersession_proof_b64: string;
  /** URL-safe base64 (no pad) device ids, at most 8; may be empty. */
  revoke_device_ids_b64: string[];
}

/** `POST /auth/session/refresh` with the session-upgrade statement. */
export interface SessionRefreshUpgradeRequest {
  refresh_token: string;
  /** URL-safe base64 (no pad) of the kind-4 account statement over
   *  `BLAKE3(refresh_token)`, signed by the account's CURRENT key. */
  key_statement_b64: string;
}

// ----------------------------------------------------------------------------
// /account/recall/{status,start,finish}   (ADR-0066 part 1, "T3 as built")
// ----------------------------------------------------------------------------
//
// Anonymous: the old device's session is superseded. `*_b64` fields are
// URL-safe base64 without padding; the OPAQUE messages are standard base64.

/** `POST /account/recall/status` request body. */
export interface RecallStatusRequest {
  /** The 138-byte `recall-status` account statement, signed by the OLD
   *  account key. */
  statement_b64: string;
}

/** `POST /account/recall/start` request body. */
export interface RecallStartRequest {
  account_id_b64: string;
  anchor_seq: number;
  /** The `recall-start` statement over the anchor seq and `login_request`. */
  statement_b64: string;
  /** `OpaqueLogin.startPassword(oldPassword).requestB64`. */
  login_request: string;
  /** `OpaqueRegister.startPassword(newPassword).requestB64`. */
  password_request: string;
  /** `OpaqueRegister.startRecovery(newSeed).requestB64`. */
  recovery_request: string;
}

/** `POST /account/recall/start` response body. */
export interface RecallStartResponse {
  session_id: string;
  login_response: string;
  password_response: string;
  recovery_response: string;
  /** 32 bytes, URL-safe base64 without padding. */
  server_nonce_b64: string;
}

/** `POST /account/recall/finish` request body. */
export interface RecallFinishRequest {
  session_id: string;
  account_id_b64: string;
  anchor_seq: number;
  login_finish: string;
  password_upload: string;
  recovery_upload: string;
  new_signature_pubkey_b64: string;
  device_id_b64?: string;
  /** The `recall` statement over the nonce, the new key and both uploads. */
  statement_b64: string;
}

/** `POST /account/recall/finish` response body: the key reset's answer plus
 *  a fresh-auth grant. */
export interface RecallFinishResponse extends KeyResetFinishResponse {
  fresh_grant?: unknown;
  fresh_grant_expires_at_ms?: unknown;
}

// ----------------------------------------------------------------------------
// /auth/genesis/attest   (ADR-0036, G-30 finding 2 — auth-attested genesis)
// ----------------------------------------------------------------------------

/** `POST /auth/genesis/attest` request body. Bearer-authenticated: the
 *  live session token (on the `Authorization` header, not the body) is
 *  proof the caller OPAQUE-authenticated as `account_id`, which is the
 *  whole security basis — auth then binds THAT account to the
 *  client-supplied genesis pubkey. */
export interface GenesisAttestRequest {
  /** URL-safe base64 (no pad) of the client's 32-byte Ed25519 genesis
   *  identity pubkey. */
  pubkey_b64: string;
}

/** `POST /auth/genesis/attest` response body. */
export interface GenesisAttestResponse {
  /** URL-safe base64 (no pad) of the signed 153-byte `GenesisAttestation`
   *  (`version ‖ account_id ‖ username_hash ‖ pubkey ‖ expires_at ‖ sig`).
   *  The client uploads it verbatim alongside the identity bundle; a peer
   *  verifies it on a first observation against the pinned attest key. */
  genesis_attest_b64: string;
}

// ----------------------------------------------------------------------------
// The passkey vault — ADR-0064 decision 6 (services/auth/src/passkey_vault.rs).
// `credential_id_b64` is base64url without padding; `sealed_entropy_b64` is
// standard base64 with padding. Every `/vault/passkey*` route answers 503
// when the server has no vault key.
// ----------------------------------------------------------------------------

/** `PUT /vault/passkey` request body. */
export interface PasskeyVaultPutRequest {
  /** URL-safe base64 (no pad) of the WebAuthn credential id. */
  credential_id_b64: string;
  /** The 16-byte phrase entropy sealed under the credential's PRF output
   *  (44 bytes sealed). */
  sealed_entropy_b64: string;
}

/** `POST /vault/passkey/get` and `DELETE /vault/passkey` request body. */
export interface PasskeyVaultGetRequest {
  credential_id_b64: string;
}

/** `POST /vault/passkey/get` response body. */
export interface PasskeyVaultGetResponse {
  sealed_entropy_b64: string | null;
}

/** `DELETE /vault/passkey` response body. */
export interface PasskeyVaultDeleteResponse {
  deleted: boolean;
}

/** `GET /vault/passkeys` response body: the credential ids (base64url, no
 *  pad) that have keys stored for them. */
export interface PasskeyVaultListResponse {
  credential_ids_b64: string[];
}

// ----------------------------------------------------------------------------
// Device requests — ADR-0049 section 2 as amended 2026-09-28, the device link
// (services/auth/src/device_request.rs). The request id is base64url without
// padding (it rides in bearer-route paths); every exchange value is standard
// base64. The server relays these values and cannot open any of them.
// ----------------------------------------------------------------------------

/** `POST /device/request` request body. Anonymous. */
export interface DeviceRequestCreateRequest {
  /** base64url (no pad) of the 32-byte login handle of the typed username
   *  (ADR-0056). Never an account id. */
  handle: string;
}

/** `POST /device/request` response body. */
export interface DeviceRequestCreateResponse {
  request_id_b64: string;
  expires_in_secs: number;
}

/** `POST /device/request/status` request / response bodies. */
export interface DeviceRequestStatusRequest {
  request_id_b64: string;
}
export interface DeviceRequestStatusResponse {
  /** `false` after a decline or expiry (indistinguishable by design). */
  pending: boolean;
  pake_b_b64: string | null;
  outcome: DeviceRequestOutcome | null;
  confirm_b_b64: string | null;
}

/** The old device's decision on a request. */
export type DeviceRequestOutcome = "accepted" | "wrong_code";

/** `POST /device/request/payload` request body. Anonymous. */
export interface DeviceRequestPayloadRequest {
  request_id_b64: string;
  pake_a_b64: string;
  sealed_payload_b64: string;
  confirm_a_b64: string;
}

/** `GET /device/requests` response body. */
export interface DeviceRequestsListResponse {
  request: {
    request_id_b64: string;
    created_at_ms: number;
    pake_b_b64: string | null;
    pake_a_b64: string | null;
    sealed_payload_b64: string | null;
    confirm_a_b64: string | null;
    outcome: DeviceRequestOutcome | null;
  } | null;
}

/** `POST /device/requests/{id}/pake` request body. Bearer. */
export interface DeviceRequestAnswerRequest {
  /** The fresh-auth grant (`fresh-auth.ts`); `403 fresh-auth-required`
   *  without a usable one. */
  fresh_grant: string;
  pake_b_b64: string;
}

/** `POST /device/requests/{id}/result` request body. Bearer. `confirm_b_b64`
 *  goes with `accepted` and never with `wrong_code`. */
export type DeviceRequestResultRequest =
  | { fresh_grant: string; outcome: "accepted"; confirm_b_b64: string }
  | { fresh_grant: string; outcome: "wrong_code" };

// ----------------------------------------------------------------------------
// /auth/login-handle   (ADR-0054 S5, sign in by name)
// ----------------------------------------------------------------------------

/** `PUT /auth/login-handle` request body. Bearer-authenticated; binds the
 *  caller's account to the handle (one per account, replacing the previous
 *  one). `409 {"error":"handle-taken"}` when another account holds it. */
export interface LoginHandlePutRequest {
  handle: string;
}

/** `PUT /auth/login-handle` response body. */
export interface LoginHandlePutResponse {
  bound: boolean;
}

/** `DELETE /auth/login-handle` response body. */
export interface LoginHandleDeleteResponse {
  removed: boolean;
}

/** `POST /auth/session/bind-device` request body (ADR-0066 section 0.4, T1
 *  point 8). Bearer. The refresh credential must be the bearer's own. */
export interface SessionBindDeviceRequest {
  device_id_b64: string;
  refresh_token: string;
}

/** `POST /auth/session/bind-device` response: a pair naming the device. */
export type SessionBindDeviceResponse = SessionPairWire;

/** `POST /account/devices/revoke` request body (ADR-0066 section 2.1).
 *  Bearer. `statement_b64` is the 138-byte `device-revoke` account
 *  statement, URL-safe base64 without padding. Answers `204`. */
export interface DeviceRevokeRequest {
  fresh_grant: string;
  device_id_b64: string;
  statement_b64: string;
}
