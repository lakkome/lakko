// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0075 — the recovery-email and account-notice routes of
// `lakko-svc-auth` (`services/auth/src/recovery_email.rs`,
// `account_notices.rs`), their wire shapes, and the mapping of their
// refusals.
//
// Encodings: `handle`, `challenge_id` and `ticket` are URL-safe base64
// without padding; `vault_salt` (16 bytes), `sealed_entropy` (44 bytes) and
// the OPAQUE messages are standard base64. The address and the codes are
// request fields only: nothing here stores or logs them.

// MARK: - Wire

struct RecoveryEmailAvailableResponse: Codable, Equatable {
    let available: Bool
}

struct EmailRecoverStartRequest: Codable, Equatable {
    let handle: String
    let address: String
    let lang: String?
}

/// `challenge_id` + how long until another letter may be asked for and how
/// long the code is good for. The same shape for a match and a miss.
public struct EmailChallenge: Codable, Equatable, Sendable {
    public let challengeId: String
    public let resendAfterSecs: Int
    public let expiresInSecs: Int

    public init(challengeId: String, resendAfterSecs: Int, expiresInSecs: Int) {
        self.challengeId = challengeId
        self.resendAfterSecs = resendAfterSecs
        self.expiresInSecs = expiresInSecs
    }

    enum CodingKeys: String, CodingKey {
        case challengeId = "challenge_id"
        case resendAfterSecs = "resend_after_secs"
        case expiresInSecs = "expires_in_secs"
    }
}

struct EmailCodeRequest: Codable, Equatable {
    let challengeId: String
    let code: String

    enum CodingKeys: String, CodingKey {
        case challengeId = "challenge_id"
        case code
    }
}

/// The answer to an accepted mailed code.
public struct EmailTicket: Codable, Equatable, Sendable {
    public let ticket: String
    public let totpRequired: Bool
    public let expiresInSecs: Int

    public init(ticket: String, totpRequired: Bool, expiresInSecs: Int) {
        self.ticket = ticket
        self.totpRequired = totpRequired
        self.expiresInSecs = expiresInSecs
    }

    enum CodingKeys: String, CodingKey {
        case ticket
        case totpRequired = "totp_required"
        case expiresInSecs = "expires_in_secs"
    }
}

struct EmailTicketCodeRequest: Codable, Equatable {
    let ticket: String
    let code: String
}

struct EmailTicketRequest: Codable, Equatable {
    let ticket: String
}

struct EmailOkResponse: Codable, Equatable {
    let ok: Bool
}

struct EmailOpenResponse: Codable, Equatable {
    let mode: String
    let vaultSalt: String?
    let sealedEntropy: String?

    enum CodingKeys: String, CodingKey {
        case mode
        case vaultSalt = "vault_salt"
        case sealedEntropy = "sealed_entropy"
    }
}

/// What `POST /recovery-email/recover/open` came to.
public enum EmailOpen: Equatable, Sendable {
    /// The account has no saved phrase: the sealed keys. The ticket is spent.
    case vault(salt: Data, sealed: Data)
    /// A saved phrase (or no vault): the mailed code changes the password.
    case password
}

struct EmailPasswordStartRequest: Codable, Equatable {
    let ticket: String
    let newPasswordRequest: String

    enum CodingKeys: String, CodingKey {
        case ticket
        case newPasswordRequest = "new_password_request"
    }
}

struct EmailPasswordStartResponse: Codable, Equatable {
    let newPasswordResponse: String

    enum CodingKeys: String, CodingKey {
        case newPasswordResponse = "new_password_response"
    }
}

struct EmailPasswordFinishRequest: Codable, Equatable {
    let ticket: String
    let newPasswordUpload: String
    let deviceIdB64: String?

    enum CodingKeys: String, CodingKey {
        case ticket
        case newPasswordUpload = "new_password_upload"
        case deviceIdB64 = "device_id_b64"
    }
}

struct EmailEnrolStartRequest: Codable, Equatable {
    let freshGrant: String
    let handle: String
    let address: String
    let lang: String?

    enum CodingKeys: String, CodingKey {
        case freshGrant = "fresh_grant"
        case handle, address, lang
    }
}

struct EmailEnrolStartResponse: Codable, Equatable {
    let challengeId: String
    let vaultSalt: String
    let resendAfterSecs: Int
    let expiresInSecs: Int

