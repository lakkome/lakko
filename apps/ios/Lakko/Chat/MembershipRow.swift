// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The named membership row of a group (`docs/ux/second-device.md` section 4,
/// "Membership rows", decided 2026-10-29).
///
/// There is no generic "Members changed" row. When a receive batch that
/// carries at least one commit changes the PEOPLE of a group (ADR-0050 person
/// keys before and after the batch differ), the batch's last commit row is
/// rewritten to carry the person keys that joined and the person keys that
/// left. Names are never stored: they are resolved when the row is drawn.
///
/// Stored in the row's text, like a call row (``CallTranscriptRow``):
/// `[mls commit joined=<b64>,<b64> left=<b64>]`. The marker keeps the
/// `[mls ` prefix and the `]` suffix, so the history bundle still leaves it
/// out (``HistoryBundleCodec/isBookkeepingSystemRow(_:)``), and a plain
/// `[mls commit]` (every stored row from before this, every device-only
/// change) is not a membership row and is never drawn.
public struct MembershipRow: Equatable {
    /// Person keys (base64) that became members, in roster order.
    public let joinedKeysB64: [String]
    /// Person keys (base64) that are no longer members, in roster order.
    public let leftKeysB64: [String]

    public init?(joinedKeysB64: [String], leftKeysB64: [String]) {
        guard !joinedKeysB64.isEmpty || !leftKeysB64.isEmpty else { return nil }
        self.joinedKeysB64 = joinedKeysB64
        self.leftKeysB64 = leftKeysB64
    }

    /// The text of a commit row with no membership change (what
    /// ``ChatRepository`` writes for every commit).
    public static let plainCommitText = "[mls commit]"
    private static let prefix = "[mls commit "

    /// The stored text of this row.
    public var marker: String {
        "\(Self.prefix)joined=\(joinedKeysB64.joined(separator: ",")) left=\(leftKeysB64.joined(separator: ","))]"
    }

    /// The row a stored text records, or `nil` when it is not a membership
    /// row (a plain commit, another system row, or a malformed marker).
    public static func parse(_ text: String) -> MembershipRow? {
        guard text.hasPrefix(prefix), text.hasSuffix("]") else { return nil }
        let body = text.dropFirst(prefix.count).dropLast()
        let parts = body.split(separator: " ", omittingEmptySubsequences: false)
        guard parts.count == 2,
              parts[0].hasPrefix("joined="), parts[1].hasPrefix("left=") else { return nil }
        guard let joined = keys(parts[0].dropFirst("joined=".count)),
              let left = keys(parts[1].dropFirst("left=".count)) else { return nil }
        return MembershipRow(joinedKeysB64: joined, leftKeysB64: left)
    }

    private static func keys(_ list: Substring) -> [String]? {
        if list.isEmpty { return [] }
        var out: [String] = []
        for item in list.split(separator: ",", omittingEmptySubsequences: false) {
            guard let bytes = Data(base64Encoded: String(item)), bytes.count == 32 else { return nil }
            out.append(String(item))
        }
        return out
    }

    /// One sentence per person, joined first: "{name} joined", "{name} left".
    public func sentences(name: (String) -> String) -> [String] {
        joinedKeysB64.map { Localization.text("chat.system.member_joined", texts: ["name": name($0)]) }
            + leftKeysB64.map { Localization.text("chat.system.member_left", texts: ["name": name($0)]) }
    }

    /// What one receive batch records.
    public struct BatchVerdict: Equatable {
        /// The row to write on the batch's last commit, `nil` for none.
        public let row: MembershipRow?
        /// The device-swap memory to keep (``ResetRepair/deviceSwapAcrossCommits``).
        public let awaited: [String: Int64]
    }

    /// The distinct people of a roster (ADR-0050 person keys, base64), in
    /// first-leaf order.
    public static func personKeysB64(_ members: [GroupMember]) -> [String] {
        PersonKey.people(members).map { base64Encode($0) }
    }

    /// What the commits of one receive batch record, each on its own row.
    public struct CommitsVerdict: Equatable {
        /// The row to write on each commit, by DS message id; a commit with
        /// no row is absent.
        public let rows: [Int64: MembershipRow]
        /// The device-swap memory to keep.
        public let awaited: [String: Int64]
    }

