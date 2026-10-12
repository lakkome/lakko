// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

// ADR-0066 part 3, Stage A — rotating the account keys and keeping the other
// devices: what EVERY device of the account needs, whichever side it is on.
//
//   * `KeyRotationWire`                 the fixed sizes and the proof window;
//   * `OwnPredecessorStoring`           the account's OWN old seed, kept after
//                                       a rotation ("predecessor slot"): the
//                                       conversation leaves stay bound to the
//                                       old key until Stage B;
//   * `PendingKeyRotation` / store      a `key-rotation` envelope a KEPT
//                                       device received and has not adopted;
//   * `KeptDeviceRule`                  the kept device's verdicts, pure:
//                                       store / drop / adopt / read the
//                                       journal first / treat as a reset;
//   * `SessionUpgradeAnswer`            the refresh route's answers, mapped;
//   * `KeysRotatedNoticeStore`          whether the "keys were changed" line
//                                       is still owed to the person.
//
// The rotating device's flow is `Devices/KeyRotationRun.swift`;
// `AuthRepository` is the state around both.

/// Fixed sizes of the rotation's artefacts.
public enum KeyRotationWire {
    /// The supersession proof (`lakko-transparency`), bytes.
    public static let proofLength = 145
    /// The BIP39 entropy of a 12-word phrase, bytes.
    public static let entropyLength = 16
    /// `revoke_device_ids_b64` holds at most this many ids.
    public static let maxRevokedDevices = 8
    /// The proof's validity: 14 days, as before ADR-0017 (ADR-0066 section
    /// 3.2 step 3). The format's ceiling is 30 days.
    public static let proofWindowSecs: Int64 = 14 * 24 * 60 * 60
}

// MARK: - The account's own predecessor seed

/// The OLD account identity seed(s) of an account this device rotated (or
/// adopted a rotation of). Kept until Stage B moves the conversation leaves:
/// this device's leaves, its stored device binding, the registrar seeds
/// sealed to it and the roots of the journals it owned are all under the old
/// key.
public protocol OwnPredecessorStoring {
    /// The stored 64-byte phrase seeds, newest last. Empty when the account
    /// never rotated on this device.
    func load(accountIdB64: String) -> [Data]
    /// Append `seed` (a no-op when already stored).
    func add(_ seed: Data, accountIdB64: String) throws
    /// Replace the whole slot with `seeds` (oldest first). Used when a
    /// pairing state bundle hands a device paired AFTER a rotation the chain
    /// it could not have adopted itself.
    func replace(_ seeds: [Data], accountIdB64: String) throws
    func clear(accountIdB64: String)
}

/// The production store: one generic-password Keychain item per account,
/// `AfterFirstUnlockThisDeviceOnly`, in the app's own Keychain group (the
/// notification extension has no use for it), the protection the recall
/// record has.
public struct KeychainOwnPredecessorStore: OwnPredecessorStoring {

    public static let serviceTag = "me.lakko.own-predecessor-seed.v1"

    public init() {}

    public func load(accountIdB64: String) -> [Data] {
        guard let data = KeychainJSONItem.read(service: Self.serviceTag, account: accountIdB64),
              let seeds = try? JSONDecoder().decode([Data].self, from: data) else { return [] }
        return seeds.filter { !$0.isEmpty }
    }

    public func add(_ seed: Data, accountIdB64: String) throws {
        guard !seed.isEmpty else { return }
        var seeds = load(accountIdB64: accountIdB64)
        guard !seeds.contains(seed) else { return }
        seeds.append(seed)
        try KeychainJSONItem.write(
            try JSONEncoder().encode(seeds), service: Self.serviceTag, account: accountIdB64)
    }

    public func replace(_ seeds: [Data], accountIdB64: String) throws {
        var out: [Data] = []
        for seed in seeds where !seed.isEmpty && !out.contains(seed) { out.append(seed) }
        guard !out.isEmpty else {
            clear(accountIdB64: accountIdB64)
            return
        }
        try KeychainJSONItem.write(
            try JSONEncoder().encode(out), service: Self.serviceTag, account: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        KeychainJSONItem.delete(service: Self.serviceTag, account: accountIdB64)
    }
}

/// The account keys this device answers to: the current one and its own
/// proven predecessors.
public enum OwnAccountKeys {

