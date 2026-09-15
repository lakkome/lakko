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

// ----------------------------------------------------------------------------
// TOTP (services/auth/src/totp.rs — ADR-0018 factor (c),
// authenticator-code recovery)
// ----------------------------------------------------------------------------

/** `POST /totp/enroll/start` request body. Bearer-authenticated (the
 *  already-logged-in account's session token rides the
 *  `Authorization` header) + PoW. */
export interface TotpEnrollStartRequest {
  pow: PowProofPayload;
}

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

/** `POST /totp/enroll/finish` request body. Bearer-auth, NO `pow`
 *  — the server does not gate finish. */
export interface TotpEnrollFinishRequest {
  /** The first 6-digit code the authenticator produced, proving it
   *  holds the secret. */
  code: string;
}

/** `POST /totp/recover` request body. Anonymous (the caller lost
 *  their device) + PoW. */
export interface TotpRecoverRequest {
  pow: PowProofPayload;
  /** URL-safe base64 (no pad) of the 16-byte account_id. */
  account_id_b64: string;
  /** Current 6-digit authenticator code. */
  code: string;
}

/** `POST /totp/recover` response body — the same `(account_id,
 *  session_token, expiry)` triple every other login flow returns. */
export interface TotpRecoverResponse {
  /** URL-safe base64 (no pad) of the 16-byte account id (echoed). */
  account_id_b64: string;
  session_token: string;
  session_expires_at_ms: number;
}

// ----------------------------------------------------------------------------
// Recovery contact — phone / email OTP (services/auth/src/recovery_contact.rs
// + otp_sender.rs — ADR-0018 factor (d), one-time-code recovery)
// ----------------------------------------------------------------------------

/** Wire-format `kind` discriminator for a recovery contact, matching the
 *  server's `ContactKind::as_db` (`services/auth/src/otp_sender.rs`). These
 *  integer values are wire-format stable: never renumber. */
export const ContactKind = {
  /** Code delivered over email (SMTP). */
  Email: 1,
  /** Code delivered over SMS. */
  Phone: 2,
} as const;

/** One of the [`ContactKind`] integer discriminators (`1 = email`,
 *  `2 = phone`). */
export type ContactKind = (typeof ContactKind)[keyof typeof ContactKind];

/** `POST /recovery-contact/enroll/start` request body. Bearer-authenticated
 *  (the already-logged-in account's session token rides the `Authorization`
 *  header) + PoW. */
export interface RecoveryContactEnrollStartRequest {
  pow: PowProofPayload;
  /** `1 = email`, `2 = phone` (see [`ContactKind`]). */
  kind: ContactKind;
  /** The raw phone / email plaintext. Used transiently server-side to hash +
   *  address the message; never stored, never logged (invariant #1). */
  contact: string;
}

/** `POST /recovery-contact/enroll/start` response body. Generic "a code was
 *  sent" marker. */
export interface RecoveryContactSentResponse {
  sent: boolean;
}

/** `POST /recovery-contact/enroll/finish` request body. Bearer-auth, NO `pow`
 *  — the server does not gate finish. */
export interface RecoveryContactEnrollFinishRequest {
  kind: ContactKind;
  /** The 6-digit code that was delivered to the contact. */
  code: string;
}

/** `POST /recovery-contact/recover/start` request body. Anonymous (the caller
 *  lost their device) + PoW. ENUMERATION-SAFE: the server returns the same
 *  `{ sent: true }` whether or not the contact matches a confirmed row. */
export interface RecoveryContactRecoverStartRequest {
  pow: PowProofPayload;
  /** URL-safe base64 (no pad) of the 16-byte account_id. */
  account_id_b64: string;
  kind: ContactKind;
  /** The raw phone / email plaintext. */
  contact: string;
}

/** `POST /recovery-contact/recover/finish` request body. Anonymous + PoW. */
export interface RecoveryContactRecoverFinishRequest {
  pow: PowProofPayload;
  /** URL-safe base64 (no pad) of the 16-byte account_id. */
  account_id_b64: string;
  kind: ContactKind;
  /** The 6-digit code that was delivered to the contact. */
  code: string;
}

/** `POST /recovery-contact/recover/finish` response body — the same
 *  `(account_id, session_token, expiry)` triple every other login flow
 *  returns. */
export interface RecoveryContactRecoverFinishResponse {
  /** URL-safe base64 (no pad) of the 16-byte account id (echoed). */
  account_id_b64: string;
  session_token: string;
  session_expires_at_ms: number;
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
// Word vault — ADR-0049 (services/auth/src/word_vault.rs)
// ----------------------------------------------------------------------------

/** `POST /vault/enroll` request body. Every field is what the wasm
 *  `wordVaultEnroll` produced; the server stores them and can open none of
 *  them. Byte fields are standard base64. */
export interface WordVaultEnrollRequest {
  /** Three distinct 1-based word positions, ascending. */
  positions: number[];
  salt_b64: string;
  argon2_m_kib: number;
  argon2_t: number;
  argon2_p: number;
  /** `SHA-256(K_verify)`, 32 bytes. */
  verifier_b64: string;
  /** The 32-byte entropy sealed under `K_seal`. */
  sealed_entropy_b64: string;
}

/** `GET /vault` response body. */
export interface WordVaultStatusResponse {
  enrolled: boolean;
  positions: number[] | null;
}

/** `POST /vault/recover/start` request body. Anonymous + PoW. */
export interface WordVaultRecoverStartRequest {
  pow: PowProofPayload;
  /** URL-safe base64 (no pad) of the 16-byte account_id. */
  account_id_b64: string;
}

/** `POST /vault/recover/start` response body — identical in shape for a
 *  real account and an unknown one (decoys). */
export interface WordVaultRecoverStartResponse {
  positions: number[];
  salt_b64: string;
  argon2_m_kib: number;
  argon2_t: number;
  argon2_p: number;
}

/** `POST /vault/recover/finish` request body. Anonymous + PoW. */
export interface WordVaultRecoverFinishRequest {
  pow: PowProofPayload;
  account_id_b64: string;
  /** The guess: `SHA-256(K_verify)` for the typed words, 32 bytes. */
  verifier_b64: string;
}

/** `POST /vault/recover/finish` response body on a match. */
export interface WordVaultRecoverFinishResponse {
  sealed_entropy_b64: string;
}

/** `PUT /vault/passkey` request body (ADR-0049 section 3). */
export interface PasskeyVaultPutRequest {
  /** URL-safe base64 (no pad) of the WebAuthn credential id. */
  credential_id_b64: string;
  /** The 32-byte entropy sealed under the credential's PRF output. */
  sealed_entropy_b64: string;
}

/** `POST /vault/passkey/get` request body. */
export interface PasskeyVaultGetRequest {
  credential_id_b64: string;
}

/** `POST /vault/passkey/get` response body. */
export interface PasskeyVaultGetResponse {
  sealed_entropy_b64: string | null;
}

// ----------------------------------------------------------------------------
// Device requests — ADR-0049 section 2 (services/auth/src/device_request.rs)
// ----------------------------------------------------------------------------

/** `POST /device/request` request body. Anonymous + PoW. */
export interface DeviceRequestCreateRequest {
  pow: PowProofPayload;
  account_id_b64: string;
  /** Standard base64 of the pairing payload (the bytes the QR carries). */
  pairing_payload_b64: string;
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
  pending: boolean;
}

/** `GET /device/requests` response body. */
export interface DeviceRequestsListResponse {
  request: {
    request_id_b64: string;
    pairing_payload_b64: string;
    created_at_ms: number;
  } | null;
}
