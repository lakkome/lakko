// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Swift-side mirror of the JSON shapes consumed and produced by
/// `lakko-svc-auth`.
///
/// Mirror of:
///
///   * `crates/lakko-auth-client/src/wire.rs` (server-side originals)
///   * `apps/web/src/auth/wire.ts` (1.17d3)
///   * `apps/android/.../auth/Wire.kt` (1.15d2)
///
/// The Rust definitions in `services/auth/src/{opaque,pow}.rs` are
/// `pub(crate)` and there is no proto pipeline on the wire (the
/// protobuf schemas in `schemas/` are E2EE-payload shapes, not
/// REST-API DTOs). When a field is added on the server, mirror it
/// here in the **same commit**; the wire format is append-only.
///
/// ## Conventions
///
///   * Every byte string crosses the wire base64-encoded (standard
///     alphabet, with padding) — same as the server.
///   * `snake_case` field names match the server's `serde` defaults.
///     Swift `Codable` lets us spell properties in idiomatic
///     `camelCase` and remap with `CodingKeys`; the JSON we emit is
///     byte-identical to what `apps/android` (`@SerialName` would
///     have done) and `apps/web` (TS interfaces with snake_case keys
///     verbatim) produce.
///   * Optional fields default to `nil`; `JSONEncoder` omits them
///     entirely because Swift's `KeyedEncodingContainer` skips
///     `encode(nil)` by default — the wire shape matches the
///     `Json { explicitNulls = false }` knob on the Kotlin side.
///   * No `equals` / `hashCode` ceremony — Swift `Equatable` is
///     auto-synthesized when every stored property is `Equatable`,
///     and all our properties are `String` / `Int`. Free for the
///     unit tests.

// ----------------------------------------------------------------------------
// PoW (services/auth/src/pow.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /pow/challenge`. The endpoint takes no input.
public struct PowChallengeResponse: Codable, Equatable {
    /// base64(seed) — 16 random bytes the server will recognise on
    /// the matching expensive request.
    public let seed: String
    /// Number of leading zero bits required on
    /// `SHA-256(seed || nonce || solution)`.
    public let effortBits: Int
    /// Seconds until the challenge row expires (consumed-on-use).
    public let expiresIn: Int

    public init(seed: String, effortBits: Int, expiresIn: Int) {
        self.seed = seed
        self.effortBits = effortBits
        self.expiresIn = expiresIn
    }

    enum CodingKeys: String, CodingKey {
        case seed
        case effortBits = "effort_bits"
        case expiresIn = "expires_in"
    }
}

/// Embedded `pow` object on every expensive endpoint. All three
/// fields are base64-encoded 16-byte values.
public struct PowProofPayload: Codable, Equatable {
    public let seed: String
    public let nonce: String
    public let solution: String

    public init(seed: String, nonce: String, solution: String) {
        self.seed = seed
        self.nonce = nonce
        self.solution = solution
    }
}

// ----------------------------------------------------------------------------
// Signup (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/// `POST /signup/start` request body.
public struct SignupStartRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// base64(account_id) — raw 16-byte UUIDv7 the client picked.
    public let accountId: String
    /// base64(RegistrationRequest) from the password OPAQUE flow.
    public let passwordRequest: String
    /// Optional base64(RegistrationRequest) from the BIP39 recovery
    /// flow. Recommended on signup; the client SHOULD always enrol a
    /// recovery mnemonic. `nil` is wire-omitted by `JSONEncoder`'s
    /// default behaviour — matches the kotlinx-serialization
    /// `explicitNulls = false` and the TS spread-operator shape.
    public let recoveryRequest: String?

    public init(
        pow: PowProofPayload,
        accountId: String,
        passwordRequest: String,
        recoveryRequest: String? = nil
    ) {
        self.pow = pow
        self.accountId = accountId
        self.passwordRequest = passwordRequest
        self.recoveryRequest = recoveryRequest
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case passwordRequest = "password_request"
        case recoveryRequest = "recovery_request"
    }
}

/// `POST /signup/start` response body.
public struct SignupStartResponse: Codable, Equatable {
    public let passwordResponse: String
    public let recoveryResponse: String?

    public init(passwordResponse: String, recoveryResponse: String? = nil) {
        self.passwordResponse = passwordResponse
        self.recoveryResponse = recoveryResponse
    }