    /// The Ed25519 public keys of the stored predecessor seeds.
    public static func predecessorPublicKeys(
        accountIdB64: String, store: OwnPredecessorStoring = KeychainOwnPredecessorStore()
    ) -> [Data] {
        store.load(accountIdB64: accountIdB64).compactMap {
            try? Identity.fromSeed(seed: $0).signaturePublicKey()
        }
    }

    /// Which account identity a stored device binding may name without being
    /// re-minted: the current account key, or one of this account's own
    /// predecessors. A device does NOT re-bind after a rotation (ADR-0066
    /// fact 14): a leaf under the new key is a different account to every
    /// gate, and a re-bound device misjudges its own standing.
    public static func bindingIsOwn(
        bindingAccountKey: Data, currentAccountKey: Data, predecessorKeys: [Data]
    ) -> Bool {
        bindingAccountKey == currentAccountKey || predecessorKeys.contains(bindingAccountKey)
    }

    /// The account seed a device's conversation binding is minted under:
    /// the OLDEST own predecessor when the account rotated (the key every
    /// device that was there before the first rotation is bound to, and the
    /// one a pairing state bundle lists first), else the current seed. A
    /// device paired after a rotation thereby binds under the SAME account
    /// key as the device that paired it.
    public static func conversationSeed(currentSeed: Data, predecessorSeeds: [Data]) -> Data {
        predecessorSeeds.first ?? currentSeed
    }

    /// What to do with a device's stored conversation binding.
    public enum BindingVerdict: Equatable {
        /// The binding is this account's and is kept.
        case keep
        /// No usable binding, or one naming a key that is not this
        /// account's: mint one under the conversation key.
        case mint
        /// The binding names the CURRENT key although the account has
        /// predecessors (it was minted before the pairing state bundle
        /// brought them) and this device holds no conversation yet: mint it
        /// again under the conversation key, before any KeyPackage naming
        /// the current key admits this device anywhere.
        case remint
        /// The same, but this device already holds conversations whose
        /// leaves carry the current-key binding: it is kept (a re-mint would
        /// split this device between two account keys). Such a device is not
        /// admitted to the account's older conversations.
        case keepUnderCurrentKey
    }

    /// - Parameters:
    ///   - boundKey: the account key the stored binding names; `nil` when
    ///     there is none or it did not verify.
    ///   - predecessorKeys: the own predecessor keys, oldest first.
    ///   - holdsConversations: whether this device is in any conversation.
    public static func bindingVerdict(
        boundKey: Data?, currentKey: Data, predecessorKeys: [Data], holdsConversations: Bool
    ) -> BindingVerdict {
        guard let boundKey else { return .mint }
        if predecessorKeys.contains(boundKey) { return .keep }
        guard boundKey == currentKey else { return .mint }
        if predecessorKeys.isEmpty { return .keep }
        return holdsConversations ? .keepUnderCurrentKey : .remint
    }

    /// ADR-0066 part 3 — whether a KeyPackage of THIS account names the
    /// wrong account key for a conversation: its binding is under one of the
    /// account's own keys, but not the one this account's leaves in that
    /// conversation are bound to. Such a device (one that minted its binding
    /// before the pairing bundle reached it) is not admitted; it re-mints
    /// and publishes again, and the re-admission arm takes it then.
    public static func namesWrongOwnKey(
        keyPackageAccountKey: Data, ownKeyInConversation: Data,
        currentAccountKey: Data?, predecessorKeys: [Data]
    ) -> Bool {
        keyPackageAccountKey != ownKeyInConversation
            && isOwn(keyPackageAccountKey, currentAccountKey: currentAccountKey, predecessorKeys: predecessorKeys)
    }

