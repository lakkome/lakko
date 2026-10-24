// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// On-device persistence for the user's accepted contacts, one list per
/// ACCOUNT.
///
/// Closes (for the accepted list) the long-standing "No persistence yet"
/// gap noted on ``ContactsRepository``: the accepted contacts (each carrying
/// `request.fromAccountId`) survive a relaunch. (A group-call tile is no
/// longer named through the account id: since ADR-0078 section 2 the SFU
/// knows no account and each peer announces itself, sealed and signed.)
///
/// ## Why per account (2026-09-30)
///
/// The list used to live under ONE device-wide key, so a second account
/// signed in on the same phone listed, messaged and searched the first
/// account's contacts as its own (found live on web, the same shape here).
/// It is now one row per account (`lakko.contacts.accepted.v2.<account>`),
/// the same per-account `UserDefaults` shape as ``ContactsInboxStore``. The
/// old global row is no longer read; ``clear(accountIdB64:)`` also removes it.
///
/// ## The bound account
///
/// Most callers read "the contacts" without knowing the account — a call
/// tile resolving a name, a roster lookup. They read through ``load()`` /
/// ``save(_:)``, which act on the account ``bind(accountIdB64:)`` last named:
/// ``AuthRepository`` binds it whenever the live session changes and unbinds
/// it on logout and forget-device. With nothing bound, ``load()`` is empty
/// and ``save(_:)`` writes nothing — it never falls back to a shared row. A
/// caller that already knows the account (backup compose and restore, the
/// pairing state bundle, ``ContactsRepository``) passes it explicitly.
///
/// The accepted list is non-secret relationship metadata (peer pubkeys +
/// account_ids the user already chose to trust, and which the contacts UI
/// already displays) — the same data class the existing `PeerPinStore`
/// already persists. It is JSON-encoded into `UserDefaults`; it is never
/// sent to any server (invariant #1/#2 unaffected).
///
/// Mirror of the web `apps/web/src/contacts/local-store.ts` accepted-list
/// store.
enum AcceptedContactsStore {
    private static let prefix = "lakko.contacts.accepted.v2."
    /// The device-wide row this store used before it was per account. Never
    /// read; removed by ``clear(accountIdB64:)``.
    static let legacyGlobalKey = "lakko.contacts.accepted.v1"

    private static let lock = NSLock()
    private static var _bound: String?
    private static var _defaults: UserDefaults = .standard

    /// The backing store. Swapped for an isolated suite by tests.
    static var defaults: UserDefaults {
        get { lock.withLock { _defaults } }
        set { lock.withLock { _defaults = newValue } }
    }

    /// The account ``load()`` / ``save(_:)`` act on, `nil` while logged out.
    static var boundAccountIdB64: String? {
        lock.withLock { _bound }
    }

    /// Make `accountIdB64` the account the unqualified ``load()`` /
    /// ``save(_:)`` act on; `nil` unbinds (logout, forget-device).
    static func bind(accountIdB64: String?) {
        lock.withLock { _bound = accountIdB64 }
    }

    static func storageKey(accountIdB64: String) -> String {
        prefix + accountIdB64
    }

    // MARK: - The bound account

    /// The bound account's accepted contacts; empty when nothing is bound.
    static func load() -> [AcceptedContact] {
        guard let accountIdB64 = boundAccountIdB64 else { return [] }
        return load(accountIdB64: accountIdB64)
    }

    /// Persist the bound account's accepted contacts. A no-op when nothing is
    /// bound: guessing an account would file the list under the wrong person.
    static func save(_ accepted: [AcceptedContact]) {
        guard let accountIdB64 = boundAccountIdB64 else {
            NSLog("contacts: accepted list not saved, no account is signed in")
            return
        }
        save(accepted, accountIdB64: accountIdB64)
    }

    // MARK: - A named account

    /// One account's persisted accepted contacts (empty on first use or a
    /// decode failure — a forward-compat / corruption guard).
    static func load(accountIdB64: String) -> [AcceptedContact] {
        guard let data = defaults.data(forKey: storageKey(accountIdB64: accountIdB64)) else {
            return []
        }
        return (try? JSONDecoder().decode([AcceptedContact].self, from: data)) ?? []
    }

    /// Persist one account's accepted contacts. A best-effort write — an
    /// encode failure (impossible for the all-`Codable` model) leaves the
    /// prior value in place rather than clearing it.
    ///
    /// Not written once the account's session has ended
    /// (``AccountStoreGate``): the writers include the restore and the
    /// pairing state bundle, which run off the main actor or after a wait.
    static func save(_ accepted: [AcceptedContact], accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(accepted) else { return }
        defaults.set(data, forKey: storageKey(accountIdB64: accountIdB64))
    }

    /// The account keys this account removed from its contacts
    /// (`docs/ux/conversation-info.md`: removal is local and leaves the
    /// conversation). The one-to-one adoption rule skips them; an explicit add
    /// takes a key out again.
    static func loadRemovedKeys(accountIdB64: String) -> Set<Data> {
        guard let data = defaults.data(forKey: removedKey(accountIdB64: accountIdB64)),
              let keys = try? JSONDecoder().decode([Data].self, from: data)
        else { return [] }
        return Set(keys)
    }

    /// Persist ``loadRemovedKeys(accountIdB64:)``. Not written once the
    /// account's session has ended (``AccountStoreGate``).
    static func saveRemovedKeys(_ keys: Set<Data>, accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(keys.sorted { $0.lexicographicallyPrecedes($1) })
        else { return }
        defaults.set(data, forKey: removedKey(accountIdB64: accountIdB64))
    }

    static func removedKey(accountIdB64: String) -> String {
        storageKey(accountIdB64: accountIdB64) + ".removed"
    }

    /// Forget-device: drop one account's list (and the old device-wide row,
    /// which nothing reads any more). Other accounts' lists are untouched.
    static func clear(accountIdB64: String) {
        let store = defaults
        store.removeObject(forKey: storageKey(accountIdB64: accountIdB64))
        store.removeObject(forKey: removedKey(accountIdB64: accountIdB64))
        store.removeObject(forKey: legacyGlobalKey)
    }
}
