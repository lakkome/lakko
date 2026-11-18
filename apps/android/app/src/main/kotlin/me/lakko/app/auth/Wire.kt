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
// PoW (services/auth/src/pow.rs) - since ADR-0054 S4 only `/signup/start`
// carries one.
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

/** The `pow` object `/signup/start` carries. All three fields are
 *  base64-encoded 16-byte values. */
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

/** `POST /signup/finish` request body. No proof: it must follow a start
 *  for the same account within 600 s (`410` otherwise). */
@Serializable
data class SignupFinishRequest(
    val account_id: String,
    val password_upload: String,
    val recovery_upload: String? = null,
    /** ADR-0074 - base64url (no padding) of this device's 32-byte id (its
     *  conversation leaf signature key, the id of its directory row). Absent
     *  = the session names no device yet (bound later). */
    val device_id_b64: String? = null,
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
    /** ADR-0073: the refresh credential only `POST /auth/session/refresh` accepts. */
    val refresh_token: String,
    /** Refresh-credential expiry as a unix-millisecond timestamp. */
    val refresh_expires_at_ms: Long,
    /** The account's key generation the pair was issued under. */
    val key_generation: Long,
    /** The fresh-auth proof of this sign-up (UX section 17): opaque, never
     *  parsed, never stored, never logged. */
    val fresh_grant: String? = null,
    /** The proof's expiry as a unix-millisecond timestamp. */
    val fresh_grant_expires_at_ms: Long? = null,
)

// ----------------------------------------------------------------------------
// Login (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/** Shared by `/login/start` and `/recovery/login/start`. ADR-0054 S5: the
 *  account is named by its login handle, never by its id; the id comes back
 *  in the finish, after the proof. */
