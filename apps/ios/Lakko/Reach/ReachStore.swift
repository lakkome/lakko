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
    /// Per one-to-one conversation (group hex), a digest of the introduction
    /// offer and relayed records last sent there, so an unchanged set is not
    /// re-sent.
    public var introductionsSentDigest: [String: String] = [:]

    public init() {}

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
