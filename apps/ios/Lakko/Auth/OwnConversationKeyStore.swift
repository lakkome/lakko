// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0074, "Joining after a rotation: the device waits for the earlier keys"
// (G-56), contract section 1.3 — what this device last read of its OWN
// account's conversation link: the conversation key `C` the link names, or
// that it names none. Written whenever a landing read the account's bundle;
// read by the DeviceGroup lineage guard and by the chat-layer binding check,
// so neither needs a fetch. Wiped by Forget device and by a key reset or a
// recall run on this device.

/// What this device recorded of its own account's conversation link.
public struct OwnConversationKeyRecord: Codable, Equatable {
    /// The 32-byte conversation key the verified link names; `nil` when the
    /// bundle carried no link (or one that did not verify against this
    /// device's current key).
    public let conversationKey: Data?
    /// When the bundle was read (Unix milliseconds).
    public let readAtMs: Int64

    public init(conversationKey: Data?, readAtMs: Int64) {
        self.conversationKey = conversationKey
        self.readAtMs = readAtMs
    }
}

/// The per-account record of the own conversation key.
public protocol OwnConversationKeyStoring {
    func load(accountIdB64: String) -> OwnConversationKeyRecord?
    func save(_ record: OwnConversationKeyRecord, accountIdB64: String)
    func clear(accountIdB64: String)
}

/// The production store: one generic-password Keychain item per account in
/// the app's own Keychain group, `AfterFirstUnlockThisDeviceOnly`, like the
/// own predecessor seeds (``KeychainOwnPredecessorStore``). The key it holds
/// is a public key; the item says which key the account's chats use.
public struct KeychainOwnConversationKeyStore: OwnConversationKeyStoring {

    public static let serviceTag = "me.lakko.own-conversation-key.v1"

    public init() {}

    public func load(accountIdB64: String) -> OwnConversationKeyRecord? {
        guard let data = KeychainJSONItem.read(service: Self.serviceTag, account: accountIdB64),
              let record = try? JSONDecoder().decode(OwnConversationKeyRecord.self, from: data) else {
            return nil
        }
        if let key = record.conversationKey, key.count != 32 { return nil }
        return record
    }

    public func save(_ record: OwnConversationKeyRecord, accountIdB64: String) {
        do {
            let gate = try AccountStoreGate.enterUnlessWipedOrEnd(accountIdB64)
            defer { gate.leave() }
            try KeychainJSONItem.write(
                try JSONEncoder().encode(record), service: Self.serviceTag, account: accountIdB64)
        } catch {
            NSLog("[g56] own conversation key record not written: \(error)")
        }
    }

    public func clear(accountIdB64: String) {
        KeychainJSONItem.delete(service: Self.serviceTag, account: accountIdB64)
    }
}

/// A store in memory (tests, and a placeholder where nothing is persisted).
public final class InMemoryOwnConversationKeyStore: OwnConversationKeyStoring {
    private var records: [String: OwnConversationKeyRecord] = [:]
    private let lock = NSLock()

    public init() {}

    public func load(accountIdB64: String) -> OwnConversationKeyRecord? {
        lock.lock(); defer { lock.unlock() }
        return records[accountIdB64]
    }

    public func save(_ record: OwnConversationKeyRecord, accountIdB64: String) {
        lock.lock(); defer { lock.unlock() }
        records[accountIdB64] = record
    }

    public func clear(accountIdB64: String) {
        lock.lock(); defer { lock.unlock() }
        records[accountIdB64] = nil
    }
}

/// G-56 — the id of the DeviceGroup this device joined by pairing, per
/// account. A device parked for the account's earlier keys cannot derive the
/// id (it is a function of the earlier key), and a re-pairing from the parked
/// screen names it in its self-removal proof and its epoch probe. Kept in the
/// app's Keychain group; wiped with the account.
public struct JoinedDeviceGroupIdStore {

    public static let serviceTag = "me.lakko.joined-device-group-id.v1"

    public init() {}

    public func load(accountIdB64: String) -> Data? {
        guard let data = KeychainJSONItem.read(service: Self.serviceTag, account: accountIdB64),
              !data.isEmpty else { return nil }
        return data
    }

    public func save(_ groupId: Data, accountIdB64: String) {
        do {
            let gate = try AccountStoreGate.enterUnlessWipedOrEnd(accountIdB64)
            defer { gate.leave() }
            try KeychainJSONItem.write(groupId, service: Self.serviceTag, account: accountIdB64)
        } catch {
            NSLog("[g56] the joined DeviceGroup id was not recorded: \(error)")
        }
    }

    public func clear(accountIdB64: String) {
        KeychainJSONItem.delete(service: Self.serviceTag, account: accountIdB64)
    }
}
