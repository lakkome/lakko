// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073, amended 2026-10-06: the fresh-auth proof.
//
// Resetting the account keys, deleting the account, adding a device, adding
// a passkey and changing the password each need the person to prove who they
// are NOW (the password or a passkey, within ten minutes). The server
// refuses the gated routes with `403 {"error":"fresh-auth-required"}`
// without a fresh-auth grant, which only the password sign-in, the passkey
// sign-in and the signup finish mint (`fresh_grant` +
// `fresh_grant_expires_at_ms`). One grant serves any number of the actions
// within its lifetime, except that a key reset spends every grant of the old
// generation.
//
// This file is the client side, free of I/O:
//
//   * `FreshAuthGrant`       the opaque grant and its expiry, as issued;
//   * `FreshAuthHolder`      where it lives: in memory only, bound to the
//                            account it was issued for;
//   * `SessionLanding`       how this session was landed (it decides which
//                            row of the reset screen opens first);
//   * `FreshAuthRefusal`     the answers a surface tells apart;
//   * `KeyResetProofGate`    what the reset warning screen shows and enables;
//   * `PasswordChangeGate`   whether a password change may run.
//
// UX: `docs/ux/identity-recovery.md` section 17 and 15.1a.

/// A fresh-auth grant as a sign-in's finish response carries it. The string
/// is opaque to the client and is never parsed, stored or logged.
public struct FreshAuthGrant: Equatable, Sendable {
    /// The opaque grant, sent back verbatim as `fresh_grant`.
    public let grant: String
    /// Absolute unix-millisecond expiry.
    public let expiresAtMs: Int64

    public init(grant: String, expiresAtMs: Int64) {
        self.grant = grant
        self.expiresAtMs = expiresAtMs
    }

    /// The grant of a finish response, or `nil` when the response carried
    /// none or carried half of one.
    static func from(grant: String?, expiresAtMs: Int64?) -> FreshAuthGrant? {
        guard let grant, !grant.isEmpty, let expiresAtMs else { return nil }
        return FreshAuthGrant(grant: grant, expiresAtMs: expiresAtMs)
    }
}

/// The one fresh-auth proof this process holds, in memory only.
///
/// Never written to durable storage and never logged: a relaunch holds no
/// proof, and a surface then asks for the password. The proof is bound to
/// the account it was issued for; it is dropped on sign-out, on the account
/// wipe, when a key reset spent it, when the server refused it, and when it
/// is read past its expiry.
struct FreshAuthHolder {
    private var held: (grant: FreshAuthGrant, accountIdB64: String)?

    /// Keep `grant` for `accountIdB64`, replacing whatever was held. A `nil`
    /// grant (a sign-in that mints none) leaves the holder as it was only
    /// when it holds a grant of the SAME account; a sign-in to another
    /// account drops the held one.
    mutating func put(_ grant: FreshAuthGrant?, accountIdB64: String) {
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
    /// mutate, for a screen's decision.
    func holdsLive(accountIdB64: String, nowMs: Int64) -> Bool {
        guard let held, held.accountIdB64 == accountIdB64 else { return false }
        return held.grant.expiresAtMs > nowMs
    }

    /// Drop the proof: a key reset spent it, the server refused it, or the
    /// account signed out or was wiped.
    mutating func clear() {
        held = nil
    }
}

/// How the current session was landed, kept in memory with the session
/// (UX 17.6). It decides which row of the reset screen opens first: a
/// person who came in with the authenticator code and has no passkey is
/// shown the "cannot reset" panel before any field.
public enum SessionLanding: Equatable, Sendable {
    case password
    case passkey
    case authenticator
    /// Three words, another device, a relaunch restore, signup.
    case other
}

/// An action was asked for without a live proof, or the server answered
/// `403 fresh-auth-required` (the proof expired on the way). Nothing was
/// done. The surface shows its proof block; this is not an error to style.
public struct FreshAuthRequired: Error, Equatable {
    public init() {}
}

/// The password typed into a proof block is wrong. Nothing was done.
public struct FreshAuthWrongPassword: Error, Equatable {
    public init() {}
}

/// The password sign-in behind a proof needs the account's username and the
/// device knows none (a passkey sign-in that named no account), and the one
/// typed is empty or refused by the username rule. Nothing was sent.
public struct FreshAuthUsernameRequired: Error, Equatable {
    public init() {}
}

/// The passkey that answered a proof belongs to another account. Nothing of
/// that sign-in was installed.
public struct FreshAuthPasskeyOtherAccount: Error, Equatable {
    public init() {}
}

/// No account is signed in (or parked on the restore prompt) to prove for.
public struct FreshAuthNoAccount: Error, Equatable {
    public init() {}
}

/// Reads the refusals out of what a client threw.
enum FreshAuthRefusal {

