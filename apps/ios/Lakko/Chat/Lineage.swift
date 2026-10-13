// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 "Phase 2 revised", R.5 - one conversation over several groups.
//
// iOS mirror of `apps/web/src/chat/lineage.ts`; the tables are the same and
// so are the cases in `LineageTests`.
//
// A conversation that cannot be repaired in place (a one-to-one whose other
// person reset their keys, a group whose owner did) continues in a NEW MLS
// group. The person sees one conversation: the same row, the history above,
// the same pin / mute / archive / folder / draft / read position. That is a
// stable conversation id with a lineage of groups, not a re-key of any store:
//
//   * the conversation id is the group id of the lineage's FIRST group (the
//     root). Everything a person attaches to a conversation is keyed on it
//     and never rewritten;
//   * the segments are the lineage's groups in order. The message log stays
//     per group; the timeline is the segments concatenated, ordered by
//     `(segment index, DS message id)`, since a successor's ids restart;
//   * everything that belongs to one MLS session stays per group (the
//     handle, the journal cursor, push routing, shared pins, watermarks,
//     proposals, the outbox). "The conversation's current group" is the
//     lineage head: sends, calls and the push subscription use it.
//
// The lineage itself is the write-once register `successor group id -> root
// conversation id` of the ADR-0058 synced state (`lakko-sync`), which this
// file reads as plain values. Ids are lower-case hex, the form the synced
// state's projection uses. Free of the core, SwiftUI and storage.

/// One lineage register, as projected from the synced state.
public struct LineageEntry: Equatable {
    /// Hex of the group that continues the conversation.
    public let successorHex: String
    /// Hex of the conversation id (the lineage's first group).
    public let rootHex: String
    /// The HLC stamp the register was written at: the only thing that
    /// carries segment order (R.13a).
    public let hlc: UInt64

    public init(successorHex: String, rootHex: String, hlc: UInt64) {
        self.successorHex = successorHex
        self.rootHex = rootHex
        self.hlc = hlc
    }
}

/// The lineage of every conversation of the account.
public struct ConversationLineage: Equatable {

    /// `successor -> the root (or parent) its register names`.
    private let rootOfSuccessor: [String: String]
    /// `conversation id -> its groups in order`, only for conversations
    /// that have at least one successor.
    private let segments: [String: [String]]

    private init(rootOfSuccessor: [String: String], segments: [String: [String]]) {
        self.rootOfSuccessor = rootOfSuccessor
        self.segments = segments
    }

    /// The empty lineage: every group is its own conversation.
    public static let none = ConversationLineage.build([])

    /// Build the lineage from the registers.
    ///
    /// A register whose root is itself a successor (a device wrote the
    /// parent instead of the root) is followed to the real root, bounded, so
    /// a chain still reads as one conversation; a register naming itself,
    /// and a cycle, are ignored. Of two registers for one successor the
    /// later in `entries` stands.
    public static func build(_ entries: [LineageEntry]) -> ConversationLineage {
        var rootOfSuccessor: [String: String] = [:]
        var stampOf: [String: UInt64] = [:]
        for e in entries where e.successorHex != e.rootHex {
            rootOfSuccessor[e.successorHex] = e.rootHex
            stampOf[e.successorHex] = e.hlc
        }
        var segments: [String: [String]] = [:]
        for successorHex in rootOfSuccessor.keys {
            let root = resolve(successorHex, in: rootOfSuccessor)
            if root == successorHex { continue }
            segments[root, default: []].append(successorHex)
        }
        for (root, list) in segments {
            let sorted = list.sorted { a, b in
                let sa = stampOf[a] ?? 0
                let sb = stampOf[b] ?? 0
                if sa != sb { return sa < sb }
                return idPrecedes(a, b)
            }
            segments[root] = [root] + sorted
        }
        return ConversationLineage(rootOfSuccessor: rootOfSuccessor, segments: segments)
    }

    /// The conversation a group belongs to: its root, or itself.
    public func conversationOf(_ groupHex: String) -> String {
        Self.resolve(groupHex, in: rootOfSuccessor)
    }

    /// A conversation's groups in order: the root, then its successors by
    /// stamp (ties broken on the successor id, so every device agrees).
    public func segmentsOf(_ conversationHex: String) -> [String] {
        segments[conversationHex] ?? [conversationHex]
    }

