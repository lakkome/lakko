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
        // One whole sentence per case (batch 6), the minutes a plural argument.
        guard let secs = retryAfterSecs, secs > 0, secs <= 3600 else {
            return Localization.text("username.error.bind_limited_later")
        }
        let minutes = (secs + 59) / 60
        return Localization.text(
            "username.error.bind_limited_minutes", counts: ["minutes": UInt64(minutes)])
    }

    /// The copy for one refusal.
    public static func copy(for refusal: Refusal) -> String {
        switch refusal.code {
        case handleBindLimited:
            return handleBindLimitedCopy(retryAfterSecs: refusal.retryAfterSecs)
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
    ///
    /// Only a refused password says "wrong username or password": the OPAQUE
    /// exchange refused on the device (``PasswordLoginRejected``), the
    /// server's `401`, or an empty field. Anything else (a malformed answer,
    /// a failed key write, an error nobody named) is not the person's typing
    /// and shows the generic failure.
    static func login(_ error: Error) -> String {
        if let common = shared(error) { return common }
        if let rule = rule(error) { return rule }
        if isWrongCredentials(error) { return t("auth.error.wrong_credentials") }
        return unavailable
    }

    /// Whether a password login failed because of what was typed.
    static func isWrongCredentials(_ error: Error) -> Bool {
        if error is PasswordLoginRejected || error is LoginFieldsEmpty { return true }
        return status(error) == 401
    }

    /// A plain-text description of a failed login for the device log: the
    /// error's type and case, the HTTP status and endpoint path when there is
    /// one. Never a message, a body, a username or a secret.
    static func loginFailureLogLine(_ error: Error) -> String {
        let kind = errorKind(error)
        var line = "error=\(kind)"
        if case .http(let status, let endpoint, _) = error as? AuthClientError {
            line += " status=\(status) endpoint=\(endpoint)"
        } else if case .network(let endpoint, let underlying) = error as? AuthClientError {
            line += " status=none endpoint=\(endpoint) underlying=\(errorKind(underlying))"
            if let url = underlying as? URLError { line += " url_error=\(url.code.rawValue)" }
        } else {
            line += " status=none"
        }
        return line
    }

    /// `Type.case` for an enum error, `Type` otherwise. Reads only the case
    /// label, never an associated value.
    static func errorKind(_ error: Error) -> String {
        let type = String(describing: Swift.type(of: error))
        let mirror = Mirror(reflecting: error)
        // A case without a payload has no child to read its label from, and
        // its description may be custom text: the type alone is logged.
        guard mirror.displayStyle == .enum, let label = mirror.children.first?.label else { return type }
        return "\(type).\(label)"
    }

    /// Whether the person dismissed the passkey sheet: not an error, the
    /// form goes back to idle.
    static func isPasskeyCancelled(_ error: Error) -> Bool {
        if case .cancelled = error as? WebauthnAssertion.AssertionError { return true }
        if case .cancelled = error as? WebauthnRegistration.RegistrationError { return true }
        return false
    }

    /// Whether passkeys are switched off on the server (`503`).
    static func isPasskeyServerDisabled(_ error: Error) -> Bool {
        if case .http(503, _, _) = error as? WebauthnClientError { return true }
        return false
    }

    /// A passkey sign-in: the Login button, the recovery list's row and the
    /// restore prompt's row.
    static func passkey(_ error: Error) -> String {
        if isPasskeyCancelled(error) { return t("auth.passkey.error.cancelled") }
        if error is PasskeyWrongAccount { return t("auth.passkey.error.wrong_account") }
        if error is RestoreNothingPending { return t("restore_identity.error.nothing") }
        if let err = error as? WebauthnClientError {
            switch err {
            case .http(503, _, _): return t("auth.passkey.error.server_disabled")
            case .http(404, _, _): return t("auth.passkey.error.not_registered")
            case .http(let status, _, _):
                return Localization.text("auth.passkey.error.failed_status", texts: ["status": String(status)])
            case .network: return t("auth.passkey.error.network")
            case .argument, .decode: return t("auth.passkey.error.generic")
            }
        }
        if let check = identityCheck(error, fromDevice: false) { return check }
        if let common = shared(error) { return common }
        return t("auth.passkey.error.generic")
    }

    /// The sign-in that follows a three-word match (the words themselves
    /// were right, so none of this is a wrong guess).
    static func threeWords(_ error: Error) -> String {
        if case .rejected = error as? ThreeWordsError { return t("recovery.words.error.rejected") }
        if case .suspended = error as? ThreeWordsError { return t("recovery.words.error.rejected") }
        if case .unavailable = error as? ThreeWordsError { return t("recovery.words.error.unavailable") }
        switch status(error) {
        // The phrase stored here is no longer the account's recovery
        // credential (a new identity was created on another device).
        case 401, 404: return t("recovery.words.error.rejected")
        default: break
        }
        if case .network = error as? AuthClientError { return offline }
        if let err = error as? AuthClientError, let limit = AuthRateLimit.copy(for: err) { return limit }
        if let rule = rule(error) { return rule }
        return t("recovery.words.error.generic")
    }

    /// "Choose a new password" and the Settings password form.
    static func newPassword(_ error: Error) -> String {
        if case .network = error as? AuthClientError { return offline }
        if let err = error as? AuthClientError, let limit = AuthRateLimit.copy(for: err) { return limit }
        return t("recovery.new_password.error.generic")
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

    /// `Create account` on a guest account (``GuestAccountClaim``).
    ///
    /// The step that sets the password is proved by the keys this device
    /// holds; the person typed no phrase, so its failures collapse into the
    /// form's generic sentence.
    static func guestClaim(_ error: Error) -> String {
        if error is GuestPhraseUnavailable { return t("guest.claim.error.phrase_missing") }
        if let common = shared(error) { return common }
        if let limited = error as? UsernameBindLimited {
            return AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: limited.retryAfterSecs)
        }
        if error is UsernameTaken || status(error) == 409 {
            return t("auth.signup.error.username_taken")
        }
        if let rule = rule(error) { return rule }
        return t("guest.claim.error.generic")
    }

    /// The recovery forms that take the phrase and a new
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
        // ADR-0056 — a join by name that landed in another account's
        // DeviceGroup, or whose grant could not be redeemed.
        if let join = error as? PairingOrchestrator.JoinByNameError {
            switch join {
            case .wrongAccount: return t("device_link.error.wrong_account")
            case .noAccess: return t("device_link.error.not_let_in")
            }
        }
        if let common = shared(error) { return common }
        return t("device_link.error.generic")
    }
}

/// The other device sent the identity but the sign-in grant it minted
/// could not be redeemed, so this device has no access (ADR-0018 factor
/// (a), path (b)).
struct DeviceRestoreNotLetIn: Error {}