    enum CodingKeys: String, CodingKey {
        case passwordResponse = "password_response"
        case recoveryResponse = "recovery_response"
    }
}

/// `POST /signup/finish` request body.
public struct SignupFinishRequest: Codable, Equatable {
    public let pow: PowProofPayload
    public let accountId: String
    public let passwordUpload: String
    public let recoveryUpload: String?

    public init(
        pow: PowProofPayload,
        accountId: String,
        passwordUpload: String,
        recoveryUpload: String? = nil
    ) {
        self.pow = pow
        self.accountId = accountId
        self.passwordUpload = passwordUpload
        self.recoveryUpload = recoveryUpload
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case passwordUpload = "password_upload"
        case recoveryUpload = "recovery_upload"
    }
}

/// `POST /signup/finish` response body.
public struct SignupFinishResponse: Codable, Equatable {
    public let ok: Bool
    /// Phase 1.5b: HMAC-SHA256 bearer token for protected
    /// `lakko-svc-discovery` writes. Stash in
    /// ``AuthRepository`` then thread through every
    /// ``DiscoveryClient`` request via the
    /// `Authorization: Bearer <token>` header.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(ok: Bool, sessionToken: String, sessionExpiresAtMs: Int64) {
        self.ok = ok
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case ok
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
    }
}

// ----------------------------------------------------------------------------
// Login (services/auth/src/opaque.rs)
// ----------------------------------------------------------------------------

/// Shared by `/login/start` and `/recovery/login/start`.
public struct LoginStartRequest: Codable, Equatable {
    public let pow: PowProofPayload
    public let accountId: String
    /// base64(CredentialRequest).
    public let request: String

    public init(pow: PowProofPayload, accountId: String, request: String) {
        self.pow = pow
        self.accountId = accountId
        self.request = request
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case request
    }
}

/// Shared by `/login/start` and `/recovery/login/start`.
public struct LoginStartResponse: Codable, Equatable {
    /// UUIDv7 (string form) the server assigned to this login attempt.
    public let sessionId: String
    /// base64(CredentialResponse).
    public let response: String

    public init(sessionId: String, response: String) {
        self.sessionId = sessionId
        self.response = response
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case response
    }
}

/// Shared by `/login/finish` and `/recovery/login/finish`. No `pow`
/// field — the server only requires PoW on the start endpoints.
public struct LoginFinishRequest: Codable, Equatable {
    public let sessionId: String
    /// base64(CredentialFinalization).
    public let finalize: String

    public init(sessionId: String, finalize: String) {
        self.sessionId = sessionId
        self.finalize = finalize
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case finalize
    }
}

/// Shared by `/login/finish` and `/recovery/login/finish`.
public struct LoginFinishResponse: Codable, Equatable {
    public let ok: Bool
    /// base64(account_id) — echoed back so the client can pin it.
    public let accountId: String
    /// Phase 1.5b: bearer token for protected discovery writes.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        ok: Bool,
        accountId: String,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.ok = ok
        self.accountId = accountId
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case ok
        case accountId = "account_id"
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
    }
}

// ----------------------------------------------------------------------------
// Password reset (services/auth/src/opaque.rs — 1.16c3c3a)
// ----------------------------------------------------------------------------

/// `POST /password/reset/start` request body. The user is rotating
/// their password by proving they know the BIP39 recovery mnemonic;
/// both OPAQUE flows (recovery login + new password registration)
/// start in this single round trip.
public struct PasswordResetStartRequest: Codable, Equatable {
    public let pow: PowProofPayload
    public let accountId: String
    /// base64(CredentialRequest) from `OpaqueLogin.startRecovery(seed:)`.
    public let recoveryRequest: String
    /// base64(RegistrationRequest) from
    /// `OpaqueRegister.startPassword(password:)`.
    public let newPasswordRequest: String

    public init(
        pow: PowProofPayload,
        accountId: String,
        recoveryRequest: String,
        newPasswordRequest: String
    ) {
        self.pow = pow
        self.accountId = accountId
        self.recoveryRequest = recoveryRequest
        self.newPasswordRequest = newPasswordRequest
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case recoveryRequest = "recovery_request"
        case newPasswordRequest = "new_password_request"
    }
}

