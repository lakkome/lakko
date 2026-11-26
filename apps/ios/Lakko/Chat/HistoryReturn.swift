// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 9 — the history comes back after a key reset, newest
// first.
//
// The requester (the reset device): once it joined a conversation inside its
// own continuity window, it asks there for history with a field-less request
// (`composeHistoryRequestEnvelope`) carrying a fresh ADR-0091 gap key and
// `before` (0: the newest). The answer is read by the ADR-0091 gap look
// (signer rule `gapHistorySignerAccepted`). Each page filed moves `before`
// to the oldest DS id it carried and asks again; an empty page ends it.
//
// The answerer: only for a request from the CURRENT key of an account whose
// reset this device accepted, whose retired key was in the conversation's
// earlier group this device holds; in a one-to-one the other person answers,
// in a group an owner or admin. The page is the earlier group's rows below
// `before`, ``pageSize`` at most, sealed to the request's gap key for the
// successor and posted to `gapHistoryRendezvous(gap key)`.
//
// iOS mirror of `apps/web/src/chat/history-return.ts`.

/// The requester's state for one conversation.
public struct HistoryRequestState: Codable, Equatable, Sendable {
    /// The next request's `before` (0 = the newest).
    public var before: Int64
    /// Pages asked for so far.
    public var pages: Int
    /// When the current request was posted (unix ms); 0 = not yet.
    public var sentAtMs: Int64
    /// How many times the current page was asked for with no answer.
    public var asks: Int
    /// No more pages.
    public var done: Bool

    public init(before: Int64 = 0, pages: Int = 0, sentAtMs: Int64 = 0, asks: Int = 0, done: Bool = false) {
        self.before = before
        self.pages = pages
        self.sentAtMs = sentAtMs
        self.asks = asks
        self.done = done
    }

    public static let first = HistoryRequestState()
}

public enum HistoryReturnRules {
    /// Rows per page (chosen, not measured).
    public static let pageSize = 200
    /// Pages asked for at most per conversation (chosen).
    public static let maxPages = 50
    /// A request with no page after this long is posted again (chosen).
    public static let resendMs: Int64 = 10 * 60 * 1000
    /// A page asked for this many times with no answer is not asked for
    /// again (chosen).
    public static let maxAsks = 6

    /// | from self | reset accepted | in earlier group | earlier held | kind  | own role       | answers |
    /// |-----------|----------------|------------------|--------------|-------|----------------|---------|
    /// | yes       | -              | -                | -            | -     | -              | no      |
    /// | no        | no             | -                | -            | -     | -              | no      |
    /// | no        | yes            | no               | -            | -     | -              | no      |
    /// | no        | yes            | yes              | no           | -     | -              | no      |
    /// | no        | yes            | yes              | yes          | chat  | -              | yes     |
    /// | no        | yes            | yes              | yes          | group | owner or admin | yes     |
    /// | no        | yes            | yes              | yes          | group | member         | no      |
    public static func answerAllowed(
        fromSelf: Bool, requesterResetAccepted: Bool, requesterWasInEarlierGroup: Bool,
        earlierGroupHeld: Bool, isOneToOne: Bool, ownIsOwnerOrAdmin: Bool
    ) -> Bool {
        if fromSelf || !requesterResetAccepted { return false }
        if !requesterWasInEarlierGroup || !earlierGroupHeld { return false }
        return isOneToOne || ownIsOwnerOrAdmin
    }

    /// The state after one more post of the current request.
    public static func asked(_ state: HistoryRequestState, nowMs: Int64) -> HistoryRequestState {
        var next = state
        next.sentAtMs = nowMs
        next.asks += 1
        return next
    }

    /// Whether the current request is to be posted now.
    public static func requestDue(_ state: HistoryRequestState, nowMs: Int64) -> Bool {
        if state.done || state.asks >= maxAsks { return false }
        return state.sentAtMs == 0 || nowMs < state.sentAtMs || nowMs - state.sentAtMs >= resendMs
    }

    /// What a filed page does to the state: the oldest DS id among its rows
    /// is the next `before`; an empty page, one whose oldest id is not below
    /// the current `before`, or the page cap, ends it.
    public static func nextStep(_ state: HistoryRequestState, filedMessageIds: [Int64]) -> HistoryRequestState {
        if state.done { return state }
        let oldest = filedMessageIds.filter { $0 > 0 }.min()
        let pages = state.pages + 1
        if oldest == nil || (state.before > 0 && oldest! >= state.before) || pages >= maxPages {
            var done = state
            done.pages = pages
            done.done = true
            return done
        }
        return HistoryRequestState(before: oldest!, pages: pages, sentAtMs: 0, asks: 0, done: false)
    }

