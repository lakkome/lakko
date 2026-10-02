// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A guest account: the account made for someone who joined a call from a
/// link without having one (`docs/ux/call-links.md` section 9, ADR-0032).
///
/// Mirror of `apps/web/src/auth/guest-account.ts`.
///
/// ## What changed (2026-10-02)
///
/// The first version showed the 24-word recovery phrase before the call. A
/// person who was sent a link wants the call; whether they want an account is
/// a question for afterwards. So now:
///
///   * the account is made silently and the person goes straight into the
///     call;
///   * while it is a guest account nothing about it is kept on a server
///     beyond what the call itself needs: no backup, no push registration;
///   * after the call the person is asked once: `Create account` or
///     `No, thanks`;
///   * `No, thanks` deletes the account at once, here and on the servers;
///   * an account that was never given a username is deleted by the servers a
///     day after it was made, so closing the app leaves nothing behind
///     either.
///
/// ## What this file is
///
/// The two facts a device keeps about a guest account, the one decision the
/// screens read, and the two flows that end a guest account (keep it, or
/// delete it) written against ports so their ordering is testable without a
/// server.
///
///   1. **The marker** (``GuestAccountMarker``): this account is a guest
///      account, and whether a username was already taken for it. Plain
///      `UserDefaults`; it holds no secret.
///   2. **The phrase row**: the recovery phrase, in the Keychain phrase cache
///      (``PhraseStore``), under a key of its own (``guestPhraseKey(accountIdB64:)``)
///      and not the account's. That choice is what keeps the automatic backup
///      from running for a guest without it knowing what a guest is: it reads
///      the phrase by the account's key and finds nothing.

/// What the device knows about a guest account.
public struct GuestAccountState: Equatable {
    /// The username already taken for this account by an earlier
    /// `Create account` that did not finish, or `nil`.
    ///
    /// A username cannot be changed and an account holds one, so a second
    /// attempt must go on with this one instead of asking again.
    public let claimedUsername: String?

    public init(claimedUsername: String?) {
        self.claimedUsername = claimedUsername
    }
}

/// The guest marker, one `UserDefaults` row per account.
struct GuestAccountMarker {

    /// Key prefix; the base64 account id follows. The account wipe removes
    /// the row (``AccountLocalDataWipe``).
    static let keyPrefix = "lakko.guest-account.v1."