    /// Whether `key` is this account's own key, now or before a rotation.
    public static func isOwn(_ key: Data, currentAccountKey: Data?, predecessorKeys: [Data]) -> Bool {
        key == currentAccountKey || predecessorKeys.contains(key)
    }
}

// MARK: - A pending rotation on a kept device

/// A `key-rotation` envelope a kept device received in its own DeviceGroup
/// and verified (`verifyKeyRotationEnvelope`), not yet adopted. Keyed on
/// `newSignaturePubkey`: a device may hold several, and which one it adopts
/// is decided by the key the server side shows as the account's, never by
/// arrival order or by the stamp.
public struct PendingKeyRotation: Codable, Equatable {
    public let newSignaturePubkey: Data
    /// The 16-byte entropy of the new phrase.
    public let newEntropy: Data
    /// The 145-byte proof, verified under the account key this device held.
    public let supersessionProof: Data
    public let preparedAtMs: UInt64
    public let proofExpiresAtUnixSecs: Int64

    public init(
        newSignaturePubkey: Data, newEntropy: Data, supersessionProof: Data,
        preparedAtMs: UInt64, proofExpiresAtUnixSecs: Int64
    ) {
        self.newSignaturePubkey = newSignaturePubkey
        self.newEntropy = newEntropy
        self.supersessionProof = supersessionProof
        self.preparedAtMs = preparedAtMs
        self.proofExpiresAtUnixSecs = proofExpiresAtUnixSecs
    }

    public init(_ verified: FfiVerifiedKeyRotation) {
        self.init(
            newSignaturePubkey: verified.newSignaturePubkey,
            newEntropy: verified.newEntropy,
            supersessionProof: verified.supersessionProof,
            preparedAtMs: verified.preparedAtMs,
            proofExpiresAtUnixSecs: verified.proofExpiresAtUnixSecs)
    }

    enum CodingKeys: String, CodingKey {
        case newSignaturePubkey = "new_signature_pubkey"
        case newEntropy = "new_entropy"
        case supersessionProof = "supersession_proof"
        case preparedAtMs = "prepared_at_ms"
        case proofExpiresAtUnixSecs = "proof_expires_at_unix_secs"
    }
}

public protocol KeyRotationPendingStoring {
    func load(accountIdB64: String) -> [PendingKeyRotation]
    func save(_ pending: [PendingKeyRotation], accountIdB64: String) throws
    func clear(accountIdB64: String)
}

/// The production store of pending rotations: one Keychain item per account
/// (it holds the account's NEXT seed), `AfterFirstUnlockThisDeviceOnly`, the
/// app's own group.
public struct KeychainKeyRotationPendingStore: KeyRotationPendingStoring {

    public static let serviceTag = "me.lakko.key-rotation-pending.v1"

    public init() {}

    public func load(accountIdB64: String) -> [PendingKeyRotation] {
        guard let data = KeychainJSONItem.read(service: Self.serviceTag, account: accountIdB64),
              let pending = try? JSONDecoder().decode([PendingKeyRotation].self, from: data) else {
            return []
        }
        return pending
    }

    public func save(_ pending: [PendingKeyRotation], accountIdB64: String) throws {
        if pending.isEmpty {
            clear(accountIdB64: accountIdB64)
            return
        }
        try KeychainJSONItem.write(
            try JSONEncoder().encode(pending), service: Self.serviceTag, account: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        KeychainJSONItem.delete(service: Self.serviceTag, account: accountIdB64)
    }
}

// MARK: - The kept device's verdicts

/// What a kept device does, decided purely.
public enum KeptDeviceRule {

    /// Where a dispatched `KeyRotation` envelope arrived.
    public enum Arrival: Equatable {
        /// As a `ServiceEnvelope` of the account's OWN DeviceGroup.
        case ownDeviceGroup
        /// Anywhere else (a conversation, a mailbox, a channel).
        case elsewhere
    }

    public enum Receipt: Equatable {
        /// Verified: keep it as pending. Nothing else changes.
        case store(PendingKeyRotation)
        /// Already held under the same new key.
        case alreadyHeld
        /// Dropped, never rendered.
        case drop(DropReason)
    }

    public enum DropReason: Equatable {
        /// The envelope is meaningful only in the account's own DeviceGroup.
        case notOwnDeviceGroup
        /// `verifyKeyRotationEnvelope` refused it (the proof is not by the
        /// account key this device holds, did not verify, has expired, names
        /// the held key itself, or the entropy does not derive the proof's
        /// key).
        case notVerified
        /// Too old to keep.
        case expired
    }