    /// The body code of the refusal.
    static let code = "fresh-auth-required"

    /// `403 {"error":"fresh-auth-required"}` from a gated route: the grant
    /// is missing, malformed, expired, another account's or of a stale
    /// generation.
    static func isRequired(_ error: Error) -> Bool {
        if error is FreshAuthRequired { return true }
        let status: Int
        let body: String
        if case .http(let s, _, let b) = error as? AuthClientError {
            (status, body) = (s, b)
        } else if case .http(let s, _, let b) = error as? WebauthnClientError {
            (status, body) = (s, b)
        } else {
            return false
        }
        guard status == 403 else { return false }
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        return json?["error"] as? String == code
    }

    /// A wrong password (by design the same answer as an unknown account):
    /// the OPAQUE exchange refused on the device (``PasswordLoginRejected``,
    /// the usual case, since no finish is sent then) or the login's `401`.
    static func isWrongPassword(_ error: Error) -> Bool {
        if error is PasswordLoginRejected { return true }
        guard case .http(let status, _, _) = error as? AuthClientError else { return false }
        return status == 401
    }

    /// No connection reached the server.
    static func isOffline(_ error: Error) -> Bool {
        if case .network = error as? AuthClientError { return true }
        if case .network = error as? WebauthnClientError { return true }
        return false
    }
}

/// What the Reset account keys warning screen shows and enables (UX 15.1,
/// 15.1a as amended, 17.2), pure.
enum KeyResetProofGate {

    /// Which form the warning screen takes under the warning's bullets.
    enum Form: Equatable {
        /// A proof is live: the typed word and the button, no block.
        case noBlock
        /// The proof block, then the typed word and the button.
        case block
        /// The "cannot reset" panel in place of the block, the word and the
        /// button (signed in with the authenticator code, no usable passkey,
        /// no proof), until `I know my password` switches to the block.
        case cannotReset
    }

    static func form(
        holdsLiveProof: Bool,
        landing: SessionLanding?,
        hasUsablePasskey: Bool,
        choseKnowsPassword: Bool,
        choseNoPassword: Bool
    ) -> Form {
        if holdsLiveProof { return .noBlock }
        if hasUsablePasskey { return .block }
        if choseNoPassword { return .cannotReset }
        if landing == .authenticator, !choseKnowsPassword { return .cannotReset }
        return .block
    }

    /// Whether the link `I do not know my password` is under the block: only
    /// when no passkey can stand in for the password.
    static func showsNoPasswordLink(form: Form, hasUsablePasskey: Bool) -> Bool {
        form == .block && !hasUsablePasskey
    }

    /// Whether "Reset account keys" is enabled: the confirmation word
    /// matches and the block (when drawn) has what it needs.
    static func canSubmit(form: Form, blockReady: Bool, confirmation: String) -> Bool {
        guard KeyResetConfirmation.matches(confirmation) else { return false }
        switch form {
        case .noBlock: return true
        case .block: return blockReady
        case .cannotReset: return false
        }
    }
}

/// Whether a password change may run on a device that holds the keys (the
/// Settings Password card and "Choose a new password"). The server step is
/// authorised by the phrase seed this device holds, so the bare session
/// would be enough for it; the client gates it behind one of two proofs:
/// a live fresh-auth proof (the current password or a passkey), or the
/// local three-word check of the recovery phrase passed within the last ten
/// minutes (for the person who forgot the password).
enum PasswordChangeGate {

    /// How long a passed three-word check opens the gate.
    static let threeWordsWindowMs: Int64 = 10 * 60 * 1000

    static func threeWordsLive(passedAtMs: Int64?, nowMs: Int64) -> Bool {
        guard let passedAtMs else { return false }
        return nowMs >= passedAtMs && nowMs - passedAtMs < threeWordsWindowMs
    }

    /// - Parameter noticeAnsweredAtMs: when the person answered `It was not
    ///   me` to an account notice (ADR-0075, UX 19.6): the same ten minutes.
    static func allowed(
        holdsLiveProof: Bool, threeWordsPassedAtMs: Int64?, noticeAnsweredAtMs: Int64? = nil, nowMs: Int64
    ) -> Bool {
        holdsLiveProof || threeWordsLive(passedAtMs: threeWordsPassedAtMs, nowMs: nowMs)
            || threeWordsLive(passedAtMs: noticeAnsweredAtMs, nowMs: nowMs)
    }
}
