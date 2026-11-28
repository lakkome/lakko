// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 7 (settled 2026-11-26) — the list of the channels an
// account owns, and its locks.
//
// A channel's owner key comes from the account seed, so a key reset loses it
// and nobody else holds it. The account therefore keeps, on the storage
// service, a LIST of the channels it owns, sealed under a random list key
// `R`; each channel's recovery key comes from `R`. `R` is kept under:
//
//   * a PASSWORD lock, opened by the password's export key and a server half
//     `S` that auth releases only at sign-up and after the password plus a
//     second factor;
//   * a KEYS lock, opened by the account keys (the lineage seed), which also
//     carries `S`;
//   * one PASSKEY lock per passkey that keeps the chats, opened by that
//     passkey's own secret and `S`.
//
// Each is an ADR-0064 vault named by its root (`vaultKeysFromRoot`,
// `VaultRef.ownedChannels`); nothing names an account. `R` and `S` are kept
// in memory while used; `S` alone is also kept under the device's Keychain
// between sign-up and the moment the locks exist (a guest makes them at its
// claim), and removed then.
//
// This file is:
//
//   * `OwnedChannelsRules`   the pure part: the entries built from what this
//                            device holds, and what a pass does;
//   * `OwnedVaulting`        one vault by its root (live: `LiveOwnedVault`);
//   * `OwnedChannels`        the runtime: made at sign-up, opened with the
//                            keys, written on change, the locks re-made, the
//                            channels taken back after a reset, all of it
//                            deleted with the account.
//
// The codecs, the lock derivations and the merge are the core's
// (`lakko_channels::owned`).

// MARK: - Pure rules

enum OwnedChannelsRules {

    static let maxChannels = 64
    static let maxAdmissionSeeds = 8
    static let maxEpochSeeds = 4

    struct Admission: Equatable {
        let version: UInt32
        let seed: Data
    }

    struct Epoch: Equatable {
        let epoch: UInt32
        let seed: Data
    }

    /// What this device holds about one channel it owns.
    struct Held: Equatable {
        let channelId: Data
        /// The read key of a public channel.
        let readKey: Data?
        let admission: [Admission]
        let epochs: [Epoch]
        /// The channel's ADR-0087 handle write seed (list v2, 2026-11-27):
        /// the one its `@name`'s key is derived from now, so a device that
        /// retakes the channel after a reset signs `rekey` from it.
        var handleSeed: Data? = nil
    }

    /// The list entry of one channel: the read key, the newest eight
    /// admission seeds (each seed once) and the newest four epoch seeds
    /// (the ADR-0031 retention). `nil` for a malformed id.
    static func entry(_ held: Held) -> FfiOwnedChannel? {
        guard held.channelId.count == 16 else { return nil }
        var seenSeeds = Set<Data>()
        let admission = held.admission
            .filter { $0.seed.count == 32 && seenSeeds.insert($0.seed).inserted }
            .sorted {
                $0.version != $1.version
                    ? $0.version < $1.version
                    : [UInt8]($0.seed).lexicographicallyPrecedes([UInt8]($1.seed))
            }
            .suffix(maxAdmissionSeeds)
            .map { FfiOwnedAdmissionSeed(version: $0.version, seed: $0.seed) }
        var byEpoch: [UInt32: Data] = [:]
        for epoch in held.epochs where epoch.seed.count == 32 { byEpoch[epoch.epoch] = epoch.seed }
        let epochs = byEpoch.keys.sorted().suffix(maxEpochSeeds)
            .map { FfiOwnedEpochSeed(epochId: $0, seed: byEpoch[$0] ?? Data()) }
        return FfiOwnedChannel(
            channelId: held.channelId,
            readKey: held.readKey.flatMap { $0.count == 32 ? $0 : nil },
            handleSeed: held.handleSeed.flatMap { $0.count == 32 ? $0 : nil },
            admission: Array(admission), epochs: epochs)
    }

    /// The list this device wants to write: the lock roots of `current`
    /// kept, the channels replaced by what it holds. An account owning more
    /// than 64 channels lists the 64 with the newest epochs (the ones it
    /// rotated most recently).
    static func desired(current: FfiOwnedChannelsList, held: [Held]) -> FfiOwnedChannelsList {
        var entries = held.compactMap(entry)
        if entries.count > maxChannels {
            entries.sort { ($0.epochs.last?.epochId ?? 0) > ($1.epochs.last?.epochId ?? 0) }
            entries = Array(entries.prefix(maxChannels))
        }
        entries.sort { [UInt8]($0.channelId).lexicographicallyPrecedes([UInt8]($1.channelId)) }
        return FfiOwnedChannelsList(
            passwordLockRoot: current.passwordLockRoot, passkeyLocks: current.passkeyLocks, channels: entries)
    }

    /// The channels `current` lists that this device no longer owns: what a
    /// merge after a conflict must drop.
    static func deleted(current: FfiOwnedChannelsList, held: [Held]) -> [Data] {
        let owned = Set(held.map(\.channelId))
        return current.channels.map(\.channelId).filter { !owned.contains($0) }
    }

    /// Two lists that say the same, whatever the order of their parts.
    static func same(_ a: FfiOwnedChannelsList, _ b: FfiOwnedChannelsList) -> Bool {
        (try? encodeOwnedChannelsList(list: a)) == (try? encodeOwnedChannelsList(list: b))
    }

    /// The list with the passkey lock of `tag` set to `root` (`nil`
    /// removes it).
    static func settingPasskeyLock(
        _ list: FfiOwnedChannelsList, tag: Data, root: Data?
    ) -> FfiOwnedChannelsList {
        var locks = list.passkeyLocks.filter { $0.tag != tag }
        if let root, locks.count < 8 { locks.append(FfiOwnedPasskeyLock(tag: tag, root: root)) }
        return FfiOwnedChannelsList(
            passwordLockRoot: list.passwordLockRoot, passkeyLocks: locks, channels: list.channels)
    }

    /// The list with its password lock root replaced.
    static func settingPasswordLock(_ list: FfiOwnedChannelsList, root: Data) -> FfiOwnedChannelsList {
        FfiOwnedChannelsList(passwordLockRoot: root, passkeyLocks: list.passkeyLocks, channels: list.channels)
    }

