// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What the feed has shown the person (ADR-0058 section 5), kept per group
/// in the message log. Mirror of web `feed/feed-seen.ts`.
///
/// A group's seen set is every message at or below the group's watermark
/// (the higher of the log's `lastViewedMessageId` and the account's
/// cross-device read cursor: the open chat marks all it holds, which is what
/// "highest viewed" always meant) plus the ids in `feedSeen`, the messages
/// the feed kept fully on screen for ``dwellSeconds``. The person's own and
/// system rows count as seen. When the seen set becomes contiguous past the
/// watermark — `contiguousSeen` in the core, the one definition every client
/// uses — the watermark moves up to it, the ids below it are dropped from
/// `feedSeen`, and the caller sends the group a read receipt and the other
/// devices a read cursor at the new watermark. It never moves past a message
/// the person did not see.
///
/// Pure: the log write lives in ``ChatRepository``, the sends in the feed's
/// view model.
enum FeedSeen {

    /// How long an item must be fully on screen to count as seen (chosen).
    static let dwellSeconds: Double = 2

    /// One row of a group's log as the seen rule reads it.
    struct Row: Equatable {
        let messageId: Int64
        /// `true` for a message someone else wrote; own and system rows are
        /// seen by definition.
        let incoming: Bool
    }

    struct Result: Equatable {
        let lastViewedMessageId: Int64?
        let feedSeen: Set<Int64>
        /// The new watermark when it moved past `through`, else `nil`.
        let advancedTo: Int64?
    }

    /// `true` when the person has seen `messageId` in a group whose watermark
    /// is `through` and whose feed-seen ids are `feedSeen`.
    static func isSeen(messageId: Int64, incoming: Bool, through: Int64?, feedSeen: Set<Int64>) -> Bool {
        if !incoming { return true }
        if messageId <= 0 { return false }
        if let through, messageId <= through { return true }
        return feedSeen.contains(messageId)
    }

    /// Add `ids` to the group's seen set and move the watermark as far as the
    /// set is contiguous. Ids that name no row are ignored.
    ///
    /// - Parameters:
    ///   - rows: the group's rows, in any order; rows without a server id
    ///     (`messageId <= 0`) are ignored.
    ///   - through: the current watermark (see the type's docs).
    ///   - contiguous: the core's `contiguousSeen`.
    static func markSeen(
        rows: [Row],
        through: Int64?,
        feedSeen: Set<Int64>,
        ids: [Int64],
        contiguous: ([Int64], [Int64]) -> Int64?
    ) -> Result {
        let present = Set(rows.lazy.filter { $0.messageId > 0 }.map(\.messageId))
        var seenIds = feedSeen.intersection(present)
        for id in ids where present.contains(id) { seenIds.insert(id) }

        var all: [Int64] = []
        var seen: [Int64] = []
        for row in rows where row.messageId > 0 {
            all.append(row.messageId)
            let isSeen = !row.incoming
                || (through.map { row.messageId <= $0 } ?? false)
                || seenIds.contains(row.messageId)
            if isSeen { seen.append(row.messageId) }
        }
        let watermark = contiguous(all, seen)
        var advancedTo: Int64?
        if let watermark, through.map({ watermark > $0 }) ?? true {
            advancedTo = watermark
        }
        let newThrough = advancedTo ?? through
        let kept = seenIds.filter { id in newThrough.map { id > $0 } ?? true }
        return Result(lastViewedMessageId: newThrough, feedSeen: kept, advancedTo: advancedTo)
    }

    /// The 8-byte big-endian form a DeviceGroup read cursor carries.
    static func readCursorBytes(_ id: Int64) -> Data {
        var out = Data(count: 8)
        var v = UInt64(bitPattern: id)
        for i in (0..<8).reversed() {
            out[i] = UInt8(v & 0xff)
            v >>= 8
        }
        return out
    }

    /// The id a DeviceGroup read cursor carries, or `nil` when it is not 8
    /// bytes.
    static func readCursorId(_ bytes: Data) -> Int64? {
        guard bytes.count == 8 else { return nil }
        var v: UInt64 = 0
        for b in bytes { v = (v << 8) | UInt64(b) }
        return Int64(bitPattern: v)
    }
}
