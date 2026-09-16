// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The rule that decides which list a conversation belongs to once a kind may
/// have been persisted (docs/ux/client-redesign.md sections 3 and 5.1).
///
/// The persisted kind wins. The people count is only the first-sight
/// fallback: a conversation seen for the first time through a Welcome or a
/// restore derives its kind from the roster once and persists it. The one
/// observation that can change a stored kind is a roster above two people,
/// which flips `chat` to `group`; nothing ever flips `group` back to `chat`,
/// so a group made through the new-group flow stays a group while it still
/// has two people, and a chat never moves between the two lists on its own.
public enum ConversationKindDecision: Equatable {

    /// What the caller should show, and what (if anything) it should write.
    public struct Verdict: Equatable {
        public let kind: ConversationKind
        /// The kind to persist now, or `nil` when the store already says the
        /// right thing (or nothing can be derived yet).
        public let persist: ConversationKind?
    }

    /// - Parameters:
    ///   - stored: the persisted kind, `nil` when none was recorded.
    ///   - peopleCount: the roster's people count (accounts, not device
    ///     leaves), `nil` while the roster cannot be read.
    public static func resolve(stored: ConversationKind?, peopleCount: Int?) -> Verdict {
        switch stored {
        case .group?:
            return Verdict(kind: .group, persist: nil)
        case .chat?:
            if let peopleCount, peopleCount > 2 {
                return Verdict(kind: .group, persist: .group)
            }
            return Verdict(kind: .chat, persist: nil)
        case nil:
            guard let peopleCount else {
                // An unreadable roster is not evidence of anything: show it
                // under Chats for now and decide once the roster is readable.
                return Verdict(kind: .chat, persist: nil)
            }
            let derived = ConversationKind.of(peopleCount: peopleCount)
            return Verdict(kind: derived, persist: derived)
        }
    }
}

/// Per-account map `groupIdHex -> "chat" | "group"` in `UserDefaults`.
///
/// Same shape and reasoning as ``KnownGroupsStore``: a tiny non-secret map
/// (group ids are already known to this device), namespaced by account so
/// two accounts on one device do not collide. App-private — the notification
/// extension never needs it.
public final class ConversationKindStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load(accountIdB64: String, groupIdHex: String) -> ConversationKind? {
        guard let raw = map(accountIdB64: accountIdB64)[groupIdHex] else { return nil }
        return ConversationKind(storageValue: raw)
    }

    /// Persist `kind`, honouring the one-way rule: a stored `group` is never
    /// downgraded to `chat`. Returns the kind now stored.
    @discardableResult
    public func record(
        accountIdB64: String, groupIdHex: String, kind: ConversationKind
    ) -> ConversationKind {
        var current = map(accountIdB64: accountIdB64)
        if current[groupIdHex] == ConversationKind.group.storageValue {
            return .group
        }
        current[groupIdHex] = kind.storageValue
        defaults.set(current, forKey: Self.key(accountIdB64))
        return kind
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func map(accountIdB64: String) -> [String: String] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: String] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "conversation_kind.\(accountIdB64)"
    }
}

extension ConversationKind {
    var storageValue: String {
        switch self {
        case .chat: return "chat"
        case .group: return "group"
        }
    }

    init?(storageValue: String) {
        switch storageValue {
        case "chat": self = .chat
        case "group": self = .group
        default: return nil
        }
    }
}
