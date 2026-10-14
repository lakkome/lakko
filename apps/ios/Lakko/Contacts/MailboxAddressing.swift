// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0074 section 3.5a — the mailbox after a key rotation.
//
// A mailbox's address and the right to read it both derive from an account
// key. After a PROVEN rotation the account moves to the mailbox of its new
// key at once, so:
//
//   1. a sender RESOLVES an account key before it derives a mailbox from it
//      (``AccountKeyResolver``): predecessor -> current, the sender's own
//      account included. An unproven change is never resolved: it is in
//      neither map;
//   2. a reach grant is kept for the PERSON (the current key), and one kept
//      under a key that later became a proven predecessor is still found
//      (``AccountKeyResolver/allKeys(of:)``);
//   4. the account's own predecessor mailboxes are READ for
//      ``PredecessorMailbox/readWindowMs`` after adopting, then left
//      (``PredecessorMailbox/reads(adoptedAtMs:nowMs:)``). Nothing is
//      registered there.
//
// Rule 3 (the profile re-send) lives with the publisher and the rotation's
// owed record.

/// Who an account key is NOW, across proven rotations: the proven-predecessor
/// map of the people this account knows, plus this account's own old keys.
///
/// A value type, so the rule is testable without a store.
public struct AccountKeyResolver: Equatable {

    /// `predecessor -> successor` for other people, recorded only by the
    /// ADR-0016 gate on a verified supersession proof.
    public var proven: ProvenPredecessors
    /// This account's current 32-byte account key.
    public var ownCurrentKey: Data?
    /// This account's own old keys (the predecessor slot): kept only by a
    /// rotation this device adopted, never by a reset.
    public var ownPredecessorKeys: [Data]

    public init(
        proven: ProvenPredecessors = ProvenPredecessors(),
        ownCurrentKey: Data? = nil,
        ownPredecessorKeys: [Data] = []
    ) {
        self.proven = proven
        self.ownCurrentKey = ownCurrentKey
        self.ownPredecessorKeys = ownPredecessorKeys
    }

    /// Whether `key` is this account's key, current or a proven predecessor.
    public func isOwn(_ key: Data) -> Bool {
        guard let ownCurrentKey else { return false }
        return key == ownCurrentKey || ownPredecessorKeys.contains(key)
    }

    /// The current key of the person `key` belongs to. `key` itself when it
    /// was never superseded BY PROOF (an unknown key, a current key, and the
    /// old key of a reset all resolve to themselves).
    public func current(of key: Data) -> Data {
        if let ownCurrentKey, ownPredecessorKeys.contains(key) { return ownCurrentKey }
        let resolved = proven.current(of: key.base64EncodedString())
        return Data(base64Encoded: resolved) ?? key
    }

    /// Every key of the person `key` belongs to: the current one first, then
    /// the proven predecessors.
    public func allKeys(of key: Data) -> [Data] {
        if isOwn(key), let ownCurrentKey {
            return [ownCurrentKey] + ownPredecessorKeys.filter { $0 != ownCurrentKey }
        }
        return proven.allKeys(of: key.base64EncodedString()).compactMap { Data(base64Encoded: $0) }
    }

    /// Whether two account keys are the same person across proven rotations.
    public func samePerson(_ a: Data, _ b: Data) -> Bool {
        a == b || current(of: a) == current(of: b)
    }

    /// Every key this resolver knows as a predecessor of something (own and
    /// other people's): what a stored mailbox pseudonym is matched against.
    public var predecessorKeys: [Data] {
        var out = ownCurrentKey == nil ? [] : ownPredecessorKeys
        out += proven.predecessorKeysB64.compactMap { Data(base64Encoded: $0) }
        return out
    }
}

/// Deriving a mailbox from an account key, resolved first (rule 1).
public enum MailboxAddress {

    /// The mailbox pseudonym of an account key, exactly as derived.
    public static func pseudonym(of accountKey: Data) -> Data? {
        try? IdentityPseudonym.fromIdentity(pubkey: accountKey).asBytes()
    }

    /// The mailbox a write for the person holding `accountKey` goes to: the
    /// mailbox of their CURRENT key.
    public static func pseudonym(forAccountKey accountKey: Data, resolver: AccountKeyResolver) -> Data? {
        pseudonym(of: resolver.current(of: accountKey))
    }

    /// Re-address a mailbox pseudonym that was derived earlier (an outbox
    /// entry recorded before the rotation was observed): when it is the
    /// mailbox of a known predecessor key, the mailbox of that person's
    /// current key; otherwise `pseudonym` itself.
    public static func current(
        of pseudonym: Data, resolver: AccountKeyResolver,
        derive: (Data) -> Data? = MailboxAddress.pseudonym(of:)
    ) -> Data {
        for key in resolver.predecessorKeys where derive(key) == pseudonym {
            return derive(resolver.current(of: key)) ?? pseudonym
        }
        return pseudonym
    }
}

extension PredecessorMailbox {

    /// How long a predecessor mailbox is read after this device adopted the
    /// rotation that retired it: the DS journal retention (ADR-0074 section
    /// 3.5a rule 4; chosen).
    static let readWindowMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// Whether a predecessor mailbox adopted at `adoptedAtMs` is still read
    /// at `nowMs`. A clock that stepped back before the adoption reads (the
    /// window is an upper bound on age, never a reason to skip early).
    static func reads(adoptedAtMs: Int64, nowMs: Int64) -> Bool {
        nowMs - adoptedAtMs < readWindowMs
    }

    /// When this device adopted the rotation that retired each of the
    /// account's own predecessor keys (unix ms), in `UserDefaults`: a public
    /// key and a time. Cleared by the account wipe.
    struct AdoptionStore {
        static let prefix = "lakko.contacts.predecessor-adopted-at.v1."
        private let defaults: UserDefaults

        init(defaults: UserDefaults = .standard) {
            self.defaults = defaults
        }

        private static func key(accountIdB64: String) -> String { prefix + accountIdB64 }

        func adoptedAtMs(accountIdB64: String, keyB64: String) -> Int64? {
            ((defaults.dictionary(forKey: Self.key(accountIdB64: accountIdB64)) as? [String: NSNumber])?[keyB64])?
                .int64Value
        }

        /// Record the adoption time of `keyB64` once; a later call never
        /// moves it (re-adopting must not reopen the window). Returns the
        /// time in effect.
        @discardableResult
        func recordIfAbsent(accountIdB64: String, keyB64: String, nowMs: Int64) -> Int64 {
            var map = (defaults.dictionary(forKey: Self.key(accountIdB64: accountIdB64)) as? [String: NSNumber]) ?? [:]
            if let held = map[keyB64]?.int64Value { return held }
            map[keyB64] = NSNumber(value: nowMs)
            guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return nowMs }
            defer { gate.leave() }
            defaults.set(map, forKey: Self.key(accountIdB64: accountIdB64))
            return nowMs
        }

        func clear(accountIdB64: String) {
            defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
        }
    }

    /// The predecessor keys (base64) still inside the read window. A key with
    /// no recorded adoption (a rotation adopted before the time was kept) is
    /// stamped `nowMs` and read: its window starts now.
    static func readable(
        keysB64: [String], accountIdB64: String, nowMs: Int64, adoptions: AdoptionStore
    ) -> [String] {
        keysB64.filter { keyB64 in
            let adopted = adoptions.recordIfAbsent(accountIdB64: accountIdB64, keyB64: keyB64, nowMs: nowMs)
            return reads(adoptedAtMs: adopted, nowMs: nowMs)
        }
    }
}
