// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// MARK: - The two stores

/// ADR-0052 decision 3 — what this device holds for one SHARED pin of one
/// message: whether it is pinned for everyone right now, and the stamp of the
/// action that said so. An unpin is stored, never deleted: the stamp is what
/// keeps a late-arriving pin of the same message (a growth re-send, a slow
/// journal) from bringing back a pin somebody took down.
public struct StoredSharedPin: Equatable, Codable {
    /// `true` = pinned for everyone; `false` = unpinned (the row stays for
    /// its stamp).
    public let pinned: Bool
    /// Setter-clock UNIX milliseconds of the action, re-emitted VERBATIM on a
    /// growth re-send (the ADR-0044 lesson: a re-send is not an update).
    public let updatedAtMs: UInt64

    public init(pinned: Bool, updatedAtMs: UInt64) {
        self.pinned = pinned
        self.updatedAtMs = updatedAtMs
    }
}

/// ADR-0052 decision 3 / UX spec section 4.1 "Data each client keeps" — the
/// per-conversation shared pin map `messageId -> { pinned, updatedAtMs }`,
/// in `UserDefaults`, the ``GroupNameStore`` shape: a small non-secret map
/// (a pin names a message the members already share), namespaced by account,
/// wiped with the conversation. App-private — the notification extension
/// only needs to recognise the envelope, never to read a pin.
public final class SharedPinStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Every stored row for the conversation, pinned and unpinned alike.
    public func load(accountIdB64: String, groupIdHex: String) -> [Int64: StoredSharedPin] {
        var out: [Int64: StoredSharedPin] = [:]
        for (key, raw) in groupMap(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            guard let messageId = Int64(key),
                  let pinned = raw["pinned"] as? Bool,
                  let stamp = raw["updatedAtMs"] as? NSNumber
            else { continue }
            out[messageId] = StoredSharedPin(pinned: pinned, updatedAtMs: stamp.uint64Value)
        }
        return out
    }

    /// Persist `value` for one message, replacing whatever was stored. The
    /// caller decides whether the write is allowed (``PinGate``); the store
    /// itself is a plain map.
    public func record(accountIdB64: String, groupIdHex: String, messageId: Int64, value: StoredSharedPin) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var groups = accountMap(accountIdB64: accountIdB64)
        var rows = groups[groupIdHex] ?? [:]
        rows[String(messageId)] = [
            "pinned": value.pinned,
            "updatedAtMs": NSNumber(value: value.updatedAtMs),
        ]
        groups[groupIdHex] = rows
        defaults.set(groups, forKey: Self.key(accountIdB64))
    }

    /// Drop one message's row outright — the message was deleted, and a pin
    /// of a message that no longer exists has nothing left to order (UX spec
    /// section 4.1: "its shared pin record is dropped with the message").
    public func remove(accountIdB64: String, groupIdHex: String, messageId: Int64) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var groups = accountMap(accountIdB64: accountIdB64)
        var rows = groups[groupIdHex] ?? [:]
        rows[String(messageId)] = nil
        groups[groupIdHex] = rows.isEmpty ? nil : rows
        defaults.set(groups, forKey: Self.key(accountIdB64))
    }

    /// Forget a whole conversation's pins (the chat was deleted locally).
    public func removeGroup(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var groups = accountMap(accountIdB64: accountIdB64)
        groups[groupIdHex] = nil
        defaults.set(groups, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func groupMap(accountIdB64: String, groupIdHex: String) -> [String: [String: Any]] {
        accountMap(accountIdB64: accountIdB64)[groupIdHex] ?? [:]
    }

    private func accountMap(accountIdB64: String) -> [String: [String: [String: Any]]] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: [String: [String: Any]]] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "shared_pins.\(accountIdB64)"
    }
}

