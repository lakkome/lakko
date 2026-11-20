// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// The process-scoped repositories and call hosts, bundled as plain
/// references so a view can REACH them without OBSERVING them.
///
/// An `@EnvironmentObject` / `@ObservedObject` re-evaluates the whole body of
/// the view that declares it on every publish of the object, whether or not
/// the body reads anything from it. The shell views (`AppRoot`, `SessionRoot`)
/// only hand these objects down and call methods on them, so they take this
/// bundle instead; the leaf that renders a published value declares the
/// object itself (`ChatListScreen` observes the sync repository for flags and
/// read cursors, `ContactsScreen` observes the auth repository, and so on).
///
/// Deliberately not an `ObservableObject`: it never changes after launch, and
/// making it observable would reintroduce the whole-shell re-render this
/// exists to remove. Compared by identity (`===`) where a view needs to know
/// whether its inputs changed.
@MainActor
final class AppServices {
    let authRepository: AuthRepository
    let contactsRepository: ContactsRepository
    let chatRepository: ChatRepository
    let callHost: CallHost
    let groupCallHost: GroupCallHost
    let linkCallHost: LinkCallHost
    let deviceGroupSync: DeviceGroupSyncRepository
    let inviteIntake: InviteIntake
    /// The Feed's "Couldn't check for new messages." state, fed by every
    /// check. In memory, per account.
    let feedRefresh: FeedRefreshCenter
    /// ADR-0074 section 2.3 — removing another device of the account, with
    /// its owed record.
    let deviceRemoval: DeviceRemovalRunner
    /// ADR-0074 section 3.2 — "It was lost or stolen": remove a device AND
    /// change the account keys, with its owed record.
    let keyRotation: KeyRotationRunner
    /// ADR-0075 — the signed-in account's protection (recovery email, saved
    /// phrase) and the notices its devices are shown.
    let accountProtection: AccountProtectionCenter

    /// The subscription that lets the chat runtime go when the session ends
    /// (``releaseRuntimesWhenTheAccountLeaves()``).
    private var accountBinding: AnyCancellable?

    init(
        authRepository: AuthRepository,
        contactsRepository: ContactsRepository,
        chatRepository: ChatRepository,
        callHost: CallHost,
        groupCallHost: GroupCallHost,
        linkCallHost: LinkCallHost,
        deviceGroupSync: DeviceGroupSyncRepository,
        inviteIntake: InviteIntake,
        feedRefresh: FeedRefreshCenter,
        deviceRemoval: DeviceRemovalRunner,
        keyRotation: KeyRotationRunner,
        accountProtection: AccountProtectionCenter
    ) {
        self.authRepository = authRepository
        self.contactsRepository = contactsRepository
        self.chatRepository = chatRepository
        self.callHost = callHost
        self.groupCallHost = groupCallHost
        self.linkCallHost = linkCallHost
        self.deviceGroupSync = deviceGroupSync
        self.inviteIntake = inviteIntake
        self.feedRefresh = feedRefresh
        self.deviceRemoval = deviceRemoval
        self.keyRotation = keyRotation
        self.accountProtection = accountProtection
        releaseRuntimesWhenTheAccountLeaves()
    }

    /// Drop the chat runtime the moment the signed-in account leaves or is
    /// replaced, not when a screen happens to appear.
    ///
    /// The chat repository lives for the process and was cleared only by the
    /// wipe: after a plain Log out it kept the account's identity and groups,
    /// and the foreground receive (also process-long) went on polling,
    /// decrypting and sweeping for an account nobody was signed in to, and
    /// rang its calls. ``ChatRepository/clear()`` keeps every store, so the
    /// next sign-in of the same account bootstraps from disk as a relaunch
    /// does.
    ///
    /// `@Published` emits before the new value is stored and on the main
    /// actor (the repository is main-actor bound), so the runtime is gone
    /// before anything can read the next account's secrets.
    private func releaseRuntimesWhenTheAccountLeaves() {
        let chat = chatRepository
        var bound: Data?
        accountBinding = authRepository.$identitySecrets
            .map { $0?.accountId }
            .sink { accountId in
                MainActor.assumeIsolated {
                    defer { bound = accountId }
                    guard let leaving = bound, leaving != accountId else { return }
                    chat.clear()
                }
            }
    }
}

