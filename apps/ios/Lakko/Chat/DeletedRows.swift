// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `docs/ux/message-actions.md` section 9 — a deleted message leaves the
/// conversation.
///
/// A deleted message is not drawn. The one exception is a deletion applied
/// in front of the person (this conversation open, the scene `active`, the
/// row already drawn before the deletion was applied): the bubble then reads
/// "Message deleted" for ``holdSeconds``, collapses over
/// ``collapseSeconds`` and is gone. Every other deletion shows nothing.
///
/// The rules here are pure; the hold itself lives in memory in the screen's
/// view model and is never persisted. The same rules serve chat messages
/// (keyed on the DS message id) and channel posts (keyed on the cursor).
public enum DeletedRows {

    /// How long a row seen going reads "Message deleted" (chosen, not
    /// measured).
    public static let holdSeconds: Double = 2.0

    /// How long its height takes to animate to zero after the hold (chosen,
    /// not measured).
    public static let collapseSeconds: Double = 0.25

    /// Where a held row is in its going.
    public enum Phase: Equatable {
        /// Drawn as "Message deleted".
        case holding
        /// Its height animating to zero.
        case collapsing
    }

    /// The ids that enter the hold when a set of deletions is applied.
    ///
    /// - Parameters:
    ///   - drawnBefore: the ids the timeline drew before the deletions were
    ///     applied.
    ///   - deletedNow: the ids whose deletion was just applied.
    ///   - alreadyHeld: ids already in their hold (their own timer runs on).
    ///   - inFront: this conversation is the open screen and the scene is
    ///     `active`.
    public static func enteringHold<ID: Hashable>(
        drawnBefore: Set<ID>, deletedNow: Set<ID>, alreadyHeld: Set<ID> = [], inFront: Bool
    ) -> Set<ID> {
        guard inFront else { return [] }
        return deletedNow.intersection(drawnBefore).subtracting(alreadyHeld)
    }

    /// The chat messages whose deletion was applied between two states of a
    /// conversation: present and not deleted before, deleted after. A message
    /// that arrived already deleted (it and its deletion in one batch) is not
    /// among them: it was never drawn.
    public static func newlyDeleted(before: [ChatMessage], after: [ChatMessage]) -> Set<Int64> {
        let deletedAfter = Set(after.lazy.filter(\.deleted).map(\.messageId))
        guard !deletedAfter.isEmpty else { return [] }
        return Set(before.lazy.filter { !$0.deleted && deletedAfter.contains($0.messageId) }.map(\.messageId))
    }

    /// The ids a chat timeline drew: every message not deleted, plus the
    /// deleted ones still in their hold.
    public static func drawnIds(_ messages: [ChatMessage], held: Set<Int64>) -> Set<Int64> {
        Set(messages.lazy.filter { isDrawn($0, held: held) }.map(\.messageId))
    }

    /// Whether the timeline draws `message`: not deleted, or deleted and
    /// still in its hold.
    public static func isDrawn(_ message: ChatMessage, held: Set<Int64>) -> Bool {
        !message.deleted || held.contains(message.messageId)
    }

    /// The messages the timeline lays out (days, runs, authors): the deleted
    /// ones are dropped except those in their hold.
    public static func drawn(_ messages: [ChatMessage], held: Set<Int64>) -> [ChatMessage] {
        messages.filter { isDrawn($0, held: held) }
    }

    /// The row the chat list previews: the newest message that is not
    /// deleted and that the list may show (`isPreviewRow`); `nil` when none
    /// is left, and the list then says what it says for a conversation with
    /// no messages yet. Never "Message deleted".
    public static func listPreviewRow(
        _ messages: [ChatMessage], isPreviewRow: (ChatMessage) -> Bool
    ) -> ChatMessage? {
        messages.last { !$0.deleted && isPreviewRow($0) }
    }
}
