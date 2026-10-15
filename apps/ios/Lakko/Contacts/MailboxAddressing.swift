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
//   4. the account's own predecessor mailboxes (ADR-0078: the mailbox key
//      of each predecessor SEED) are READ for
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
    /// other people's).
    public var predecessorKeys: [Data] {
        var out = ownCurrentKey == nil ? [] : ownPredecessorKeys
        out += proven.predecessorKeysB64.compactMap { Data(base64Encoded: $0) }
        return out
    }
}

/// ADR-0078 phase 1 — one mailbox a write goes to, VERIFIED.
///
/// A mailbox address is no longer a function of an identity key: it is the
/// hash of a mailbox key derived from the owner's account seed, and a writer
/// learns that key only from the owner's 97-byte mailbox binding (the key
/// signed by the owner's account identity key). A target exists only once the
/// binding verified against the account key this device holds for the person
/// (``verified(binding:accountKey:)``, ``verified(grant:accountKey:)``), so a
/// row is never sealed to, or addressed at, a mailbox the person does not
/// read. A failed check writes nothing.
public struct MailboxTarget: Equatable, Hashable {
    /// The 32-byte Ed25519 mailbox public key a row is sealed to.
    public let mailboxPk: Data
    /// The 32-byte address the row is posted under (`/ds/mailbox/{hex}`).
    public let address: Data
    /// The 97-byte binding the key came from (what a committer delegation
    /// carries for the added person).
    public let binding: Data
    /// The account key the binding verified under.
    public let accountKey: Data

    public init(mailboxPk: Data, address: Data, binding: Data, accountKey: Data) {
        self.mailboxPk = mailboxPk
        self.address = address
        self.binding = binding
        self.accountKey = accountKey
    }

    /// The mailbox `binding` names, after verifying it against `accountKey`,
    /// the account key this device holds pinned for its owner. Throws when it
    /// does not verify (or is malformed): the caller writes nothing.
    public static func verified(binding: Data, accountKey: Data) throws -> MailboxTarget {
        let pk = try verifyMailboxBinding(binding: binding, accountPk: accountKey)
        return MailboxTarget(
            mailboxPk: pk, address: try mailboxAddress(mailboxPk: pk), binding: binding,
            accountKey: accountKey)
    }

    /// The mailbox a reach grant writes to, after verifying its binding
    /// against `accountKey`. Throws when it does not verify.
    public static func verified(grant: FfiReachGrant, accountKey: Data) throws -> MailboxTarget {
        let pk = try reachGrantMailboxPkVerified(grant: grant, accountPk: accountKey)
        return MailboxTarget(
            mailboxPk: pk, address: try mailboxAddress(mailboxPk: pk), binding: grant.binding,
            accountKey: accountKey)
    }

    /// ``verified(binding:accountKey:)`` that logs a refusal (without any
    /// identifier) and answers `nil`.
    public static func verifiedOrNil(binding: Data, accountKey: Data) -> MailboxTarget? {
        do {
            return try verified(binding: binding, accountKey: accountKey)
        } catch {
            NSLog("ADR-0078: a mailbox binding did not verify; nothing is written there")
            return nil
        }
    }

    /// ``verified(grant:accountKey:)`` that logs a refusal (without any
    /// identifier) and answers `nil`.
    public static func verifiedOrNil(grant: FfiReachGrant, accountKey: Data) -> MailboxTarget? {
        do {
            return try verified(grant: grant, accountKey: accountKey)
        } catch {
            NSLog("ADR-0078: a reach grant's mailbox binding did not verify; nothing is written there")
            return nil
        }
    }

    /// This account's own mailbox (every device of the account derives the
    /// same): where a Welcome for one of its own devices goes.
    public static func own(accountIdentity: Identity) -> MailboxTarget {
        let key = MailboxKey.fromAccountIdentity(accountIdentity: accountIdentity)
        return MailboxTarget(
            mailboxPk: key.publicKey(), address: key.address(),
            binding: signMailboxBinding(accountIdentity: accountIdentity),
            accountKey: accountIdentity.signaturePublicKey())
    }

    /// Seal `body` as one row of `kind` to this mailbox.
    public func seal(kind: FfiMailboxKind, body: Data) throws -> Data {
        try sealMailboxRow(mailboxPk: mailboxPk, kind: kind, body: body)
    }
}

/// A write to one person, decided (ADR-0078): the VERIFIED mailbox it goes
/// to and every authorisation this device holds for that mailbox, in the
/// order a writer tries them (empty: the write goes without a proof and
/// spends a `mailbox-write` anonymous token, which only an open mailbox
/// stores).
public struct MailboxWritePlan: Equatable {
    public let target: MailboxTarget
    public let candidates: [DsMailboxWriteAuth]

    public init(target: MailboxTarget, candidates: [DsMailboxWriteAuth]) {
        self.target = target
        self.candidates = candidates
    }

    /// The first authorisation, `nil` for a proofless write.
    public var auth: DsMailboxWriteAuth? { candidates.first }
}

/// No mailbox is known for the person a write is meant for (no grant, no
/// binding), or the binding that named one did not verify: nothing is
/// written. Carries no identifier.
public struct MailboxUnknown: Error, Equatable, LocalizedError {
    public init() {}
    public var errorDescription: String? { "No way to reach this person yet." }
}

/// ADR-0078 phase 1 — reading the own mailbox.
public enum MailboxRows {

    /// Open one row of the own mailbox: its kind and the inner wire, or `nil`
    /// for a row that does not open (garbage, a row for another mailbox, an
    /// unknown kind). A row that does not open is dropped by the caller; the
    /// cursor still moves past it. Logged without identifiers.
    public static func open(_ row: Data, key: MailboxKey) -> FfiOpenedMailboxRow? {
        do {
            return try openMailboxRow(key: key, row: row)
        } catch {
            NSLog("ADR-0078: a mailbox row did not open and was dropped")
            return nil
        }
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