/// ADR-0052 decision 3 — "Pin for me": a flag on THIS device, keyed on the
/// conversation and the message, `messageId -> pinnedAtMs`. Nothing is sent
/// and nobody else sees it. The stamp is local and exists only so the bar can
/// show the newest personal pin first. Namespaced by account like every
/// sibling store, so two accounts on one device never share a bookmark.
public final class PersonalPinStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load(accountIdB64: String, groupIdHex: String) -> [Int64: UInt64] {
        var out: [Int64: UInt64] = [:]
        for (key, raw) in accountMap(accountIdB64: accountIdB64)[groupIdHex] ?? [:] {
            guard let messageId = Int64(key), let stamp = raw as? NSNumber else { continue }
            out[messageId] = stamp.uint64Value
        }
        return out
    }

    public func set(accountIdB64: String, groupIdHex: String, messageId: Int64, pinnedAtMs: UInt64) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var groups = accountMap(accountIdB64: accountIdB64)
        var rows = groups[groupIdHex] ?? [:]
        rows[String(messageId)] = NSNumber(value: pinnedAtMs)
        groups[groupIdHex] = rows
        defaults.set(groups, forKey: Self.key(accountIdB64))
    }

    public func remove(accountIdB64: String, groupIdHex: String, messageId: Int64) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var groups = accountMap(accountIdB64: accountIdB64)
        var rows = groups[groupIdHex] ?? [:]
        rows[String(messageId)] = nil
        groups[groupIdHex] = rows.isEmpty ? nil : rows
        defaults.set(groups, forKey: Self.key(accountIdB64))
    }

    public func removeGroup(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var groups = accountMap(accountIdB64: accountIdB64)
        groups[groupIdHex] = nil
        defaults.set(groups, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func accountMap(accountIdB64: String) -> [String: [String: Any]] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: [String: Any]] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "personal_pins.\(accountIdB64)"
    }
}

// MARK: - The receive verdict

/// ADR-0052 decision 3 / UX spec section 4.1 "What a receiver does" — what the
/// receive path does with one incoming pin envelope. The shape copies
/// ``GroupNameVerdict``: the identical case is decided before authority (a
/// re-send that changes nothing needs no standing), and the reject arms are
/// distinguished because the log has to say which gate refused.
public enum PinVerdict: Equatable {
    /// Byte-for-byte what is stored for that message (action and stamp).
    /// Nothing to write.
    case acceptIdentical
    /// Authorised and strictly newer than what is stored for that message:
    /// store it.
    case accept
    /// A plain member of a role-table group. Dropped (invariant #4).
    case rejectNotAuthorized
    /// Authorised, but not newer than the stored stamp FOR THAT MESSAGE — a
    /// growth re-send, or a pin that arrived after a later unpin. Dropped: a
    /// late pin can never undo a later unpin.
    case rejectStale
    /// The roles or the roster could not be read, or the sender leaf is not
    /// in the roster. Cannot authorise: dropped, fail-closed.
    case rejectUnverifiable
}

/// The pure receive-side decision for a shared pin, with the authority table
/// the group name uses (``PushGroupRouting/SecretAuthority``, read from the
/// LIVE group) plus the one row the pin adds — a one-to-one:
///
/// | Conversation | Sender | Verdict |
/// |---|---|---|
/// | one-to-one | either participant, resolved | accept if newer |
/// | one-to-one | unresolvable | `rejectUnverifiable` |
/// | role table | owner or admin | accept if newer |
/// | role table | member | `rejectNotAuthorized` |
/// | role table | unresolvable / table unreadable | `rejectUnverifiable` |
/// | no role table | any member | accept if newer |
///
/// "One-to-one" is the conversation's ADR-0062 kind, read from its
/// GroupContext — NOT a people count: a two-person GROUP follows the role
/// table (only the owner or an admin pins for everyone). It comes before the
/// role table because a one-to-one does carry a table with one owner — and
/// ADR-0052 says either participant may pin for both and either may clear it.
///
/// `isNewer` is the core's `pinIsNewer` (strictly greater, so a re-send with
/// the same stamp is never "newer"), injectable so the matrix is testable
/// without the FFI.
public enum PinGate {

    public static func classify(
        incomingPinned: Bool,
        incomingUpdatedAtMs: UInt64,
        stored: StoredSharedPin?,
        senderKey: Data?,
        authority: PushGroupRouting.SecretAuthority,
        oneToOne: Bool,
        isNewer: (_ incoming: UInt64, _ stored: UInt64) -> Bool = {
            pinIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1)
        }
    ) -> PinVerdict {
        // (1) Idempotent first — a re-observed identical action changes
        // nothing, so it needs no authority and no ordering.
        if let stored, stored.pinned == incomingPinned, stored.updatedAtMs == incomingUpdatedAtMs {
            return .acceptIdentical
        }

        // (2) Authority — the receiver's check, never the sender's claim.
        let senderResolved = senderKey.map { !$0.isEmpty } ?? false
        if oneToOne {
            // A one-to-one: either participant, as long as the sender is a
            // participant this roster can name.
            guard senderResolved else { return .rejectUnverifiable }
        } else {
            switch authority {
            case .noRoleTable:
                break
            case .unreadable:
                return .rejectUnverifiable
            case .roleTable(let ownerKey, let admins):
                guard let senderKey, senderResolved else { return .rejectUnverifiable }
                if senderKey == ownerKey || admins.contains(senderKey) {
                    break
                }
                return .rejectNotAuthorized
            }
        }

        // (3) Nothing stored for that message — the first action lands.
        guard let stored else { return .accept }

        // (4) The order decides which authorised write survives, per message.
        return isNewer(incomingUpdatedAtMs, stored.updatedAtMs) ? .accept : .rejectStale
    }

    /// The LOCAL half of the same rule, for the sheet: may this device set or
    /// clear a shared pin here? Either participant of a one-to-one; the owner
    /// or an admin of a role-table group; anyone in a group with no table
    /// (`ownRole == nil`); nobody once their membership ended, and nobody
    /// while the account is suspended (ADR-0099 step 3: a shared pin or
    /// unpin sends an envelope; `Pin for me` stays).
    public static func maySetShared(
        oneToOne: Bool, ownRole: Role?, membershipEnded: Bool, suspended: Bool = false
    ) -> Bool {
        guard !membershipEnded, !suspended else { return false }
        if oneToOne { return true }
        return ownRole != .member
    }
}

