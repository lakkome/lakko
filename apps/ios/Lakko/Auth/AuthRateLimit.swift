// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The auth service's limits, read off a refusal (ADR-0054 S4).
///
/// A limit answers `429 {"error": "<code>", "retry_after_secs": N}` with a
/// `Retry-After: N` header, and `503 {"error": "rate-limiter-unavailable"}`
/// when the counter store is down. Neither is ever a sign-out or a wrong
/// credential. Copy verbatim from `docs/ux/anti-abuse.md` section 2.
public enum AuthRateLimit {

    /// One refusal.
    public struct Refusal: Equatable {
        public let code: String
        public let retryAfterSecs: Int?

        public init(code: String, retryAfterSecs: Int?) {
            self.code = code
            self.retryAfterSecs = retryAfterSecs
        }
    }

    public static let tooManyAttempts = "too-many-attempts"
    public static let recoveryCodeSendLimited = "recovery-code-send-limited"
    public static let deviceRequestLimited = "device-request-limited"
    public static let signupRateLimited = "signup-rate-limited"
    /// `429` from `PUT /auth/login-handle` (`docs/ux/anti-abuse.md` section 2,
    /// added 2026-09-28): the account changed its username too often.
    public static let handleBindLimited = "handle-bind-limited"
    public static let limiterUnavailable = "rate-limiter-unavailable"

    // ADR-0061: the copy is read from the catalog when an error is shown,
    // in the interface language of that moment.

    /// Login, recovery login, reset, TOTP, three words, device authorize.
    public static var tooManyAttemptsCopy: String { Localization.text("common.error.too_many_attempts") }
    public static var recoveryCodeSendCopy: String { Localization.text("recovery.error.code_send_limited") }
    public static var deviceRequestCopy: String { Localization.text("device_link.error.request_limited") }
    public static var signupCopy: String { Localization.text("auth.signup.error.rate_limited") }
    /// The generic failure copy a `503 rate-limiter-unavailable` shows.
    public static var unavailableCopy: String { Localization.text("common.error.unavailable") }

    /// The refusal a `429` carries, or `nil` for any other response.
    public static func refusal(status: Int, body: String) -> Refusal? {
        guard status == 429 else { return nil }
        let object = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        let code = object?["error"] as? String ?? tooManyAttempts
        let retry = (object?["retry_after_secs"] as? NSNumber).map { $0.intValue }
        return Refusal(code: code, retryAfterSecs: retry)
    }

    /// The refusal inside `error`, or `nil` when it is not a limit.
    public static func refusal(in error: AuthClientError) -> Refusal? {
        guard case .http(let status, _, let body) = error else { return nil }
        return refusal(status: status, body: body)
    }

    /// Whether `error` is the auth `503 rate-limiter-unavailable`.
    public static func isLimiterUnavailable(_ error: AuthClientError) -> Bool {
        guard case .http(503, _, let body) = error else { return false }
        return body.contains(limiterUnavailable)
    }

    /// Choosing or changing a username, limit reached: `Try again in M
    /// minutes.` with M = ceil(retry_after_secs / 60), `Try again later.` when
    /// the wait is over an hour or unknown.
    public static func handleBindLimitedCopy(retryAfterSecs: Int?) -> String {
        let lead = "You have changed your username too often."
        guard let secs = retryAfterSecs, secs > 0, secs <= 3600 else {
            return lead + " Try again later."
        }
        let minutes = (secs + 59) / 60
        return lead + " Try again in \(minutes) " + (minutes == 1 ? "minute." : "minutes.")
    }

    /// The copy for one refusal.
    public static func copy(for refusal: Refusal) -> String {
        switch refusal.code {
        case handleBindLimited:
            return handleBindLimitedCopy(retryAfterSecs: refusal.retryAfterSecs)
        case recoveryCodeSendLimited:
            return recoveryCodeSendCopy
        case deviceRequestLimited:
            return deviceRequestCopy
        case signupRateLimited:
            return signupCopy
        default:
            // `too-many-attempts`, and any limit this client does not name yet.
            if let secs = refusal.retryAfterSecs, secs > 60 {
                let minutes = (secs + 59) / 60
                return Localization.text(
                    "common.error.too_many_attempts_minutes", counts: ["minutes": UInt64(minutes)])
            }
            return tooManyAttemptsCopy
        }
    }

    /// Whether `error` is `503 login-by-name-disabled` (ADR-0054 S5): the
    /// server has no handle pepper, so no sign-in or recovery by name works.
    /// Not a limit, but the same answer for the user: the generic failure
    /// copy, never a factor-specific "not available" line or a wrong
    /// credential.
    public static func isLoginByNameDisabled(_ error: AuthClientError) -> Bool {
        guard case .http(503, _, let body) = error else { return false }
        return body.contains(loginByNameDisabledCode)
    }

    /// The limit copy for `error`, or `nil` when `error` is not a limit (the
    /// caller then shows its own copy). `503 login-by-name-disabled` also
    /// answers here, with the generic failure copy.
    public static func copy(for error: AuthClientError) -> String? {
        if let refusal = refusal(in: error) { return copy(for: refusal) }
        if isLimiterUnavailable(error) || isLoginByNameDisabled(error) { return unavailableCopy }
        return nil
    }
}

/// What the sign-in, sign-up and recovery screens say when something fails
/// (ADR-0061: errors render from their code, never from a server or core
/// message). Mirror of the web `auth/auth-error-copy.ts`: an unknown
/// username and a wrong password reach the user as the same sentence.
enum AuthErrorCopy {

