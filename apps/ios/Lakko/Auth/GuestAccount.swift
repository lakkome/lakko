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
///     either;
///   * the device stops an hour earlier (``guestLifetimeMs``): a guest past
///     that age is deleted unasked, and a guest whose session cannot be
///     restored is wiped at launch, so a guest account never ends up as a
///     cached, signed-out account nobody knows the password of.
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

/// How long a guest account lives on a device before the device deletes it
/// unasked, in milliseconds.
///
/// The servers delete an unkept account a day after it was made. The device
/// stops an hour earlier, so `Create account` is never offered for an account
/// the servers are about to remove under it. Chosen, not measured.
let guestLifetimeMs: Int64 = 23 * 60 * 60 * 1000

/// What the device knows about a guest account.
public struct GuestAccountState: Equatable {
    /// When this device marked the account as a guest (its creation, to the
    /// second or so), unix milliseconds.
    public let markedAtMs: Int64
    /// The username already taken for this account by an earlier
    /// `Create account` that did not finish, or `nil`.
    ///
    /// A username cannot be changed and an account holds one, so a second
    /// attempt must go on with this one instead of asking again.
    public let claimedUsername: String?

    public init(markedAtMs: Int64, claimedUsername: String?) {
        self.markedAtMs = markedAtMs
        self.claimedUsername = claimedUsername
    }
}

/// The guest marker, one `UserDefaults` row per account.
///
/// The value is `guest@<unix ms>` or `named@<unix ms>:<username>`. A username
/// is `[a-z0-9_]`, so the first `:` after the stamp ends it. Anything else is
/// not a guest.
struct GuestAccountMarker {

    /// Key prefix; the base64 account id follows. The account wipe removes
    /// the row (``AccountLocalDataWipe``).
    static let keyPrefix = "lakko.guest-account.v1."

    private static let markGuestPrefix = "guest@"
    private static let markNamedPrefix = "named@"

