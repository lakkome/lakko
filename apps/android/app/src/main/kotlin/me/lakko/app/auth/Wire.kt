// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:Suppress("ConstructorParameterNaming")
// `account_id`, `password_request` etc. mirror the server's snake_case
// `serde` defaults verbatim. Naming-convention lint would push us to
// camelCase, which would force every wire DTO to grow a
// `@SerialName("account_id")` annotation — pure noise. We suppress the
// lint at the file level instead.

package me.lakko.app.auth

import kotlinx.serialization.Serializable

/**
 * Kotlin-side mirror of the JSON shapes consumed and produced by
 * `lakko-svc-auth`.
 *
 * The Rust originals live in `services/auth/src/{opaque,pow}.rs`
 * and `crates/lakko-auth-client/src/wire.rs`. We mirror them by
 * hand here — same posture as `apps/web/src/auth/wire.ts`: the
 * Rust definitions are `pub(crate)` and there is no proto pipeline
 * on the wire (the protobuf schemas in `schemas/` are E2EE-payload
 * shapes, not REST-API DTOs). When a field is added on the server,
 * mirror it here in the **same commit**; the wire format is
 * append-only.
 *
 * Conventions:
 *   * Every byte string crosses the wire base64-encoded (standard
 *     alphabet, with padding) — same as the server.
 *   * `snake_case` field names match the server's `serde` defaults;
 *     we use Kotlin property names of the same shape so the
 *     kotlinx-serialization JSON output is byte-identical to what
 *     `apps/web` sends. The `@file:Suppress` at the top of this
 *     file silences the kotlin-naming lint that would otherwise
 *     bark on every property.
 */

// ----------------------------------------------------------------------------
// PoW (services/auth/src/pow.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /pow/challenge`. The endpoint takes no input. */
@Serializable
data class PowChallengeResponse(
    /** base64(seed) — 16 random bytes the server will recognise on
     *  the matching expensive request. */
    val seed: String,
    /** Number of leading zero bits required on
     *  `SHA-256(seed || nonce || solution)`. */
    val effort_bits: Int,
    /** Seconds until the challenge row expires (consumed-on-use). */
    val expires_in: Int,
)

/** Embedded `pow` object on every expensive endpoint. All three
 *  fields are base64-encoded 16-byte values. */
@Serializable
data class PowProofPayload(
    val seed: String,
    val nonce: String,
    val solution: String,
)

// ----------------------------------------------------------------------------
// Signup (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/** `POST /signup/start` request body. */
@Serializable
data class SignupStartRequest(
    val pow: PowProofPayload,
    /** base64(account_id) — raw 16-byte UUIDv7 the client picked. */
    val account_id: String,
    /** base64(RegistrationRequest) from the password OPAQUE flow. */
    val password_request: String,
    /** Optional base64(RegistrationRequest) from the BIP39 recovery
     *  flow. Recommended on signup; the client SHOULD always enrol a
     *  recovery mnemonic. `null` is serialized as field-omitted by
     *  `Json { explicitNulls = false }` — matches the TS
     *  spread-operator-omitted-field shape on the wire. */
    val recovery_request: String? = null,
)

/** `POST /signup/start` response body. */
@Serializable
data class SignupStartResponse(
    val password_response: String,
    val recovery_response: String? = null,
)

/** `POST /signup/finish` request body. */
@Serializable
data class SignupFinishRequest(
    val pow: PowProofPayload,
    val account_id: String,
    val password_upload: String,
    val recovery_upload: String? = null,
)

/** `POST /signup/finish` response body. */
@Serializable
data class SignupFinishResponse(
    val ok: Boolean,
    /** Phase 1.5b: HMAC-SHA256 bearer token. Carry in the
     *  `Authorization: Bearer <token>` header on every protected
     *  `lakko-svc-discovery` write. */
    val session_token: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val session_expires_at_ms: Long,
)

// ----------------------------------------------------------------------------
// Login (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/** Shared by `/login/start` and `/recovery/login/start`. */
@Serializable
data class LoginStartRequest(
    val pow: PowProofPayload,
    val account_id: String,
    /** base64(CredentialRequest). */
    val request: String,
)

/** Shared by `/login/start` and `/recovery/login/start`. */
@Serializable
data class LoginStartResponse(
    /** UUIDv7 (string form) the server assigned to this login attempt. */
    val session_id: String,
    /** base64(CredentialResponse). */
    val response: String,
)

/** Shared by `/login/finish` and `/recovery/login/finish`. No `pow`
 *  field — the server only requires PoW on the start endpoints. */
@Serializable
data class LoginFinishRequest(
    val session_id: String,
    /** base64(CredentialFinalization). */
    val finalize: String,
)

/** Shared by `/login/finish` and `/recovery/login/finish`. */
@Serializable
data class LoginFinishResponse(
    val ok: Boolean,
    /** base64(account_id) — echoed back so the client can pin it. */
    val account_id: String,
    /** Phase 1.5b: HMAC-SHA256 bearer token. */
    val session_token: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val session_expires_at_ms: Long,
)

// ----------------------------------------------------------------------------
// Password reset (services/auth/src/opaque.rs — 1.16c3c3a)
// ----------------------------------------------------------------------------

/**
 * `POST /password/reset/start` request body. The user is rotating
 * their password by proving they know the BIP39 recovery
 * mnemonic; both OPAQUE flows (recovery login + new password
 * registration) start in this single round trip.
 */
