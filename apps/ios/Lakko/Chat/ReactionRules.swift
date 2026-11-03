// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Reactions-and-emoji spec sections 5, 7 and 10 — the client side of the
/// reaction rules. Every decision (what a person holds, what a tap does,
/// which chips draw, what the quick row holds) is the one Rust rule in
/// `lakko_ui_rules`, reached through the FFI; this type only converts.
public enum ReactionFold {

    /// What the op log comes to: per person at most 3, in journal order, a 4th
    /// ignored for good (`foldReactions`). An op filed under a reactor that is
    /// not a 32-byte key (the own sentinel of a torn roster read) is folded
    /// under a placeholder key and mapped back, so this device still shows its
    /// own tap.
    public static func held(ops: [Reaction]) -> [Reaction] {
        guard !ops.isEmpty else { return [] }
        var placeholders: [MemberIdentity: Data] = [:]
        var back: [Data: MemberIdentity] = [:]
        func key(_ reactor: MemberIdentity) -> Data {
            if let data = Data(base64Encoded: reactor), data.count == 32 {
                back[data] = reactor
                return data
            }
            if let held = placeholders[reactor] { return held }
            var data = Data(repeating: 0xFF, count: 31)
            data.append(UInt8(truncatingIfNeeded: placeholders.count))
            placeholders[reactor] = data
            back[data] = reactor
            return data
        }
        let ffiOps = ops.map {
            FfiReactionOp(person: key($0.reactor), emoji: $0.emoji, change: $0.add ? .add : .remove, at: $0.at)
        }
        guard let held = try? foldReactions(ops: ffiOps) else { return [] }
        return held.compactMap { h in
            back[h.person].map { Reaction(reactor: $0, emoji: h.emoji, add: true, at: h.at) }
        }
    }
}

/// What the message menu's reaction row offers for one message (section 5).
public struct ReactionRowModel: Equatable {
    /// The emoji, in order (at most six).
    public let emoji: [String]
    /// The emoji this person holds on the message (drawn with the
    /// accent-soft circle; pressing one takes it off).
    public let own: [String]
    /// The person already holds the most reactions: the others are dimmed,
    /// the line shows.
    public let capReached: Bool
    /// Whether the trailing expand control is shown.
    public let showsExpand: Bool

    public static let empty = ReactionRowModel(emoji: [], own: [], capReached: false, showsExpand: false)
}

public enum ReactionRules {

    /// The most reactions one person gives one message (section 10).
    public static var maxPerPerson: Int { Int(maxReactionsPerMessage()) }

    /// The quick reaction when the person never chose one.
    public static var defaultQuick: String { defaultQuickReaction() }

    /// Section 5 — the row for `message`.
    public static func row(
        message: ChatMessage,
        own: MemberIdentity?,
        quick: String?,
        recent: [String],
        allowed: FfiAllowedReactions
    ) -> ReactionRowModel {
        if case .off = allowed { return .empty }
        let held = message.heldEmoji(of: own)
        let emoji = quickReactionRow(
            quick: quick, recent: recent, allowed: allowed, messageKinds: message.heldKinds)
        return ReactionRowModel(
            emoji: emoji,
            own: held,
            capReached: held.count >= maxPerPerson,
            showsExpand: rowHasExpand(allowed: allowed))
    }

    /// Sections 5, 6, 7 — what pressing `emoji` on `message` does.
    public static func verdict(
        emoji: String,
        message: ChatMessage,
        own: MemberIdentity?,
        allowed: FfiAllowedReactions
    ) -> FfiReactionTapVerdict {
        reactionTapVerdict(
            emoji: emoji, own: message.heldEmoji(of: own), allowed: allowed,
            messageKinds: message.heldKinds)
    }

    /// Section 7 — the chips under `message`.
    public static func chips(
        message: ChatMessage,
        own: MemberIdentity?,
        conversation: FfiChipConversation,
        allowed: FfiAllowedReactions
    ) -> FfiChipRow {
        let empty = FfiChipRow(chips: [], overflowKinds: 0)
        let held = message.heldReactions
        guard !held.isEmpty else { return empty }
        let ffiHeld: [FfiHeldReaction] = held.compactMap { r in
            guard let person = Data(base64Encoded: r.reactor), person.count == 32 else {
                // The own sentinel: filed under this person's own key.
                guard r.reactor == AuthorIdentity.ownReactionSentinel,
                      let mine = own.flatMap({ Data(base64Encoded: $0) }), mine.count == 32 else { return nil }
                return FfiHeldReaction(person: mine, emoji: r.emoji, at: r.at)
            }
            return FfiHeldReaction(person: person, emoji: r.emoji, at: r.at)
        }
        let ownKey = own.flatMap { Data(base64Encoded: $0) } ?? Data(count: 32)
        guard let sources = try? chipSourcesFromHeld(held: ffiHeld, own: ownKey),
              let row = try? reactionChips(sources: sources, conversation: conversation, allowed: allowed)
        else { return empty }
        return row
    }

