// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Which of the two lists a conversation belongs to (UX spec section 3).
///
/// Since ADR-0060 the kind is NOT derived on this device. The creator writes
/// it into the group's GroupContext (`0xFF03`) and every member reads it with
/// `Group.groupKind()`, so it is the same answer on every device and never
/// changes: a two-person group is a group, and a one-to-one never becomes a
/// group by growing (it cannot grow). The people count is for display only.
public enum ConversationKind: Equatable {
    case chat
    case group

    /// The list a conversation belongs to, from what `Group.groupKind()`
    /// returned.
    ///
    /// `nil` counts as a GROUP, the same choice as web and Android: every
    /// conversation carries a kind (every `Group.create` passes one and
    /// `joinFromWelcome` refuses a conversation without one), so `nil` only
    /// covers objects that are not conversations (`PublisherGroup`,
    /// `DeviceGroup`), a failed read, or a handle not loaded yet. Reading
    /// those as a group never names one member as "the person", never offers
    /// the one-to-one pin standing, and never places a 1x1 call.
    public static func of(groupKind: GroupKind?) -> ConversationKind {
        groupKind == .oneToOne ? .chat : .group
    }

    /// ADR-0060 section 7 — whether a group-name envelope (ADR-0051) may be
    /// applied in a conversation of `groupKind`. A one-to-one has no group
    /// name, and applying one would retitle the person, so it is dropped.
    public static func acceptsGroupName(groupKind: GroupKind?) -> Bool {
        of(groupKind: groupKind) == .group
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
