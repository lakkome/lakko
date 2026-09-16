// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The key that names a PERSON in a roster (ADR-0050).
///
/// iOS mirror of `apps/web/src/chat/person-key.ts`.
///
/// Since ADR-0050 a conversation leaf is a device, and one person holds one
/// leaf per device. Everything the UI says about a member — the name, the 1x1
/// test, roles, authorship, read state — is about the person, so it keys on the
/// leaf's resolved **account key** (`GroupMember.accountKey`), which every
/// device of the account shares. A leaf whose account did not resolve (empty
/// `accountKey`) falls back to its own leaf key so it still renders.
public enum PersonKey {

    /// The account key of a roster leaf, else its leaf signature key.
    public static func of(leafKey: Data, accountKey: Data) -> Data {
        accountKey.count == 32 ? accountKey : leafKey
    }

    /// ``of(leafKey:accountKey:)`` over a live roster member.
    public static func of(_ member: GroupMember) -> Data {
        of(leafKey: member.signaturePublicKey, accountKey: member.accountKey)
    }

    /// One row per person, in first-leaf order. The row carries the person
    /// key as its `signaturePublicKey` (what roles, names and authorship key
    /// on) and a representative leaf index: this device's own leaf for its own
    /// person when `ownLeafIndex` is among that person's leaves, else the
    /// person's first leaf. Removing a row removes every leaf of the person.
    public static func collapse(_ roster: [GroupMember], ownLeafIndex: UInt32?) -> [GroupMember] {
        var order: [Data] = []
        var rows: [Data: GroupMember] = [:]
        for member in roster {
            let person = of(member)
            if var existing = rows[person] {
                if let ownLeafIndex, member.leafIndex == ownLeafIndex {
                    existing = GroupMember(
                        leafIndex: ownLeafIndex,
                        signaturePublicKey: person,
                        accountKey: member.accountKey)
                    rows[person] = existing
                }
                continue
            }
            order.append(person)
            rows[person] = GroupMember(
                leafIndex: member.leafIndex,
                signaturePublicKey: person,
                accountKey: member.accountKey)
        }
        return order.compactMap { rows[$0] }
    }

    /// The distinct people in a roster, in first-leaf order.
    public static func people(_ roster: [GroupMember]) -> [Data] {
        var out: [Data] = []
        for member in roster {
            let person = of(member)
            if !out.contains(person) { out.append(person) }
        }
        return out
    }

    /// Every leaf index whose person is the person of `leafIndex`, the named
    /// leaf first. `[leafIndex]` when the leaf is not in the roster (the core
    /// then refuses).
    public static func leavesOfSamePerson(_ roster: [GroupMember], leafIndex: UInt32) -> [UInt32] {
        guard let named = roster.first(where: { $0.leafIndex == leafIndex }) else {
            return [leafIndex]
        }
        let person = of(named)
        let rest = roster
            .filter { $0.leafIndex != leafIndex && of($0) == person }
            .map { $0.leafIndex }
        return [leafIndex] + rest
    }
}