    /// The people who hold `emoji` on `message`, earliest first.
    public static func reactors(of emoji: String?, message: ChatMessage) -> [Reaction] {
        message.heldReactions.filter { emoji == nil || $0.emoji == emoji }
    }
}

/// Section 6 — the account's quick reaction, held in memory for the views and
/// written through the account's synced state (D3), so every device of the
/// person double-taps the same emoji.
@MainActor
public final class QuickReactionSetting: ObservableObject {
    public static let shared = QuickReactionSetting()

    /// The emoji the person chose, `nil` for the default.
    @Published public private(set) var chosen: String?

    /// Called when the person picks one; the composition root points it at
    /// the synced-state write.
    public var onChoose: ((String) -> Void)?

    public var current: String { chosen ?? ReactionRules.defaultQuick }

    /// The synced state learned a value (from this or another device).
    public func adopt(_ value: String?) {
        if chosen != value { chosen = value }
    }

    public func choose(_ emoji: String) {
        guard isSingleEmoji(text: emoji) else { return }
        chosen = emoji
        onChoose?(emoji)
    }
}

/// Section 12 — the `Reactions` notification switch, per device, on by
/// default. Kept in the App Group defaults so the notification extension can
/// read it too.
public enum ReactionNotificationPreference {
    static let key = ReactionNotificationMirror.key

    public static var defaults: UserDefaults {
        SharedStorage.sharedDefaults() ?? .standard
    }

    public static var isOn: Bool {
        get { defaults.object(forKey: key) as? Bool ?? true }
        set { defaults.set(newValue, forKey: key) }
    }
}

/// Section 5 — the person's most recently used reactions on this account
/// (reactions only, newest first), the second source of the quick row.
public enum RecentReactions {
    static let max = 12

    static func key(_ accountIdB64: String) -> String { "lakko.reaction.recent.\(accountIdB64)" }

    public static func load(_ defaults: UserDefaults = .standard, accountIdB64: String?) -> [String] {
        guard let accountIdB64 else { return [] }
        return defaults.stringArray(forKey: key(accountIdB64)) ?? []
    }

    public static func push(_ emoji: String, _ defaults: UserDefaults = .standard, accountIdB64: String?) {
        guard let accountIdB64 else { return }
        let next = Array(([emoji] + load(defaults, accountIdB64: accountIdB64).filter { $0 != emoji }).prefix(max))
        defaults.set(next, forKey: key(accountIdB64))
    }
}

/// Section 12 — per conversation, this person's own messages that others
/// reacted to and that have not been on screen since. Local to the device.
public final class UnseenReactionsStore {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(_ accountIdB64: String) -> String { "lakko.unseen-reactions.v1.\(accountIdB64)" }

    private func map(_ accountIdB64: String) -> [String: [Int64]] {
        guard let raw = defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: [NSNumber]] else { return [:] }
        return raw.mapValues { $0.map(\.int64Value) }
    }

    private func save(_ accountIdB64: String, _ value: [String: [Int64]]) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(value.mapValues { $0.map { NSNumber(value: $0) } }, forKey: Self.key(accountIdB64))
    }

    /// The unseen message ids, oldest first.
    public func ids(accountIdB64: String, groupIdHex: String) -> [Int64] {
        (map(accountIdB64)[groupIdHex] ?? []).sorted()
    }

    @discardableResult
    public func add(accountIdB64: String, groupIdHex: String, messageId: Int64) -> Bool {
        var all = map(accountIdB64)
        var row = all[groupIdHex] ?? []
        guard !row.contains(messageId) else { return false }
        row.append(messageId)
        all[groupIdHex] = row
        save(accountIdB64, all)
        return true
    }

    @discardableResult
    public func remove(accountIdB64: String, groupIdHex: String, messageId: Int64) -> Bool {
        var all = map(accountIdB64)
        guard var row = all[groupIdHex], let index = row.firstIndex(of: messageId) else { return false }
        row.remove(at: index)
        all[groupIdHex] = row.isEmpty ? nil : row
        save(accountIdB64, all)
        return true
    }

    public func removeGroup(accountIdB64: String, groupIdHex: String) {
        var all = map(accountIdB64)
        guard all.removeValue(forKey: groupIdHex) != nil else { return }
        save(accountIdB64, all)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }
}