    private static let markGuest = "guest"
    private static let markNamedPrefix = "named:"

    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }

    /// Record that `accountIdB64` is a guest account.
    func mark(accountIdB64: String) {
        defaults.set(Self.markGuest, forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// Record the username taken for a guest account that is being kept.
    func noteUsernameClaimed(accountIdB64: String, username: String) {
        defaults.set(Self.markNamedPrefix + username, forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// What the device knows about `accountIdB64` as a guest, or `nil` when
    /// it is not a guest account.
    func state(accountIdB64: String) -> GuestAccountState? {
        guard let raw = defaults.string(forKey: Self.key(accountIdB64: accountIdB64)) else {
            return nil
        }
        if raw == Self.markGuest { return GuestAccountState(claimedUsername: nil) }
        if raw.hasPrefix(Self.markNamedPrefix) {
            let name = String(raw.dropFirst(Self.markNamedPrefix.count))
            return GuestAccountState(claimedUsername: name.isEmpty ? nil : name)
        }
        // An unknown value is not a guest: never offer to delete an account
        // on the strength of bytes this build did not write.
        return nil
    }

    /// Forget that `accountIdB64` is a guest: it became an account, or it is
    /// being removed from this device.
    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

/// The ``PhraseStore`` key a guest's recovery phrase is kept under.
///
/// Not the account id: see the file comment.
func guestPhraseKey(accountIdB64: String) -> String {
    "guest:" + accountIdB64
}

/// What a screen shows a guest.
enum GuestScreen: Equatable {
    /// Not a guest: the screen's own content.
    case none
    /// A call is running. The guest is in it; the only thing to offer is the
    /// way back to it. Deleting the account under a live call is never one
    /// press away.
    case inCall
    /// No call is running: `Create account` or `No, thanks`.
    case offer
}

/// What a signed-in screen shows in place of the app.
///
/// A guest account has no conversations and no contacts, so the app behind
/// it is an empty shell; a guest never lands in it. While a call runs the
/// person is sent back to the call, and once there is none they are asked the
/// one question.
func guestScreen(guest: Bool, callInProgress: Bool) -> GuestScreen {
    guard guest else { return .none }
    return callInProgress ? .inCall : .offer
}

/// A guest account cannot be kept because this device no longer holds its
/// recovery phrase. The phrase is what proves the account to the password
/// step, and what the person is shown at the end; without it the only thing
/// left to offer is `No, thanks`.
public struct GuestPhraseUnavailable: Error, Equatable {
    public init() {}
}

/// No guest account is signed in, so there is none to keep or delete.
public struct NoGuestAccount: Error, Equatable {
    public init() {}
}

// MARK: - Keep the account

/// `Create account` on a guest account: give it a username and a password.
///
/// Written against ports so the order below is what a test drives. Mirror of
/// `claimGuestAccount` in `apps/web/src/auth/AuthContext.tsx`.
@MainActor
enum GuestAccountClaim {

    struct Ports {
        /// The guest's recovery phrase from this device, or `nil` when it is
        /// not here. A read fault is `nil` too.
        var readPhrase: @MainActor () -> String?
        /// The username an earlier attempt already took, or `nil`.
        var claimedUsername: @MainActor () -> String?
        /// Claim `username`: the login handle at auth first, then the name
        /// in the directory, the handle released if the directory refuses
        /// (the signup order). Returns the canonical name.
        var claimUsername: @MainActor (_ username: String) async throws -> String
        /// Record the claimed name in the guest marker and in the persisted
        /// account.
        var noteUsernameClaimed: @MainActor (_ username: String) -> Void
        /// Take this device's current KeyPackage out of the directory.
        /// Best effort: it never throws.
        var deleteOwnKeyPackage: @MainActor () async -> Void
        /// Set the password: the platform's phrase-proved reset
        /// (``AuthRepository/recover(mnemonicPhrase:newPassword:username:)``).
        var setPassword: @MainActor (_ phrase: String, _ password: Data, _ username: String) async throws -> Void
        /// The account stopped being a guest: the phrase becomes owed and
        /// pending, the guest marker and the guest phrase row go.
        var finish: @MainActor (_ phrase: String) -> Void
    }

    /// 1. The phrase first: without it nothing below may start
    ///    (``GuestPhraseUnavailable``).
    /// 2. The username, once. An account holds one and it cannot be changed,
    ///    so a retry after a failed later step goes on with the same name and
    ///    never asks the directory again.
    /// 3. This device's KeyPackage leaves the directory before the password
    ///    step, so nobody is left offering a Welcome to a device identity the
    ///    account no longer uses (the Forget device rule, ADR-0050 2.10).
    /// 4. The password.
    /// 5. Only then does the account stop being a guest.
    static func run(username: String, password: Data, ports: Ports) async throws {
        guard let phrase = ports.readPhrase() else { throw GuestPhraseUnavailable() }

        let name: String
        if let claimed = ports.claimedUsername() {
            name = claimed
        } else {
            name = try await ports.claimUsername(username)
            ports.noteUsernameClaimed(name)
        }

        await ports.deleteOwnKeyPackage()

        try await ports.setPassword(phrase, password, name)

        ports.finish(phrase)
    }
}

// MARK: - Delete the account

/// `No, thanks` on a guest account: delete it at once, on the auth service,
/// in the directory, and every trace on this device.
///
/// Mirror of `discardGuestAccount` in `apps/web/src/auth/AuthContext.tsx`.
@MainActor
enum GuestAccountDiscard {

    struct Ports {
        /// `DELETE /account` on auth. A guest has no passkey, so no action
        /// grant is needed.
        var deleteAtAuth: @MainActor () async throws -> Void
        /// The directory's delete-account: the identity bundle, the
        /// KeyPackage, any username a half-finished `Create account` took.
        var deleteInDirectory: @MainActor () async throws -> Void
        /// Forget device: every local store of the account, the guest marker
        /// and the guest phrase row included; lands signed out.
        var wipe: @MainActor () -> Void
    }

    /// The auth service first, and alone decisive: once the account is gone
    /// there, nothing can sign in to it. A failure throws and leaves
    /// everything in place so the person can try again (the server deletes an
    /// account without a username after a day regardless). The directory is
    /// best effort; its own daily sweep removes what is left.
    static func run(ports: Ports) async throws {
        try await ports.deleteAtAuth()
        do {
            try await ports.deleteInDirectory()
        } catch {
            NSLog("[guest] directory rows not deleted: %@", String(describing: type(of: error)))
        }
        ports.wipe()
    }
}

// MARK: - The app-level hooks

/// What ``AuthRepository`` needs from outside itself to end a guest account.
/// The two server calls have live defaults; the KeyPackage delete and the
/// local wipe belong to objects the repository does not own, so the
/// composition root sets them (`LakkoApp`), and tests replace all four.
struct GuestAccountEnvironment {
    /// `DELETE /account` on auth with the session token.
    var deleteAccountAtAuth: @MainActor (_ sessionToken: String) async throws -> Void = { token in
        let client = WebauthnClient()
        client.setSessionToken(token)
        try await client.deleteAccount()
    }

    /// `DELETE /discovery/account` with the session token.
    var deleteAccountInDirectory: @MainActor (_ sessionToken: String) async throws -> Void = { token in
        try await DiscoveryClient(sessionToken: token).deleteAccount()
    }

    /// Delete this device's directory KeyPackage. The chat layer knows the
    /// device's leaf key; unset, there is nothing to delete.
    var deleteOwnKeyPackage: @MainActor () async -> Void = {}

    /// Forget device for `accountIdB64`: release the runtimes, wipe every
    /// store, end the session (``AppServices/wipeAccountLocalData(accountIdB64:)``).
    /// `nil` falls back to the store wipe and the session end alone.
    var wipeLocalData: (@MainActor (_ accountIdB64: String) -> Void)?
}
