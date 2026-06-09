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

// ----------------------------------------------------------------------------
// PoW (services/auth/src/pow.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /pow/challenge`. The endpoint takes no input. */
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

/** Embedded `pow` object on every expensive endpoint. All three
 *  fields are base64-encoded 16-byte values. */
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
  pow: PowProofPayload;
  account_id: string;
  password_upload: string;
  recovery_upload?: string;
}

/** `POST /signup/finish` response body. */
export interface SignupFinishResponse {
  ok: boolean;
  /** Phase 1.5b: HMAC-SHA256 bearer token. Stash in the
   *  `AuthContext` then thread through every protected
   *  `lakko-svc-discovery` write via the `Authorization: Bearer …`
   *  header. */
  session_token: string;
  /** Token expiry as a unix-millisecond timestamp. */
  session_expires_at_ms: number;
}

// ----------------------------------------------------------------------------
// Login (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/** Shared by `/login/start` and `/recovery/login/start`. */
export interface LoginStartRequest {
  pow: PowProofPayload;
  account_id: string;
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

/** Shared by `/login/finish` and `/recovery/login/finish`. No `pow`
 *  field — the server only requires PoW on the start endpoints. */
export interface LoginFinishRequest {
  session_id: string;
  /** base64(CredentialFinalization). */
  finalize: string;
}

/** Shared by `/login/finish` and `/recovery/login/finish`. */
export interface LoginFinishResponse {
  ok: boolean;
  /** base64(account_id) — echoed back so the client can pin it. */
  account_id: string;
  /** Phase 1.5b: HMAC-SHA256 bearer token. */
  session_token: string;
  /** Token expiry as a unix-millisecond timestamp. */
  session_expires_at_ms: number;
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
  pow: PowProofPayload;
  /** base64(account_id). */
  account_id: string;
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

/** `POST /password/reset/finish` request body. No `pow` field —
 *  the server only requires PoW on the start endpoints, by analogy
 *  with `/login/{start,finish}`. */
export interface PasswordResetFinishRequest {
  session_id: string;
  /** base64(CredentialFinalization) — proves recovery ownership. */
  recovery_finalize: string;
  /** base64(RegistrationUpload) — the new password envelope. */
  new_password_upload: string;
}

/** `POST /password/reset/finish` response body. */
export interface PasswordResetFinishResponse {
  ok: boolean;
  /** base64(account_id) — echoed back, same shape as `LoginFinishResponse`. */
  account_id: string;
  /** Phase 1.5b: fresh bearer token for the rotated password. */
  session_token: string;
  /** Token expiry as a unix-millisecond timestamp. */
  session_expires_at_ms: number;
}

// ----------------------------------------------------------------------------
// Device authorization (services/auth/src/device_authorize.rs — ADR-0018
// factor (a), "another-device" recovery)
// ----------------------------------------------------------------------------

/** `POST /device/authorize/start` request body. Bearer-authenticated
 *  (the EXISTING device's session token rides the `Authorization`
 *  header, not the body) + PoW. */
export interface DeviceAuthorizeStartRequest {
  pow: PowProofPayload;
}

/** `POST /device/authorize/start` response body. */
export interface DeviceAuthorizeStartResponse {
  /** URL-safe base64 (no pad) of the RAW 32-byte single-use grant
   *  token — the only time it leaves the server (the server stores
   *  only its SHA-256). The existing device seals it to the fresh
   *  device over the E2EE pairing channel. */
  grant_token_b64: string;
}

/** `POST /device/authorize/redeem` request body. Anonymous (the
 *  fresh device has no token yet) + PoW. */
export interface DeviceAuthorizeRedeemRequest {
  pow: PowProofPayload;
  /** URL-safe base64 (no pad) of the 16-byte account_id the fresh
   *  device learned from the pairing QR. */
  account_id_b64: string;
  /** URL-safe base64 (no pad) of the raw 32-byte grant token
   *  received over the rendezvous channel. */
  grant_token_b64: string;
}

/** `POST /device/authorize/redeem` response body — the same
 *  `(account_id, session_token, expiry)` triple every login flow
 *  returns. */
export interface DeviceAuthorizeRedeemResponse {
  /** URL-safe base64 (no pad) of the 16-byte account id (echoed). */
  account_id_b64: string;
  session_token: string;
  session_expires_at_ms: number;
}
