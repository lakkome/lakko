// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073, amended 2026-11-26 / UX `identity-recovery.md` 15.1b and 15.1c —
// a key reset needs the password AND one second factor the account has: a
// code from its authenticator app, a code mailed to its recovery email, or
// one of its passkeys. The recovery phrase is not one of them (whoever has
// it restores the keys and has nothing to reset).
//
// This file is the pure part:
//
//   * `ResetFactorKind`          the three factors, in their fixed order;
//   * `ResetFactorSection`       what the reset panel draws: the section,
//                                or the "cannot reset" panel;
//   * `ResetFactorConfirmation`  a confirmed factor: its grant, for ten
//                                minutes;
//   * `ResetFactorRefusal`       the answers of the factor routes;
//   * `ResetSealLine`            the line between the confirmed factor and
//                                the word: will this reset bring things
//                                back (15.1c).
//
// The routes are in `AuthClient` (`resetFactorTotp` and its siblings); the
// state machine over them is the reset screen's model.

/// One second factor of a key reset.
public enum ResetFactorKind: String, Equatable, CaseIterable, Sendable {
    case authenticator
    case email
    case passkey

    /// `proof.reset.factor.done.*`: the one line the section collapses to.
    public var doneKey: String {
        switch self {
        case .authenticator: return "proof.reset.factor.done.totp"
        case .email: return "proof.reset.factor.done.email"
        case .passkey: return "proof.reset.factor.done.passkey"
        }
    }
}

/// What the reset panel shows under the warning (UX 15.1b), pure.
public enum ResetFactorSection {

    public enum Form: Equatable {
        /// The section: the password block (absent while a password proof
        /// is live), then the rows of the factors the account has.
        case section(rows: [ResetFactorKind], showsPassword: Bool, titleKey: String)
        /// The "cannot reset" panel in place of the section, the typed word
        /// and the button.
        case cannotReset(bodyKey: String, showsBack: Bool)
        /// The account's factors are not known yet (the device is asking).
        case loading
    }

    public static let titleKey = "proof.reset.factor.title"
    public static let titleNoPasswordKey = "proof.reset.factor.title_no_password"
    public static let cannotTitleKey = "proof.reset.no_factor.title"
    public static let cannotBodyKey = "proof.reset.no_factor.body"
    public static let cannotBodyNoneKey = "proof.reset.no_factor.body_none"

    /// The rows: the factors THIS account has, in the fixed order
    /// authenticator, email, passkey. A row the account does not have is
    /// absent, never disabled. The passkey row is also absent where this
    /// device cannot use passkeys.
    public static func rows(factors: AccountFactors, passkeysSupported: Bool) -> [ResetFactorKind] {
        var rows: [ResetFactorKind] = []
        if factors.authenticator { rows.append(.authenticator) }
        if factors.email { rows.append(.email) }
        if factors.passkey, passkeysSupported { rows.append(.passkey) }
        return rows
    }

    /// - Parameters:
    ///   - factors: the account's factors; `nil` while unknown.
    ///   - passwordProofLive: a PASSWORD sign-in in the last ten minutes (a
    ///     passkey sign-in does not stand in for the password here).
    ///   - choseNone: the person pressed `I cannot use any of these`.
    public static func form(
        factors: AccountFactors?, passkeysSupported: Bool, passwordProofLive: Bool, choseNone: Bool
    ) -> Form {
        guard let factors else { return .loading }
        let rows = rows(factors: factors, passkeysSupported: passkeysSupported)
        if rows.isEmpty {
            // None of the three (or only a passkey this device cannot use):
            // no way back to the section, so no `Back`.
            return .cannotReset(
                bodyKey: factors.any ? cannotBodyKey : cannotBodyNoneKey, showsBack: false)
        }
        if choseNone { return .cannotReset(bodyKey: cannotBodyKey, showsBack: true) }
        return .section(
            rows: rows, showsPassword: !passwordProofLive,
            titleKey: passwordProofLive ? titleNoPasswordKey : titleKey)
    }

    /// `Reset account keys` is enabled only when a factor is confirmed and
    /// the word is typed.
    public static func canReset(form: Form, factorConfirmed: Bool, confirmation: String) -> Bool {
        guard case .section = form else { return false }
        return factorConfirmed && KeyResetConfirmation.matches(confirmation)
    }
}

/// A confirmed factor: the reset factor grant the server minted. In memory
/// only, never stored.
public struct ResetFactorConfirmation: Equatable, Sendable {
    public let kind: ResetFactorKind
    public let grant: String
    public let expiresAtMs: Int64

    public init(kind: ResetFactorKind, grant: String, expiresAtMs: Int64) {
        self.kind = kind
        self.grant = grant
        self.expiresAtMs = expiresAtMs
    }

    /// A confirmation lasts ten minutes; past it the section opens again as
    /// it was, with no error styling (nothing went wrong).
    public func live(nowMs: Int64) -> Bool { expiresAtMs > nowMs }
}