extension AppServices {
    /// Settings' "Forget this device", and the removed-device screen's Sign
    /// out (docs/ux/second-device.md section 3.1): this device's keys and
    /// local data for the account go, then the login screen.
    ///
    /// ADR-0050 section 2.10 — take this device's KeyPackage out of the
    /// directory BEFORE the wipe, while the bearer token still exists: the
    /// account's other devices retire this device's leaves when their
    /// scheduled check sees the row gone.
    func forgetThisDevice() {
        // ADR-0042 (amended 2026-09-30) — FIRST, before the directory call
        // (which waits on the network): no background wake is handled for
        // this account from here on, and nothing it received stays on the
        // lock screen. The wipe repeats both.
        PushAccountBindingStore.live.clear()
        AuthRepository.removeAllDeliveredNotifications()
        let accountIdB64 = authRepository.identitySecrets.map { base64Encode($0.accountId) }
        let seed = authRepository.identitySecrets?.accountIdentitySeed
        Task { @MainActor in
            // ADR-0090 section 2.4 — best effort, before the wipe: a retire
            // request for this device's leaf in every channel PublisherGroup
            // it holds (an owner device of the account applies it; if lost,
            // the roster reconcile retires the leaf).
            if let seed, let channels = ChannelDeviceAdmission.waitingState {
                await ChannelDeviceAdmission.sendOwnRetireRequests(state: channels, currentSeed: seed)
            }
            if let own = chatRepository.ownDeviceLeafKey {
                await chatRepository.deleteDirectoryKeyPackage(deviceKey: own)
            }
            // The directory call waited on the network. Whoever is signed in
            // by now, the wipe below is of the account captured above
            // (``wipeAccountLocalData(accountIdB64:)`` keeps another
            // account's session).
            wipeAccountLocalData(accountIdB64: accountIdB64)
        }
    }

    /// ADR-0073 section 3.5 — the automatic wipe of the signed-out screen:
    /// the full forget-device wipe WITHOUT the directory call
    /// ``forgetThisDevice()`` makes first (the reset already deleted this
    /// device's KeyPackage row, and the token is superseded) and without any
    /// DeviceGroup message (the old root is a stranger to the re-founded
    /// journal). Publishes nothing. The push unsubscribe inside
    /// ``AuthRepository/endSessionAfterLocalWipe()`` spends an anonymous
    /// token, which is the one network call the wipe makes.
    func wipeAfterKeyReset(accountIdB64: String) {
        wipeAccountLocalData(accountIdB64: accountIdB64)
    }

    /// ADR-0074 section 1.1 — the STAGED wipe of a device whose account keys
    /// were reset elsewhere: the same runtimes let go and the same stores
    /// are removed, except the recall record, which is what "Was it you?"
    /// needs to take the account back.
    func stagedWipeAfterKeyReset(accountIdB64: String) {
        wipeAccountLocalData(accountIdB64: accountIdB64, keeping: [.recallRecord])
    }

    /// Forget device and Delete account, after their server-side part: remove
    /// every piece of the account's local data on this device and end the
    /// session. The one path both flows take; Log out does not come here.
    ///
    /// Order: the push binding and delivered notifications first; then the
    /// runtimes let go of the account, so nothing they flush re-writes a
    /// store the wipe has just emptied; then every store
    /// (``AccountLocalDataWipe``); then the session ends.
    ///
    /// - Parameter accountIdB64: the account to wipe; `nil` (no session to
    ///   name one) falls back to the persisted session record.
    func wipeAccountLocalData(accountIdB64: String?, keeping: Set<AccountLocalDataWipe.Kept> = []) {
        PushAccountBindingStore.live.clear()
        AuthRepository.removeAllDeliveredNotifications()
        let target = accountIdB64 ?? SessionStore().load()?.accountIdB64
        // Every caller reaches this after a network wait (the directory
        // delete, the account delete, a guest's server deletes). If ANOTHER
        // account is signed in by now, the runtimes and the session are that
        // account's: only the stores of the account this wipe was asked for
        // go.
        if case .otherAccountSignedIn = AccountWipeScope.decide(
            target: target, signedIn: authRepository.signedInAccountIdB64) {
            if let target { AccountLocalDataWipe.wipe(accountIdB64: target) }
            return
        }
        // The runtimes. `clear()` keeps the stores (it is also the logout
        // path); the wipe below is what removes them.
        CallHistoryStore.shared.unbind()
        chatRepository.clear()
        deviceGroupSync.clear()
        feedRefresh.reset()
        if let target {
            AccountLocalDataWipe.wipe(accountIdB64: target, keeping: keeping)
        } else {
            NSLog("[account-wipe] WARN no account to wipe: no session and no session record")
        }
        authRepository.endSessionAfterLocalWipe()
    }
}

/// What ``AppServices/wipeAccountLocalData(accountIdB64:)`` may touch.
enum AccountWipeScope: Equatable {
    /// The account to wipe is the one signed in, or nobody is: the runtimes
    /// let go and the session ends, as Forget device and Delete account mean.
    case wholeDevice
    /// Another account signed in while the wipe's server step was out: its
    /// runtimes and its session are not this wipe's to end.
    case otherAccountSignedIn

    static func decide(target: String?, signedIn: String?) -> AccountWipeScope {
        guard let signedIn, let target, signedIn != target else { return .wholeDevice }
        return .otherAccountSignedIn
    }
}