    /// Decide what each commit of a batch records, judged commit by commit:
    /// the people after the previous commit (`personKeysBefore` for the first)
    /// against the people right after this one.
    ///
    /// Judging the batch as a whole loses a change that a later commit of the
    /// same batch undoes: a person removed and added back between two polls
    /// is the same set of people before and after, and nothing was recorded
    /// (seen live 2026-11-22: the committer drew both lines, a member that
    /// received both commits in one poll drew none). `commits` is in DS
    /// order; a `nil` roster (a torn read) records nothing for that commit or
    /// the next one, whose "before" is then unknown.
    public static func forCommits(
        isGroup: Bool,
        personKeysBefore: [String]?,
        commits: [(messageId: Int64, personKeysAfter: [String]?)],
        ownPersonKeyB64: String?,
        accountOfKey: (String) -> String?,
        isSuperseded: (String) -> Bool,
        awaited: [String: Int64],
        nowMs: Int64
    ) -> CommitsVerdict {
        var rows: [Int64: MembershipRow] = [:]
        var memory = awaited
        var before = personKeysBefore
        for commit in commits {
            let verdict = forBatch(
                isGroup: isGroup, hadCommit: true,
                personKeysBefore: before, personKeysAfter: commit.personKeysAfter,
                ownPersonKeyB64: ownPersonKeyB64, accountOfKey: accountOfKey,
                isSuperseded: isSuperseded, awaited: memory, nowMs: nowMs)
            memory = verdict.awaited
            if let row = verdict.row { rows[commit.messageId] = row }
            before = commit.personKeysAfter
        }
        return CommitsVerdict(rows: rows, awaited: memory)
    }

    /// Decide what a batch records.
    ///
    /// - Nothing outside a group (a one-to-one cannot change its people,
    ///   ADR-0062) or for a batch with no commit.
    /// - Nothing for a device change or for one half of a reset's device swap
    ///   (``ResetRepair/deviceSwapAcrossCommits``, whose memory is returned).
    /// - Otherwise the people that came and went, a person named by its
    ///   account (`accountOfKey`, so a key an accepted reset retired and its
    ///   successor are one person); this device's own person is left out.
    /// - A torn roster read on either side records nothing.
    public static func forBatch(
        isGroup: Bool,
        hadCommit: Bool,
        personKeysBefore: [String]?,
        personKeysAfter: [String]?,
        ownPersonKeyB64: String?,
        accountOfKey: (String) -> String?,
        isSuperseded: (String) -> Bool,
        awaited: [String: Int64],
        nowMs: Int64
    ) -> BatchVerdict {
        guard isGroup, hadCommit else { return BatchVerdict(row: nil, awaited: awaited) }
        let swap = ResetRepair.deviceSwapAcrossCommits(
            personKeysBefore: personKeysBefore, personKeysAfter: personKeysAfter,
            accountOfKey: accountOfKey, isSuperseded: isSuperseded,
            awaited: awaited, nowMs: nowMs)
        guard !swap.suppress, let before = personKeysBefore, let after = personKeysAfter else {
            return BatchVerdict(row: nil, awaited: swap.awaited)
        }
        var personOf: [String: String] = [:]
        for key in before + after + (ownPersonKeyB64.map { [$0] } ?? []) {
            personOf[key] = accountOfKey(key) ?? key
        }
        let peopleBefore = Set(before.compactMap { personOf[$0] })
        let peopleAfter = Set(after.compactMap { personOf[$0] })
        let ownPerson = ownPersonKeyB64.flatMap { personOf[$0] }
        func changed(_ keys: [String], absentFrom people: Set<String>) -> [String] {
            var out: [String] = []
            var seen: Set<String> = []
            for key in keys {
                guard let person = personOf[key], !people.contains(person), person != ownPerson,
                      seen.insert(person).inserted else { continue }
                out.append(key)
            }
            return out
        }
        let joined = changed(after, absentFrom: peopleBefore)
        let left = changed(before, absentFrom: peopleAfter)
        return BatchVerdict(
            row: MembershipRow(joinedKeysB64: joined, leftKeysB64: left), awaited: swap.awaited)
    }
}
