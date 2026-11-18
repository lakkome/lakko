// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0074, "Joining after a rotation: the device waits for the earlier keys"
// (G-56). A device that joins an account does not bind its device identity,
// publish a KeyPackage, join a conversation or open its DeviceGroup while the
// account's earlier keys exist and have not reached it. The rule itself is
// the core's (`ownBindingVerdict`, `ownConversationKey`); this file reads its
// inputs from this device's stores and maps the answers. Nothing here writes.

/// Thrown by the chat-layer binding when the verdict is `wait`: the device
/// must not mint a binding, and the session root parks it (contract 2.2).
public struct EarlierKeysMissing: Error, Equatable {
    public let accountIdB64: String
    public init(accountIdB64: String) { self.accountIdB64 = accountIdB64 }
}

public enum EarlierKeysGate {

    /// What the verdict reads, from this device.
    public struct Inputs: Equatable {
        /// The account key the stored device binding names; `nil` when there
        /// is none or it does not verify.
        public var boundKey: Data?
        /// The current account key (the account seed this device holds).
        public var currentKey: Data
        /// The keys of the own predecessor seeds, oldest first, current
        /// excluded.
        public var predecessorKeys: [Data]
        /// Whether this device is in any conversation.
        public var holdsConversations: Bool

        public init(boundKey: Data?, currentKey: Data, predecessorKeys: [Data], holdsConversations: Bool) {
            self.boundKey = boundKey
            self.currentKey = currentKey
            self.predecessorKeys = predecessorKeys
            self.holdsConversations = holdsConversations
        }
    }

    /// Read the inputs for `accountIdB64` whose current account seed is
    /// `accountSeed`. `nil` when the seed does not build an identity.
    public static func readInputs(
        accountIdB64: String,
        accountSeed: Data,
        predecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        holdsConversations: Bool? = nil
    ) -> Inputs? {
        guard let current = try? Identity.fromSeed(seed: accountSeed).signaturePublicKey() else {
            return nil
        }
        var boundKey: Data?
        if let deviceSeed = (try? DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: accountIdB64)) ?? nil,
           let binding = DeviceIdentityStore.getDeviceBinding(accountIdB64: accountIdB64),
           let device = try? Identity.fromSeedWithBinding(seed: deviceSeed, binding: binding) {
            boundKey = device.accountPublicKey()
        }
        let keys = predecessors.load(accountIdB64: accountIdB64)
            .compactMap { try? Identity.fromSeed(seed: $0).signaturePublicKey() }
            .filter { $0 != current }
        return Inputs(
            boundKey: boundKey,
            currentKey: current,
            predecessorKeys: keys,
            holdsConversations: holdsConversations
                ?? !KnownGroupsStore().load(accountIdB64: accountIdB64).isEmpty)
    }

    /// The core's verdict. A refusal of the core (a malformed key) is `wait`:
    /// nothing is minted on an input the rule cannot read.
    public static func verdict(_ inputs: Inputs, link: FfiOwnLink) -> FfiBindingVerdict {
        do {
            return try ownBindingVerdict(
                boundKey: inputs.boundKey,
                currentKey: inputs.currentKey,
                predecessorKeys: inputs.predecessorKeys,
                link: link,
                holdsConversations: inputs.holdsConversations)
        } catch {
            NSLog("[g56] binding verdict refused its input; waiting")
            return .wait
        }
    }

    /// `true` for the two kept answers.
    public static func isKept(_ verdict: FfiBindingVerdict) -> Bool {
        switch verdict {
        case .keep, .keepUnderCurrentKey: return true
        case .wait, .mint: return false
        }
    }

    /// The link answer of a bundle fetched for this device's OWN account:
    /// `.key(C)` when the bundle carries a conversation link that verifies
    /// against `currentKey` (this device's current account key, never the
    /// fetched one), else `.absent`. A link that does not verify (minted
    /// before a reset, or forged) is ignored.
    public static func linkAnswer(bundle: IdentityFetchResponse?, currentKey: Data) -> FfiOwnLink {
        guard let raw = bundle?.conversationLink, !raw.isEmpty,
              let link = Data(base64Encoded: raw) ?? base64Decode(raw) else {
            return .absent
        }
        guard let key = (try? ownConversationKey(link: link, currentKey: currentKey)) ?? nil else {
            return .absent
        }
        return .key(key: key)
    }

    /// The link answer this device recorded (``OwnConversationKeyStoring``):
    /// `.key(C)` when it recorded one, else `.absent`.
    public static func linkFromRecord(_ record: OwnConversationKeyRecord?) -> FfiOwnLink {
        if let key = record?.conversationKey { return .key(key: key) }
        return .absent
    }

    /// The record a read answer writes; `.unknown` writes nothing.
    public static func record(for link: FfiOwnLink, nowMs: Int64) -> OwnConversationKeyRecord? {
        switch link {
        case .absent: return OwnConversationKeyRecord(conversationKey: nil, readAtMs: nowMs)
        case .key(let key): return OwnConversationKeyRecord(conversationKey: key, readAtMs: nowMs)
        case .unknown: return nil
        }
    }

    /// The account seed a `.mint(key)` verdict mints under: the held seed
    /// whose key is `key` (the current seed or an own predecessor). `nil`
    /// when none derives it (the core never answers that; a caller treats it
    /// as `wait`).
    public static func seed(for key: Data, currentSeed: Data, predecessorSeeds: [Data]) -> Data? {
        for seed in [currentSeed] + predecessorSeeds {
            if (try? Identity.fromSeed(seed: seed).signaturePublicKey()) == key { return seed }
        }
        return nil
    }
}
