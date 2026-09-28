// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

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
    public static let limiterUnavailable = "rate-limiter-unavailable"

    /// Login, recovery login, reset, TOTP, three words, device authorize.
    public static let tooManyAttemptsCopy = "Too many attempts. Wait a moment, then try again."
    public static let recoveryCodeSendCopy =
        "We sent a code recently. Wait a few minutes before asking for another."
    public static let deviceRequestCopy =
        "Too many requests to your other devices. Try again in a few minutes."
    public static let signupCopy = "Too many new accounts from this network right now. Try again later."
    /// The generic failure copy a `503 rate-limiter-unavailable` shows.
    public static let unavailableCopy = "Lakko is having trouble right now. Try again in a moment."

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

    /// The copy for one refusal.
    public static func copy(for refusal: Refusal) -> String {
        switch refusal.code {
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
                return "Too many attempts. Try again in \(minutes) minutes."
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
