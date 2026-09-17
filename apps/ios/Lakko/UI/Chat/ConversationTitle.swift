// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which of the two lists a conversation belongs to (UX spec section 3).
///
/// Two PEOPLE (counted by account, never by device leaf) make a chat;
/// anything else is a group. A conversation whose roster cannot be read yet
/// (`nil` people count, e.g. the group handle is still being restored) is
/// listed under Chats until the roster says otherwise. This is only the
/// first-sight rule: the kind is persisted per conversation
/// (``ConversationKindStore``) and ``ConversationKindDecision`` decides how
/// the stored kind and the people count combine, which is what lets a group
/// made through the new-group flow stay a group while it still has two
/// people.
public enum ConversationKind: Equatable {
    case chat
    case group

    public static func of(peopleCount: Int?) -> ConversationKind {
        guard let peopleCount, peopleCount > 2 else { return .chat }
        return .group
    }
}

/// How a group without a name is titled (UX spec section 5.1): the other
/// members' names, up to three, then "+N"; "Group" when nobody has a name.
public enum ConversationTitle {

    /// - Parameter otherMemberNames: one entry per other person, `nil` when
    ///   that person has no readable name (a key fingerprint is not a name).
    public static func groupTitle(otherMemberNames: [String?]) -> String {
        let named = otherMemberNames.compactMap { name -> String? in
            guard let name else { return nil }
            let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        guard !named.isEmpty else { return "Group" }
        let shown = named.prefix(3)
        let rest = otherMemberNames.count - shown.count
        let head = shown.joined(separator: ", ")
        return rest > 0 ? "\(head) +\(rest)" : head
    }

    /// ADR-0051 §5 — the group title's precedence: the shared group name when
    /// one is held (trimmed, non-empty), else the member-derived title.
    public static func resolvedGroupTitle(sharedName: String?, otherMemberNames: [String?]) -> String {
        if let shared = sharedName?.trimmingCharacters(in: .whitespacesAndNewlines), !shared.isEmpty {
            return shared
        }
        return groupTitle(otherMemberNames: otherMemberNames)
    }

    /// "N members", singular-aware.
    public static func memberCountLabel(_ count: Int) -> String {
        count == 1 ? "1 member" : "\(count) members"
    }
}
