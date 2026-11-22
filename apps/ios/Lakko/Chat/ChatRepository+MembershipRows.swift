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
        return MembershipRow.personKeysB64(members)
    }

    /// The poll's batch: each commit it applied names the people it changed
    /// on its own row, judged against the roster right after the commit
    /// before it (``MembershipRow/forCommits(isGroup:personKeysBefore:commits:ownPersonKeyB64:accountOfKey:isSuperseded:awaited:nowMs:)``).
    /// `personKeysAfterCommit` is what the apply read after each commit.
    func noteMembershipChanges(
        groupIdHex: String,
        record: GroupRecord,
        events: [IncomingChatEvent],
        personKeysBefore: [String]?,
        personKeysAfterCommit: [Int64: [String]],
        firstNewRow: Int
    ) {
        let commitIds: [Int64] = events.compactMap {
            if case .system(let messageId, .commit, _) = $0 { return messageId }
            return nil
        }
        guard !commitIds.isEmpty, conversationKind(groupIdHex: groupIdHex) == .group else { return }
        let context = membershipContext(groupIdHex: groupIdHex)
        let verdict = MembershipRow.forCommits(
            isGroup: true,
            personKeysBefore: personKeysBefore,
            commits: commitIds.map { ($0, personKeysAfterCommit[$0]) },
            ownPersonKeyB64: AuthorIdentity.own(group: record.group),
            accountOfKey: context.accountOfKey,
            isSuperseded: context.isSuperseded,
            awaited: context.awaited,
            nowMs: Self.nowMs())
        context.keep(verdict.awaited)
        for (messageId, row) in verdict.rows {
            record.recordMembershipChange(row, messageId: messageId, fromIndex: firstNewRow)
        }
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
        guard conversationKind(groupIdHex: groupIdHex) == .group else { return nil }
        let context = membershipContext(groupIdHex: groupIdHex)
        let verdict = MembershipRow.forBatch(
            isGroup: true,
            hadCommit: true,
            personKeysBefore: personKeysBefore,
            personKeysAfter: Self.personKeysB64(of: record.group),
            ownPersonKeyB64: AuthorIdentity.own(group: record.group),
            accountOfKey: context.accountOfKey,
            isSuperseded: context.isSuperseded,
            awaited: context.awaited,
            nowMs: Self.nowMs())
        context.keep(verdict.awaited)
        return verdict.row
    }

    /// What naming the people of a group's change reads: who a key belongs
    /// to, which keys an accepted reset retired, and the device-swap memory
    /// (read now, kept by `keep`).
    private func membershipContext(groupIdHex: String) -> (
        accountOfKey: (String) -> String?,
        isSuperseded: (String) -> Bool,
        awaited: [String: Int64],
        keep: ([String: Int64]) -> Void
    ) {
        // A key an accepted reset retired names its account; so does a pinned
        // current key: the reset's two keys are then one person.
        let superseded = supersededKeyMap()
        var accountByPinnedKey: [String: String] = [:]
        for (accountIdB64, pin) in readmissionPins().allPins() {
            accountByPinnedKey[pin.pinnedSignaturePubkeyB64] = accountIdB64
        }
        let groupIdB64 = groupIdB64(ofHex: groupIdHex)
        let store = continuityStore
        return (
            accountOfKey: { superseded[$0] ?? accountByPinnedKey[$0] },
            isSuperseded: { superseded[$0] != nil },
            awaited: groupIdB64.map { store.awaitedSwaps(groupIdB64: $0) } ?? [:],
            keep: { awaited in
                if let groupIdB64 { store.setAwaitedSwaps(groupIdB64: groupIdB64, awaited) }
            }
        )
    }
}