    /// What sealed a reset decides which lock the resetting device opens
    /// (section 7 item 8).
    enum ResetLock: Equatable {
        /// The password lock: `lock_pw(export_key, S)`.
        case password(exportKey: Data)
        /// The passkey lock of the passkey whose secret signed the seal.
        case passkey(prfOutput: Data)
        /// Alignment item B (ADR-0101 section 7 item 8, ADR-0074): the OLD
        /// keys lock, opened by a recall device that held the account's old
        /// seed. Its root was taken before the staged wipe; the lock carries
        /// `R` and `S` itself.
        case keys(root: Data)
    }

    /// The root of the lock to open. `serverHalf` is needed for the password
    /// and passkey locks only.
    static func lockRoot(_ lock: ResetLock, serverHalf: Data?) -> Data? {
        switch lock {
        case .password(let exportKey):
            return serverHalf.flatMap { try? ownedPasswordLockRoot(exportKey: exportKey, serverHalf: $0) }
        case .passkey(let prfOutput):
            return serverHalf.flatMap { try? ownedPasskeyLockRoot(prfOutput: prfOutput, serverHalf: $0) }
        case .keys(let root):
            return root.count == 32 ? root : nil
        }
    }

    /// Alignment item M (the web rule): which subscriber rows of a retaken
    /// channel stay readers. A private channel whose entry lists admission
    /// seeds is judged row by row by its tickets under those seeds (a reader
    /// let in before the channel went private, holding no ticket, is
    /// removed by the admission pass); otherwise every row the service lists
    /// stays (the entry carries nothing to judge a row with).
    enum RetakenAdmission: Equatable {
        case tickets
        case everyoneListed
    }

    static func retakenAdmission(_ entry: FfiOwnedChannel) -> RetakenAdmission {
        if entry.readKey != nil { return .everyoneListed }
        return entry.admission.isEmpty ? .everyoneListed : .tickets
    }
}

// MARK: - One vault by its root

/// One owned-channels vault: the list or one of its locks.
protocol OwnedVaulting {
    /// The head and its version; `nil` when there is none (never claimed,
    /// or claimed and empty).
    func read() async throws -> (version: UInt64, plaintext: Data)?
    /// Write the head at `version` (current + 1). ``VaultClientError/conflict``
    /// when another device wrote meanwhile.
    func write(version: UInt64, plaintext: Data) async throws
    /// Claim the vault under its write key (one `vault-claim` token; the
    /// same key again succeeds).
    func claim() async throws
    func delete() async throws
}

struct LiveOwnedVault: OwnedVaulting {
    let keys: VaultKeys
    let client: VaultClient

    init(root: Data, client: VaultClient = VaultClient()) throws {
        self.keys = try vaultKeysFromRoot(root: root)
        self.client = client
    }

    private func idHex() throws -> String { hexEncode(try keys.vaultId(vault: .ownedChannels)) }

    func read() async throws -> (version: UInt64, plaintext: Data)? {
        let hex = try idHex()
        let proof = try keys.signRequest(
            vault: .ownedChannels, method: "GET", path: VaultClient.headPath(hex), body: Data())
        guard let object = try await client.getHead(vaultIdHex: hex, proof: proof) else { return nil }
        return (object.version, try keys.openHead(vault: .ownedChannels, version: object.version, sealed: object.sealed))
    }

    func write(version: UInt64, plaintext: Data) async throws {
        let hex = try idHex()
        let sealed = try keys.sealHead(vault: .ownedChannels, version: version, plaintext: plaintext)
        let body = VaultClient.objectBody(version: version, sealed: sealed)
        let proof = try keys.signRequest(
            vault: .ownedChannels, method: "PUT", path: VaultClient.headPath(hex), body: body)
        try await client.putHead(vaultIdHex: hex, body: body, proof: proof)
    }

    func claim() async throws {
        try await client.claim(vaultIdHex: try idHex(), writePublicKey: try keys.writePublicKey(vault: .ownedChannels))
    }

    func delete() async throws {
        let hex = try idHex()
        let proof = try keys.signRequest(
            vault: .ownedChannels, method: "DELETE", path: VaultClient.vaultPath(hex), body: Data())
        try await client.deleteVault(vaultIdHex: hex, proof: proof)
    }
}

/// The server half `S`, kept under the device's Keychain only between the
/// sign-up that released it and the moment the locks exist (a guest makes
/// them at its claim, which may be days later). Removed once they do, and by
/// the account wipe.
enum OwnedChannelsPendingSecret {
    static let service = "me.lakko.owned-channels-secret.v1"