    enum CodingKeys: String, CodingKey {
        case challengeId = "challenge_id"
        case vaultSalt = "vault_salt"
        case resendAfterSecs = "resend_after_secs"
        case expiresInSecs = "expires_in_secs"
    }
}

/// An enrolment's challenge and the salt the vault is sealed with.
public struct EmailEnrolChallenge: Equatable, Sendable {
    public let challenge: EmailChallenge
    /// 16 bytes.
    public let vaultSalt: Data

    public init(challenge: EmailChallenge, vaultSalt: Data) {
        self.challenge = challenge
        self.vaultSalt = vaultSalt
    }
}

struct EmailEnrolFinishRequest: Codable, Equatable {
    let challengeId: String
    let code: String
    let sealedEntropy: String?

    enum CodingKeys: String, CodingKey {
        case challengeId = "challenge_id"
        case code
        case sealedEntropy = "sealed_entropy"
    }
}

public struct EmailEnrolled: Codable, Equatable, Sendable {
    public let enabled: Bool
    public let vault: Bool

    public init(enabled: Bool, vault: Bool) {
        self.enabled = enabled
        self.vault = vault
    }
}

struct EmailVaultPutRequest: Codable, Equatable {
    let handle: String
    let address: String
    let vaultSalt: String
    let sealedEntropy: String

    enum CodingKeys: String, CodingKey {
        case handle, address
        case vaultSalt = "vault_salt"
        case sealedEntropy = "sealed_entropy"
    }
}

struct EmailVaultPutResponse: Codable, Equatable {
    let vault: Bool?
}

struct FreshGrantRequest: Codable, Equatable {
    let freshGrant: String

    enum CodingKeys: String, CodingKey { case freshGrant = "fresh_grant" }
}

struct EmailRemovedResponse: Codable, Equatable {
    let removed: Bool?
}

struct AccountNoticeWire: Codable, Equatable {
    let id: UInt64
    let kind: String
    let atMs: Int64

    enum CodingKeys: String, CodingKey {
        case id, kind
        case atMs = "at_ms"
    }
}

struct AccountNoticesResponse: Codable, Equatable {
    let notices: [AccountNoticeWire]
    let nextAfter: UInt64
    let more: Bool

    enum CodingKeys: String, CodingKey {
        case notices
        case nextAfter = "next_after"
        case more
    }
}

// MARK: - Refusals

/// Every refusal the recovery-email routes answer with, mapped.
public enum RecoveryEmailRefusal: Equatable {
    /// `401 code-refused`: a wrong code, or one that was never sent.
    case codeRefused
    /// `410 code-expired`.
    case codeExpired
    /// `401 ticket-refused`: the ticket is spent or ran out.
    case ticketRefused
    /// `403 totp-required`.
    case totpRequired
    /// `400 address-invalid`.
    case addressInvalid
    /// `429 email-rate-limited`: too many letters.
    case lettersLimited(retryAfterSecs: Int?)
    /// `429 too-many-attempts`: too many wrong codes.
    case tooManyAttempts(retryAfterSecs: Int?)
    /// `409 handle-mismatch`.
    case handleMismatch
    /// `409 username-required`: a guest account.
    case usernameRequired
    /// `409 phrase-saved`: a vault was sent for a phrase-protected account.
    case phraseSaved
    /// `409 wrong-mode`.
    case wrongMode
    /// `502 mail-send-failed`.
    case mailSendFailed
    /// `503`: mail is not set up on this server.
    case unavailable
    /// `403 fresh-auth-required`.
    case freshAuthRequired
    case offline
    case failed

    public static func of(status: Int, body: String) -> RecoveryEmailRefusal {
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        let code = json?["error"] as? String
        let retry = (json?["retry_after_secs"] as? NSNumber)?.intValue
        switch (status, code) {
        case (401, "code-refused"): return .codeRefused
        case (401, "ticket-refused"): return .ticketRefused
        case (410, _): return .codeExpired
        case (403, "totp-required"): return .totpRequired
        case (403, "fresh-auth-required"): return .freshAuthRequired
        case (400, "address-invalid"): return .addressInvalid
        case (429, "email-rate-limited"): return .lettersLimited(retryAfterSecs: retry)
        case (429, _): return .tooManyAttempts(retryAfterSecs: retry)
        case (409, "handle-mismatch"): return .handleMismatch
        case (409, "username-required"): return .usernameRequired
        case (409, "phrase-saved"): return .phraseSaved
        case (409, "wrong-mode"): return .wrongMode
        case (502, _): return .mailSendFailed
        case (503, "rate-limiter-unavailable"): return .failed
        case (503, _): return .unavailable
        default: return .failed
        }
    }

