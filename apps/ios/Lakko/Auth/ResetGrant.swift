// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065, amended 2026-10-04: the password-fresh reset grant.
//
// `POST /account/keys/reset/{start,finish}` are refused (`403
// password-required`) without a reset grant, and only the PASSWORD login's
// finish mints one (ten minutes, one use). A stolen session, or an unlocked
// phone in someone else's hands, cannot reset the account keys.
//
// This file is the client side, free of I/O:
//
//   * `ResetGrant`           the opaque grant and its expiry, as issued;
//   * `ResetGrantHolder`     where it lives: in memory only, bound to the
//                            account it was issued for;
//   * `KeyResetRefusal`      the two answers the warning screen tells apart
//                            (the server wants the password; the password
//                            was wrong);
//   * `KeyResetPasswordGate` what the warning screen shows and enables.
//
// UX: `docs/ux/identity-recovery.md` section 15.1a.

/// A reset grant as the password login's finish response carries it. The
/// string is opaque to the client and is never parsed, stored or logged.
public struct ResetGrant: Equatable, Sendable {
    /// The opaque grant, sent back verbatim as `reset_grant`.
    public let grant: String
    /// Absolute unix-millisecond expiry.
    public let expiresAtMs: Int64

    public init(grant: String, expiresAtMs: Int64) {
        self.grant = grant
        self.expiresAtMs = expiresAtMs
    }

    /// The grant of a login finish, or `nil` when the response carried none
    /// (every route but the password login) or carried half of one.
    static func from(grant: String?, expiresAtMs: Int64?) -> ResetGrant? {
        guard let grant, !grant.isEmpty, let expiresAtMs else { return nil }
        return ResetGrant(grant: grant, expiresAtMs: expiresAtMs)
    }
}

/// The one reset grant this process holds, in memory only.
///
/// Never written to durable storage and never logged: a relaunch holds no
/// grant, and the warning screen then asks for the password. The grant is
/// bound to the account it was issued for; it is dropped on sign-out, on
/// the account wipe, when a reset used it, and when it is read past its
/// expiry.
struct ResetGrantHolder {
    private var held: (grant: ResetGrant, accountIdB64: String)?

    /// Keep `grant` for `accountIdB64`, replacing whatever was held. A `nil`
    /// grant (a sign-in that mints none) leaves the holder as it was only
    /// when it holds a grant of the SAME account; a sign-in to another
    /// account drops the held one.
    mutating func put(_ grant: ResetGrant?, accountIdB64: String) {
        if let grant {
            held = (grant, accountIdB64)
        } else if let held, held.accountIdB64 != accountIdB64 {
            self.held = nil
        }
    }

    /// The grant to send for `accountIdB64` at `nowMs`, or `nil` when none
    /// is held, the held one belongs to another account, or it expired (an
    /// expired grant is dropped by this read).
    mutating func live(accountIdB64: String, nowMs: Int64) -> String? {
        guard let held, held.accountIdB64 == accountIdB64 else { return nil }
        guard held.grant.expiresAtMs > nowMs else {
            self.held = nil
            return nil
        }
        return held.grant.grant
    }

    /// Whether ``live(accountIdB64:nowMs:)`` would return a grant. Does not
    /// mutate, for the screen's decision.
    func holdsLive(accountIdB64: String, nowMs: Int64) -> Bool {
        guard let held, held.accountIdB64 == accountIdB64 else { return false }
        return held.grant.expiresAtMs > nowMs
    }

    /// Drop the grant: it was used (a successful reset spends it and every
    /// grant minted before it), the server refused it, or the account
    /// signed out or was wiped.
    mutating func clear() {
        held = nil
    }
}

/// A reset was asked for without a live grant and without a password, or
/// the server answered `403 password-required` (the grant expired on the
/// way). Nothing was reset. The warning screen reveals its password field;
/// this is not an error to style.
public struct KeyResetPasswordRequired: Error, Equatable {
    public init() {}
}

/// The password typed on the warning screen is wrong. Nothing was reset.
public struct KeyResetWrongPassword: Error, Equatable {
    public init() {}
}

/// The password sign-in in front of a reset needs the account's username and
/// the device knows none (a passkey sign-in that named no account), and the
/// one typed on the warning screen is empty or refused by the username rule.
/// Nothing was sent and nothing was reset.
public struct KeyResetUsernameRequired: Error, Equatable {
    public init() {}
}

/// Reads the two refusals out of what the auth client threw.
enum KeyResetRefusal {

    /// `403 {"error":"password-required"}` from either reset call: the grant
    /// is missing, malformed, expired, another account's or of a stale
    /// generation.
    static func isPasswordRequired(_ error: Error) -> Bool {
        guard case .http(let status, _, let body) = error as? AuthClientError, status == 403 else {
            return false
        }
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        return json?["error"] as? String == "password-required"
    }

    /// The password login's `401`: a wrong password (by design the same
    /// answer as an unknown account).
    static func isWrongPassword(_ error: Error) -> Bool {
        guard case .http(let status, _, _) = error as? AuthClientError else { return false }
        return status == 401
    }
}

/// What the Reset account keys warning screen shows and enables (UX 15.1 +
/// 15.1a), pure.
enum KeyResetPasswordGate {

    /// Whether the `Password` field is on screen: no live grant is held
    /// (signed in another way, the grant expired, the app was restarted),
    /// or a reset was refused for the password and revealed it.
    static func showsPasswordField(holdsLiveGrant: Bool, revealed: Bool) -> Bool {
        revealed || !holdsLiveGrant
    }

    /// Whether the username field is on screen, above the password: only
    /// with the password field, and only when the device does not know the
    /// account's username (a passkey sign-in that named no account). The
    /// password sign-in names the account by the handle of its username.
    static func showsUsernameField(showsPasswordField: Bool, usernameKnown: Bool) -> Bool {
        showsPasswordField && !usernameKnown
    }

    /// Whether "Reset account keys" is enabled: the confirmation word
    /// matches and every field that is shown is filled (the password; the
    /// username, ignoring surrounding whitespace).
    static func canSubmit(
        showsPasswordField: Bool,
        showsUsernameField: Bool = false,
        password: String,
        username: String = "",
        confirmation: String
    ) -> Bool {
        guard KeyResetConfirmation.matches(confirmation) else { return false }
        if showsPasswordField, password.isEmpty { return false }
        if showsUsernameField,
           username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return false }
        return true
    }
}
