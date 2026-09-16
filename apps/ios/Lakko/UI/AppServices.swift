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