// MARK: - The bar

/// Which scope a pin in the bar came from.
public enum PinScope: Equatable {
    /// Pinned for everyone; the whole room sees it.
    case shared
    /// Pinned for me only; the bar says "For you" in place of the author.
    case personal
}

/// One entry of the pinned bar: a message this device HOLDS that is pinned in
/// at least one scope. A message pinned in both scopes appears once, as
/// shared, because the shared pin outranks the personal one (ADR-0052).
public struct PinnedEntry: Equatable, Identifiable {
    public let messageId: Int64
    public let scope: PinScope
    /// The stamp that orders it inside its scope (newest first).
    public let stampMs: UInt64

    public var id: Int64 { messageId }

    public init(messageId: Int64, scope: PinScope, stampMs: UInt64) {
        self.messageId = messageId
        self.scope = scope
        self.stampMs = stampMs
    }
}

/// UX spec section 4 / 4.1 "The bar" — the pure rules of the pinned bar: what
/// it lists and in what order, how a tap advances it, and what a deletion
/// takes with it. The bar is DERIVED from the two stores and the held
/// messages, never stored.
public enum PinnedBar {

    /// The bar's entries, in display order: shared pins first, then personal
    /// ones, newest first inside each scope (by stamp, then by message id so
    /// two pins with one stamp still have one order).
    ///
    /// A pin whose message this device does not hold — not downloaded, purged
    /// — is skipped, not shown: the pin itself is kept in its store until the
    /// message arrives. A deleted message never appears, shared or personal.
    public static func entries(
        shared: [Int64: StoredSharedPin],
        personal: [Int64: UInt64],
        held: [ChatMessage]
    ) -> [PinnedEntry] {
        var holdable: Set<Int64> = []
        // ADR-0073 R.5 — a row of an older segment is held under its history
        // id and stays in the bar (it can be opened and unpinned, never
        // pinned anew).
        for message in held where isPinnable(message) || isHeldHistoryRow(message) {
            holdable.insert(message.messageId)
        }
        let sharedEntries = shared
            .filter { $0.value.pinned && holdable.contains($0.key) }
            .map { PinnedEntry(messageId: $0.key, scope: .shared, stampMs: $0.value.updatedAtMs) }
            .sorted(by: newestFirst)
        let sharedIds = Set(sharedEntries.map(\.messageId))
        let personalEntries = personal
            .filter { holdable.contains($0.key) && !sharedIds.contains($0.key) }
            .map { PinnedEntry(messageId: $0.key, scope: .personal, stampMs: $0.value) }
            .sorted(by: newestFirst)
        return sharedEntries + personalEntries
    }

    /// A message the bar may show: landed, not a system row, not a tombstone.
    /// A row of an older segment of the conversation that a pin can still
    /// point at.
    static func isHeldHistoryRow(_ message: ChatMessage) -> Bool {
        LineageRules.isHistoryRowId(message.messageId) && message.direction != .system && !message.deleted
    }

    static func isPinnable(_ message: ChatMessage) -> Bool {
        message.messageId > 0 && message.direction != .system && !message.deleted && message.ack == .sent
    }

    private static func newestFirst(_ a: PinnedEntry, _ b: PinnedEntry) -> Bool {
        if a.stampMs != b.stampMs { return a.stampMs > b.stampMs }
        return a.messageId > b.messageId
    }

    /// Keep an index meaningful when the list changed under it: past the end
    /// wraps to the newest, and an empty list has no index.
    public static func clamped(_ index: Int, count: Int) -> Int {
        guard count > 0 else { return 0 }
        guard index >= 0, index < count else { return 0 }
        return index
    }

    /// The index the bar shows after a tap: the next OLDER pin, wrapping past
    /// the oldest to the newest (section 4: "past the oldest it wraps").
    public static func advanced(from index: Int, count: Int) -> Int {
        guard count > 0 else { return 0 }
        return (clamped(index, count: count) + 1) % count
    }

