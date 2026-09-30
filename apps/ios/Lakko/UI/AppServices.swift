// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

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

    init(
        authRepository: AuthRepository,
        contactsRepository: ContactsRepository,
        chatRepository: ChatRepository,
        callHost: CallHost,
        groupCallHost: GroupCallHost,
        linkCallHost: LinkCallHost,
        deviceGroupSync: DeviceGroupSyncRepository,
        inviteIntake: InviteIntake
    ) {
        self.authRepository = authRepository
        self.contactsRepository = contactsRepository
        self.chatRepository = chatRepository
        self.callHost = callHost
        self.groupCallHost = groupCallHost
        self.linkCallHost = linkCallHost
        self.deviceGroupSync = deviceGroupSync
        self.inviteIntake = inviteIntake
    }
}

extension AppServices {
    /// Settings' "Forget this device", and the removed-device screen's Sign
    /// out (docs/ux/second-device.md section 3.1): this device's keys and
    /// local history for the account go, then the login screen.
    ///
    /// ADR-0050 section 2.10 — take this device's KeyPackage out of the
    /// directory BEFORE the wipe, while the bearer token still exists: the
    /// account's other devices retire this device's leaves when their
    /// scheduled check sees the row gone.
    func forgetThisDevice() {
        // ADR-0042 (amended 2026-09-30) — FIRST, before the rest of the wipe
        // (some of which waits on the network): no background wake is handled
        // for this account from here on, and nothing it received stays on the
        // lock screen. ``AuthRepository/forgetDevice()`` repeats both at its
        // own start for its other callers.
        PushAccountBindingStore.live.clear()
        AuthRepository.removeAllDeliveredNotifications()
        // This device's calls go with the device. The transcript rows in each
        // conversation are the message log's to wipe, not this index's.
        CallHistoryStore.shared.clear()
        let accountIdB64 = authRepository.identitySecrets.map { base64Encode($0.accountId) }
        Task { @MainActor in
            if let own = chatRepository.ownDeviceLeafKey {
                await chatRepository.deleteDirectoryKeyPackage(deviceKey: own)
            }
            // The account's chat state on this device: the MLS snapshot, the
            // message log, known groups, read cursors, the push routing
            // secrets and the rest ``ChatRepository/forgetDevice()`` lists.
            // Its documentation always said AuthRepository called it; nothing
            // did, so a forgotten account's chats stayed on disk.
            chatRepository.forgetDevice()
            // The account's folded cross-device state (folders, flags,
            // drafts, read cursors) and its pending-sync flag; logout keeps
            // them since 2026-09-30.
            if let accountIdB64 {
                deviceGroupSync.forget(accountIdB64: accountIdB64)
            }
            authRepository.forgetDevice()
        }
    }
}
