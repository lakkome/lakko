// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// ADR-0060 — what this device keeps about reach, per account.
///
/// Held grant seeds are SECRET (whoever holds one can write to its owner's
/// mailbox), and the owner's own reply and link scopes derive live keys, so
/// the whole record lives in the Keychain (after first unlock, this device
/// only, never backed up) rather than `UserDefaults`.
public struct ReachRecord: Codable, Equatable {
    /// The "who can find me" setting, as the core's Visibility envelope.
    public var settingEnvelopeB64: String?
    /// Every reach key version counter, as the core's ReachState envelope.
    public var reachStateEnvelopeB64: String?
    /// The owner's live `/i/` and `/c/` link scopes.
    public var links: [LinkReach] = []
    /// The owner's outstanding reply scopes.
    public var replies: [ReplyReach] = []
    /// Base64 of every public key this device last registered on the DS.
    public var registeredB64: [String] = []
    /// Whether the last registration also set the mailbox open.
    public var registeredOpen: Bool?
    /// Grants received from other people, keyed by the base64 of the person's
    /// account key resolved at receipt (C-68).
    public var held: [String: [ReachRules.HeldGrant]] = [:]
    /// Whether this account's name terms are published in discovery (Everyone).
    public var publishedEveryone: Bool?
    /// This account's discovery-signed username certificate (ADR-0060 section
    /// 5), carried in every profile v3 that names the username. Public.
    public var ownUsernameCertificateB64: String?
    /// ADR-0060 section 4 — the newest introduction record each contact
    /// offered (base64 record wire), keyed by the contact's account key.
    public var offers: [String: String] = [:]
    /// The newest introductions bundle each contact relayed.
    public var bundles: [String: StoredBundle] = [:]
    /// The introduction record this device last offered its contacts
    /// (base64), and the fof version it carries.
    public var ownOfferB64: String?
    public var ownOfferFofVersion: UInt32?
    /// What the cached own record was composed for (`fof version|name|username`).
    public var ownOfferCacheKey: [String] = []
    /// ADR-0060 section 4 - the OPRF OUTPUT of each term already evaluated
    /// (`"n:<term>"` / `"u:<username>"` -> base64). Never sent anywhere.
    public var introductionTermOutputsB64: [String: String]?
    /// Per one-to-one conversation (group hex), the key of the own
    /// introduction record last offered there, so an unchanged offer is not
    /// re-sent.
    public var introductionsSentDigest: [String: String] = [:]
    /// Per one-to-one conversation (group hex), a digest of the relayed
    /// records last sent there as a bundle. Tracked apart from the offer, as
    /// web does: either can change without the other. Optional so a record
    /// stored before the split still decodes.
    public var bundleSentDigest: [String: String]?
    /// ADR-0060 section 4, "A device that holds nothing asks" — per one-to-one
    /// conversation (group hex), when an ask from the other person last
    /// reset the two sent records above. Optional so a record stored before
    /// it still decodes.
    public var introductionsResetAtMs: [String: UInt64]?
    /// ADR-0060 section 1, "Versions after a restore" — the counter slots
    /// (``ReachRules/VersionSlot/storageKey``) this device has checked against
    /// the keys its mailbox holds on the DS. A slot with no version in the
    /// reach state and not listed here is recovered before anything is
    /// registered. Optional so a record stored before it still decodes.
    public var versionCheckedSlots: [String]?
    /// ADR-0074 section 3.5a rule 5 — the link and reply scopes that were live
    /// when this account's keys were rotated. Their grants were derived from
    /// the OLD account seed and name the old mailbox: they are spent. Nothing
    /// is registered for them; the rotation's owed step revokes the links at
    /// their ledger and re-sends the requests, then clears both.
    public var spentLinks: [LinkReach]?
    public var spentReplies: [ReplyReach]?
    /// ADR-0078 section 9 — the groups and channels the account was in at the
    /// last registration, so the next one sees which it left and rotates
    /// that class's one key. `nil` until the first registration.
    public var memberOf: ReachRules.Membership?
    /// ADR-0060 section 4, "Narrowing" — when this device last rotated the
    /// `fof` key on its 7-day schedule (or adopted a rotation another device
    /// made). Device-local bookkeeping, not part of the shared reach state.
    /// `nil` until the first registration starts the schedule.
    public var fofRotatedAtMs: UInt64?
    /// ADR-0060 section 6, "An add of a person found by search" — the people
    /// this device proposed to add to a group after finding them in
    /// Everyone, with the mailbox binding their public record carried. A
    /// device never receives its own group messages, so its own sweep reads
    /// this in place of the committer-delegation envelope it posted. Removed
    /// once the Welcome to that person in that group was written. Optional
    /// so a record stored before it still decodes.
    public var openAdds: [OpenAdd]?
    /// ADR-0060 section 6 — the committer-delegation envelopes received in
    /// this account's groups, newest per (group, sender, added person), oldest
    /// first. Public to the group, no secret; kept here so a committer that
    /// restarts can still write a Welcome it owes. An envelope whose
    /// delegations expired is dropped. Optional so a record stored before it
    /// still decodes.
    public var keptEnvelopes: [KeptEnvelope]?

    public init() {}

    /// One ``openAdds`` entry.
    public struct OpenAdd: Codable, Equatable {
        public var groupIdHex: String
        public var accountKeyB64: String
        public var bindingB64: String
        public var storedAtMs: UInt64
    }

    /// One ``keptEnvelopes`` entry.
    public struct KeptEnvelope: Codable, Equatable {
        /// Lower-case hex of the group it arrived in; empty when the caller
        /// named none.
        public var groupIdHex: String
        /// The sender's account key (base64), resolved at receipt (C-68).
        public var senderKeyB64: String
        /// The composed envelope (base64).
        public var envelopeB64: String
        public var storedAtMs: UInt64
    }

    public struct StoredBundle: Codable, Equatable {
        public var updatedAtMs: UInt64
        public var recordsB64: [String]
    }
}

/// Where ``ReachRecord``s are kept. Injected so tests run without a Keychain.
public protocol ReachPersistence: AnyObject {
    func load(accountIdB64: String) -> ReachRecord?
    func save(_ record: ReachRecord, accountIdB64: String)
    func delete(accountIdB64: String)
}

/// Keychain-backed ``ReachPersistence``: one generic-password item per
/// account, JSON-encoded.
public final class KeychainReachPersistence: ReachPersistence {
    static let service = "me.lakko.reach.v1"

    public init() {}

    public func load(accountIdB64: String) -> ReachRecord? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: accountIdB64,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess,
              let data = out as? Data else { return nil }
        return try? JSONDecoder().decode(ReachRecord.self, from: data)
    }

    public func save(_ record: ReachRecord, accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(record) else { return }
        delete(accountIdB64: accountIdB64)
        let add: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: data,
        ]
        let status = SecItemAdd(add as CFDictionary, nil)
        if status != errSecSuccess {
            NSLog("ADR-0060: reach record not persisted (status \(status))")
        }
    }

    public func delete(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: accountIdB64,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// In-memory ``ReachPersistence`` for tests.
public final class InMemoryReachPersistence: ReachPersistence {
    public private(set) var records: [String: ReachRecord] = [:]

    public init() {}

    public func load(accountIdB64: String) -> ReachRecord? { records[accountIdB64] }
    public func save(_ record: ReachRecord, accountIdB64: String) { records[accountIdB64] = record }
    public func delete(accountIdB64: String) { records[accountIdB64] = nil }
}