@Serializable
data class PasswordResetStartRequest(
    val pow: PowProofPayload,
    /** base64(account_id). */
    val account_id: String,
    /** base64(CredentialRequest) from `OpaqueLogin.startRecovery(seed)`. */
    val recovery_request: String,
    /** base64(RegistrationRequest) from `OpaqueRegister.startPassword(newPassword)`. */
    val new_password_request: String,
)

/** `POST /password/reset/start` response body. */
@Serializable
data class PasswordResetStartResponse(
    /** UUIDv7 — used to bind the matching `/password/reset/finish`. */
    val session_id: String,
    /** base64(CredentialResponse) — feed to `OpaqueLogin.finish`. */
    val recovery_response: String,
    /** base64(RegistrationResponse) — feed to `OpaqueRegister.finish`. */
    val new_password_response: String,
)

/** `POST /password/reset/finish` request body. No `pow` field —
 *  the server only requires PoW on the start endpoints, by analogy
 *  with `/login/{start,finish}`. */
@Serializable
data class PasswordResetFinishRequest(
    val session_id: String,
    /** base64(CredentialFinalization) — proves recovery ownership. */
    val recovery_finalize: String,
    /** base64(RegistrationUpload) — the new password envelope. */
    val new_password_upload: String,
)

/** `POST /password/reset/finish` response body. */
@Serializable
data class PasswordResetFinishResponse(
    val ok: Boolean,
    /** base64(account_id) — echoed back, same shape as `LoginFinishResponse`. */
    val account_id: String,
    /** Phase 1.5b: fresh bearer token for the rotated password. */
    val session_token: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val session_expires_at_ms: Long,
)

// ----------------------------------------------------------------------------
// Device authorization (services/auth/src/device_authorize.rs —
// ADR-0018 factor (a), "another-device" recovery)
// ----------------------------------------------------------------------------

/** `POST /device/authorize/start` request body. Bearer-authenticated
 *  (the EXISTING device's session token rides the `Authorization`
 *  header, not the body). */
@Serializable
data class DeviceAuthorizeStartRequest(
    val pow: PowProofPayload,
)

/** `POST /device/authorize/start` response body. */
@Serializable
data class DeviceAuthorizeStartResponse(
    /** URL-safe base64 (no pad) of the RAW 32-byte grant token —
     *  the only time it leaves the server (it stores only the
     *  SHA-256). Seal it to the fresh device over the E2EE pairing
     *  channel; never log or persist it. */
    val grant_token_b64: String,
)

/** `POST /device/authorize/redeem` request body. Anonymous — the
 *  fresh device has no token yet; the PoW is the only anti-spam
 *  gate. */
@Serializable
data class DeviceAuthorizeRedeemRequest(
    val pow: PowProofPayload,
    /** URL-safe base64 (no pad) of the 16-byte account id the fresh
     *  device learned from the pairing QR. */
    val account_id_b64: String,
    /** URL-safe base64 (no pad) of the raw 32-byte grant token
     *  received over the rendezvous channel. */
    val grant_token_b64: String,
)

/** `POST /device/authorize/redeem` response body — the same
 *  `(account_id, session_token, expiry)` triple every other login
 *  flow returns. */
@Serializable
data class DeviceAuthorizeRedeemResponse(
    /** URL-safe base64 (no pad) of the 16-byte account id. */
    val account_id_b64: String,
    val session_token: String,
    val session_expires_at_ms: Long,
)

// ----------------------------------------------------------------------------
// TOTP (services/auth/src/totp.rs — ADR-0018 factor (c),
// authenticator-code recovery)
// ----------------------------------------------------------------------------

/** `POST /totp/enroll/start` request body. Bearer-authenticated
 *  (the already-logged-in account's session token rides the
 *  `Authorization` header) + PoW. */
@Serializable
data class TotpEnrollStartRequest(
    val pow: PowProofPayload,
)

/** `POST /totp/enroll/start` response body — the provisioning
 *  material the authenticator app consumes. The raw secret leaves
 *  the server exactly once, here. */
@Serializable
data class TotpEnrollStartResponse(
    /** `otpauth://totp/...` provisioning URI for the QR code. */
    val otpauth_uri: String,
    /** RFC 4648 base32 (no padding) of the raw secret, for manual
     *  entry into the authenticator app. */
    val secret_b32: String,
)

/** `POST /totp/enroll/finish` request body. Bearer-auth, NO `pow`
 *  — the server does not gate finish. */
@Serializable
data class TotpEnrollFinishRequest(
    /** The first 6-digit code the authenticator produced, proving
     *  it holds the secret. */
    val code: String,
)

/** `POST /totp/recover` request body. Anonymous (the caller lost
 *  their device) + PoW. */
@Serializable
data class TotpRecoverRequest(
    val pow: PowProofPayload,
    /** URL-safe base64 (no pad) of the 16-byte account_id. */
    val account_id_b64: String,
    /** Current 6-digit authenticator code. */
    val code: String,
)

/** `POST /totp/recover` response body — the same `(account_id,
 *  session_token, expiry)` triple every other login flow returns. */
@Serializable
data class TotpRecoverResponse(
    /** URL-safe base64 (no pad) of the 16-byte account id (echoed). */
    val account_id_b64: String,
    val session_token: String,
    val session_expires_at_ms: Long,
)
