// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// The beta account cap (`docs/ux/beta-account-cap.md`, ADR-0046 amendment of
// 2026-11-18). The server counts accounts that hold a username; when the
// count is at the cap a FIRST username bind is refused and `GET /signup/open`
// answers `{"open": false}`. Mirror of the web and Android handling.

/// `403 {"error":"registration-closed"}` from `PUT /auth/login-handle`.
public let registrationClosedCode = "registration-closed"

/// A first username bind was refused because the beta is full. Final: the
/// bind decides, whatever `/signup/open` said before.
public struct RegistrationClosed: Error, Equatable {
    public init() {}
}

/// The pure decisions of the account cap, kept apart from any view so a test
/// drives them.
public enum SignupCap {

    /// Whether `error` is the bind's `403 registration-closed`.
    public static func isRegistrationClosed(_ error: Error) -> Bool {
        if error is RegistrationClosed { return true }
        guard case .http(let status, _, let body)? = error as? AuthClientError,
              status == 403 else { return false }
        let object = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        return (object?["error"] as? String) == registrationClosedCode
    }

    /// Whether `error` is the bind's `403 supporter-code-used` (ADR-0097): the
    /// held place was spent by another account.
    public static func isSupporterCodeUsed(_ error: Error) -> Bool {
        if error is SupporterCodeUsed { return true }
        guard case .http(let status, _, let body)? = error as? AuthClientError,
              status == 403 else { return false }
        let object = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        return (object?["error"] as? String) == supporterCodeUsedCode
    }

    /// What the client takes from `GET /signup/open`: only a well-formed
    /// `{"open": false}` closes sign-up. No connection, an error, an older
    /// server answering `404`, a body that does not parse: open, and the bind
    /// decides later (spec section 2 item 2).
    public static func isOpen(_ answer: Result<Bool, Error>) -> Bool {
        switch answer {
        case .success(let open): return open
        case .failure: return true
        }
    }
}

/// What the sign-up screen shows (spec section 2).
public enum SignupScreenMode: Equatable {
    /// The ordinary form.
    case form
    /// "Lakko is full for now", with `Try again` and `Log in`.
    case closed
    /// The closed state while `Try again` asks again: `Checking...`, disabled.
    case checking
    /// "Supporter code" in place of the full state (`docs/ux/supporters.md`
    /// section 1.1).
    case supporterCode

    /// After the sign-up endpoint answered `open`. A supporter place held on
    /// this device passes the cap, so it shows the form (with the held line)
    /// even when sign-up is closed.
    static func after(open: Bool, placeHeld: Bool = false) -> SignupScreenMode {
        open || placeHeld ? .form : .closed
    }
}

/// What a guest is asked after a call (spec section 3).
public enum GuestOfferVariant: Equatable {
    /// `Create account` / `No, thanks`.
    case ask
    /// "Lakko is full for now, so this guest account cannot be kept." with
    /// one `OK`, which does what `No, thanks` does.
    case closed

    /// The question about to show, after `/signup/open` answered.
    ///
    /// A guest whose earlier `Create account` already bound a username
    /// (`docs/ux/call-links.md` section 9.2) holds a place: its re-bind is
    /// never refused, so it is never asked the closed question.
    static func after(open: Bool, holdsUsername: Bool = false) -> GuestOfferVariant {
        open || holdsUsername ? .ask : .closed
    }

    /// `Create account` failed with `error`: the closed question when the
    /// bind refused (never for a guest already holding a username), else the
    /// variant stays as it was.
    static func afterClaimFailure(
        _ error: Error, current: GuestOfferVariant, holdsUsername: Bool = false
    ) -> GuestOfferVariant {
        SignupCap.isRegistrationClosed(error) && !holdsUsername ? .closed : current
    }
}

/// Sign-up refused at the bind (spec section 2 item 4): the account the
/// sign-up just made has no username. Delete it on auth at once
/// (`DELETE /account`, which an account with no username and no passkey
/// passes on its session alone), then wipe it from this device; a failed
/// delete still wipes, and the server sweeps the account within a day.
///
/// No directory entry exists to delete: the bind at auth comes before the
/// directory registration (``claimSignupUsername(accountId:username:sessionToken:directory:binder:derive:)``),
/// and the identity bundle is published only after the claim.
@MainActor
enum SignupRefusedCleanup {

    static func run(
        deleteAtAuth: @MainActor () async throws -> Void,
        wipeLocal: @MainActor () -> Void
    ) async {
        do {
            try await deleteAtAuth()
        } catch {
            NSLog("[auth] deleting the refused sign-up account failed: %@", String(describing: type(of: error)))
        }
        wipeLocal()
    }
}
