// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 R.3 - the contact requests kept WAITING, unshown, for an identity
// verdict (`KnownContactRequest.Verdict.wait`).
//
// A request from an account that is already an established contact is never
// a request row. When the identity check of that account could not conclude
// (the directory, the attest key or the key log could not be reached), the
// request waits for the next check; the mailbox cursor has moved past its
// row by then, so it is kept here and judged again at the next poll and when
// a reset is accepted. iOS twin of the waiting rows of the web
// `contacts/inbox-store.ts` (which keeps the raw rows and decodes them
// again; here the verified, opened request is what is kept).
//
// `UserDefaults`, per account, in the inbox's own protection class: the same
// decoded requests the pending list already persists. Bounded; removed with
// the account's data.
public final class KnownRequestWaitingStore {

    static let prefix = "me.lakko.contacts.inbox.waiting.v1."

    /// At most this many requests wait; the oldest goes first.
    public static let maxWaiting = 32

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func storageKey(accountIdB64: String) -> String { prefix + accountIdB64 }

    /// The waiting requests, oldest first. Unreadable data reads as none.
    public func load(accountIdB64: String) -> [IncomingContactRequest] {
        guard let data = defaults.data(forKey: Self.storageKey(accountIdB64: accountIdB64)) else {
            return []
        }
        return (try? JSONDecoder().decode([IncomingContactRequest].self, from: data)) ?? []
    }

    /// Keep `request` waiting. A request already waiting (by DS message id)
    /// is kept once.
    public func hold(_ request: IncomingContactRequest, accountIdB64: String) {
        var waiting = load(accountIdB64: accountIdB64)
        guard !waiting.contains(where: { $0.messageId == request.messageId }) else { return }
        waiting.append(request)
        if waiting.count > Self.maxWaiting { waiting.removeFirst(waiting.count - Self.maxWaiting) }
        save(waiting, accountIdB64: accountIdB64)
    }

    /// A request was answered, dropped or listed: it waits no longer.
    public func drop(messageId: Int64, accountIdB64: String) {
        let waiting = load(accountIdB64: accountIdB64)
        let kept = waiting.filter { $0.messageId != messageId }
        guard kept.count != waiting.count else { return }
        save(kept, accountIdB64: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.storageKey(accountIdB64: accountIdB64))
    }

    private func save(_ waiting: [IncomingContactRequest], accountIdB64: String) {
        let key = Self.storageKey(accountIdB64: accountIdB64)
        if waiting.isEmpty {
            defaults.removeObject(forKey: key)
        } else if let data = try? JSONEncoder().encode(waiting) {
            defaults.set(data, forKey: key)
        }
    }
}