@Serializable
data class LoginStartRequest(
    /** base64url (no padding) of the 32-byte login handle (`loginHandle`). */
    val handle: String,
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

/** Shared by `/login/finish` and `/recovery/login/finish`. */
@Serializable
data class LoginFinishRequest(
    val session_id: String,
    /** base64(CredentialFinalization). */
    val finalize: String,
    /** ADR-0074 - base64url (no padding) of this device's 32-byte id (its
     *  conversation leaf signature key, the id of its directory row). Absent
     *  = the session names no device yet (bound later). */
    val device_id_b64: String? = null,
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
    /** ADR-0073: the refresh credential only `POST /auth/session/refresh` accepts. */
    val refresh_token: String,
    /** Refresh-credential expiry as a unix-millisecond timestamp. */
    val refresh_expires_at_ms: Long,
    /** The account's key generation the pair was issued under. */
    val key_generation: Long,
    /** The fresh-auth proof (UX section 17). Of the routes that share this
     *  body only the PASSWORD login's `/login/finish` carries one; opaque,
     *  never parsed, never stored, never logged. */
    val fresh_grant: String? = null,
    /** The proof's expiry as a unix-millisecond timestamp. */
    val fresh_grant_expires_at_ms: Long? = null,
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
    /** base64url (no padding) of the 32-byte login handle (ADR-0054 S5). */
    val handle: String,
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

/** `POST /password/reset/finish` request body. */
@Serializable
data class PasswordResetFinishRequest(
    val session_id: String,
    /** base64(CredentialFinalization) — proves recovery ownership. */
    val recovery_finalize: String,
    /** base64(RegistrationUpload) — the new password envelope. */
    val new_password_upload: String,
    /** ADR-0074 - base64url (no padding) of this device's 32-byte id (its
     *  conversation leaf signature key, the id of its directory row). Absent
     *  = the session names no device yet (bound later). */
    val device_id_b64: String? = null,
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
    /** ADR-0073: the refresh credential only `POST /auth/session/refresh` accepts. */
    val refresh_token: String,
    /** Refresh-credential expiry as a unix-millisecond timestamp. */
    val refresh_expires_at_ms: Long,
    /** The account's key generation the pair was issued under. */
    val key_generation: Long,
)

// ----------------------------------------------------------------------------
// Device authorization (services/auth/src/device_authorize.rs —
// ADR-0018 factor (a), "another-device" recovery)
// ----------------------------------------------------------------------------

/** `POST /device/authorize/start` request body (bearer): the fresh-auth
 *  proof of the device that lets the new one in (UX 17.4). */
@Serializable
data class DeviceAuthorizeStartRequest(
    val fresh_grant: String,
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
 *  fresh device has no token yet; the grant token is the
 *  authorization and the server's counters the anti-spam gate. */
@Serializable
data class DeviceAuthorizeRedeemRequest(
    /** URL-safe base64 (no pad) of the raw 32-byte grant token
     *  received over the rendezvous channel. The grant alone names
     *  the account (ADR-0056); the response carries its id. */
    val grant_token_b64: String,
    /** ADR-0074 - base64url (no padding) of this device's 32-byte id (its
     *  conversation leaf signature key, the id of its directory row). Absent
     *  = the session names no device yet (bound later). */
    val device_id_b64: String? = null,
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
    /** ADR-0073: the refresh credential only `POST /auth/session/refresh` accepts. */
    val refresh_token: String,
    /** Refresh-credential expiry as a unix-millisecond timestamp. */
    val refresh_expires_at_ms: Long,
    /** The account's key generation the pair was issued under. */
    val key_generation: Long,
)

// ----------------------------------------------------------------------------
// Auth-attested KT-log genesis (services/auth/src/genesis_attest.rs —
// ADR-0036, G-30 finding 2)
// ----------------------------------------------------------------------------

/** `POST /auth/genesis/attest` request body. Bearer-authenticated
 *  (the OPAQUE-authenticated account's session token rides the
 *  `Authorization` header, not the body); the body carries ONLY the
 *  client's genesis identity pubkey. No `pow` — the bearer is the
 *  anti-abuse gate. */
@Serializable
data class GenesisAttestRequest(
    /** URL-safe base64 (no pad) of the client's 32-byte Ed25519 genesis
     *  identity pubkey. The server binds it to the authenticated account
     *  under `lakko/genesis-attest/v1`; it does NOT independently verify
     *  the key is "correct" (it cannot — invariant #2). */
    val pubkey_b64: String,
)

/** `POST /auth/genesis/attest` response body. */
@Serializable
data class GenesisAttestResponse(
    /** URL-safe base64 (no pad) of the signed 153-byte
     *  `GenesisAttestation`. The client uploads it alongside the identity
     *  bundle (next to the ADR-0019 self-sig); discovery forwards it
     *  opaquely to the transparency append gate, which prevents first-entry
     *  squatting (only the account holder could have minted it). */
    val genesis_attest_b64: String,
)

// ----------------------------------------------------------------------------
// TOTP (services/auth/src/totp.rs — ADR-0018 factor (c),
// authenticator-code recovery)
// ----------------------------------------------------------------------------

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
 *  their device); per-name + per-address limited server-side. */
@Serializable
data class TotpRecoverRequest(
    /** base64url (no padding) of the 32-byte login handle (ADR-0054 S5). */
    val handle: String,
    /** Current 6-digit authenticator code. */
    val code: String,
    /** ADR-0074 - base64url (no padding) of this device's 32-byte id (its
     *  conversation leaf signature key, the id of its directory row). Absent
     *  = the session names no device yet (bound later). */
    val device_id_b64: String? = null,
)

/** `POST /totp/recover` response body — the same `(account_id,
 *  session_token, expiry)` triple every other login flow returns. */
@Serializable
data class TotpRecoverResponse(
    /** URL-safe base64 (no pad) of the 16-byte account id (echoed). */
    val account_id_b64: String,
    val session_token: String,
    val session_expires_at_ms: Long,
    /** ADR-0073: the refresh credential only `POST /auth/session/refresh` accepts. */
    val refresh_token: String,
    /** Refresh-credential expiry as a unix-millisecond timestamp. */
    val refresh_expires_at_ms: Long,
    /** The account's key generation the pair was issued under. */
    val key_generation: Long,
)

// ----------------------------------------------------------------------------
// Login handle (services/auth/src/login_handle.rs - ADR-0054 S5)
// ----------------------------------------------------------------------------

/** `PUT /auth/login-handle` request body. Bearer-authenticated. */
@Serializable
data class LoginHandleRequest(
    /** base64url (no padding) of the 32-byte login handle. */
    val handle: String,
    /** ADR-0076: the fresh-auth grant; needed only when the bind CHANGES
     *  the account's handle. */
    val fresh_grant: String? = null,
    /** The handle the account holds now (base64url, no padding); with
     *  [recovery_email]. */
    val previous_handle: String? = null,
    /** Keep the recovery email across the change by re-keying it. */
    val recovery_email: LoginHandleRecoveryEmail? = null,
)

/** The recovery email carried across a handle change (ADR-0076 section 2). */
@Serializable
data class LoginHandleRecoveryEmail(
    val address: String,
    /** Standard base64 of the 16-byte salt of the vault sealed under the NEW
     *  handle; with [sealed_entropy], or neither. */
    val vault_salt: String? = null,
    val sealed_entropy: String? = null,
) {
    /** The address never reaches a log line. */
    override fun toString(): String = "LoginHandleRecoveryEmail(<redacted>)"
}

/** `POST /auth/login-handle/check`. */
@Serializable
data class LoginHandleCheckRequest(val handle: String)

@Serializable
data class LoginHandleCheckResponse(val state: String)

/** `PUT /auth/login-handle` 200 body. */
@Serializable
data class LoginHandleBoundResponse(
    val bound: Boolean,
)

/** `DELETE /auth/login-handle` 200 body. */
@Serializable
data class LoginHandleRemovedResponse(
    val removed: Boolean,
)

// -----------------------------------------------------------------
// Session refresh (ADR-0073 section 3.2)
// -----------------------------------------------------------------

/** `POST /auth/session/refresh` (anonymous: the refresh credential is the
 *  authorisation). */
@Serializable
data class SessionRefreshRequest(
    val refresh_token: String,
    /** ADR-0074 section 3.3: an account statement of kind 4 by the CURRENT
     *  account key over this credential (URL-safe base64, no padding). Read
     *  only when the credential is of a lower generation; absent otherwise. */
    val key_statement_b64: String? = null,
)

/** `POST /auth/session/refresh` 200 body: the new session pair. */
@Serializable
data class SessionRefreshResponse(
    val session_token: String,
    val session_expires_at_ms: Long,
    val refresh_token: String,
    val refresh_expires_at_ms: Long,
    val key_generation: Long,
)

// -----------------------------------------------------------------
// Reset account keys (ADR-0073 section 2; replaces the deleted
// /recovery/credential/{start,finish} pair)
// -----------------------------------------------------------------

/** `POST /account/keys/reset/start` (bearer). The request is built exactly
 *  as signup builds its recovery half from the NEW 64-byte phrase seed. */
@Serializable
data class KeyResetStartRequest(
    /** The fresh-auth proof; one grant serves both calls of one reset.
     *  Absent or unusable: `403 fresh-auth-required`. */
    val fresh_grant: String,
    val recovery_request: String,
)

@Serializable
data class KeyResetStartResponse(
    val recovery_response: String,
)

/** `POST /account/keys/reset/finish` (bearer). */
@Serializable
data class KeyResetFinishRequest(
    /** The same fresh-auth proof `start` carried. */
    val fresh_grant: String,
    /** base64(RegistrationUpload) for the new recovery credential. */
    val recovery_upload: String,
    /** base64url (no padding) of the NEW AccountIdentity Ed25519 public key
     *  the attestation binds the generation to. */
    val new_signature_pubkey_b64: String,
)

/** `POST /account/keys/rotate/finish` (bearer, ADR-0074 section 3.4): the
 *  reset's finish with the old key's proof and the devices to revoke. */
@Serializable
data class KeyRotateFinishRequest(
    val fresh_grant: String,
    /** base64(RegistrationUpload) for the new recovery credential. */
    val recovery_upload: String,
    /** base64url (no padding) of the NEW AccountIdentity public key. */
    val new_signature_pubkey_b64: String,
    /** base64url (no padding) of the 145-byte supersession proof, the
     *  recorded account key over the new one. */
    val supersession_proof_b64: String,
    /** base64url (no padding) device ids, at most 8. */
    val revoke_device_ids_b64: List<String> = emptyList(),
    /** ADR-0075 section 3: this rotation is "Save a recovery phrase". Sent
     *  only when `true`. */
    val phrase_saved: Boolean? = null,
)

/** `POST /account/keys/reset/finish` 200 body: the attestation and the pair
 *  issued at the new generation (the caller's old pair is superseded). */
@Serializable
data class KeyResetFinishResponse(
    /** base64url (no padding) of the signed 145-byte `KeyResetAttestation`. */
    val key_reset_attest_b64: String,
    val key_generation: Long,
    val session_token: String,
    val session_expires_at_ms: Long,
    val refresh_token: String,
    val refresh_expires_at_ms: Long,
)

// -----------------------------------------------------------------
// ADR-0074 part 1: taking the account back (anonymous routes)
// -----------------------------------------------------------------

/** `POST /account/recall/status`. */
@Serializable
data class RecallStatusRequest(
    /** base64url (no padding) of a 138-byte account statement of kind
     *  `recall-status`, signed by the OLD account identity key. */
    val statement_b64: String,
    /** ADR-0075 section 8: base64url (no padding) of the 32-byte login
     *  handle, from a device that holds no account id. The statement then
     *  carries the first 16 bytes of the handle in its account field. */
    val handle: String? = null,
)

@Serializable
data class RecallStatusResponse(
    val anchor_seq: Long,
    val expires_at_ms: Long,
    val changed_at_ms: Long,
    val attempts_remaining: Int,
    /** ADR-0075: base64url (no padding) of the 16-byte account id. */
    val account_id_b64: String? = null,
    /** ADR-0075: the account can be taken back with its recovery phrase. */
    val phrase_arm: Boolean = false,
)

/** `POST /account/recall/start`. Each accepted start is one password attempt. */
@Serializable
data class RecallStartRequest(
    val account_id_b64: String,
    val anchor_seq: Long,
    /** Kind `recall-start`: `anchor_seq | BLAKE3(login_request)`. */
    val statement_b64: String,
    /** base64(CredentialRequest) of a login with the password BEFORE the reset. */
    val login_request: String,
    /** base64(RegistrationRequest) for the NEW password. */
    val password_request: String,
    /** base64(RegistrationRequest) for the NEW recovery credential. */
    val recovery_request: String,
    /** ADR-0075 section 8: `"phrase"` when `login_request` is a login with
     *  the OLD recovery credential; absent for the password. */
    val proof: String? = null,
)

@Serializable
data class RecallStartResponse(
    val session_id: String,
    val login_response: String,
    val password_response: String,
    val recovery_response: String,
    /** base64url (no padding) of 32 bytes the finish statement binds. */
    val server_nonce_b64: String,
)

/** `POST /account/recall/finish`. */
@Serializable
data class RecallFinishRequest(
    val session_id: String,
    val account_id_b64: String,
    val anchor_seq: Long,
    val login_finish: String,
    val password_upload: String,
    val recovery_upload: String,
    val new_signature_pubkey_b64: String,
    val device_id_b64: String? = null,
    /** Kind `recall`, signed by the OLD account identity key. */
    val statement_b64: String,
)

/** The key reset's answer plus a fresh-auth grant. */
@Serializable
data class RecallFinishResponse(
    val key_reset_attest_b64: String,
    val key_generation: Long,
    val session_token: String,
    val session_expires_at_ms: Long,
    val refresh_token: String,
    val refresh_expires_at_ms: Long,
    val fresh_grant: String? = null,
    val fresh_grant_expires_at_ms: Long? = null,
)

// -----------------------------------------------------------------
// Passkey vault: the keys a passkey restores, per credential
// -----------------------------------------------------------------

/** `PUT /vault/passkey` (bearer). `credential_id_b64` is base64url without
 *  padding; `sealed_entropy_b64` is standard base64 of the 44-byte blob. */
@Serializable
data class PasskeyVaultPutRequest(
    val credential_id_b64: String,
    val sealed_entropy_b64: String,
)

@Serializable
data class PasskeyVaultPutResponse(
    val stored: Boolean = true,
)

/** `POST /vault/passkey/get` and `DELETE /vault/passkey` (bearer). */
@Serializable
data class PasskeyVaultCredentialRequest(
    val credential_id_b64: String,
)

@Serializable
data class PasskeyVaultGetResponse(
    val sealed_entropy_b64: String? = null,
)

/** `GET /vault/passkeys` (bearer): the credential ids that have keys stored. */
@Serializable
data class PasskeyVaultListResponse(
    val credential_ids_b64: List<String> = emptyList(),
)

// ---------------------------------------------------------------------
// ADR-0074 - logout, device binding, device removal
// ---------------------------------------------------------------------

/** `POST /auth/session/bind-device` (bearer): name the device of a session
 *  that landed before the device identity existed. Answers a session pair. */
@Serializable
data class BindDeviceRequest(
    /** base64url (no padding) of the 32-byte device id. */
    val device_id_b64: String,
    /** The refresh credential of the SAME session as the bearer. */
    val refresh_token: String,
)

/** `POST /account/devices/revoke` (bearer): remove ANOTHER device. `204`. */
@Serializable
data class DeviceRevokeRequest(
    val fresh_grant: String,
    /** base64url (no padding) of the removed device's 32-byte id. */
    val device_id_b64: String,
    /** base64url (no padding) of the 138-byte `device-revoke` account
     *  statement, signed by the account identity key. */
    val statement_b64: String,
)

// -----------------------------------------------------------------
// ADR-0075: the recovery email, and the account's notices
// -----------------------------------------------------------------

/** `GET /recovery-email/available` (anonymous). */
@Serializable
data class RecoveryEmailAvailableResponse(val available: Boolean = false)

/** `GET /recovery-email` (bearer). Never the address. */
@Serializable
data class RecoveryEmailStatusResponse(
    val available: Boolean = false,
    val enabled: Boolean = false,
    val vault: Boolean = false,
    val phrase_saved: Boolean = false,
    val totp: Boolean = false,
)

/** `GET /account/phrase` (bearer, ADR-0075 section 3 as amended 2026-11-01).
 *  Answered on a server that sends no mail too. Both fields are required: a
 *  body without them is a failure, never a guessed path. */
@Serializable
data class AccountPhraseResponse(
    /** The owner saved a recovery phrase. */
    val saved: Boolean,
    /** The current phrase was ever sealed in an email vault: saving it takes
     *  a key rotation. */
    val rotation_needed: Boolean,
)

/** `POST /account/phrase/saved` (bearer + fresh-auth grant). */
@Serializable
data class PhraseSavedRequest(
    val fresh_grant: String,
)

/** `POST /recovery-email/enrol/start` (bearer). The address is sent as typed
 *  and kept nowhere on this device. */
@Serializable
data class RecoveryEmailEnrolStartRequest(
    val fresh_grant: String?,
    /** base64url (no padding) of the account's 32-byte login handle. */
    val handle: String,
    val address: String,
    val lang: String? = null,
) {
    /** The address and the grant never reach a log line. */
    override fun toString(): String = "RecoveryEmailEnrolStartRequest(<redacted>)"
}

@Serializable
data class RecoveryEmailEnrolStartResponse(
    val challenge_id: String,
    /** Standard base64 of the 16-byte vault salt. */
    val vault_salt: String,
    val resend_after_secs: Long = 0,
    val expires_in_secs: Long = 0,
)

/** `POST /recovery-email/enrol/finish` (bearer). */
@Serializable
data class RecoveryEmailEnrolFinishRequest(
    val challenge_id: String,
    val code: String,
    /** Standard base64 of the 44-byte email vault; absent for an account
     *  with a saved phrase. */
    val sealed_entropy: String? = null,
)

@Serializable
data class RecoveryEmailEnrolFinishResponse(val enabled: Boolean = false, val vault: Boolean = false)

/** `PUT /recovery-email/vault` (bearer, no grant): install the vault for the
 *  address the account already confirmed (the automatic re-seal). */
@Serializable
data class RecoveryEmailVaultPutRequest(
    val handle: String,
    val address: String,
    /** Standard base64 of 16 random bytes chosen by this device. */
    val vault_salt: String,
    /** Standard base64 of the 44-byte sealed entropy. */
    val sealed_entropy: String,
) {
    /** The address never reaches a log line. */
    override fun toString(): String = "RecoveryEmailVaultPutRequest(<redacted>)"
}

@Serializable
data class RecoveryEmailVaultPutResponse(val vault: Boolean = false)

/** Body of `DELETE /recovery-email`. */
@Serializable
data class RecoveryEmailGrantRequest(val fresh_grant: String?)

/** `DELETE /recovery-email` 200 body. */
@Serializable
data class RecoveryEmailRemoveResponse(val removed: Boolean = false)

/** `POST /recovery-email/recover/start` (anonymous). */
@Serializable
data class EmailRecoverStartRequest(val handle: String, val address: String, val lang: String? = null) {
    /** The address never reaches a log line. */
    override fun toString(): String = "EmailRecoverStartRequest(<redacted>)"
}

@Serializable
data class EmailRecoverStartResponse(
    val challenge_id: String,
    val resend_after_secs: Long = 0,
    val expires_in_secs: Long = 0,
)

/** `POST /recovery-email/recover/verify`. */
@Serializable
data class EmailRecoverVerifyRequest(val challenge_id: String, val code: String)

@Serializable
data class EmailRecoverVerifyResponse(
    val ticket: String,
    val totp_required: Boolean = false,
    val expires_in_secs: Long = 0,
)

/** `POST /recovery-email/recover/totp`. */
@Serializable
data class EmailRecoverTotpRequest(val ticket: String, val code: String)

@Serializable
data class EmailRecoverOkResponse(val ok: Boolean = false)

/** `POST /recovery-email/recover/open`. */
@Serializable
data class EmailRecoverTicketRequest(val ticket: String)

@Serializable
data class EmailRecoverOpenResponse(
    /** `"vault"` or `"password"`. */
    val mode: String,
    val vault_salt: String? = null,
    val sealed_entropy: String? = null,
)

/** `POST /recovery-email/recover/password/start`. */
@Serializable
data class EmailRecoverPasswordStartRequest(val ticket: String, val new_password_request: String)

@Serializable
data class EmailRecoverPasswordStartResponse(val new_password_response: String)

/** `POST /recovery-email/recover/password/finish`. */
@Serializable
data class EmailRecoverPasswordFinishRequest(
    val ticket: String,
    val new_password_upload: String,
    val device_id_b64: String? = null,
)

/** The session pair and the account id. No fresh-auth grant. */
@Serializable
data class EmailRecoverPasswordFinishResponse(
    val account_id_b64: String,
    val session_token: String,
    val session_expires_at_ms: Long,
    val refresh_token: String,
    val refresh_expires_at_ms: Long,
    val key_generation: Long,
)

/** One row of `GET /account/notices`. */
@Serializable
data class AccountNoticeWire(val id: Long, val kind: String, val at_ms: Long)

/** `GET /account/notices?after=<id>` (bearer). */
@Serializable
data class AccountNoticesResponse(
    val notices: List<AccountNoticeWire> = emptyList(),
    val next_after: Long = 0,
    val more: Boolean = false,
)