    public static func of(_ error: Error) -> RecoveryEmailRefusal {
        if error is FreshAuthRequired { return .freshAuthRequired }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return of(status: status, body: body)
        case .network: return .offline
        default: return .failed
        }
    }
}

// MARK: - Routes

public extension AuthClient {

    /// `GET /recovery-email/available` (anonymous): whether this server can
    /// send a recovery code.
    func recoveryEmailAvailable() async throws -> Bool {
        var request = URLRequest(url: try buildURL(endpoint: "/recovery-email/available"))
        request.httpMethod = "GET"
        let response: RecoveryEmailAvailableResponse = try await execute(
            endpoint: "/recovery-email/available", request: request)
        return response.available
    }

    /// `GET /recovery-email` (bearer): the session account's status.
    func recoveryEmailStatus(sessionToken: String) async throws -> RecoveryEmailStatus {
        var request = URLRequest(url: try buildURL(endpoint: "/recovery-email"))
        request.httpMethod = "GET"
        request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        return try await execute(endpoint: "/recovery-email", request: request)
    }

    /// `POST /recovery-email/recover/start` (anonymous). The answer is the
    /// same whatever was typed; a letter leaves only for a matching pair.
    func recoveryEmailRecoverStart(handle: Data, address: String, lang: String?) async throws -> EmailChallenge {
        try requireHandle(handle)
        return try await post(
            endpoint: "/recovery-email/recover/start",
            body: EmailRecoverStartRequest(
                handle: base64UrlEncodeNoPad(handle), address: address, lang: lang))
    }

    /// `POST /recovery-email/recover/verify`.
    func recoveryEmailRecoverVerify(challengeId: String, code: String) async throws -> EmailTicket {
        try await post(
            endpoint: "/recovery-email/recover/verify",
            body: EmailCodeRequest(challengeId: challengeId, code: code))
    }

    /// `POST /recovery-email/recover/totp`.
    func recoveryEmailRecoverTotp(ticket: String, code: String) async throws {
        let _: EmailOkResponse = try await post(
            endpoint: "/recovery-email/recover/totp",
            body: EmailTicketCodeRequest(ticket: ticket, code: code))
    }

    /// `POST /recovery-email/recover/open`.
    func recoveryEmailRecoverOpen(ticket: String) async throws -> EmailOpen {
        let endpoint = "/recovery-email/recover/open"
        let response: EmailOpenResponse = try await post(
            endpoint: endpoint, body: EmailTicketRequest(ticket: ticket))
        switch response.mode {
        case "password":
            return .password
        case "vault":
            guard let salt = response.vaultSalt.flatMap(base64Decode),
                  let sealed = response.sealedEntropy.flatMap(base64Decode) else {
                throw AuthClientError.http(status: 200, endpoint: endpoint, body: "malformed vault in response")
            }
            return .vault(salt: salt, sealed: sealed)
        default:
            throw AuthClientError.http(status: 200, endpoint: endpoint, body: "unknown mode in response")
        }
    }