    private static func t(_ key: String) -> String { Localization.text(key) }

    static var offline: String { t("common.error.offline") }
    static var unavailable: String { t("common.error.unavailable") }

    /// The answer every flow shares: a server limit, no connection, or the
    /// server failing. `nil` when the flow decides.
    static func shared(_ error: Error) -> String? {
        if let err = error as? AuthClientError {
            if let limit = AuthRateLimit.copy(for: err) { return limit }
            switch err {
            case .network: return offline
            case .http(let status, _, _) where (500...599).contains(status): return unavailable
            default: return nil
            }
        }
        if let err = error as? DiscoveryClientError {
            switch err {
            case .network: return offline
            case .rateLimited: return AuthRateLimit.tooManyAttemptsCopy
            case .http(429, _, _): return AuthRateLimit.tooManyAttemptsCopy
            case .http(let status, _, _) where (500...599).contains(status): return unavailable
            default: return nil
            }
        }
        return nil
    }

    /// The reason a form refused a name before any request, or `nil`.
    static func rule(_ error: Error) -> String? {
        if let err = error as? UsernameRuleViolation { return err.reason.message }
        if let err = error as? DisplayNameRuleViolation { return err.message }
        return nil
    }

    private static func status(_ error: Error) -> Int? {
        if case .http(let status, _, _) = error as? AuthClientError { return status }
        return nil
    }

    /// The login form.
    static func login(_ error: Error) -> String {
        if let common = shared(error) { return common }
        if let rule = rule(error) { return rule }
        return t("auth.error.wrong_credentials")
    }

    /// The passkey button on the login form.
    static func passkey(_ error: Error) -> String {
        if case .cancelled = error as? WebauthnAssertion.AssertionError {
            return t("auth.passkey.error.cancelled")
        }
        if let common = shared(error) { return common }
        return t("auth.passkey.error.generic")
    }

    /// The sign-up form.
    static func signup(_ error: Error) -> String {
        if let common = shared(error) { return common }
        if let rule = rule(error) { return rule }
        if error is UsernameTaken || status(error) == 409 {
            return t("auth.signup.error.username_taken")
        }
        return t("auth.signup.error.generic")
    }

    /// The recovery forms that take the phrase (or the shares) and a new
    /// password. A wrong phrase and an unknown username are one sentence.
    static func recover(_ error: Error) -> String {
        if case .Mnemonic = error as? FfiError { return t("restore_identity.error.invalid_phrase") }
        if let common = shared(error) { return common }
        if let rule = rule(error) { return rule }
        switch status(error) {
        case 400, 401, 404: return t("recovery.error.no_match")
        default: return t("recovery.error.generic")
        }
    }

    /// The authenticator recovery. `typedUsername` picks the wrong-code
    /// sentence that also names the username.
    static func totp(_ error: Error, typedUsername: Bool) -> String {
        if let common = shared(error) { return common }
        if let rule = rule(error) { return rule }
        switch status(error) {
        case 401:
            return t(typedUsername ? "recovery.totp.error.wrong_username" : "recovery.totp.error.wrong")
        case 503: return t("recovery.totp.error.unavailable")
        default: return t("recovery.totp.error.generic")
        }
    }

    /// The email or phone recovery: asking for the code (`sending`) or
    /// checking it.
    static func contact(_ error: Error, sending: Bool) -> String {
        if let common = shared(error) { return common }
        if let rule = rule(error) { return rule }
        switch status(error) {
        case 400: return t("recovery.contact.error.bad_contact")
        case 401: return t("recovery.contact.error.wrong")
        case 503: return t("recovery.contact.error.unavailable")
        default: return t(sending ? "recovery.contact.error.send_failed" : "recovery.contact.error.generic")
        }
    }

    /// The identity checks of a restore (ADR-0017): a typed phrase or the
    /// identity from another device must match the published identity.
    private static func identityCheck(_ error: Error, fromDevice: Bool) -> String? {
        if error is PhraseMismatch {
            return t(fromDevice ? "device_link.error.identity_mismatch" : "restore_identity.error.phrase_mismatch")
        }
        if let err = error as? IdentityVerificationUnavailable {
            switch err.kind {
            case .missing: return t("restore_identity.error.not_published")
            case .unreachable: return t("restore_identity.error.unreachable")
            }
        }
        return nil
    }

    /// The typed-phrase restore on a device that has no identity yet.
    static func identityRestore(_ error: Error) -> String {
        if case .Mnemonic = error as? FfiError { return t("restore_identity.error.invalid_phrase") }
        if let check = identityCheck(error, fromDevice: false) { return check }
        if let common = shared(error) { return common }
        return t("restore_identity.error.generic")
    }

    /// The another-device restore, on the new device.
    static func deviceRestore(_ error: Error) -> String {
        if let check = identityCheck(error, fromDevice: true) { return check }
        if let refusal = error as? TypedNameCheckRefusal { return refusal.errorDescription ?? offline }
        if error is DeviceRestoreNotLetIn { return t("device_link.error.not_let_in") }
        if let common = shared(error) { return common }
        return t("device_link.error.generic")
    }
}

/// The other device sent the identity but the sign-in grant it minted
/// could not be redeemed, so this device has no access (ADR-0018 factor
/// (a), path (b)).
struct DeviceRestoreNotLetIn: Error {}
