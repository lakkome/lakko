// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// A typed username turned into what the pre-session auth routes take
/// (ADR-0054 S5).
///
/// Since S5 sign-in and every recovery factor name the account by its login
/// handle, `BLAKE3-derive-key("lakko/auth/login-handle/v1", canonical)`,
/// computed by the core (`loginHandle`). No discovery lookup runs before a
/// session: a lookup spends an anonymous token, and a token needs a session.
/// The account id reaches the client only in a finish response, after a
/// successful proof. A name nobody holds gets the ordinary wrong-password /
/// wrong-code outcome, never "no such user".
public struct LoginName: Equatable, Sendable {
    /// The canonical username (lowercase, no `@`) - what the marker stores
    /// and the screens show.
    public let canonical: String
    /// The 32-byte handle.
    public let handle: Data

    /// The wire form: base64url without padding.
    public var wire: String { base64UrlEncodeNoPad(handle) }

    public init(canonical: String, handle: Data) {
        self.canonical = canonical
        self.handle = handle
    }

    /// Derive the handle for `typed`.
    ///
    /// The username rule runs first (``requireValidUsername(_:emptyError:validate:)``),
    /// so a non-conforming name is refused with its reason copy before any
    /// request; an empty field raises `emptyError`.
    ///
    /// - Parameters:
    ///   - validate: the core's `validateUsername` in production.
    ///   - derive: the core's `loginHandle` in production. Its own username
    ///     refusals (it applies the same rule) map to the same reason copy.
    static func make(
        _ typed: String,
        emptyError: @autoclosure () -> Error,
        validate: (String) throws -> String = { try validateUsername(input: $0) },
        derive: (String) throws -> Data = { try loginHandle(username: $0) }
    ) throws -> LoginName {
        let canonical = try requireValidUsername(typed, emptyError: emptyError(), validate: validate)
        let handle: Data
        do {
            handle = try derive(canonical)
        } catch let error as FfiError {
            switch checkUsername(canonical, validate: { _ in throw error }) {
            case .empty: throw emptyError()
            case .invalid(let reason): throw UsernameRuleViolation(reason: reason)
            case .valid: throw UsernameRuleViolation(reason: .invalidCharacters)
            }
        }
        guard handle.count == loginHandleLength else {
            throw AuthClientError.argument("login handle must be \(loginHandleLength) bytes")
        }
        return LoginName(canonical: canonical, handle: handle)
    }
}

/// The login handle is 32 bytes.
public let loginHandleLength = 32

/// `503 {"error":"login-by-name-disabled"}`: the auth service has no handle
/// pepper, so no route that names an account by handle works. Shown with the
/// generic failure copy (``AuthRateLimit/copy(for:)``).
public let loginByNameDisabledCode = "login-by-name-disabled"

/// `409 {"error":"handle-taken"}` from `PUT /auth/login-handle`: another
/// account holds the handle of this name.
public let handleTakenCode = "handle-taken"

/// Minimal shape of the locally-cached `(accountId, username)` record.
struct CachedAccount {
    let accountIdB64: String
    let username: String
}

/// Which username a sign-in or a recovery names.
///
/// The typed name wins; an empty field falls back to the name of the account
/// cached on this device. A guest account (ADR-0032) caches an empty name and
/// has no handle, so it cannot sign in or recover by name: `emptyError` is
/// raised, as for a fresh device with nothing typed.
func signInName(
    typed: String?,
    cached: CachedAccount?,
    emptyError: @autoclosure () -> Error,
    validate: (String) throws -> String = { try validateUsername(input: $0) },
    derive: (String) throws -> Data = { try loginHandle(username: $0) }
) throws -> LoginName {
    let trimmed = typed?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    let name = trimmed.isEmpty ? (cached?.username ?? "") : trimmed
    guard !name.isEmpty else { throw emptyError() }
    return try LoginName.make(name, emptyError: emptyError(), validate: validate, derive: derive)
}

/// Which `(session, username)` pairs this process has already bound the login
/// handle for (ADR-0054 S5).
///
/// The handle is bound at signup and re-bound after a sign-in by name, but a
/// session can land on other paths too - a relaunch restore, a passkey, a
/// device pairing or another-device confirmation, a phrase completion - and
/// an account whose handle was never bound cannot sign in by name after a
/// logout. ``AuthRepository`` therefore re-binds on every landing, and this
/// ledger keeps it to one `PUT` per pair per process: a pair is claimed when
/// its bind is fired, not when it succeeds, so a failed bind is retried by
/// the next session or the next launch, never in a loop.
///
/// Process-wide (``shared``) because the app may build more than one
/// repository; tests pass their own.
final class LoginHandleBindLedger: @unchecked Sendable {
    /// The ledger the app uses.
    static let shared = LoginHandleBindLedger()

    private let lock = NSLock()
    private var claimed: Set<String> = []

    /// `true` the first time `(sessionToken, canonical)` is seen, `false`
    /// on every later call.
    func claim(sessionToken: String, canonical: String) -> Bool {
        lock.withLock { claimed.insert("\(canonical)\n\(sessionToken)").inserted }
    }
}

/// The login form was submitted with an empty username or password. Shown as
/// the wrong-credentials sentence (nothing was sent).
public struct LoginFieldsEmpty: Error, Equatable {
    public init() {}
}