    /// A `KeyRotation` envelope was dispatched. `verified` is the outcome of
    /// `verifyKeyRotationEnvelope(plaintext, heldAccountPubkey, now)`, `nil`
    /// when it threw; it is only consulted for an own-DeviceGroup arrival.
    public static func receive(
        arrival: Arrival,
        verified: PendingKeyRotation?,
        held: [PendingKeyRotation],
        nowMs: UInt64,
        expired: (_ preparedAtMs: UInt64, _ nowMs: UInt64) -> Bool
    ) -> Receipt {
        guard arrival == .ownDeviceGroup else { return .drop(.notOwnDeviceGroup) }
        guard let verified else { return .drop(.notVerified) }
        if expired(verified.preparedAtMs, nowMs) { return .drop(.expired) }
        if held.contains(where: { $0.newSignaturePubkey == verified.newSignaturePubkey }) {
            return .alreadyHeld
        }
        return .store(verified)
    }

    /// `held` without the pending envelopes that are too old to keep (14
    /// days, the crate's constant through `expired`).
    public static func pruned(
        _ held: [PendingKeyRotation],
        nowMs: UInt64,
        expired: (_ preparedAtMs: UInt64, _ nowMs: UInt64) -> Bool
    ) -> [PendingKeyRotation] {
        held.filter { !expired($0.preparedAtMs, nowMs) }
    }

    /// The pending envelope whose key the published identity bundle carries,
    /// or `nil`. The landing rule's "other keys at a higher generation" is a
    /// rotation exactly when this finds one.
    public static func adoptable(
        for publishedSignaturePubkey: Data?, among held: [PendingKeyRotation]
    ) -> PendingKeyRotation? {
        guard let publishedSignaturePubkey else { return nil }
        return held.first { $0.newSignaturePubkey == publishedSignaturePubkey }
    }

    /// What a device does when it learns its session is of an old key
    /// generation.
    public enum SupersededVerdict: Equatable {
        /// Try the session upgrade with these pending envelopes, in this
        /// order. The server decides: the statement verifies only under the
        /// key that is the account's.
        case tryAdopting([PendingKeyRotation])
        /// No pending envelope is held: read the DeviceGroup journal to its
        /// head, ONCE, before deciding anything (the DS does not use session
        /// tokens, so the device still can).
        case readJournalFirst
        /// Nothing to adopt: this is a reset as far as this device can tell.
        /// The staged wipe and "Was it you?" (ADR-0066 section 1.1).
        case treatAsReset
    }

    /// - Parameters:
    ///   - held: the pending envelopes (already pruned).
    ///   - publishedSignaturePubkey: the key the directory publishes, when it
    ///     could be read; it orders the candidates, it never removes one.
    ///   - journalRead: the one journal read already ran for this signal.
    public static func onSuperseded(
        held: [PendingKeyRotation],
        publishedSignaturePubkey: Data?,
        journalRead: Bool
    ) -> SupersededVerdict {
        if held.isEmpty {
            return journalRead ? .treatAsReset : .readJournalFirst
        }
        let first = held.filter { $0.newSignaturePubkey == publishedSignaturePubkey }
        let rest = held.filter { $0.newSignaturePubkey != publishedSignaturePubkey }
            .sorted { $0.preparedAtMs > $1.preparedAtMs }
        return .tryAdopting(first + rest)
    }

    /// What one upgrade attempt leads to.
    public enum AfterUpgrade: Equatable {
        /// The pair is at the new generation: swap the seed and the phrase,
        /// keep every local store, keep the old key as predecessor.
        case adopt
        /// This envelope's key is not the account's: try the next one; when
        /// none is left, the change is a reset to this device.
        case tryNext
        /// This device was the one removed: the full wipe.
        case wipeRevoked
        /// The refresh credential is dead: sign in again; nothing is wiped.
        case signInAgain
        /// No verdict (offline, a limit, a clock still refused): keep
        /// everything, stay in "Updating keys...", try again later. NEVER a
        /// wipe.
        case retryLater
    }