    /// The rows a history page may carry from the earlier one-to-one: every
    /// row but the ones ``ResetRepair/messagesToResend(_:resetSignedAtMs:afterMessageId:)``
    /// sends again into the successor (this account's own, after the reset
    /// was signed), which the reset person receives as re-sends. A group
    /// re-sends nothing, and with no reset time nothing is left out. Pure.
    public static func withoutResent(
        _ rows: [PersistedMessage], isOneToOne: Bool, resetSignedAtMs: Int64?
    ) -> [PersistedMessage] {
        guard isOneToOne, let resetSignedAtMs else { return rows }
        let resent = Set(ResetRepair.messagesToResend(
            rows.filter { $0.messageId > 0 }.map {
                ResetRepair.OldSegmentMessage(
                    messageId: $0.messageId,
                    own: $0.direction == "outgoing",
                    sentAtMs: ChatRepository.parseDsTimestampMs($0.createdAt),
                    resendable: !$0.deleted && (!$0.text.isEmpty || !$0.attachments.isEmpty))
            },
            resetSignedAtMs: resetSignedAtMs))
        guard !resent.isEmpty else { return rows }
        return rows.filter { !resent.contains($0.messageId) }
    }

    /// The rows of one history page: DS id below `before` (0: no bound),
    /// the newest `pageSize` of them, oldest first. Ids not above 0 are left
    /// out.
    public static func pageRows<T>(_ rows: [T], messageId: (T) -> Int64?, before: Int64, pageSize: Int) -> [T] {
        guard pageSize > 0 else { return [] }
        var withIds: [(row: T, id: Int64)] = []
        for row in rows {
            guard let id = messageId(row), id > 0 else { continue }
            if before > 0 && id >= before { continue }
            withIds.append((row, id))
        }
        withIds.sort { $0.id < $1.id }
        return withIds.suffix(pageSize).map(\.row)
    }
}

/// The requester's states, per account: group id (base64) -> state.
public struct HistoryReturnStore {
    static let prefix = "lakko.history-return.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> [String: HistoryRequestState] {
        guard let data = defaults.data(forKey: Self.prefix + accountIdB64) else { return [:] }
        return (try? JSONDecoder().decode([String: HistoryRequestState].self, from: data)) ?? [:]
    }

    public func write(_ state: HistoryRequestState, groupIdB64: String, accountIdB64: String) {
        var all = read(accountIdB64: accountIdB64)
        all[groupIdB64] = state
        guard let data = try? JSONEncoder().encode(all), let gate = AccountStoreGate.enter(accountIdB64) else {
            return
        }
        defer { gate.leave() }
        defaults.set(data, forKey: Self.prefix + accountIdB64)
    }

    /// Start asking for a returned conversation's history (idempotent).
    public func noteReturned(groupIdB64: String, accountIdB64: String) {
        guard read(accountIdB64: accountIdB64)[groupIdB64] == nil else { return }
        write(.first, groupIdB64: groupIdB64, accountIdB64: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}

/// ADR-0101 section 9 (live walk 2026-11-26, D4) — the founder of an owned
/// group's successor serves the reset person's history before it hands them
/// the ownership. Per account: successor group id (base64) -> when the
/// founder first held the hand-over back, and whether the history was
/// served (an empty page, the requester's last, was posted).
public struct HistoryServedStore {
    static let prefix = "lakko.history-served.v1."
    /// How long the hand-over waits for the history at most (chosen): the
    /// reset person may never ask (an older client, or not online).
    public static let waitMs: Int64 = 3 * 24 * 60 * 60 * 1000

    public struct Entry: Codable, Equatable, Sendable {
        public var firstHeldMs: Int64
        public var served: Bool
    }

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> [String: Entry] {
        guard let data = defaults.data(forKey: Self.prefix + accountIdB64) else { return [:] }
        return (try? JSONDecoder().decode([String: Entry].self, from: data)) ?? [:]
    }

    private func write(_ all: [String: Entry], accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(all), let gate = AccountStoreGate.enter(accountIdB64) else {
            return
        }
        defer { gate.leave() }
        defaults.set(data, forKey: Self.prefix + accountIdB64)
    }

    /// | served | held back since       | due |
    /// |--------|-----------------------|-----|
    /// | yes    | -                     | yes |
    /// | no     | never (now recorded)  | no  |
    /// | no     | less than ``waitMs``  | no  |
    /// | no     | ``waitMs`` or more    | yes |
    ///
    /// A clock that went back reads as just held back. Pure.
    public static func transferDue(_ entry: Entry?, nowMs: Int64) -> Bool {
        guard let entry else { return false }
        if entry.served { return true }
        return nowMs >= entry.firstHeldMs && nowMs - entry.firstHeldMs >= waitMs
    }

    /// Whether the hand-over of `successorGroupIdB64` is due; the first ask
    /// starts the wait.
    public func transferDue(successorGroupIdB64: String, accountIdB64: String, nowMs: Int64) -> Bool {
        var all = read(accountIdB64: accountIdB64)
        guard let entry = all[successorGroupIdB64] else {
            all[successorGroupIdB64] = Entry(firstHeldMs: nowMs, served: false)
            write(all, accountIdB64: accountIdB64)
            return false
        }
        return Self.transferDue(entry, nowMs: nowMs)
    }

    /// The history of `successorGroupIdB64` was served.
    public func markServed(successorGroupIdB64: String, accountIdB64: String, nowMs: Int64) {
        var all = read(accountIdB64: accountIdB64)
        var entry = all[successorGroupIdB64] ?? Entry(firstHeldMs: nowMs, served: false)
        guard !entry.served else { return }
        entry.served = true
        all[successorGroupIdB64] = entry
        write(all, accountIdB64: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}