    /// The index of a group in its conversation's segments.
    public func segmentIndexOf(_ groupHex: String) -> Int {
        segmentsOf(conversationOf(groupHex)).firstIndex(of: groupHex) ?? 0
    }

    /// Whether a group is some conversation's successor (not a root).
    public func isSuccessor(_ groupHex: String) -> Bool {
        conversationOf(groupHex) != groupHex
    }

    /// Follow the registers from a group to its root. A cycle names no
    /// root: the group stands alone.
    private static func resolve(_ groupHex: String, in rootOfSuccessor: [String: String]) -> String {
        var current = groupHex
        var seen: Set<String> = [current]
        while let next = rootOfSuccessor[current] {
            if seen.contains(next) { return groupHex }
            seen.insert(next)
            current = next
        }
        return current
    }

    /// The tie-break order of two ids: by UTF-16 code unit, which is what
    /// the web client's string comparison is, so the three clients order
    /// equal-stamp successors identically whatever the ids contain.
    private static func idPrecedes(_ a: String, _ b: String) -> Bool {
        a.utf16.lexicographicallyPrecedes(b.utf16)
    }
}

/// The pure rules over a `ConversationLineage`.
public enum LineageRules {

    // MARK: - The conversation's current group

    /// This device's membership of one group.
    public enum Membership: Equatable {
        case active, ended, absent
    }

    /// "The conversation's current group" (R.5): the LAST segment this
    /// device is an active member of. A successor the device has not joined
    /// yet is not the head for it: it keeps sending where it is (R.7 case 3)
    /// or holds the send (case 1). `nil` when the device holds no segment at
    /// all.
    ///
    /// When no segment is active (every membership ended), the last segment
    /// the device holds is the head, so the row and its history stay
    /// reachable.
    public static func headSegment(_ segments: [String], membership: (String) -> Membership) -> String? {
        var lastHeld: String?
        var lastActive: String?
        for hex in segments {
            let m = membership(hex)
            if m == .absent { continue }
            lastHeld = hex
            if m == .active { lastActive = hex }
        }
        return lastActive ?? lastHeld
    }

    // MARK: - Timeline order

    /// A position in a conversation's timeline.
    public struct TimelinePosition: Equatable {
        public let segment: Int
        public let messageId: Int64

        public init(segment: Int, messageId: Int64) {
            self.segment = segment
            self.messageId = messageId
        }
    }

    /// Timeline order (R.5): `(segment index, DS message id)`. Negative when
    /// `a` is earlier, positive when later, zero when the same position.
    public static func compareTimeline(_ a: TimelinePosition, _ b: TimelinePosition) -> Int {
        if a.segment != b.segment { return a.segment < b.segment ? -1 : 1 }
        if a.messageId != b.messageId { return a.messageId < b.messageId ? -1 : 1 }
        return 0
    }

    // MARK: - The read position, keyed on the conversation

    /// The largest message id a read cursor may carry: the web client's
    /// largest exact integer (2^53 - 1). Kept here so a cursor one client
    /// refuses is refused by all three.
    static let maxReadMessageId: UInt64 = (1 << 53) - 1