    /// The largest stamp accepted: the web client's safe-integer bound, so
    /// the two clients accept the same values.
    private static let maxStampMs: Int64 = 9_007_199_254_740_991

    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }

    /// 1 to 16 ASCII digits, within ``maxStampMs``.
    private static func parseStamp(_ text: Substring) -> Int64? {
        guard (1...16).contains(text.count),
              text.allSatisfy({ $0.isASCII && $0.isNumber }),
              let ms = Int64(text), ms <= maxStampMs else { return nil }
        return ms
    }

    /// Record that `accountIdB64` is a guest account, made at `nowMs`.
    func mark(accountIdB64: String, nowMs: Int64) {
        defaults.set(
            Self.markGuestPrefix + String(max(0, nowMs)),
            forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// Record the username taken for a guest account that is being kept. The
    /// account keeps the age it had: taking a name does not buy it another
    /// day. No-op for an account that is not marked as a guest.
    func noteUsernameClaimed(accountIdB64: String, username: String) {
        guard let current = state(accountIdB64: accountIdB64) else { return }
        defaults.set(
            Self.markNamedPrefix + String(current.markedAtMs) + ":" + username,
            forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// What the device knows about `accountIdB64` as a guest, or `nil` when
    /// it is not a guest account.
    func state(accountIdB64: String) -> GuestAccountState? {
        guard let raw = defaults.string(forKey: Self.key(accountIdB64: accountIdB64)) else {
            return nil
        }
        if raw.hasPrefix(Self.markGuestPrefix) {
            guard let markedAtMs = Self.parseStamp(raw.dropFirst(Self.markGuestPrefix.count)) else {
                return nil
            }
            return GuestAccountState(markedAtMs: markedAtMs, claimedUsername: nil)
        }
        if raw.hasPrefix(Self.markNamedPrefix) {
            let rest = raw.dropFirst(Self.markNamedPrefix.count)
            guard let colon = rest.firstIndex(of: ":"),
                  let markedAtMs = Self.parseStamp(rest[rest.startIndex..<colon]) else {
                return nil
            }
            let name = String(rest[rest.index(after: colon)...])
            return GuestAccountState(markedAtMs: markedAtMs, claimedUsername: name.isEmpty ? nil : name)
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

/// Whether a guest account has outlived ``guestLifetimeMs`` on this device.
///
/// A clock that reads earlier than the mark (it was set back) says nothing
/// about the account's age, so it is not expired on that evidence.
func guestExpired(_ guest: GuestAccountState, nowMs: Int64) -> Bool {
    let (age, overflow) = nowMs.subtractingReportingOverflow(guest.markedAtMs)
    return !overflow && age >= guestLifetimeMs
}

/// What a launch does about a guest account this device holds.
enum GuestRestoreAction: Equatable {
    /// Not a guest, or a guest young enough to go on: the ordinary restore.
    case none
    /// A guest past its lifetime whose session can still be restored: delete
    /// it on the servers (best effort), then wipe the device.
    case discard
    /// A guest whose session cannot be restored (the token expired or is
    /// missing, a Keychain item is unreadable): nothing can sign in to it
    /// from here and nothing can show its phrase, so every trace of it leaves
    /// the device. The servers remove their side on their own clock.
    case wipe
}

/// Decide it. `restorable` is the ordinary restore's own verdict: every piece
/// a relaunch restore needs is present
/// (``SessionRestore/decide(marker:persistence:nowMs:)`` answered a session).
///
/// A guest account never ends up as a cached, signed-out account: there is no
/// password anyone could type to get back into it.
func guestRestoreAction(guest: GuestAccountState?, restorable: Bool, nowMs: Int64) -> GuestRestoreAction {
    guard let guest else { return .none }
    if !restorable { return .wipe }
    return guestExpired(guest, nowMs: nowMs) ? .discard : .none
}

/// One step of deleting a guest account on the servers.
struct GuestDeleteStep: Equatable {
    enum Service: Equatable {
        /// `DELETE /account` on the auth service.
        case auth
        /// The directory's delete-account.
        case directory
    }

    let service: Service
    /// A required step that fails stops a strict delete.
    let required: Bool
}

/// The order `No, thanks` deletes in, and which steps may fail quietly.
///
/// An unnamed guest: the auth service decides (once the account is gone
/// there, nothing can sign in to it), and the directory is best effort, since
/// its own sweep removes an entry with no username.
///
/// A guest for whom a username was already taken: the directory goes FIRST
/// and must succeed. Its sweep never removes an entry that has a username, so
/// a directory delete skipped here would leave that name taken for good. Both
/// deletes are idempotent, so a retry after a half-finished attempt
/// completes.
func guestDeletePlan(_ guest: GuestAccountState) -> [GuestDeleteStep] {
    if guest.claimedUsername == nil {
        return [
            GuestDeleteStep(service: .auth, required: true),
            GuestDeleteStep(service: .directory, required: false),
        ]
    }
    return [
        GuestDeleteStep(service: .directory, required: true),
        GuestDeleteStep(service: .auth, required: true),
    ]
}

/// A guest account cannot be kept because this device no longer holds its
/// recovery phrase. The phrase is what proves the account to the password
/// step, and what the person is shown at the end; without it the only thing
/// left to offer is `No, thanks`.
public struct GuestPhraseUnavailable: Error, Equatable {
    public init() {}
}

/// `Create account` was pressed for a guest account past its lifetime. The
/// account has been deleted and the session is over by the time this is
/// thrown; the screen that asked is going away, so it shows no message.
public struct GuestAccountExpired: Error, Equatable {
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
        /// not here: the guest's own row, else the account's row (an earlier
        /// attempt whose password step went through cached it there before
        /// it was interrupted). A read fault is `nil` too.
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

/// Delete a guest account on the servers, in the order and with the
/// strictness ``guestDeletePlan(_:)`` gives.
///
/// Mirror of `deleteGuestOnServers` in `apps/web/src/auth/AuthContext.tsx`.
@MainActor
enum GuestServerDelete {

    struct Ports {
        /// `DELETE /account` on auth. A guest has no passkey, so no action
        /// grant is needed.
        var deleteAtAuth: @MainActor () async throws -> Void
        /// The directory's delete-account: the identity bundle, the
        /// KeyPackage, any username a half-finished `Create account` took.
        var deleteInDirectory: @MainActor () async throws -> Void
    }

    /// `strict` (`No, thanks`): a required step that fails throws, and the
    /// steps after it do not run. Not strict (an expired guest): every
    /// failure is logged and swallowed; both deletes are idempotent and the
    /// servers sweep an unkept account themselves.
    static func run(guest: GuestAccountState, strict: Bool, ports: Ports) async throws {
        for step in guestDeletePlan(guest) {
            do {
                switch step.service {
                case .auth: try await ports.deleteAtAuth()
                case .directory: try await ports.deleteInDirectory()
                }
            } catch {
                if strict && step.required { throw error }
                NSLog(
                    "[guest] %@ rows not deleted: %@",
                    step.service == .auth ? "auth" : "directory",
                    String(describing: type(of: error)))
            }
        }
    }
}

/// `No, thanks` on a guest account: delete it at once, on the servers and
/// every trace on this device.
///
/// Mirror of `discardGuestAccount` in `apps/web/src/auth/AuthContext.tsx`.
@MainActor
enum GuestAccountDiscard {

    struct Ports {
        var servers: GuestServerDelete.Ports
        /// Forget device: every local store of the account, the guest marker
        /// and the guest phrase row included; lands signed out.
        var wipe: @MainActor () -> Void
    }

    /// The servers first, strictly: a required delete that fails throws and
    /// leaves everything in place so the person can try again. Then the
    /// device.
    static func run(guest: GuestAccountState, ports: Ports) async throws {
        try await GuestServerDelete.run(guest: guest, strict: true, ports: ports.servers)
        ports.wipe()
    }
}

/// A guest account past its lifetime (``guestExpired(_:nowMs:)``): delete it
/// on the servers best effort, then the device. Never throws: whatever the
/// servers answered, the account leaves this device.
@MainActor
enum GuestAccountExpiry {

    static func run(guest: GuestAccountState, ports: GuestAccountDiscard.Ports) async {
        // Not strict, so nothing is thrown.
        try? await GuestServerDelete.run(guest: guest, strict: false, ports: ports.servers)
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