    static func put(_ secret: Data, accountIdB64: String) {
        guard secret.count == 32 else { return }
        delete(accountIdB64: accountIdB64)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, kSecValueData: secret,
        ]
        SecItemAdd(query as CFDictionary, nil)
    }

    static func get(accountIdB64: String) -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecReturnData: true, kSecMatchLimit: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess,
              let data = out as? Data, data.count == 32 else { return nil }
        return data
    }

    static func delete(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// A reset's carry (``OwnedChannels/ResetCarry``) under the device's
/// Keychain, from the reset until the owned channels were taken back.
enum OwnedChannelsResetCarryStore {
    static let service = "me.lakko.owned-channels-reset-carry.v1"

    private struct Stored: Codable {
        let serverHalf: Data?
        /// `"password"`, `"passkey"` or `"keys"` (a recall).
        let kind: String
        /// The export key (password), the passkey's secret (passkey) or the
        /// old keys lock's root (keys).
        let secret: Data
        let exportKey: Data?
    }

    static func put(_ carry: OwnedChannels.ResetCarry, accountIdB64: String) {
        let stored: Stored
        switch carry.lock {
        case .password(let exportKey):
            stored = Stored(serverHalf: carry.serverHalf, kind: "password", secret: exportKey, exportKey: carry.exportKey)
        case .passkey(let prfOutput):
            stored = Stored(serverHalf: carry.serverHalf, kind: "passkey", secret: prfOutput, exportKey: carry.exportKey)
        case .keys(let root):
            stored = Stored(serverHalf: carry.serverHalf, kind: "keys", secret: root, exportKey: carry.exportKey)
        }
        guard let data = try? JSONEncoder().encode(stored) else { return }
        delete(accountIdB64: accountIdB64)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, kSecValueData: data,
        ]
        SecItemAdd(query as CFDictionary, nil)
    }

    static func get(accountIdB64: String) -> OwnedChannels.ResetCarry? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecReturnData: true, kSecMatchLimit: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data,
              let stored = try? JSONDecoder().decode(Stored.self, from: data),
              stored.serverHalf == nil || stored.serverHalf?.count == 32 else {
            return nil
        }
        let lock: OwnedChannelsRules.ResetLock
        switch stored.kind {
        case "password": lock = .password(exportKey: stored.secret)
        case "passkey": lock = .passkey(prfOutput: stored.secret)
        case "keys": lock = .keys(root: stored.secret)
        default: return nil
        }
        return OwnedChannels.ResetCarry(serverHalf: stored.serverHalf, lock: lock, exportKey: stored.exportKey)
    }

    static func delete(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// One JSON value per account under the device's Keychain (the owed steps
/// below carry secrets: a handle write seed, a passkey's secret).
enum OwnedKeychainJSON {
    static func put<T: Encodable>(_ value: T, service: String, accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(value) else { return }
        delete(service: service, accountIdB64: accountIdB64)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, kSecValueData: data,
        ]
        SecItemAdd(query as CFDictionary, nil)
    }

    static func get<T: Decodable>(_ type: T.Type, service: String, accountIdB64: String) -> T? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecReturnData: true, kSecMatchLimit: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else {
            return nil
        }
        return try? JSONDecoder().decode(type, from: data)
    }

    static func delete(service: String, accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// Alignment item C — the owed steps of the owned channels, each kept until
/// it succeeded (a crash or a failed write never drops one):
///
///   * the retake's progress: the channels already taken back (or dropped)
///     after a reset or a recall, so a retake that continues after a
///     relaunch asks for no channel twice;
///   * the `@name` rekeys owed after a retake (item H): the channel id and
///     the handle write seed the list carried;
///   * the passkey locks to make (the passkey's secret) and to delete.
enum OwnedChannelsOwed {
    static let retakeService = "me.lakko.owned-channels-retake.v1"
    static let handleService = "me.lakko.owned-channels-handle-rekey.v1"
    static let passkeyService = "me.lakko.owned-channels-passkey-locks.v1"

    struct HandleRekey: Codable, Equatable {
        let channelIdHex: String
        /// The handle write seed the list carried; `nil` once the rekey
        /// landed (the channel's record is still to be re-signed).
        var oldHandleSeed: Data?
    }

    struct PasskeyLocks: Codable, Equatable {
        struct Add: Codable, Equatable {
            let credentialId: Data
            let prfOutput: Data
        }
        var add: [Add] = []
        var remove: [Data] = []
        var isEmpty: Bool { add.isEmpty && remove.isEmpty }
    }

    // The retake's progress.
    static func retakeDone(accountIdB64: String) -> Set<String>? {
        OwnedKeychainJSON.get([String].self, service: retakeService, accountIdB64: accountIdB64).map(Set.init)
    }

    static func startRetake(accountIdB64: String) {
        if retakeDone(accountIdB64: accountIdB64) == nil {
            OwnedKeychainJSON.put([String](), service: retakeService, accountIdB64: accountIdB64)
        }
    }

    static func noteRetakeDone(_ idHex: String, accountIdB64: String) {
        var done = retakeDone(accountIdB64: accountIdB64) ?? []
        done.insert(idHex)
        OwnedKeychainJSON.put(done.sorted(), service: retakeService, accountIdB64: accountIdB64)
    }

    static func finishRetake(accountIdB64: String) {
        OwnedKeychainJSON.delete(service: retakeService, accountIdB64: accountIdB64)
    }

    // The handle rekeys.
    static func handleRekeys(accountIdB64: String) -> [HandleRekey] {
        OwnedKeychainJSON.get([HandleRekey].self, service: handleService, accountIdB64: accountIdB64) ?? []
    }

    static func setHandleRekeys(_ entries: [HandleRekey], accountIdB64: String) {
        if entries.isEmpty {
            OwnedKeychainJSON.delete(service: handleService, accountIdB64: accountIdB64)
        } else {
            OwnedKeychainJSON.put(entries, service: handleService, accountIdB64: accountIdB64)
        }
    }

    static func oweHandleRekey(channelIdHex: String, oldHandleSeed: Data, accountIdB64: String) {
        guard oldHandleSeed.count == 32 else { return }
        var all = handleRekeys(accountIdB64: accountIdB64).filter { $0.channelIdHex != channelIdHex }
        all.append(HandleRekey(channelIdHex: channelIdHex, oldHandleSeed: oldHandleSeed))
        setHandleRekeys(all, accountIdB64: accountIdB64)
    }

    // The passkey locks.
    static func passkeyLocks(accountIdB64: String) -> PasskeyLocks {
        OwnedKeychainJSON.get(PasskeyLocks.self, service: passkeyService, accountIdB64: accountIdB64)
            ?? PasskeyLocks()
    }

    static func setPasskeyLocks(_ owed: PasskeyLocks, accountIdB64: String) {
        if owed.isEmpty {
            OwnedKeychainJSON.delete(service: passkeyService, accountIdB64: accountIdB64)
        } else {
            OwnedKeychainJSON.put(owed, service: passkeyService, accountIdB64: accountIdB64)
        }
    }

    static func deleteAll(accountIdB64: String) {
        for service in [retakeService, handleService, passkeyService] {
            OwnedKeychainJSON.delete(service: service, accountIdB64: accountIdB64)
        }
    }
}

/// Alignment item H — what an owed `@name` rekey does with a retaken
/// channel, from what this device holds of its metadata.
enum HandleRekeyStep: Equatable {
    /// No metadata record read yet: the name is not known. Tried again.
    case wait
    /// The record names no `@name`: nothing is owed.
    case drop
    /// Move `handle` to the key the new root derives (`isPublic` its type).
    case rekey(handle: String, isPublic: Bool)

    static func decide(metaKnown: Bool, handle: String?, isPublic: Bool) -> HandleRekeyStep {
        guard metaKnown else { return .wait }
        guard let handle, !handle.isEmpty else { return .drop }
        return .rekey(handle: handle, isPublic: isPublic)
    }

    /// A refusal that ends the rekey: `404` (no row: the name lapsed) and
    /// `401` (another key's row: the name is not this channel's any more).
    static func isFinal(_ error: Error) -> Bool {
        guard let refusal = error as? DiscoveryClient.HandleWriteError else { return false }
        return refusal.status == 404 || refusal.status == 401
    }
}

// MARK: - The runtime

@MainActor
final class OwnedChannels {

    static let shared = OwnedChannels()

    /// `R` and `S`, in memory for the session of one account.
    struct Keys: Equatable {
        let listKey: Data
        let serverHalf: Data
    }

    struct Ports {
        var vault: (_ root: Data) throws -> OwnedVaulting = { try LiveOwnedVault(root: $0) }
        /// The lineage identity the keys lock is derived from: the oldest
        /// own predecessor, else the current account identity.
        var lineage: (_ accountIdB64: String, _ currentSeed: Data) -> Identity? = { accountIdB64, seed in
            DeviceGroupLifecycle.lineageSeed(accountIdB64: accountIdB64, currentSeed: seed)
                .flatMap { try? Identity.fromSeed(seed: $0) }
        }
        /// Alignment item N: only an export key a password produced in this
        /// run (``RunExportKey``), never the cached one.
        var exportKey: @MainActor (_ accountIdB64: String) -> Data? = { RunExportKey.get(accountIdB64: $0) }
        var pendingSecret: (_ accountIdB64: String) -> Data? = { OwnedChannelsPendingSecret.get(accountIdB64: $0) }
        var clearPendingSecret: (_ accountIdB64: String) -> Void = {
            OwnedChannelsPendingSecret.delete(accountIdB64: $0)
        }
        var resetCarry: (_ accountIdB64: String) -> ResetCarry? = { OwnedChannelsResetCarryStore.get(accountIdB64: $0) }
        var clearResetCarry: (_ accountIdB64: String) -> Void = {
            OwnedChannelsResetCarryStore.delete(accountIdB64: $0)
        }
        /// One retaken entry: the owner change with its recovery key and,
        /// on `204`, the channel put on this device.
        var recover: @MainActor (
            _ entry: FfiOwnedChannel, _ listKey: Data, _ channels: ChannelsUiState, _ client: ChannelsClient,
            _ seed: Data, _ accountIdB64: String
        ) async -> ChannelReturnRuntime.RecoveryOutcome = { entry, listKey, channels, client, seed, account in
            await ChannelReturnRuntime.recoverOwnedChannel(
                entry: entry, listKey: listKey, channels: channels, client: client, seed: seed,
                accountIdB64: account)
        }
        // The owed steps (item C), under the Keychain.
        var retakeDone: (_ accountIdB64: String) -> Set<String>? = { OwnedChannelsOwed.retakeDone(accountIdB64: $0) }
        var startRetake: (_ accountIdB64: String) -> Void = { OwnedChannelsOwed.startRetake(accountIdB64: $0) }
        var noteRetakeDone: (_ idHex: String, _ accountIdB64: String) -> Void = {
            OwnedChannelsOwed.noteRetakeDone($0, accountIdB64: $1)
        }
        var finishRetake: (_ accountIdB64: String) -> Void = { OwnedChannelsOwed.finishRetake(accountIdB64: $0) }
        var oweHandleRekey: (_ channelIdHex: String, _ seed: Data, _ accountIdB64: String) -> Void = {
            OwnedChannelsOwed.oweHandleRekey(channelIdHex: $0, oldHandleSeed: $1, accountIdB64: $2)
        }
        var passkeyLocks: (_ accountIdB64: String) -> OwnedChannelsOwed.PasskeyLocks = {
            OwnedChannelsOwed.passkeyLocks(accountIdB64: $0)
        }
        var setPasskeyLocks: (_ owed: OwnedChannelsOwed.PasskeyLocks, _ accountIdB64: String) -> Void = {
            OwnedChannelsOwed.setPasskeyLocks($0, accountIdB64: $1)
        }
    }

    var ports = Ports()
    private var cached: (accountIdB64: String, keys: Keys)?
    /// The encoded list this session last read or wrote, so an unchanged
    /// pass writes nothing.
    private var lastList: (accountIdB64: String, version: UInt64, list: FfiOwnedChannelsList)?
    /// One operation at a time: a pass and a lock change both read, change
    /// and write the list.
    private var chain: Task<Void, Never>?

    func reset() {
        cached = nil
        lastList = nil
    }

    private func serial<T>(_ work: @escaping @MainActor () async -> T) async -> T {
        let previous = chain
        let task = Task { @MainActor () -> T in
            await previous?.value
            return await work()
        }
        chain = Task { _ = await task.value }
        return await task.value
    }

    // MARK: Making the locks (sign-up, a guest's claim)

    /// Make the list and its password and keys locks: `R` is new, the
    /// password lock is under the export key and `S`, the keys lock under
    /// the lineage identity. Returns `false` when anything could not be
    /// written; `S` then stays pending and the next pass tries again.
    @discardableResult
    func create(accountIdB64: String, currentSeed: Data, exportKey: Data, serverHalf: Data) async -> Bool {
        await serial { [self] in
            guard let lineage = ports.lineage(accountIdB64, currentSeed),
                  let passwordRoot = try? ownedPasswordLockRoot(exportKey: exportKey, serverHalf: serverHalf)
            else { return false }
            let keysRoot = ownedKeysLockRoot(lineageIdentity: lineage)
            do {
                // A keys lock that already exists is this account's: its
                // list key stands (a second device, or a retry after the
                // first attempt wrote some of it).
                let keysVault = try ports.vault(keysRoot)
                var listKey = generateOwnedChannelsListKey()
                if let held = try await keysVault.read(),
                   let opened = try? decodeOwnedKeysLock(head: held.plaintext) {
                    listKey = opened.listKey
                } else {
                    try await keysVault.claim()
                    try await keysVault.write(
                        version: 1, plaintext: try encodeOwnedKeysLock(listKey: listKey, serverHalf: serverHalf))
                }
                try await put(root: passwordRoot, plaintext: try encodeOwnedListKeyLock(listKey: listKey))
                let listVault = try ports.vault(listKey)
                if try await listVault.read() == nil {
                    try await listVault.claim()
                    let list = FfiOwnedChannelsList(passwordLockRoot: passwordRoot, passkeyLocks: [], channels: [])
                    try await listVault.write(version: 1, plaintext: try encodeOwnedChannelsList(list: list))
                    lastList = (accountIdB64, 1, list)
                }
                cached = (accountIdB64, Keys(listKey: listKey, serverHalf: serverHalf))
                ports.clearPendingSecret(accountIdB64)
                return true
            } catch {
                NSLog("[owned-channels] the list and its locks were not made (%@)", Self.label(error))
                return false
            }
        }
    }

    /// Claim the vault of `root` and write `plaintext` as its head, whatever
    /// version it is at.
    private func put(root: Data, plaintext: Data) async throws {
        let vault = try ports.vault(root)
        try await vault.claim()
        let version = (try await vault.read()?.version ?? 0) + 1
        try await vault.write(version: version, plaintext: plaintext)
    }

    // MARK: Opening with the keys

    /// `R` and `S` from the keys lock; `nil` when this account has no keys
    /// lock the lineage identity opens (or it cannot be read now).
    private func open(accountIdB64: String, currentSeed: Data) async -> Keys? {
        if let cached, cached.accountIdB64 == accountIdB64 { return cached.keys }
        guard let lineage = ports.lineage(accountIdB64, currentSeed),
              let vault = try? ports.vault(ownedKeysLockRoot(lineageIdentity: lineage)),
              let head = try? await vault.read(),
              let opened = try? decodeOwnedKeysLock(head: head.plaintext) else { return nil }
        let keys = Keys(listKey: opened.listKey, serverHalf: opened.serverHalf)
        cached = (accountIdB64, keys)
        return keys
    }

    private func readList(listKey: Data) async throws -> (version: UInt64, list: FfiOwnedChannelsList)? {
        guard let head = try await ports.vault(listKey).read() else { return nil }
        return (head.version, try decodeOwnedChannelsList(bytes: head.plaintext))
    }

    /// Read, change and write the list; on a version conflict read again,
    /// merge (`deleted`: the channels this device dropped) and write once
    /// more. Returns the list as written, or `nil`.
    @discardableResult
    private func update(
        accountIdB64: String, listKey: Data, deleted: [Data] = [],
        _ change: (FfiOwnedChannelsList) -> FfiOwnedChannelsList
    ) async -> FfiOwnedChannelsList? {
        do {
            guard let held = try await readList(listKey: listKey) else { return nil }
            let mine = change(held.list)
            if OwnedChannelsRules.same(mine, held.list) {
                lastList = (accountIdB64, held.version, held.list)
                return held.list
            }
            let vault = try ports.vault(listKey)
            do {
                try await vault.write(version: held.version + 1, plaintext: try encodeOwnedChannelsList(list: mine))
                lastList = (accountIdB64, held.version + 1, mine)
                return mine
            } catch VaultClientError.conflict {
                guard let theirs = try await readList(listKey: listKey) else { return nil }
                let merged = try mergeOwnedChannelsLists(mine: mine, theirs: theirs.list, deleted: deleted)
                try await vault.write(
                    version: theirs.version + 1, plaintext: try encodeOwnedChannelsList(list: merged))
                lastList = (accountIdB64, theirs.version + 1, merged)
                return merged
            }
        } catch {
            NSLog("[owned-channels] the list was not written (%@)", Self.label(error))
            return nil
        }
    }

    // MARK: The pass (a device with the keys)

    /// The handle write seed of a channel this account owns: the one its
    /// `@name`'s key is derived from now (ADR-0087; `ChannelSettings`'s
    /// derivation). While an ADR-0074 rotation's rekey of that name is still
    /// owed, the name is still under the predecessor's key.
    static func currentHandleSeed(
        channelIdHex: String, channelId: Data, currentSeed: Data, accountIdB64: String
    ) -> Data? {
        guard let current = try? Identity.fromSeed(seed: currentSeed) else { return nil }
        let lineage = ChannelLineage.identity(accountIdB64: accountIdB64, currentSeed: currentSeed) ?? current
        guard let journal = try? publisherGroupIdFor(ownerIdentity: lineage, channelId: channelId) else { return nil }
        var signingSeed = current.ed25519Seed()
        let held = HeldHandleStore.load(accountIdB64).first {
            $0.kind == "channel" && $0.channelIdHex?.lowercased() == channelIdHex.lowercased()
        }
        if let fromB64 = held?.rekeyFromB64,
           let predecessor = KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64)
            .compactMap({ try? Identity.fromSeed(seed: $0) })
            .first(where: { $0.signaturePublicKey().base64EncodedString() == fromB64 }) {
            signingSeed = predecessor.ed25519Seed()
        }
        return try? ConversationHandles.seed(accountSeed: signingSeed, journalId: journal)
    }

    /// What this device holds about the channels it owns.
    static func held(
        channels: ChannelsUiState, currentSeed: Data? = nil, accountIdB64: String? = nil
    ) -> [OwnedChannelsRules.Held] {
        channels.keysByChannel.keys
            .filter { ChannelPublisherLoops.rotates(channelId: $0, channels: channels) }
            .compactMap { idHex -> OwnedChannelsRules.Held? in
                guard let channelId = ChannelsClient.hexToData(idHex) else { return nil }
                let extras = channels.extras(idHex)
                let isPublic = extras.recordPublic ?? extras.isPublic
                var handleSeed: Data?
                if let currentSeed, let accountIdB64 {
                    handleSeed = currentHandleSeed(
                        channelIdHex: idHex, channelId: channelId, currentSeed: currentSeed,
                        accountIdB64: accountIdB64)
                }
                return OwnedChannelsRules.Held(
                    channelId: channelId,
                    readKey: isPublic == true ? extras.readKey : nil,
                    admission: ChannelInvites.admissionSeeds(channelIdHex: idHex)
                        .map { OwnedChannelsRules.Admission(version: UInt32($0.version), seed: $0.seed) },
                    epochs: channels.readSeeds(channelId: idHex)
                        .map { OwnedChannelsRules.Epoch(epoch: $0.key, seed: $0.value) },
                    handleSeed: handleSeed)
            }
    }

    /// One pass, after the publisher pass: continue a retake, make the locks
    /// when the sign-up's `S` is still pending, make or delete the passkey
    /// locks still owed, write the list when what this device owns changed
    /// (a channel made or deleted, a rotation, an admission seed), register
    /// the recovery keys still owed, and move the `@name`s of retaken
    /// channels. Nothing is shown.
    func pass(channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String) async {
        // A key reset made on this device, or a recall: the owned channels
        // are taken back first (section 7 item 8). A lock that did not open
        // ends it; anything else unfinished keeps the carry for the next
        // pass (item C).
        if let carry = ports.resetCarry(accountIdB64) {
            switch await retake(carry: carry, channels: channels, client: client, seed: seed, accountIdB64: accountIdB64) {
            case .done, .lockDidNotOpen:
                ports.clearResetCarry(accountIdB64)
            case .unreachable:
                return
            }
        }
        if let pending = ports.pendingSecret(accountIdB64), let exportKey = ports.exportKey(accountIdB64) {
            await create(
                accountIdB64: accountIdB64, currentSeed: seed, exportKey: exportKey, serverHalf: pending)
        }
        await flushPasskeyLocks(accountIdB64: accountIdB64, currentSeed: seed)
        let keys = await serial { [self] in await open(accountIdB64: accountIdB64, currentSeed: seed) }
        guard let keys, AcceptedContactsStore.boundAccountIdB64 == accountIdB64 else { return }
        let held = Self.held(channels: channels, currentSeed: seed, accountIdB64: accountIdB64)
        await serial { [self] in
            // Nothing owned and nothing listed: nothing to read or write.
            if let last = lastList, last.accountIdB64 == accountIdB64,
               OwnedChannelsRules.same(OwnedChannelsRules.desired(current: last.list, held: held), last.list) {
                return
            }
            guard let current = try? await readList(listKey: keys.listKey) else { return }
            lastList = (accountIdB64, current.version, current.list)
            let deleted = OwnedChannelsRules.deleted(current: current.list, held: held)
            await update(accountIdB64: accountIdB64, listKey: keys.listKey, deleted: deleted) {
                OwnedChannelsRules.desired(current: $0, held: held)
            }
        }
        await ChannelReturnRuntime.registerOwedRecoveryKeys(
            channels: channels, client: client, seed: seed, accountIdB64: accountIdB64, listKey: keys.listKey)
        await OwnedHandleRekeys.flush(channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
    }

    // MARK: The password and passkey locks

    /// The password changed on a device that holds the keys (or such a
    /// device signed in with it): the password lock is made again under the
    /// new export key, its root recorded, and the old lock deleted. A lock
    /// that is already this one changes nothing.
    ///
    /// Returns `true` when the list names the lock of this export key
    /// afterwards; `false` when that could not be established now (no keys
    /// lock opened, the list not read or not written): the caller keeps the
    /// run's export key pending and asks again at the next landing.
    @discardableResult
    func passwordProven(accountIdB64: String, currentSeed: Data, exportKey: Data) async -> Bool {
        await serial { [self] in
            guard let keys = await open(accountIdB64: accountIdB64, currentSeed: currentSeed),
                  let root = try? ownedPasswordLockRoot(exportKey: exportKey, serverHalf: keys.serverHalf),
                  let current = try? await readList(listKey: keys.listKey) else { return false }
            guard current.list.passwordLockRoot != root else { return true }
            do {
                try await put(root: root, plaintext: try encodeOwnedListKeyLock(listKey: keys.listKey))
            } catch {
                NSLog("[owned-channels] the password lock was not made again (%@)", Self.label(error))
                return false
            }
            let old = current.list.passwordLockRoot
            guard await update(accountIdB64: accountIdB64, listKey: keys.listKey, {
                OwnedChannelsRules.settingPasswordLock($0, root: root)
            }) != nil else { return false }
            try? await ports.vault(old).delete()
            return true
        }
    }

    /// A passkey that keeps the chats was added: its lock is owed (item C:
    /// kept under the Keychain until written) and made now when it can be.
    func passkeyAdded(accountIdB64: String, currentSeed: Data, credentialId: Data, prfOutput: Data) async {
        var owed = ports.passkeyLocks(accountIdB64)
        owed.add.removeAll { $0.credentialId == credentialId }
        owed.remove.removeAll { $0 == credentialId }
        owed.add.append(.init(credentialId: credentialId, prfOutput: prfOutput))
        ports.setPasskeyLocks(owed, accountIdB64)
        await flushPasskeyLocks(accountIdB64: accountIdB64, currentSeed: currentSeed)
    }

    /// A passkey was removed: its lock's deletion is owed and done now when
    /// it can be.
    func passkeyRemoved(accountIdB64: String, currentSeed: Data, credentialId: Data) async {
        var owed = ports.passkeyLocks(accountIdB64)
        owed.add.removeAll { $0.credentialId == credentialId }
        if !owed.remove.contains(credentialId) { owed.remove.append(credentialId) }
        ports.setPasskeyLocks(owed, accountIdB64)
        await flushPasskeyLocks(accountIdB64: accountIdB64, currentSeed: currentSeed)
    }

    /// Make and delete the passkey locks still owed. A step that fails stays
    /// owed for the next pass.
    func flushPasskeyLocks(accountIdB64: String, currentSeed: Data) async {
        await serial { [self] in
            var owed = ports.passkeyLocks(accountIdB64)
            guard !owed.isEmpty,
                  let keys = await open(accountIdB64: accountIdB64, currentSeed: currentSeed) else { return }
            for add in owed.add {
                guard let root = try? ownedPasskeyLockRoot(prfOutput: add.prfOutput, serverHalf: keys.serverHalf)
                else {
                    owed.add.removeAll { $0 == add }
                    continue
                }
                do {
                    try await put(root: root, plaintext: try encodeOwnedListKeyLock(listKey: keys.listKey))
                } catch {
                    NSLog("[owned-channels] a passkey lock was not made; owed (%@)", Self.label(error))
                    continue
                }
                let tag = resetSealPasskeyTag(credentialId: add.credentialId)
                guard await update(accountIdB64: accountIdB64, listKey: keys.listKey, {
                    OwnedChannelsRules.settingPasskeyLock($0, tag: tag, root: root)
                }) != nil else { continue }
                owed.add.removeAll { $0 == add }
                ports.setPasskeyLocks(owed, accountIdB64)
            }
            for credentialId in owed.remove {
                guard let current = try? await readList(listKey: keys.listKey) else { break }
                let tag = resetSealPasskeyTag(credentialId: credentialId)
                if let lock = current.list.passkeyLocks.first(where: { $0.tag == tag }) {
                    guard await update(accountIdB64: accountIdB64, listKey: keys.listKey, {
                        OwnedChannelsRules.settingPasskeyLock($0, tag: tag, root: nil)
                    }) != nil else { continue }
                    do {
                        try await ports.vault(lock.root).delete()
                    } catch {
                        // The list no longer names it; the storage
                        // service's idle sweep takes an orphan.
                        NSLog("[owned-channels] a removed passkey's lock was not deleted (%@)", Self.label(error))
                    }
                }
                owed.remove.removeAll { $0 == credentialId }
                ports.setPasskeyLocks(owed, accountIdB64)
            }
            ports.setPasskeyLocks(owed, accountIdB64)
        }
    }

    // MARK: After a key reset or a recall (section 7 item 8)

    /// What the device kept from before the reset's `finish` (or the
    /// recall's): `S` and what opens the list. Under the device's Keychain
    /// (``OwnedChannelsResetCarryStore``) from before the server step until
    /// the channels were taken back, since `S` cannot be asked for again
    /// once the reset spent its grant.
    struct ResetCarry: Equatable {
        /// `S`; `nil` for a recall, whose old keys lock carries it.
        let serverHalf: Data?
        let lock: OwnedChannelsRules.ResetLock
        /// The export key of the password just proven or just set, for the
        /// password lock made again when it is not the one the list names.
        let exportKey: Data?
    }

    /// Take the owned channels back with the new account seed (the web
    /// order): the new keys lock first (an earlier attempt's opens again),
    /// else open the lock of what sealed the reset and make the new keys
    /// lock under the new lineage identity; make the password lock again
    /// under the password just proven or set when the list names another;
    /// drop every passkey lock (the reset deleted the passkeys); then for
    /// each entry not taken back yet sign an owner change with its recovery
    /// key (a refusal drops the entry). Every step is recorded so a retake
    /// that continues after a relaunch repeats nothing (item C).
    enum Retake: Equatable {
        /// Every entry is settled; this many channels came back in this call.
        case done(Int)
        /// No lock under what sealed the reset (a password changed without
        /// the keys, or an account from before the list): the owned
        /// channels do not come back.
        case lockDidNotOpen
        /// Something could not be reached or written: tried again.
        case unreachable
    }

    @discardableResult
    func retake(
        carry: ResetCarry, channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async -> Retake {
        await serial { [self] in
            ports.startRetake(accountIdB64)
            guard let lineage = ports.lineage(accountIdB64, seed) else { return .unreachable }
            let newKeysRoot = ownedKeysLockRoot(lineageIdentity: lineage)
            var keys: Keys
            do {
                if let held = try await ports.vault(newKeysRoot).read(),
                   let opened = try? decodeOwnedKeysLock(head: held.plaintext) {
                    // An earlier attempt already made the new keys lock.
                    keys = Keys(listKey: opened.listKey, serverHalf: opened.serverHalf)
                } else {
                    guard let root = OwnedChannelsRules.lockRoot(carry.lock, serverHalf: carry.serverHalf),
                          let head = try await ports.vault(root).read() else {
                        NSLog("[owned-channels] the list did not open after the reset; no owned channel comes back")
                        ports.finishRetake(accountIdB64)
                        return .lockDidNotOpen
                    }
                    switch carry.lock {
                    case .keys:
                        guard let opened = try? decodeOwnedKeysLock(head: head.plaintext) else {
                            ports.finishRetake(accountIdB64)
                            return .lockDidNotOpen
                        }
                        keys = Keys(listKey: opened.listKey, serverHalf: opened.serverHalf)
                    case .password, .passkey:
                        guard let listKey = try? decodeOwnedListKeyLock(head: head.plaintext),
                              let serverHalf = carry.serverHalf else {
                            ports.finishRetake(accountIdB64)
                            return .lockDidNotOpen
                        }
                        keys = Keys(listKey: listKey, serverHalf: serverHalf)
                    }
                    try await put(
                        root: newKeysRoot,
                        plaintext: try encodeOwnedKeysLock(listKey: keys.listKey, serverHalf: keys.serverHalf))
                }
            } catch {
                NSLog("[owned-channels] the retake could not open or make its locks; tried again (%@)",
                      Self.label(error))
                return .unreachable
            }
            cached = (accountIdB64, keys)
            let listKey = keys.listKey
            guard let current = try? await readList(listKey: listKey) else { return .unreachable }

            // The password lock again, when the password just proven or set
            // is not the one the list names (a passkey-sealed reset, a
            // recall's new password).
            var passwordRoot = current.list.passwordLockRoot
            if let exportKey = carry.exportKey,
               let wanted = try? ownedPasswordLockRoot(exportKey: exportKey, serverHalf: keys.serverHalf),
               wanted != passwordRoot {
                do {
                    try await put(root: wanted, plaintext: try encodeOwnedListKeyLock(listKey: listKey))
                } catch {
                    return .unreachable
                }
                try? await ports.vault(passwordRoot).delete()
                passwordRoot = wanted
            }
            let passkeyLocks = current.list.passkeyLocks
            if passwordRoot != current.list.passwordLockRoot || !passkeyLocks.isEmpty {
                let root = passwordRoot
                guard await update(accountIdB64: accountIdB64, listKey: listKey, {
                    FfiOwnedChannelsList(passwordLockRoot: root, passkeyLocks: [], channels: $0.channels)
                }) != nil else { return .unreachable }
                for lock in passkeyLocks { try? await ports.vault(lock.root).delete() }
            }

            // The entries.
            let done = ports.retakeDone(accountIdB64) ?? []
            var dropped: [Data] = []
            var retry = false
            var back = 0
            for entry in current.list.channels {
                let idHex = ChannelsComposer.bytesToHex(entry.channelId).lowercased()
                guard !done.contains(idHex) else { continue }
                switch await ports.recover(entry, listKey, channels, client, seed, accountIdB64) {
                case .owned:
                    // Item H: the `@name` follows, owed until moved. Recorded
                    // before the entry counts as done.
                    if let handleSeed = entry.handleSeed, handleSeed.count == 32 {
                        ports.oweHandleRekey(idHex, handleSeed, accountIdB64)
                    }
                    ports.noteRetakeDone(idHex, accountIdB64)
                    back += 1
                case .refused:
                    ports.noteRetakeDone(idHex, accountIdB64)
                    dropped.append(entry.channelId)
                case .retry:
                    retry = true
                }
            }
            if !dropped.isEmpty {
                let gone = Set(dropped)
                guard await update(accountIdB64: accountIdB64, listKey: listKey, deleted: dropped, {
                    FfiOwnedChannelsList(
                        passwordLockRoot: $0.passwordLockRoot, passkeyLocks: $0.passkeyLocks,
                        channels: $0.channels.filter { !gone.contains($0.channelId) })
                }) != nil else { return .unreachable }
            }
            if retry { return .unreachable }
            ports.finishRetake(accountIdB64)
            return .done(back)
        }
    }

    // MARK: Account deletion (section 7 item 9)

    /// Delete the list and every lock, before `DELETE /account`. Best
    /// effort: a vault that cannot be reached is left to the storage
    /// service's idle sweep.
    func deleteAll(accountIdB64: String, currentSeed: Data) async {
        await serial { [self] in
            guard let keys = await open(accountIdB64: accountIdB64, currentSeed: currentSeed) else {
                ports.clearPendingSecret(accountIdB64)
                OwnedChannelsOwed.deleteAll(accountIdB64: accountIdB64)
                return
            }
            if let current = try? await readList(listKey: keys.listKey) {
                try? await ports.vault(current.list.passwordLockRoot).delete()
                for lock in current.list.passkeyLocks { try? await ports.vault(lock.root).delete() }
            }
            try? await ports.vault(keys.listKey).delete()
            if let lineage = ports.lineage(accountIdB64, currentSeed) {
                try? await ports.vault(ownedKeysLockRoot(lineageIdentity: lineage)).delete()
            }
            ports.clearPendingSecret(accountIdB64)
            OwnedChannelsOwed.deleteAll(accountIdB64: accountIdB64)
            cached = nil
            lastList = nil
        }
    }

    static func label(_ error: Error) -> String { String(describing: type(of: error)) }
}

// MARK: - Item H: the `@name`s of retaken channels

/// A retaken channel's `@name` is held by a handle key derived from the
/// account seed of before the reset; the list carried that handle write
/// seed, so this device signs the ADR-0087 `rekey` from it to the key its
/// new root derives (``ChannelSettings``'s derivation), then republishes the
/// channel's handle under the new key and re-signs the metadata record's
/// handle block. Owed under the Keychain (``OwnedChannelsOwed``) until done;
/// retried at every pass.
@MainActor
enum OwnedHandleRekeys {

    static func flush(channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String) async {
        let owed = OwnedChannelsOwed.handleRekeys(accountIdB64: accountIdB64)
        guard !owed.isEmpty, let current = try? Identity.fromSeed(seed: seed) else { return }
        var remaining = owed
        for entry in owed {
            let idHex = entry.channelIdHex
            guard let channelId = ChannelsClient.hexToData(idHex) else {
                remaining.removeAll { $0 == entry }
                continue
            }
            let meta = channels.sharedMeta(channelId: idHex)
            let step = HandleRekeyStep.decide(
                metaKnown: meta != nil, handle: meta?.handle?.handle, isPublic: meta?.handle?.public ?? false)
            switch step {
            case .wait:
                continue
            case .drop:
                remaining.removeAll { $0 == entry }
            case .rekey(let handle, let isPublic):
                let lineage = ChannelLineage.identity(accountIdB64: accountIdB64, currentSeed: seed) ?? current
                guard let journal = try? publisherGroupIdFor(ownerIdentity: lineage, channelId: channelId) else {
                    continue
                }
                var next = entry
                if let oldSeed = entry.oldHandleSeed {
                    do {
                        let written = try await ConversationHandles.rekey(
                            discovery: DiscoveryClient(), oldHandleSeed: oldSeed,
                            newAccountSeed: current.ed25519Seed(), journalId: journal, handle: handle)
                        HeldHandleStore.put(HeldHandleStore.Entry(
                            kind: "channel", handle: written.handle, isPublic: isPublic,
                            journalIdHex: AttachmentBridge.hex(journal), channelIdHex: idHex,
                            name: meta?.name ?? "", description: meta?.description ?? "",
                            leaseUntilMs: written.leaseUntilMs), accountIdB64)
                        next.oldHandleSeed = nil
                        if let at = remaining.firstIndex(of: entry) { remaining[at] = next }
                        OwnedChannelsOwed.setHandleRekeys(remaining, accountIdB64: accountIdB64)
                    } catch let error where HandleRekeyStep.isFinal(error) {
                        NSLog("[owned-channels] a retaken channel's name is not this channel's any more")
                        remaining.removeAll { $0 == entry }
                        continue
                    } catch {
                        NSLog("[owned-channels] a retaken channel's name was not moved yet; owed")
                        continue
                    }
                }
                // The name is under the new key: its row and the record's
                // handle block are written again under it.
                do {
                    guard let meta else { continue }
                    let value = try await ChannelSettings.writeHandle(
                        channelId: idHex, state: channels, currentSeed: seed,
                        change: ChannelSettings.TypeChange(
                            isPublic: isPublic, handle: handle, topic: meta.topic, language: meta.language))
                    let now = max(UInt64(Date().timeIntervalSince1970 * 1000), meta.updatedAtMs &+ 1)
                    try await ChannelMetaRecord.post(
                        state: channels, client: client, channelId: idHex,
                        meta: meta.with(updatedAtMs: now, handle: .some(value)), accountIdentity: current)
                    remaining.removeAll { $0 == next }
                } catch {
                    NSLog("[owned-channels] a retaken channel's name record was not re-signed yet; owed")
                }
            }
        }
        if remaining != owed { OwnedChannelsOwed.setHandleRekeys(remaining, accountIdB64: accountIdB64) }
    }
}