    /// The cross-device read cursor of a conversation is one register keyed
    /// on the conversation id. Its value has always been the 8-byte
    /// big-endian DS message id; a successor's ids restart, so a position
    /// past the first segment also carries the segment:
    ///
    ///   * segment 0: `message_id(8 BE)`: unchanged, what every build writes;
    ///   * segment n > 0: `message_id(8 BE) || segment(4 BE)`.
    ///
    /// A reader that knows only the 8-byte form reads a position in a later
    /// segment as "that message id in the first segment", which can only
    /// make it show LESS as read than was read, never more than exists there.
    ///
    /// A negative message id is written as 0.
    public static func encodeReadPosition(_ position: TimelinePosition) -> Data {
        var out = Data()
        out.reserveCapacity(position.segment > 0 ? 12 : 8)
        let id = UInt64(max(0, position.messageId))
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8(truncatingIfNeeded: id >> UInt64(shift)))
        }
        if position.segment > 0 {
            let segment = UInt32(clamping: position.segment)
            for shift in stride(from: 24, through: 0, by: -8) {
                out.append(UInt8(truncatingIfNeeded: segment >> UInt32(shift)))
            }
        }
        return out
    }

    /// Decode a read cursor's bytes; `nil` when shorter than a message id,
    /// or when the id is past `maxReadMessageId`.
    public static func decodeReadPosition(_ bytes: Data) -> TimelinePosition? {
        let raw = [UInt8](bytes)
        guard raw.count >= 8 else { return nil }
        var id: UInt64 = 0
        for byte in raw[0..<8] { id = (id << 8) | UInt64(byte) }
        guard id <= maxReadMessageId else { return nil }
        var segment: UInt32 = 0
        if raw.count >= 12 {
            for byte in raw[8..<12] { segment = (segment << 8) | UInt32(byte) }
        }
        return TimelinePosition(segment: Int(segment), messageId: Int64(id))
    }

    /// How far ONE segment is read.
    public enum ReadUpTo: Equatable {
        /// Nothing of this segment is read.
        case nothing
        /// Read up to this message id of the segment.
        case upTo(Int64)
        /// Every message of this segment is read.
        case all
    }

    /// What the read position means for ONE segment's own message ids: the
    /// id up to which that segment is read.
    ///
    /// | the position is in ...   | this segment is read up to |
    /// |--------------------------|----------------------------|
    /// | (no position)            | `nothing`                  |
    /// | an earlier segment       | `nothing`                  |
    /// | this segment             | the position's message id  |
    /// | a later segment          | `all`                      |
    public static func readUpToInSegment(_ position: TimelinePosition?, segment: Int) -> ReadUpTo {
        guard let position else { return .nothing }
        if position.segment < segment { return .nothing }
        if position.segment > segment { return .all }
        return .upTo(position.messageId)
    }

    // MARK: - The conversation list

    /// A group this device holds, as the list planning reads it.
    public struct HeldGroup: Equatable {
        public let groupHex: String
        public let active: Bool

        public init(groupHex: String, active: Bool) {
            self.groupHex = groupHex
            self.active = active
        }
    }

    /// One conversation of the list.
    public struct ConversationPlan: Equatable {
        /// The conversation id (the root's hex): the row's identity, and the
        /// key of every person-facing store.
        public let conversationHex: String
        /// The group the row opens and sends into.
        public let headHex: String
        /// Every segment this device holds, in timeline order (the head
        /// included, and any successor it holds beyond the head).
        public let heldSegments: [String]

        public init(conversationHex: String, headHex: String, heldSegments: [String]) {
            self.conversationHex = conversationHex
            self.headHex = headHex
            self.heldSegments = heldSegments
        }
    }

    /// One row per conversation (R.5: "a second one-to-one group with the
    /// same account never becomes a second row once it is in the lineage").
    /// Groups the device holds are folded by conversation; the order of the
    /// result follows the first held group of each conversation in `held`.
    /// A group listed twice is read as its last entry.
    public static func planConversations(_ held: [HeldGroup], lineage: ConversationLineage) -> [ConversationPlan] {
        var byHex: [String: HeldGroup] = [:]
        for g in held { byHex[g.groupHex] = g }
        var out: [ConversationPlan] = []
        var seen: Set<String> = []
        for g in held {
            let conversationHex = lineage.conversationOf(g.groupHex)
            if !seen.insert(conversationHex).inserted { continue }
            let segments = lineage.segmentsOf(conversationHex)
            let heldSegments = segments.filter { byHex[$0] != nil }
            let head = headSegment(segments) { hex in
                guard let entry = byHex[hex] else { return .absent }
                return entry.active ? .active : .ended
            }
            guard let headHex = head else { continue }
            out.append(ConversationPlan(conversationHex: conversationHex, headHex: headHex, heldSegments: heldSegments))
        }
        return out
    }

    // MARK: - The old segment after the move

    /// The DS journal retention (30 days today): how long an old group's
    /// journal is still read after the move. Chosen for parity with the DS,
    /// not measured.
    public static let oldSegmentPollWindowMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// R.5 "the old segment after the move": whether an old segment's
    /// journal is still polled.
    ///
    /// | the group is ...                                   | polled |
    /// |----------------------------------------------------|--------|
    /// | the conversation's head                            | yes    |
    /// | an older segment of a ONE-TO-ONE                   | no     |
    /// | an older segment of a group, inside the retention  | yes    |
    /// | an older segment of a group, past the retention    | no     |
    ///
    /// A one-to-one's old journal has no sender left but the superseded
    /// identity; a group's still carries members who have not moved yet (R.7
    /// case 3), for as long as the DS keeps the journal.
    ///
    /// `movedAtMs` is when this device learned the conversation moved, UNIX
    /// ms; `nil` when unknown (then the window has not started).
    public static func oldSegmentPolled(isHead: Bool, oneToOne: Bool, movedAtMs: Int64?, nowMs: Int64) -> Bool {
        if isHead { return true }
        if oneToOne { return false }
        guard let movedAtMs else { return true }
        let (elapsed, overflow) = nowMs.subtractingReportingOverflow(movedAtMs)
        // Out of range either way: far in the past is past the window, far
        // in the future is inside it.
        if overflow { return nowMs < movedAtMs }
        return elapsed < oldSegmentPollWindowMs
    }

    /// R.5 "push routing ... the old group's subscription is retired":
    /// whether this device retires its push subscription for one group now.
    ///
    /// | the group is ...                                             | retires |
    /// |--------------------------------------------------------------|---------|
    /// | its own conversation, or the conversation's current group    | no      |
    /// | an older segment, and the current group's membership is live | yes     |
    /// | an older segment, the current group not live here            | no      |
    /// | an older segment already retired                             | no      |
    ///
    /// "The current group" is the head as THIS device holds it
    /// (`headSegment`): a device that has not joined the successor yet still
    /// has the old group as its head and keeps its subscription, since that
    /// is where it still sends and is woken (R.7 case 3). `headHex` is the
    /// head of the group's conversation on this device, or `nil`.
    public static func retiresOldSegmentPush(
        groupHex: String,
        headHex: String?,
        headActive: Bool,
        alreadyRetired: Bool
    ) -> Bool {
        if alreadyRetired { return false }
        guard let headHex, headHex != groupHex else { return false }
        return headActive
    }

    // MARK: - The timeline of an open conversation

    /// A successor's DS message ids restart, so the rows of an OLDER segment
    /// cannot share the head's id space on screen (the list keys, dedups and
    /// jumps by message id). An older segment's row is shown under
    ///
    ///     id - distance * historyIdSpan        (distance >= 1 from the head)
    ///
    /// which is negative, unique per `(segment, message id)`, ascending
    /// inside a segment and ascending across segments: the rule `(segment
    /// index, message id)` as one number. Never stored and never sent: a
    /// history row is read-only (no edit, delete, reaction, reply or pin can
    /// name it).
    public static let historyIdSpan: Int64 = 1 << 40

    /// The largest magnitude an on-screen row id may have: the web client's
    /// largest exact integer (2^53 - 1), so the three clients agree on which
    /// numbers are history ids.
    static let maxRowIdMagnitude: Int64 = (1 << 53) - 1

    /// The on-screen id of an older segment's row. A distance below 1 is
    /// read as 1. A product out of range saturates at `Int64.min` rather
    /// than trapping; no real conversation has that many segments.
    public static func historyRowId(_ messageId: Int64, distanceFromHead: Int) -> Int64 {
        let distance = Int64(max(1, distanceFromHead))
        let (offset, mulOverflow) = distance.multipliedReportingOverflow(by: historyIdSpan)
        if mulOverflow { return Int64.min }
        let (rowId, subOverflow) = messageId.subtractingReportingOverflow(offset)
        return subOverflow ? Int64.min : rowId
    }

    /// What an on-screen history id stands for.
    public struct HistoryRowOrigin: Equatable {
        public let messageId: Int64
        public let distanceFromHead: Int

        public init(messageId: Int64, distanceFromHead: Int) {
            self.messageId = messageId
            self.distanceFromHead = distanceFromHead
        }
    }

    /// The inverse of `historyRowId`: the message id and the segment
    /// distance an on-screen history id stands for, or `nil` for an id that
    /// is not one (a head row, or a number no history row can have).
    public static func historyRowOrigin(_ rowId: Int64) -> HistoryRowOrigin? {
        guard rowId < 0, rowId >= -maxRowIdMagnitude else { return nil }
        let magnitude = -rowId
        // ceil(magnitude / span), in integers.
        let distance = (magnitude + historyIdSpan - 1) / historyIdSpan
        let messageId = rowId + distance * historyIdSpan
        guard messageId > 0, messageId < historyIdSpan else { return nil }
        return HistoryRowOrigin(messageId: messageId, distanceFromHead: Int(distance))
    }

    /// Whether an on-screen id belongs to an older segment (read-only).
    public static func isHistoryRowId(_ id: Int64?) -> Bool {
        guard let id else { return false }
        return id < 0
    }
}