/// `POST /password/reset/start` response body.
public struct PasswordResetStartResponse: Codable, Equatable {
    /// UUIDv7 — used to bind the matching `/password/reset/finish`.
    public let sessionId: String
    /// base64(CredentialResponse) — feed to `OpaqueLogin.finish(_:_:)`.
    public let recoveryResponse: String
    /// base64(RegistrationResponse) — feed to `OpaqueRegister.finish(_:_:)`.
    public let newPasswordResponse: String

    public init(
        sessionId: String,
        recoveryResponse: String,
        newPasswordResponse: String
    ) {
        self.sessionId = sessionId
        self.recoveryResponse = recoveryResponse
        self.newPasswordResponse = newPasswordResponse
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case recoveryResponse = "recovery_response"
        case newPasswordResponse = "new_password_response"
    }
}

/// `POST /password/reset/finish` request body. No `pow` field — the
/// server only requires PoW on the start endpoints, by analogy with
/// `/login/{start,finish}`.
public struct PasswordResetFinishRequest: Codable, Equatable {
    public let sessionId: String
    /// base64(CredentialFinalization) — proves recovery ownership.
    public let recoveryFinalize: String
    /// base64(RegistrationUpload) — the new password envelope.
    public let newPasswordUpload: String

    public init(
        sessionId: String,
        recoveryFinalize: String,
        newPasswordUpload: String
    ) {
        self.sessionId = sessionId
        self.recoveryFinalize = recoveryFinalize
        self.newPasswordUpload = newPasswordUpload
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case recoveryFinalize = "recovery_finalize"
        case newPasswordUpload = "new_password_upload"
    }
}

// ----------------------------------------------------------------------------
// Device authorization (services/auth/src/device_authorize.rs —
// ADR-0018 factor (a), "another-device" recovery)
// ----------------------------------------------------------------------------

/// `POST /device/authorize/start` request body. Bearer-authenticated
/// (the EXISTING device's session token rides the `Authorization`
/// header, not the body).
public struct DeviceAuthorizeStartRequest: Codable, Equatable {
    public let pow: PowProofPayload

    public init(pow: PowProofPayload) {
        self.pow = pow
    }
}

/// `POST /device/authorize/start` response body.
public struct DeviceAuthorizeStartResponse: Codable, Equatable {
    /// URL-safe base64 (no pad) of the RAW 32-byte grant token —
    /// the only time it leaves the server (it stores only the
    /// SHA-256). Seal it to the fresh device over the E2EE pairing
    /// channel; never log or persist it.
    public let grantTokenB64: String

    public init(grantTokenB64: String) {
        self.grantTokenB64 = grantTokenB64
    }

    enum CodingKeys: String, CodingKey {
        case grantTokenB64 = "grant_token_b64"
    }
}

/// `POST /device/authorize/redeem` request body. Anonymous — the
/// fresh device has no token yet; the PoW is the only anti-spam
/// gate.
public struct DeviceAuthorizeRedeemRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// URL-safe base64 (no pad) of the 16-byte account id the fresh
    /// device learned from the pairing QR.
    public let accountIdB64: String
    /// URL-safe base64 (no pad) of the raw 32-byte grant token
    /// received over the rendezvous channel.
    public let grantTokenB64: String

    public init(pow: PowProofPayload, accountIdB64: String, grantTokenB64: String) {
        self.pow = pow
        self.accountIdB64 = accountIdB64
        self.grantTokenB64 = grantTokenB64
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountIdB64 = "account_id_b64"
        case grantTokenB64 = "grant_token_b64"
    }
}

// ----------------------------------------------------------------------------
// Genesis attestation (ADR-0036, G-30 finding 2 — anti-squatting)
// ----------------------------------------------------------------------------

/// `POST /auth/genesis/attest` request body. Bearer-authenticated (the
/// account's session token rides the `Authorization` header, not the body —
/// holding a valid token IS the proof the caller OPAQUE-authenticated as
/// `account_id`, the whole security basis). No PoW.
public struct GenesisAttestRequest: Codable, Equatable {
    /// URL-safe base64 (no pad) of the client's 32-byte genesis identity
    /// (Ed25519) public key. The server binds THIS key to the authenticated
    /// account inside the signed attestation.
    public let pubkeyB64: String

