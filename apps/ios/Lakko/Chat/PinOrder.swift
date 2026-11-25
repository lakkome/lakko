// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The order of pinned conversations (`docs/ux/navigation.md` section 2.2.1):
/// the client-side half.
///
/// **The rule is not here.** Where a pin is drawn is `order_conversations`
/// (and `order_conversations_in_folder` inside a folder); what a pin, an
/// unpin and a move write is `pin_order_after_pin` / `_unpin` / `_move`, and
/// whether Move up / Move down applies is `pin_can_move`, all in
/// `crates/lakko-ui-rules`, shared with web and Android over the FFI. What
/// this file owns is the mapping between the synced register (bytes) and the
/// row ids iOS draws, which helper each action calls, and the arithmetic that
/// turns SwiftUI's `.onMove` arguments into the index the helper takes.
///
/// Pure of SwiftUI and of I/O, so a test can run every piece of it.
public enum PinOrder {

    /// What the person did to a pin.
    public enum Action: Equatable {
        case pin
        case unpin
        /// `toIndex` is the pin's index among the SHOWN pins after the move
        /// (0 = first; anything at or past the end = last).
        case move(toIndex: Int)
    }

    // MARK: - Register <-> row ids

    /// The synced `ConversationOrder` register as row ids: the same lower-case
    /// hex the flags register is keyed on (a conversation id for a chat or a
    /// group, a channel id for a channel).
    public static func storedIds(_ groupIds: [Data]) -> [String] {
        groupIds.map(ChatFolders.hex)
    }

    /// The register as row ids, from the published state (`nil` = never
    /// written).
    public static func storedIds(_ order: OrderState?) -> [String] {
        storedIds(order?.groupIds ?? [])
    }

    /// The bytes a list of row ids writes back. An id that is not hex (which
    /// no row id is) is dropped rather than written as garbage.
    public static func bytes(_ ids: [String]) -> [Data] {
        ids.compactMap { ChatFolders.bytes(fromHex: $0) }
    }

    /// `ids` spelled the way `rows` spell them, so the shared rule matches a
    /// stored id to its row whatever the letter case. Ids naming no row are
    /// kept as they are: the rule ignores them.
    public static func spelledAsRows(_ ids: [String], rows: [ConversationListRow]) -> [String] {
        var byLower: [String: String] = [:]
        for row in rows { byLower[row.id.lowercased()] = row.id }
        return ids.map { byLower[$0.lowercased()] ?? $0 }
    }

    /// The pins as the list draws them, top first: the pinned rows of an
    /// ordered list. Archived pins stay with the pins (navigation 2.2).
    public static func shownPins(_ ordered: [ConversationListRow]) -> [String] {
        ordered.filter(\.pinned).map(\.id)
    }

    // MARK: - What an action writes

    /// The WHOLE pin list `action` writes, from the shared helpers. `stored`
    /// is the synced list as row ids (entries this device does not show are
    /// kept in place by the helpers, so they are never filtered out here);
    /// `shown` the pins in the order the list draws them.
    public static func next(
        _ action: Action, stored: [String], shown: [String], id: String
    ) -> [String] {
        switch action {
        case .pin:
            return LakkoCore.pinOrderAfterPin(stored: stored, shown: shown, id: id)
        case .unpin:
            return LakkoCore.pinOrderAfterUnpin(stored: stored, shown: shown, id: id)
        case .move(let toIndex):
            return LakkoCore.pinOrderAfterMove(
                stored: stored, shown: shown, id: id, toIndex: UInt32(clamping: max(0, toIndex)))
        }
    }

    /// The index among the shown pins a Move up (`up`) or Move down lands
    /// on, or `nil` when the shared rule says the move does not apply (the
    /// first pin up, the last pin down, a single pin, a row that is no pin).
    public static func stepTarget(shown: [String], id: String, up: Bool) -> Int? {
        guard LakkoCore.pinCanMove(shown: shown, id: id, up: up),
              let index = shown.firstIndex(of: id) else { return nil }
        return up ? index - 1 : index + 1
    }

    // MARK: - Drag

    /// Whether a drag may start in this view: two pins or more, nothing typed
    /// in search, and not the archive (which holds no pins).
    public static func dragEnabled(pinCount: Int, searching: Bool, inArchive: Bool) -> Bool {
        pinCount >= 2 && !searching && !inArchive
    }

    /// SwiftUI's `.onMove(source, destination)` over the pinned rows as the
    /// moved pin's id and the index it ends at among the pins. `destination`
    /// counts positions BEFORE the move, so a pin moved down lands one place
    /// above it. `nil` when there is nothing to do.
    public static func dropTarget(
        pins: [String], source: IndexSet, destination: Int
    ) -> (id: String, toIndex: Int)? {
        guard source.count == 1, let from = source.first, pins.indices.contains(from) else {
            return nil
        }
        let clamped = max(0, min(pins.count, destination))
        let to = clamped > from ? clamped - 1 : clamped
        guard to != from else { return nil }
        return (pins[from], to)
    }
}

/// The leading run of pinned rows of an ordered list, and the rest. The pins
/// are drawn as their own run so a drag reorders them among themselves and
/// never drops one among the unpinned rows (navigation 2.2.1).
public struct PinnedRun<Row> {
    public let pinned: [Row]
    public let rest: [Row]

    /// Split `ordered` (pins first, by the shared order) at the first row
    /// that is not pinned.
    public static func split(_ ordered: [Row], isPinned: (Row) -> Bool) -> PinnedRun<Row> {
        let pinned = Array(ordered.prefix(while: isPinned))
        return PinnedRun(pinned: pinned, rest: Array(ordered.dropFirst(pinned.count)))
    }
}