    /// `POST /recovery-email/recover/password/{start,finish}`: a new
    /// password under the ticket. Answers the session pair and the account
    /// id; no fresh-auth grant. The session names no device: the account is
    /// learned from this answer, and the session is bound after the landing.
    func recoveryEmailRecoverPassword(ticket: String, newPassword: Data) async throws -> RecoverResult {
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("newPassword must be non-empty")
        }
        let registration = try OpaqueRegister.startPassword(password: newPassword)
        let start: EmailPasswordStartResponse = try await post(
            endpoint: "/recovery-email/recover/password/start",
            body: EmailPasswordStartRequest(
                ticket: ticket, newPasswordRequest: registration.requestB64()))
        let output = try registration.finish(
            secret: newPassword, serverResponseB64: start.newPasswordResponse)
        let endpoint = "/recovery-email/recover/password/finish"
        let finish: TotpRecoverResponse = try await post(
            endpoint: endpoint,
            body: EmailPasswordFinishRequest(
                ticket: ticket, newPasswordUpload: output.uploadB64(), deviceIdB64: nil))
        guard let accountId = base64UrlDecodeNoPad(finish.accountIdB64), accountId.count == accountIdLength else {
            throw AuthClientError.http(status: 200, endpoint: endpoint, body: "malformed account_id_b64 in response")
        }
        return RecoverResult(accountId: accountId, exportKey: output.exportKey(), session: finish.sessionPair)
    }

    /// `POST /recovery-email/enrol/start` (bearer + fresh-auth grant).
    func recoveryEmailEnrolStart(
        sessionToken: String, freshGrant: String, handle: Data, address: String, lang: String?
    ) async throws -> EmailEnrolChallenge {
        try requireHandle(handle)
        let endpoint = "/recovery-email/enrol/start"
        let response: EmailEnrolStartResponse = try await post(
            endpoint: endpoint,
            body: EmailEnrolStartRequest(
                freshGrant: freshGrant, handle: base64UrlEncodeNoPad(handle), address: address, lang: lang),
            bearer: sessionToken)
        guard let salt = base64Decode(response.vaultSalt) else {
            throw AuthClientError.http(status: 200, endpoint: endpoint, body: "malformed vault_salt in response")
        }
        return EmailEnrolChallenge(
            challenge: EmailChallenge(
                challengeId: response.challengeId,
                resendAfterSecs: response.resendAfterSecs,
                expiresInSecs: response.expiresInSecs),
            vaultSalt: salt)
    }

    /// `POST /recovery-email/enrol/finish` (bearer). `sealedEntropy` is the
    /// 44-byte vault for an account without a saved phrase, `nil` otherwise.
    func recoveryEmailEnrolFinish(
        sessionToken: String, challengeId: String, code: String, sealedEntropy: Data?
    ) async throws -> EmailEnrolled {
        try await post(
            endpoint: "/recovery-email/enrol/finish",
            body: EmailEnrolFinishRequest(
                challengeId: challengeId, code: code, sealedEntropy: sealedEntropy.map(base64Encode)),
            bearer: sessionToken)
    }

    /// `PUT /recovery-email/vault` (bearer, no fresh-auth grant): put the
    /// email vault back for the address already confirmed. `vaultSalt` is 16
    /// random bytes chosen here; `sealedEntropy` the 44-byte vault sealed
    /// with it. Every well-formed request spends one of ten per day: never
    /// called in a loop.
    func recoveryEmailVaultPut(
        sessionToken: String, handle: Data, address: String, vaultSalt: Data, sealedEntropy: Data
    ) async throws {
        try requireHandle(handle)
        let _: EmailVaultPutResponse = try await send(
            method: "PUT", endpoint: "/recovery-email/vault",
            body: EmailVaultPutRequest(
                handle: base64UrlEncodeNoPad(handle), address: address,
                vaultSalt: base64Encode(vaultSalt), sealedEntropy: base64Encode(sealedEntropy)),
            bearer: sessionToken)
    }

    /// `DELETE /recovery-email` (bearer + fresh-auth grant).
    func recoveryEmailRemove(sessionToken: String, freshGrant: String) async throws {
        let _: EmailRemovedResponse = try await send(
            method: "DELETE", endpoint: "/recovery-email",
            body: FreshGrantRequest(freshGrant: freshGrant), bearer: sessionToken)
    }

    /// `GET /account/notices?after=<id>` or `?since_ms=<unix ms>` (bearer):
    /// one page.
    func accountNotices(sessionToken: String, query: NoticeQuery) async throws -> AccountNoticePage {
        let endpoint = "/account/notices"
        var request = URLRequest(url: try buildURL(endpoint: "\(endpoint)?\(query.queryItem)"))
        request.httpMethod = "GET"
        request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        let response: AccountNoticesResponse = try await execute(endpoint: endpoint, request: request)
        return AccountNoticePage(
            notices: response.notices.map { AccountNotice(id: $0.id, kind: $0.kind, atMs: $0.atMs) },
            nextAfter: response.nextAfter,
            more: response.more)
    }
}