    public init(pubkeyB64: String) {
        self.pubkeyB64 = pubkeyB64
    }

    enum CodingKeys: String, CodingKey {
        case pubkeyB64 = "pubkey_b64"
    }
}

/// `POST /auth/genesis/attest` response body — the signed 153-byte
/// `GenesisAttestation`. The client uploads it alongside the identity bundle
/// (as `genesis_attest`) so the transparency genesis entry is
/// account-authenticated.
public struct GenesisAttestResponse: Codable, Equatable {
    /// URL-safe base64 (no pad) of the signed `GenesisAttestation`.
    public let genesisAttestB64: String

    public init(genesisAttestB64: String) {
        self.genesisAttestB64 = genesisAttestB64
    }

    enum CodingKeys: String, CodingKey {
        case genesisAttestB64 = "genesis_attest_b64"
    }
}

/// `POST /device/authorize/redeem` response body — the same
/// `(account_id, session_token, expiry)` triple every other login
/// flow returns.
public struct DeviceAuthorizeRedeemResponse: Codable, Equatable {
    /// URL-safe base64 (no pad) of the 16-byte account id.
    public let accountIdB64: String
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64

    public init(
        accountIdB64: String,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountIdB64 = accountIdB64
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case accountIdB64 = "account_id_b64"
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
    }
}

// ----------------------------------------------------------------------------
// TOTP (services/auth/src/totp.rs — ADR-0018 factor (c),
// authenticator-code recovery)
// ----------------------------------------------------------------------------

/// `POST /totp/enroll/start` request body. Bearer-authenticated (the
/// already-logged-in account's session token rides the
/// `Authorization` header, not the body) + PoW.
public struct TotpEnrollStartRequest: Codable, Equatable {
    public let pow: PowProofPayload

    public init(pow: PowProofPayload) {
        self.pow = pow
    }
}

/// `POST /totp/enroll/start` response body — the provisioning
/// material the authenticator app consumes. The raw secret leaves
/// the server exactly once, here; display the QR + the manual-entry
/// secret once, never persist either client-side.
public struct TotpEnrollStartResponse: Codable, Equatable {
    /// `otpauth://totp/...` provisioning URI for the QR code.
    public let otpauthUri: String
    /// RFC 4648 base32 (no padding) secret for manual entry.
    public let secretB32: String

    public init(otpauthUri: String, secretB32: String) {
        self.otpauthUri = otpauthUri
        self.secretB32 = secretB32
    }

    enum CodingKeys: String, CodingKey {
        case otpauthUri = "otpauth_uri"
        case secretB32 = "secret_b32"
    }
}

/// `POST /totp/enroll/finish` request body. Bearer-auth, NO `pow`
/// field — the server does not gate finish.
public struct TotpEnrollFinishRequest: Codable, Equatable {
    /// The first 6-digit code the authenticator produced, proving it
    /// holds the secret.
    public let code: String

    public init(code: String) {
        self.code = code
    }
}

/// `POST /totp/recover` request body. Anonymous (the caller lost
/// their device) + PoW.
public struct TotpRecoverRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// URL-safe base64 (no pad) of the 16-byte account id.
    public let accountIdB64: String
    /// The current 6-digit authenticator code.
    public let code: String

    public init(pow: PowProofPayload, accountIdB64: String, code: String) {
        self.pow = pow
        self.accountIdB64 = accountIdB64
        self.code = code
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountIdB64 = "account_id_b64"
        case code
    }
}

/// `POST /totp/recover` response body — the same `(account_id,
/// session_token, expiry)` triple every other login flow returns.
public struct TotpRecoverResponse: Codable, Equatable {
    /// URL-safe base64 (no pad) of the 16-byte account id.
    public let accountIdB64: String
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64

    public init(
        accountIdB64: String,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountIdB64 = accountIdB64
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case accountIdB64 = "account_id_b64"
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
    }
}

// ----------------------------------------------------------------------------
// Recovery contact — phone / email OTP (services/auth/src/recovery_contact.rs
// + otp_sender.rs — ADR-0018 factor (d), one-time-code recovery)
// ----------------------------------------------------------------------------

