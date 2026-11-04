// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The persisted half of the account stranger inbox: the poll cursor and the
/// requests still awaiting Accept / Decline.
///
/// Before this store both lived only in ``ContactsState`` (seen live on
/// 2026-09-16): every relaunch re-polled the inbox from cursor `0`, so every
/// past request was fetched again — an accepted request came back under
/// "Requests" beside its contact row, and a declined one came back outright.
/// Persisting the cursor is what stops the re-fetch; persisting `pending` is
/// what keeps a not-yet-answered request visible across the relaunch that the
/// cursor now skips past.
///
/// One JSON row per account (`me.lakko.contacts.inbox.v1.<account>`), the
/// same per-account `UserDefaults` shape as the ADR-0050 device-mailbox cursor
/// next to it. Cursor and pending are written together so the two can never
/// disagree on disk. Non-secret relationship metadata only (peer pubkeys and
/// account ids the contacts UI already displays — the same class
/// ``AcceptedContactsStore`` holds); never sent to a server.
public struct PersistedContactsInbox: Codable, Equatable {
    /// DS journal cursor of the account stranger inbox.
    public var cursor: Int64
    /// Verified incoming requests awaiting Accept / Decline.
    public var pending: [IncomingContactRequest]

    public init(cursor: Int64 = 0, pending: [IncomingContactRequest] = []) {
        self.cursor = cursor
        self.pending = pending
    }

    public static let empty = PersistedContactsInbox()
}

/// `UserDefaults`-backed persistence for ``PersistedContactsInbox``, keyed by
/// account. Injectable defaults so tests run against an isolated suite.
public final class ContactsInboxStore {
    private static let prefix = "me.lakko.contacts.inbox.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func storageKey(accountIdB64: String) -> String {
        prefix + accountIdB64
    }

    /// Load one account's inbox. Absent or undecodable rows read as
    /// ``PersistedContactsInbox/empty`` — cursor `0`, no pending — which is the
    /// pre-store behaviour (re-poll everything), never a crash.
    public func load(accountIdB64: String) -> PersistedContactsInbox {
        guard let data = defaults.data(forKey: Self.storageKey(accountIdB64: accountIdB64)) else {
            return .empty
        }
        return (try? JSONDecoder().decode(PersistedContactsInbox.self, from: data)) ?? .empty
    }

    /// Persist one account's inbox. Best-effort: an encode failure (not
    /// reachable for the all-`Codable` model) leaves the prior row in place.
    public func save(_ inbox: PersistedContactsInbox, accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(inbox) else { return }
        defaults.set(data, forKey: Self.storageKey(accountIdB64: accountIdB64))
    }

    /// Drop one account's row — `forgetDevice` hygiene ("leave nothing
    /// per-account behind").
    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.storageKey(accountIdB64: accountIdB64))
    }
}

/// The pure rules ``ContactsRepository/refreshInbox(waitSecs:)`` applies to a
/// fetched batch. Kept free of the repository so they can be unit-tested
/// without an `AuthRepository`.
public enum ContactsInboxMerge {
    /// Why a fetched request was not added to `pending`.
    public enum DropReason: Equatable {
        /// A request with this `messageId` is already pending (or appeared
        /// earlier in the same batch).
        case alreadyPending
        /// The sender is already an accepted contact (by signature key).
        case alreadyAccepted
    }

    public struct Result: Equatable {
        /// The requests to append to `pending`, in fetch order.
        public let kept: [IncomingContactRequest]
        /// The requests dropped, with the reason, for the debug log.
        public let dropped: [(request: IncomingContactRequest, reason: DropReason)]
        /// ADR-0074 section 3.5a rule 5 — the `messageId`s of PENDING requests
        /// a fetched one replaces: the same person asked again from the key
        /// that superseded theirs by proof. One request per person.
        public let replaced: [Int64]

        public init(
            kept: [IncomingContactRequest],
            dropped: [(request: IncomingContactRequest, reason: DropReason)],
            replaced: [Int64] = []
        ) {
            self.kept = kept
            self.dropped = dropped
            self.replaced = replaced
        }

        public static func == (lhs: Result, rhs: Result) -> Bool {
            lhs.kept == rhs.kept
                && lhs.dropped.map(\.request) == rhs.dropped.map(\.request)
                && lhs.dropped.map(\.reason) == rhs.dropped.map(\.reason)
                && lhs.replaced == rhs.replaced
        }

