// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// `docs/ux/second-device.md` section 4, "Membership rows" (decided
/// 2026-10-29): a receive batch that changes the people of a group names them
/// on its last commit row (``MembershipRow``). Called from both receive paths
/// (the poll's apply and the ADR-0045 sweep's verdict read), right after the
/// rows were filed and before the message log is written.
extension ChatRepository {

    /// The distinct people of a group's roster (ADR-0050 person keys,
    /// base64), or `nil` when the roster cannot be read.
    static func personKeysB64(of group: Group) -> [String]? {
        guard let members = try? group.members() else { return nil }
        return PersonKey.people(members).map { base64Encode($0) }
    }

    func noteMembershipChange(
        groupIdHex: String,
        record: GroupRecord,
        events: [IncomingChatEvent],
        personKeysBefore: [String]?,
        firstNewRow: Int
    ) {
        let hadCommit = events.contains {
            if case .system(_, .commit, _) = $0 { return true }
            return false
        }
        guard hadCommit, let row = membershipRow(
            groupIdHex: groupIdHex, record: record, personKeysBefore: personKeysBefore)
        else { return }
        record.recordMembershipChange(row, fromIndex: firstNewRow)
    }

    /// The device that commits never receives its own commit back (ADR-0045
    /// sweep): when its own commit merged, it writes the same row itself, at
    /// the own commit's DS message id. `personKeysBefore` is the roster read
    /// just before the merge. No row without the commit's id.
    func noteOwnCommitMembershipChange(
        groupIdHex: String,
        record: GroupRecord,
        personKeysBefore: [String]?,
        commitMessageId: Int64?
    ) {
        guard let commitMessageId,
              !record.messages.contains(where: { $0.messageId == commitMessageId }),
              let row = membershipRow(
                groupIdHex: groupIdHex, record: record, personKeysBefore: personKeysBefore)
        else { return }
        record.appendLocalRow(
            text: row.marker, messageId: commitMessageId, createdAt: Self.isoTimestamp(ms: Self.nowMs()))
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// The row one committed change of `record` records, against the roster
    /// as it is now, or `nil` (a one-to-one, a device change, a reset's
    /// device swap, a torn read).
    private func membershipRow(
        groupIdHex: String, record: GroupRecord, personKeysBefore: [String]?
    ) -> MembershipRow? {
        let isGroup = conversationKind(groupIdHex: groupIdHex) == .group
        guard isGroup else { return nil }
        // A key an accepted reset retired names its account; so does a pinned
        // current key: the reset's two keys are then one person.
        let superseded = supersededKeyMap()
        var accountByPinnedKey: [String: String] = [:]
        for (accountIdB64, pin) in readmissionPins().allPins() {
            accountByPinnedKey[pin.pinnedSignaturePubkeyB64] = accountIdB64
        }
        let groupIdB64 = groupIdB64(ofHex: groupIdHex)
        let store = continuityStore
        let verdict = MembershipRow.forBatch(
            isGroup: isGroup,
            hadCommit: true,
            personKeysBefore: personKeysBefore,
            personKeysAfter: Self.personKeysB64(of: record.group),
            ownPersonKeyB64: AuthorIdentity.own(group: record.group),
            accountOfKey: { superseded[$0] ?? accountByPinnedKey[$0] },
            isSuperseded: { superseded[$0] != nil },
            awaited: groupIdB64.map { store.awaitedSwaps(groupIdB64: $0) } ?? [:],
            nowMs: Self.nowMs())
        if let groupIdB64 { store.setAwaitedSwaps(groupIdB64: groupIdB64, verdict.awaited) }
        return verdict.row
    }
}