    public static func afterUpgrade(_ answer: SessionUpgradeAnswer) -> AfterUpgrade {
        switch answer {
        case .upgraded: return .adopt
        case .notAccepted: return .tryNext
        case .deviceRevoked: return .wipeRevoked
        case .refreshInvalid: return .signInAgain
        case .stale, .offline, .failed: return .retryLater
        }
    }

    /// The end of a run over every candidate.
    public enum Outcome: Equatable {
        case adopted
        case treatAsReset
        case wipeRevoked
        case signInAgain
        case retryLater
    }

    /// Fold the per-candidate answers, in order, into the run's outcome:
    /// the first verdict that is not "try the next one" wins; when every
    /// candidate was refused, the change is a reset to this device.
    public static func outcome(of answers: [SessionUpgradeAnswer]) -> Outcome {
        for answer in answers {
            switch afterUpgrade(answer) {
            case .adopt: return .adopted
            case .tryNext: continue
            case .wipeRevoked: return .wipeRevoked
            case .signInAgain: return .signInAgain
            case .retryLater: return .retryLater
            }
        }
        return .treatAsReset
    }
}

/// `POST /auth/session/refresh` with a `key_statement_b64`, mapped (ADR-0066
/// "T5 as built" items 15 to 17).
public enum SessionUpgradeAnswer: Equatable {
    /// A pair at the current generation, naming the same device.
    case upgraded
    /// `401 key-generation-superseded`: the statement was not accepted.
    case notAccepted
    /// `401 device-revoked`: the statement verified and this device's floor
    /// says "removed".
    case deviceRevoked
    /// `401 refresh-invalid`.
    case refreshInvalid
    /// `401 statement-stale` again, after the one retry at the server's time.
    case stale
    case offline
    case failed

    public static func of(_ error: Error?) -> SessionUpgradeAnswer {
        guard let error else { return .upgraded }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return of(status: status, body: body)
        case .network: return .offline
        default: return .failed
        }
    }

    public static func of(status: Int, body: String) -> SessionUpgradeAnswer {
        if (200..<300).contains(status) { return .upgraded }
        guard status == 401 else { return .failed }
        let code = ((try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any])?["error"]
            as? String
        switch code {
        case "key-generation-superseded": return .notAccepted
        case "device-revoked": return .deviceRevoked
        case "refresh-invalid": return .refreshInvalid
        case StatementStale.code: return .stale
        default: return .failed
        }
    }
}

// MARK: - The "keys were changed" line (UX 18.3)

/// Whether a kept device still owes the person the one dismissible line
/// "Your account keys were changed on another device. This device has the new
/// ones." Per account, in `UserDefaults`: a flag, no key material.
public struct KeysRotatedNoticeStore {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        "lakko.keys-rotated-notice.v1.\(accountIdB64)"
    }

    public func isOwed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.key(accountIdB64: accountIdB64))
    }

    public func markOwed(accountIdB64: String) {
        defaults.set(true, forKey: Self.key(accountIdB64: accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

/// The catalog keys of the kept device's surfaces (UX 18.3, 18.6).
public enum KeysRotatedCopy {
    public static let notice = "devices.rotated.notice"
    public static let showPhrase = "devices.rotated.show_phrase"
    public static let updating = "devices.rotated.updating"

    public static let all: [String] = [notice, showPhrase, updating]
}

public extension Notification.Name {
    /// The account's keys were rotated and this device holds the new ones;
    /// what it re-keyed outside the auth repository has run. The session
    /// shell lets its per-session state follow (the channels' key
    /// generation).
    static let lakkoAccountKeysRotated = Notification.Name("me.lakko.account-keys-rotated")
}

// MARK: - Keychain plumbing

/// One generic-password item holding a JSON blob: `AfterFirstUnlockThisDeviceOnly`
/// (device-bound, no iCloud Keychain, the hardware-backed class key), no
/// access group (the app's own).
enum KeychainJSONItem {

    static func read(service: String, account: String) -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }

    static func write(_ data: Data, service: String, account: String) throws {
        delete(service: service, account: account)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: data,
        ]
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else { throw KeychainJSONItemError.keychain(status) }
    }

    static func delete(service: String, account: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ]
        _ = SecItemDelete(query as CFDictionary)
    }
}

enum KeychainJSONItemError: Error, Equatable {
    case keychain(OSStatus)
}