        /// `pending` after this batch: the replaced rows gone, the kept ones
        /// appended.
        public func applied(to pending: [IncomingContactRequest]) -> [IncomingContactRequest] {
            pending.filter { !replaced.contains($0.messageId) } + kept
        }
    }

    /// Dedupe a fetched batch against the current state. A request whose
    /// `messageId` is already pending, or whose `fromPubkey` already names an
    /// accepted contact, is dropped. The second rule is what keeps a request
    /// that was accepted on this device from reappearing under "Requests"
    /// when a poll re-reads it (a cursor rewind, or a request the sender
    /// re-sent after being accepted).
    ///
    /// ADR-0074 section 3.5a rule 5 — `currentKey` resolves an account key
    /// across PROVEN rotations (the key itself otherwise). A fetched request
    /// from the key that superseded a pending request's key replaces that row
    /// (the requester rotated and asked again: its old request's reply grant
    /// names a mailbox it left); one from a key a pending or kept request's
    /// key superseded is dropped; and an accepted contact is recognised under
    /// any of their keys. An unproven key change is two people here. A newer
    /// request from the same key replaces the row too: the person asked again
    /// (search offers that once they stop waiting), and they are one row.
    public static func merge(
        fetched: [IncomingContactRequest],
        pending: [IncomingContactRequest],
        accepted: [AcceptedContact],
        currentKey: (Data) -> Data = { $0 }
    ) -> Result {
        var pendingIds = Set(pending.map(\.messageId))
        let acceptedKeys = Set(accepted.map { currentKey($0.request.fromPubkey) })
        // What is on the list as the batch is walked: the pending rows not
        // replaced so far, then the kept ones.
        var live = pending
        var kept: [IncomingContactRequest] = []
        var replaced: [Int64] = []
        var dropped: [(request: IncomingContactRequest, reason: DropReason)] = []
        for request in fetched {
            let person = currentKey(request.fromPubkey)
            if pendingIds.contains(request.messageId) {
                dropped.append((request, .alreadyPending))
            } else if acceptedKeys.contains(person) {
                dropped.append((request, .alreadyAccepted))
            } else if person != request.fromPubkey,
                      (live + kept).contains(where: { currentKey($0.fromPubkey) == person }) {
                // Sent from a key the person has since left: the row already
                // there (from this key or a later one) stands.
                dropped.append((request, .alreadyPending))
            } else {
                // From the person's current key: every earlier row of theirs
                // goes, under this key (they asked again) or an EARLIER one.
                let stale: (IncomingContactRequest) -> Bool = { currentKey($0.fromPubkey) == person }
                replaced += live.filter(stale).map(\.messageId)
                live.removeAll(where: stale)
                kept.removeAll(where: stale)
                pendingIds.insert(request.messageId)
                kept.append(request)
            }
        }
        return Result(kept: kept, dropped: dropped, replaced: replaced)
    }

    /// A fetched request and a pending one that name the SAME account id
    /// under two different keys: the pairs worth asking the directory about
    /// (did the pending key hand over to the fetched one by proof?). The
    /// account id of a request is its sender's own claim, so a pair is only a
    /// question, never an answer.
    public struct RotationCandidate: Equatable {
        public let accountId: Data
        public let oldKey: Data
        public let newKey: Data
    }

    public static func rotationCandidates(
        fetched: [IncomingContactRequest], pending: [IncomingContactRequest]
    ) -> [RotationCandidate] {
        var out: [RotationCandidate] = []
        for request in fetched {
            guard let accountId = request.fromAccountId, accountId.count == 16 else { continue }
            for row in pending where row.fromAccountId == accountId && row.fromPubkey != request.fromPubkey {
                let candidate = RotationCandidate(
                    accountId: accountId, oldKey: row.fromPubkey, newKey: request.fromPubkey)
                if !out.contains(candidate) { out.append(candidate) }
            }
        }
        return out
    }

    /// The cursor to persist after a batch. Never moves backwards: a poll
    /// answered from a stale cursor (a batch that started before a later one
    /// landed) must not rewind the inbox to re-fetch rows already seen.
    public static func advancedCursor(current: Int64, polled: Int64) -> Int64 {
        max(current, polled)
    }
}
