// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `docs/ux/message-actions.md` section 8 — Select: Delete and Forward in
/// bulk. Every rule of the mode that is not a pixel lives here, so the chat
/// screen only draws what this value says and the tests can drive the mode
/// without a screen.
///
/// A selection is a UI mode and nothing else: it adds no wire surface, it is
/// never persisted, and what it does at the end is the one-message Delete or
/// Forward repeated over a set (section 8, "building it is what stops those two
/// from being built a second time"). So this type holds a set of message ids
/// and answers the questions the bar and the rows ask about it; the sends
/// themselves are the single actions the view model already has.
public struct MessageSelection: Equatable {

    /// Section 8 — "at most 100 messages can be selected; at the limit
    /// further taps do nothing".
    public static let limit = 100

    /// The selected message ids. A set: the order a person tapped in is not
    /// the order anything is sent in (that is the timeline's, oldest first).
    public private(set) var ids: Set<Int64>

    /// Enter the mode with the message the card was opened on already
    /// selected (section 8, "Entering").
    public init(initial: Int64) {
        ids = [initial]
    }

    /// Section 8, "Leaving" — a completed action keeps only what it did not
    /// send; a selection rebuilt from those is the state the person is left in.
    public init(remaining: some Sequence<Int64>) {
        ids = Set(remaining)
    }

    public var count: Int { ids.count }
    public var isEmpty: Bool { ids.isEmpty }
    public var isAtLimit: Bool { ids.count >= Self.limit }

    public func contains(_ id: Int64) -> Bool { ids.contains(id) }

    /// A tap (or a long press, which toggles like a tap in this mode) on a
    /// selectable row. Returns whether anything changed: at the limit a tap
    /// that would add does nothing, while a tap that removes always works, so
    /// the person is never stuck at 100.
    @discardableResult
    public mutating func toggle(_ id: Int64) -> Bool {
        if ids.contains(id) {
            ids.remove(id)
            return true
        }
        guard !isAtLimit else { return false }
        ids.insert(id)
        return true
    }

    /// Drop ids the timeline no longer offers (a selected message that was
    /// deleted from another device, or that a sweep tombstoned). Called with
    /// the ids the timeline currently draws as selectable.
    public mutating func retain(onlyIn selectable: Set<Int64>) {
        ids.formIntersection(selectable)
    }

    // MARK: - The bar

    /// Section 8 — the count in the middle of the selection bar: "1 selected",
    /// "12 selected", and at the cap "100 selected, the limit".
    public var countLabel: String {
        isAtLimit ? "\(Self.limit) selected, the limit" : "\(count) selected"
    }

    /// Section 8 — `Forward` is enabled when the selection is not empty.
    public var forwardEnabled: Bool { !isEmpty }

    /// Section 8 — `Delete` is enabled when the selection is not empty and
    /// every selected message is the person's own: Lakko has no "delete for
    /// me", a delete goes to everyone and peers accept it only from the author
    /// (ADR-0023), so a delete of someone else's message would be a control
    /// that silently does nothing.
    ///
    /// `allOwn` is the caller's read over the selected messages, once for the
    /// whole selection (section 8, "the gates are the ones the single actions
    /// already compute ... read once for the whole selection").
    public func deleteEnabled(allOwn: Bool) -> Bool {
        !isEmpty && allOwn
    }

    /// Section 8 — the helper line under the bar, shown exactly when the
    /// selection holds someone else's message. `nil` otherwise: an empty
    /// selection has nothing to explain, and an all-own one needs no help.
    public func deleteHelper(allOwn: Bool) -> String? {
        (!isEmpty && !allOwn) ? Self.deleteHelperCopy : nil
    }

    public static let deleteHelperCopy = "Only your own messages can be deleted."

    /// Section 8 — a bulk delete confirms: "Delete 12 messages?", "Delete 1
    /// message?" for one. The confirming button is `Delete`, destructive, with
    /// `Cancel` beside it.
    public static func confirmDeleteLabel(_ n: Int) -> String {
        n == 1 ? "Delete 1 message?" : "Delete \(n) messages?"
    }

    // MARK: - Which rows

    /// Section 8 — rows that cannot be acted on are not selectable and show
    /// no mark: a date separator or a system row, a deleted message, a
    /// message whose send has not completed or has failed (the Forward gate of
    /// section 5.1). The gates are the ones the row already computes for the
    /// single actions, handed in as facts so this stays a pure rule.
    ///
    /// `actionsAvailable` is ADR-0045 UX 6.2.5: once this device's membership
    /// has ended nothing can be deleted or forwarded from here, so nothing is
    /// selectable either.
    public static func canSelect(
        actionsAvailable: Bool,
        isSystemRow: Bool,
        hasId: Bool,
        deleted: Bool,
        sendComplete: Bool
    ) -> Bool {
        actionsAvailable && !isSystemRow && hasId && !deleted && sendComplete
    }

    /// The selected ids in the timeline's own order — oldest first — given the
    /// ids the timeline draws, in the order it draws them. Section 8 sends in
    /// this order for both actions, and the target conversation then shows the
    /// forwards "in their original order".
    public func selectedInOrder(of timelineIds: [Int64]) -> [Int64] {
        timelineIds.filter { ids.contains($0) }
    }

    // MARK: - Accessibility

    /// Section 8 — the mark is a toggle named by the message's text or kind,
    /// stating "Selected" or "Not selected".
    public static func accessibilityName(text: String, attachmentCount: Int) -> String {
        if !text.isEmpty { return text }
        return attachmentCount > 0 ? "Media message" : "Message"
    }

    public static func accessibilityValue(selected: Bool) -> String {
        selected ? "Selected" : "Not selected"
    }

    // MARK: - The bulk run

    /// Which of the two multiplied actions is running; it decides the verb in
    /// the progress copy.
    public enum BulkKind: Equatable {
        case delete
        case forward
    }

    /// Section 8 — what the bar says after a run stopped short: "Deleted 5 of
    /// 12." / "Forwarded 5 of 12."
    public static func progressLabel(kind: BulkKind, done: Int, total: Int) -> String {
        switch kind {
        case .delete: return "Deleted \(done) of \(total)."
        case .forward: return "Forwarded \(done) of \(total)."
        }
    }

    /// The result of a bulk run over `total` messages.
    public struct BulkOutcome<Item: Equatable>: Equatable {
        /// How many steps completed before the run stopped (all of them on
        /// success).
        public let done: Int
        public let total: Int
        /// The items not sent: empty on success, else the failed one and every
        /// one after it, in the order they were given. Section 8 keeps exactly
        /// these selected.
        public let remaining: [Item]

        public var succeeded: Bool { remaining.isEmpty }

        public init(done: Int, total: Int, remaining: [Item]) {
            self.done = done
            self.total = total
            self.remaining = remaining
        }
    }

    /// Run `step` over `items` in the order given (the caller hands them oldest
    /// first), awaiting each, stopping at the first failure (section 8: "on a
    /// failure it stops, keeps the unsent messages selected"). The step's error
    /// is not surfaced: what the person is told is the count, which is the
    /// progress copy's job.
    ///
    /// Generic over the item, so the tests drive it with integers and the
    /// screen with messages, through the same loop.
    public static func run<Item: Equatable>(
        over items: [Item],
        step: (Item) async throws -> Void
    ) async -> BulkOutcome<Item> {
        var done = 0
        for (index, item) in items.enumerated() {
            do {
                try await step(item)
                done += 1
            } catch {
                return BulkOutcome(done: done, total: items.count, remaining: Array(items[index...]))
            }
        }
        return BulkOutcome(done: done, total: items.count, remaining: [])
    }
}