/// Wire-format `kind` discriminator for a recovery contact, matching
/// the server's `ContactKind::as_db` (`services/auth/src/otp_sender.rs`)
/// and the web `ContactKind` (`apps/web/src/auth/wire.ts`). These
/// integer values are wire-format stable: never renumber.
///
/// Serialised as the bare integer on the wire (the server's `kind`
/// field is a `u8`), so the Swift enum is `Int`-backed and encodes via
/// its raw value — no `CodingKeys` ceremony needed at the field level.
public enum ContactKind: Int, Codable, Equatable {
    /// Code delivered over email (SMTP).
    case email = 1
    /// Code delivered over SMS.
    case phone = 2
}

/// `POST /recovery-contact/enroll/start` request body. Bearer-
/// authenticated (the already-logged-in account's session token rides
/// the `Authorization` header, not the body) + PoW.
public struct RecoveryContactEnrollStartRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// `1 = email`, `2 = phone` (see ``ContactKind``).
    public let kind: ContactKind
    /// The raw phone / email plaintext. Used transiently server-side
    /// to hash + address the message; never stored, never logged
    /// (invariant #1).
    public let contact: String

    public init(pow: PowProofPayload, kind: ContactKind, contact: String) {
        self.pow = pow
        self.kind = kind
        self.contact = contact
    }
}

/// Generic "a code was sent" response, shared by enroll/start and
/// recover/start. On recover/start it is returned IDENTICALLY whether
/// or not the contact matched a confirmed row (enumeration-safe).
public struct RecoveryContactSentResponse: Codable, Equatable {
    public let sent: Bool

    public init(sent: Bool) {
        self.sent = sent
    }
}

/// `POST /recovery-contact/enroll/finish` request body. Bearer-auth,
/// NO `pow` field — the server does not gate finish.
public struct RecoveryContactEnrollFinishRequest: Codable, Equatable {
    public let kind: ContactKind
    /// The 6-digit code that was delivered to the contact.
    public let code: String

    public init(kind: ContactKind, code: String) {
        self.kind = kind
        self.code = code
    }
}

/// `POST /recovery-contact/recover/start` request body. Anonymous (the
/// caller lost their device) + PoW. ENUMERATION-SAFE: the server
/// returns the same `{ sent: true }` whether or not the contact matches
/// a confirmed row.
public struct RecoveryContactRecoverStartRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// URL-safe base64 (no pad) of the 16-byte account_id.
    public let accountIdB64: String
    public let kind: ContactKind
    /// The raw phone / email plaintext.
    public let contact: String

    public init(
        pow: PowProofPayload,
        accountIdB64: String,
        kind: ContactKind,
        contact: String
    ) {
        self.pow = pow
        self.accountIdB64 = accountIdB64
        self.kind = kind
        self.contact = contact
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountIdB64 = "account_id_b64"
        case kind
        case contact
    }
}

/// `POST /recovery-contact/recover/finish` request body. Anonymous +
/// PoW.
public struct RecoveryContactRecoverFinishRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// URL-safe base64 (no pad) of the 16-byte account_id.
    public let accountIdB64: String
    public let kind: ContactKind
    /// The 6-digit code that was delivered to the contact.
    public let code: String

    public init(
        pow: PowProofPayload,
        accountIdB64: String,
        kind: ContactKind,
        code: String
    ) {
        self.pow = pow
        self.accountIdB64 = accountIdB64
        self.kind = kind
        self.code = code
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountIdB64 = "account_id_b64"
        case kind
        case code
    }
}

/// `POST /recovery-contact/recover/finish` response body — the same
/// `(account_id, session_token, expiry)` triple every other login flow
/// returns.
public struct RecoveryContactRecoverFinishResponse: Codable, Equatable {
    /// URL-safe base64 (no pad) of the 16-byte account id (echoed).
    public let accountIdB64: String
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64

    public init(
        accountIdB64: String,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountIdB64 = accountIdB64
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case accountIdB64 = "account_id_b64"
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
    }
}

/// `POST /password/reset/finish` response body.
public struct PasswordResetFinishResponse: Codable, Equatable {
    public let ok: Bool
    /// base64(account_id) — echoed back, same shape as
    /// ``LoginFinishResponse``.
    public let accountId: String
    /// Phase 1.5b: fresh bearer token for the rotated password.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        ok: Bool,
        accountId: String,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.ok = ok
        self.accountId = accountId
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case ok
        case accountId = "account_id"
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
    }
}
