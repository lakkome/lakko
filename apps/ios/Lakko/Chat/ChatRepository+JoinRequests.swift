// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// `docs/ux/group-and-channel-privacy.md` section 8.1 — the repository half of
// the request-to-join signals. The rules are ``JoinRequestSignals``; this
// reads what they need (the role, the retained requests, the two persisted
// sets, mute, what is on screen) and does what they say.

extension ChatRepository {

    /// Posts the request notifications. Injectable for tests.
    static var joinRequestPoster: JoinRequestNotificationPoster = SystemJoinRequestNotifier()

    /// Whether a group is open in front of the person. Injectable for tests.
    static var joinRequestVisibility: (String) -> Bool = { VisibleGroupConversation.isOpenAndVisible($0) }

    /// Whether the section 8.1 signals apply to this device in `groupIdHex`:
    /// owner or admin, membership not ended.
    func joinRequestSignalsApply(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex] else { return false }
        let ended = readIsActive { try record.group.isActive() } == false
        return JoinRequestSignals.signalsApply(role: ownRole(groupIdHex: groupIdHex), membershipEnded: ended)
    }

    /// The retained requests still waiting, as base64 joiner keys.
    func pendingJoinRequestKeys(groupIdHex: String) -> Set<String> {
        Set(pendingExternalJoins(groupIdHex: groupIdHex).map { base64Encode($0) })
    }

    /// A request this device just read for `groupIdHex` (a kept knock row, or
    /// a journal external-join proposal the core retained). Announces it once:
    /// the bar when the conversation is in front, else the unshown set and
    /// the notification (none for a muted group).
    func noteJoinRequestArrived(groupIdHex: String, joinerSignatureKey: Data) {
        guard let accountIdB64 = currentAccountIdB64,
              joinRequestSignalsApply(groupIdHex: groupIdHex) else { return }
        let joiner = base64Encode(joinerSignatureKey)
        let conversation = conversationIdHex(ofGroup: groupIdHex)
        let store = JoinRequestSignalStore()
        let before = store.sets(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let visible = Self.joinRequestVisibility(groupIdHex)
            || (conversation != groupIdHex && Self.joinRequestVisibility(conversation))
        let muted = ConversationMuteMirror.isMuted(accountIdB64: accountIdB64, groupIdHex: conversation)
        let decision = JoinRequestSignals.arrival(
            joinerKeyB64: joiner, sets: before, openAndVisible: visible, muted: muted)
        guard decision.sets != before else { return }
        store.save(accountIdB64: accountIdB64, groupIdHex: groupIdHex, sets: decision.sets)
        guard decision.arrival == .notify else { return }
        Self.joinRequestPoster.post(
            conversationIdHex: conversation,
            joinerKeyB64: joiner,
            title: Localization.text("notification.join_request.title"),
            body: JoinRequestSignals.notificationBody(
                name: joinRequestName(groupIdHex: groupIdHex, joinerSignatureKey: joinerSignatureKey),
                groupTitle: ChatListViewModel.title(repository: self, groupIdHex: groupIdHex)))
    }

    /// The name a request shows: the contact (or profile) name of the
    /// joiner's account, else the name the request carried, else the leaf's
    /// resolved name; `nil` for none.
    func joinRequestName(groupIdHex: String, joinerSignatureKey: Data) -> String? {
        let carried = currentAccountIdB64.flatMap {
            groupSettingsStore.requestNames(accountIdB64: $0, groupIdHex: groupIdHex)[
                base64Encode(joinerSignatureKey)]
        }
        let directory = ProfileDirectory.shared
        return JoinRequestSignals.requestName(
            accountKeyName: pendingExternalJoinAccountKey(
                groupIdHex: groupIdHex, joinerSignatureKey: joinerSignatureKey)
                .map { directory.resolve(signatureKey: $0) },
            carried: carried,
            leafName: directory.resolve(signatureKey: joinerSignatureKey))
    }

    /// Cut both sets of `groupIdHex` to the requests still waiting. Called
    /// wherever the retained set is read (the bar, the knock read).
    func pruneJoinRequestSignals(groupIdHex: String, pending: Set<String>) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        // UX section 8 — the "Asked / Invited ... ago" time and the carried
        // name of a request that stopped waiting go with it.
        groupSettingsStore.pruneRequestRecords(accountIdB64: accountIdB64, groupIdHex: groupIdHex, waiting: pending)
        let store = JoinRequestSignalStore()
        let before = store.sets(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard !before.isEmpty else { return }
        let next = JoinRequestSignals.pruned(before, pending: pending)
        guard next != before else { return }
        store.save(accountIdB64: accountIdB64, groupIdHex: groupIdHex, sets: next)
        state = state.with(threads: buildThreadList())
    }

    /// The bar was drawn in the open, visible conversation: its requests
    /// count as shown, and the row is no longer unread for them.
    func joinRequestBarShown(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let store = JoinRequestSignalStore()
        let before = store.sets(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard !before.unshown.isEmpty else { return }
        store.save(accountIdB64: accountIdB64, groupIdHex: groupIdHex, sets: JoinRequestSignals.barDrawn(before))
        state = state.with(threads: buildThreadList())
    }

    /// The unshown requests that add to the row of the conversation whose
    /// current group is `groupIdHex`, from an already-loaded account row.
    func joinRequestUnshownCount(groupIdHex: String, loaded: [String: JoinRequestSignals.Sets]) -> Int {
        guard let sets = loaded[groupIdHex.lowercased()], !sets.unshown.isEmpty else { return 0 }
        return JoinRequestSignals.unshownCount(
            sets: sets,
            pending: pendingJoinRequestKeys(groupIdHex: groupIdHex),
            signalsApply: joinRequestSignalsApply(groupIdHex: groupIdHex))
    }

    /// Every group's sets for the bound account (one read per list build).
    func loadJoinRequestSignals() -> [String: JoinRequestSignals.Sets] {
        currentAccountIdB64.map { JoinRequestSignalStore().load(accountIdB64: $0) } ?? [:]
    }
}