    /// The shared records a deletion takes with it: every stored row whose
    /// message is held here as a tombstone (section 4.1: "its shared pin
    /// record is dropped with the message"). A row for a message this device
    /// does not hold at all is NOT on this list — absent is not deleted.
    public static func recordsToDrop(shared: [Int64: StoredSharedPin], held: [ChatMessage]) -> [Int64] {
        let deleted = Set(held.filter { $0.deleted }.map(\.messageId))
        return shared.keys.filter { deleted.contains($0) }.sorted()
    }

    /// VoiceOver's reading of the stacked indicator.
    public static func positionLabel(index: Int, count: Int) -> String {
        count > 1
            ? Localization.text(
                "chat.pinned_bar.label_of",
                texts: ["index": (index + 1).formatted(.number.locale(Localization.currentLocale)), "count": count.formatted(.number.locale(Localization.currentLocale))])
            : Localization.text("chat.pinned_bar.label")
    }
}

// MARK: - The sheet

/// What tapping `Pin` / `Unpin` may do (UX spec section 4.1 "The sheet").
public enum PinChoice: Equatable {
    case pinForMe
    case pinForEveryone
    case unpinForMe
    case unpinForEveryone
}

/// One row of the pin sheet: the choice, its title, and the one line under it
/// that says what it does.
public struct PinSheetOption: Equatable, Identifiable {
    public let choice: PinChoice
    public let title: String
    public let subtitle: String

    public var id: String { title }
}

/// Which word the action card shows for this message.
public enum PinMenuState: Equatable {
    /// The row reads `Pin`.
    case pin
    /// The row reads `Unpin`.
    case unpin
    /// No row at all (a message that cannot be pinned).
    case unavailable
}

/// UX spec section 4.1 — the row's word and the sheet's rows, from the
/// message's pin state in each scope and this device's standing.
///
/// The one non-obvious case is written into the spec and pinned by a test: a
/// plain member holding a message an admin pinned sees `Unpin for me` only
/// when they ALSO pinned it for themselves; otherwise the row is `Pin` and the
/// sheet offers `Pin for me` alone — they cannot clear the room's pin, and the
/// missing row is the explanation.
public enum PinSheet {

    /// `Pin` or `Unpin` for the card.
    public static func menuState(sharedPinned: Bool, personalPinned: Bool, maySetShared: Bool) -> PinMenuState {
        if personalPinned || (sharedPinned && maySetShared) { return .unpin }
        return .pin
    }

    /// The sheet's rows, in order. One row means the action happens at once
    /// (section 4.1: "in one scope it acts at once") — the caller checks
    /// ``actsAtOnce(_:)``.
    public static func options(
        sharedPinned: Bool,
        personalPinned: Bool,
        maySetShared: Bool,
        oneToOne: Bool
    ) -> [PinSheetOption] {
        let onlyHere = Localization.text("chat.pin.detail_personal")
        // In a one-to-one the shared scope is the two people, not "every
        // member" (UX spec section 4.1).
        let everyoneSees = Localization.text(oneToOne ? "chat.pin.detail_shared_both" : "chat.pin.detail_shared")
        switch menuState(sharedPinned: sharedPinned, personalPinned: personalPinned, maySetShared: maySetShared) {
        case .pin, .unavailable:
            var rows = [PinSheetOption(choice: .pinForMe, title: Localization.text("chat.pin.for_me"), subtitle: onlyHere)]
            if maySetShared, !sharedPinned {
                rows.append(PinSheetOption(
                    choice: .pinForEveryone,
                    title: Localization.text(oneToOne ? "chat.pin.for_both" : "chat.pin.for_everyone"),
                    subtitle: everyoneSees))
            }
            return rows
        case .unpin:
            var rows: [PinSheetOption] = []
            if personalPinned {
                rows.append(PinSheetOption(choice: .unpinForMe, title: Localization.text("chat.unpin.for_me"), subtitle: onlyHere))
            }
            if sharedPinned, maySetShared {
                rows.append(PinSheetOption(
                    choice: .unpinForEveryone,
                    title: Localization.text(oneToOne ? "chat.unpin.for_both" : "chat.unpin.for_everyone"),
                    subtitle: everyoneSees))
            }
            return rows
        }
    }

    /// An unpin with exactly one way to do it acts without a sheet.
    public static func actsAtOnce(_ options: [PinSheetOption]) -> PinChoice? {
        guard options.count == 1, let only = options.first else { return nil }
        switch only.choice {
        case .unpinForMe, .unpinForEveryone: return only.choice
        case .pinForMe, .pinForEveryone: return nil
        }
    }
}