/// What a factor route answered (ADR-0073, "As built in the services").
public enum ResetFactorRefusal: Equatable {
    /// `401 factor-refused`: a wrong or used code, a decoy or spent
    /// challenge, no such factor on the account.
    case refused
    /// `403 fresh-auth-required`: the password proof is missing, expired or
    /// was minted by a passkey sign-in. The password is asked for again.
    case passwordRequired
    /// `403 reset-factor-required` from the reset itself: the confirmation
    /// expired on the way.
    case factorRequired
    /// `429 email-rate-limited`.
    case lettersLimited(retryAfterSecs: Int?)
    /// Any other `429`.
    case tooManyAttempts(retryAfterSecs: Int?)
    /// `410`: the mailed code expired.
    case codeExpired
    /// `502`: the letter could not be sent.
    case mailSendFailed
    /// `503` on an email route: email codes are not available.
    case unavailable
    case offline
    case failed

    public static let factorRefusedCode = "factor-refused"
    public static let factorRequiredCode = "reset-factor-required"

    public static func of(status: Int, body: String) -> ResetFactorRefusal {
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        let code = json?["error"] as? String
        let retry = (json?["retry_after_secs"] as? NSNumber)?.intValue
        switch (status, code) {
        case (401, _): return .refused
        case (403, factorRequiredCode): return .factorRequired
        case (403, FreshAuthRefusal.code): return .passwordRequired
        case (410, _): return .codeExpired
        case (429, "email-rate-limited"): return .lettersLimited(retryAfterSecs: retry)
        case (429, _): return .tooManyAttempts(retryAfterSecs: retry)
        case (502, _): return .mailSendFailed
        case (503, "rate-limiter-unavailable"): return .failed
        case (503, _): return .unavailable
        default: return .failed
        }
    }

    public static func of(_ error: Error) -> ResetFactorRefusal {
        if error is FreshAuthRequired { return .passwordRequired }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return of(status: status, body: body)
        case .network: return .offline
        default: break
        }
        if case .network = error as? WebauthnClientError { return .offline }
        return .failed
    }

    /// `403 reset-factor-required` from the reset routes.
    public static func isFactorRequired(_ error: Error) -> Bool { of(error) == .factorRequired }
}

/// UX 15.1c — the line between the confirmed factor and `Type RESET to
/// confirm`, decided on the device (``ResetSealPreview``); it replaces the
/// warning's "Chats, groups and channels come back..." bullet.
public enum ResetSealLine: Equatable {
    /// `restore_identity.reset.sealed`.
    case sealed
    /// `restore_identity.reset.unsealed`, in the warning colour.
    /// `offersPasskey`: the account has a passkey that keeps the chats and
    /// it was not the factor used: `restore_identity.reset.unsealed_use_passkey`
    /// with `Use passkey` follows.
    case unsealed(offersPasskey: Bool)

    public static let sealedKey = "restore_identity.reset.sealed"
    public static let unsealedKey = "restore_identity.reset.unsealed"
    public static let unsealedUsePasskeyKey = "restore_identity.reset.unsealed_use_passkey"

    /// - Parameters:
    ///   - sealed: what was just proven matches a seal the account
    ///     published.
    ///   - announcesPasskey: the account's announcement lists a passkey that
    ///     keeps the chats.
    ///   - passkeysSupported: this device can use passkeys.
    public static func decide(sealed: Bool, announcesPasskey: Bool, passkeysSupported: Bool) -> ResetSealLine {
        sealed ? .sealed : .unsealed(offersPasskey: announcesPasskey && passkeysSupported)
    }
}

public extension FreshAuthGrant {
    /// How the grant was minted, read from its text
    /// (`k1.<account>.<generation>.<method>.<expires>.<tag>`): `p` a
    /// password sign-in or the sign-up, `w` a passkey sign-in. `nil` for a
    /// grant of another shape.
    var method: Character? {
        let parts = grant.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 6, parts[0] == "k1", parts[3].count == 1 else { return nil }
        return parts[3].first
    }

    /// A password sign-in (or the sign-up) minted it: the only kind the
    /// reset's factor routes accept.
    var isPasswordMinted: Bool { method == "p" }
}

/// The reset's proof, as the reset screen hands it to
/// ``AuthRepository/mintFreshIdentity(proof:)``: the confirmed factor, and
/// what the device found about the seal before the word was typed.
public struct KeyResetProof {
    /// The confirmed second factor (its grant is the reset's).
    public let factor: ResetFactorConfirmation
    /// The seal key that will sign the reset, with the announcement that
    /// lists it; `nil` for an unsealed reset.
    let sealed: ResetSealPreview.Sealed?
    /// The account's announcement lists a passkey that keeps the chats.
    public let announcesPasskey: Bool
    /// Which owned-channels lock what sealed the reset opens; `nil` when
    /// nothing sealed it (the owned channels do not come back).
    let ownedChannelsLock: OwnedChannelsRules.ResetLock?
    /// The export key of the password just proven, when one was.
    let passwordExportKey: Data?

    public var isSealed: Bool { sealed != nil }

    /// The line of UX 15.1c.
    public func line(passkeysSupported: Bool) -> ResetSealLine {
        ResetSealLine.decide(
            sealed: isSealed, announcesPasskey: announcesPasskey, passkeysSupported: passkeysSupported)
    }
}

/// The reset was pressed without a live factor confirmation (it ran out, or
/// the server answered `403 reset-factor-required`): the section opens again
/// as it was. Nothing was reset.
public struct ResetFactorRequired: Error, Equatable {
    public init() {}
}
