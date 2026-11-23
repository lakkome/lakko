// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// What a device does with the history bundle it received at pairing
// (`docs/ux/second-device.md` section 2).
//
// A bundle lands in the message log, for groups this device mostly does not
// hold yet: the Welcomes that let it into them come later, one by one. Two
// rules follow.
//
//  - The log is written whole from the repository's memory, so a group not in
//    memory keeps its slot here (``WaitingLogSlots``), and a group that is
//    joined takes its slot into memory the moment it is registered. Without
//    this the first write after the first join dropped every bundle row.
//  - History is not news: each group's floor is the highest id its bundle
//    carried, and a row at or below it is never unread on this device. The
//    floor is local (no read receipt, no read cursor) and moves up only.

enum PairingHistory {

    /// The floor a bundle's `rows` set: their highest message id, `nil` when
    /// they hold no id the server assigned.
    static func bundleFloor(_ rows: [PersistedMessage]) -> Int64? {
        rows.map(\.messageId).max().flatMap { $0 > 0 ? $0 : nil }
    }

    /// The higher of two floors. A floor never moves down.
    static func higherFloor(_ a: Int64?, _ b: Int64?) -> Int64? {
        [a, b].compactMap { $0 }.max()
    }

    /// `rows` taken into the log rows `held`: a row already held stays as
    /// held. Rows the server numbered go in id order; a row a device made
    /// locally (a call row, its id not the server's) goes where its time puts
    /// it (``placeLocalRows(_:into:time:stopsAt:)``). Returns how many were
    /// added.
    static func mergeLogRows(
        _ held: [PersistedMessage], _ rows: [PersistedMessage]
    ) -> (rows: [PersistedMessage], added: Int) {
        var ids = Set(held.map(\.messageId))
        let fresh = rows.filter { ids.insert($0.messageId).inserted }
        let all = held + fresh
        var out = all.filter { $0.messageId > 0 }.sorted { $0.messageId < $1.messageId }
        placeLocalRows(all.filter { $0.messageId <= 0 }, into: &out, time: \.createdAt, stopsAt: { _ in false })
        return (out, fresh.count)
    }

    /// `held`, the rows of a live record, with a bundle's `bundle` rows taken
    /// in: a row already held stays as held, each new row goes before the
    /// first held row with a higher id (and before a row the server has not
    /// named yet), and a row the sender made locally (a call row, its id not
    /// the server's) goes where its time puts it, as a live call row does.
    static func mergeLiveRows(held: [ChatMessage], bundle: [ChatMessage]) -> [ChatMessage] {
        var ids = Set(held.map(\.messageId))
        let fresh = bundle.filter { ids.insert($0.messageId).inserted }
        var out = held
        var from = 0
        for row in fresh.filter({ $0.messageId > 0 }).sorted(by: { $0.messageId < $1.messageId }) {
            var at = from
            while at < out.count, out[at].messageId <= row.messageId, out[at].ack == .sent { at += 1 }
            out.insert(row, at: at)
            from = at + 1
        }
        placeLocalRows(
            fresh.filter { $0.messageId <= 0 }, into: &out, time: \.createdAt, stopsAt: { $0.ack != .sent })
        return out
    }

    /// Put `locals` into `out` by time, oldest first: each before the first
    /// row of `out` at its time or later, and before the first row `stopsAt`
    /// names (a row the server has not named yet). A row whose time cannot be
    /// read goes first, in the order given.
    static func placeLocalRows<Row>(
        _ locals: [Row], into out: inout [Row], time: KeyPath<Row, String>, stopsAt: (Row) -> Bool
    ) {
        let timed = locals.map { (row: $0, ms: DsTimestamp.parseMs($0[keyPath: time])) }
        var unknown = 0
        for entry in timed where entry.ms == nil {
            out.insert(entry.row, at: unknown)
            unknown += 1
        }
        let known = timed.compactMap { e in e.ms.map { (row: e.row, ms: $0) } }
        for entry in known.enumerated().sorted(by: { ($0.element.ms, $0.offset) < ($1.element.ms, $1.offset) }) {
            let at = out.indices.first { i in
                i >= unknown && (stopsAt(out[i])
                    || (DsTimestamp.parseMs(out[i][keyPath: time]).map { $0 >= entry.element.ms } ?? false))
            } ?? out.count
            out.insert(entry.element.row, at: at)
        }
    }

    /// A polled row that is already held from a bundle: the same row came
    /// twice (sent after the Welcome, before the bundle was made).
    static func heldFromBundle(floor: Int64?, messageId: Int64, rows: [ChatMessage]) -> Bool {
        guard let floor, messageId <= floor else { return false }
        return rows.contains { $0.messageId == messageId }
    }

    /// Whether a row of a group whose floor is `floor` came with the history.
    /// Never for an id the server did not assign.
    static func isHistory(messageId: Int64, floor: Int64?) -> Bool {
        guard let floor else { return false }
        return messageId > 0 && messageId <= floor
    }
}

/// The slots of the log for groups the repository does not hold.
struct WaitingLogSlots {
    private(set) var slots: [String: PersistedGroup] = [:]

    /// The bootstrap's split of the log: whatever it holds for a group outside
    /// `held` waits here.
    mutating func fill(from log: [String: PersistedGroup], held: Set<String>) {
        slots = log.filter { !held.contains($0.key) }
    }

    /// A bundle reached a group not held: its rows join what already waits.
    mutating func add(_ groupIdHex: String, _ incoming: PersistedGroup) {
        guard let slot = slots[groupIdHex] else {
            slots[groupIdHex] = incoming
            return
        }
        slots[groupIdHex] = PersistedGroup(
            cursor: slot.cursor,
            messages: PairingHistory.mergeLogRows(slot.messages, incoming.messages).rows,
            lastViewedMessageId: slot.lastViewedMessageId,
            feedSeen: slot.feedSeen,
            syncedThroughMessageId: PairingHistory.higherFloor(
                slot.syncedThroughMessageId, incoming.syncedThroughMessageId),
            cursorEpoch: slot.cursorEpoch)
    }

    /// The slot of `groupIdHex`, taken out: its group has just been joined.
    mutating func take(_ groupIdHex: String) -> PersistedGroup? {
        slots.removeValue(forKey: groupIdHex)
    }

    mutating func clear() {
        slots = [:]
    }

    /// The whole log a write puts on disk: the `held` groups, and the slots of
    /// the groups that wait for a join.
    func composed(with held: [String: PersistedGroup]) -> [String: PersistedGroup] {
        slots.merging(held) { _, live in live }
    }
}
