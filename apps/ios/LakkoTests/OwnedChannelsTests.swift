// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0101 section 7 — the list of the channels an account owns and its
/// locks on this device: what a list entry holds, the lifecycle of the locks
/// (made at sign-up, the password lock re-made with the password, a passkey
/// lock following its passkey, all of it deleted with the account) and the
/// opening of the right lock after a key reset. The vaults are in-memory
/// fakes keyed by their root; the codecs and the lock derivations are the
/// core's, unmocked.
@MainActor
final class OwnedChannelsTests: XCTestCase {

    // MARK: - In-memory vaults

    private final class Storage {
        var vaults: [Data: (version: UInt64, plaintext: Data)] = [:]
        var claimed: Set<Data> = []
        var deleted: [Data] = []
        var offline = false
        /// Roots whose next write is refused as a version conflict, after
        /// "another device" wrote `plaintext` first.
        var conflictOnce: [Data: Data] = [:]
    }

    private struct Offline: Error {}

    private struct FakeVault: OwnedVaulting {
        let root: Data
        let storage: Storage

        func read() async throws -> (version: UInt64, plaintext: Data)? {
            if storage.offline { throw Offline() }
            return storage.vaults[root]
        }

        func write(version: UInt64, plaintext: Data) async throws {
            if storage.offline { throw Offline() }
            guard storage.claimed.contains(root) else { throw VaultClientError.unclaimed }
            if let theirs = storage.conflictOnce.removeValue(forKey: root) {
                storage.vaults[root] = ((storage.vaults[root]?.version ?? 0) + 1, theirs)
                throw VaultClientError.conflict
            }
            guard version == (storage.vaults[root]?.version ?? 0) + 1 else { throw VaultClientError.conflict }
            storage.vaults[root] = (version, plaintext)
        }

        func claim() async throws {
            if storage.offline { throw Offline() }
            storage.claimed.insert(root)
        }

        func delete() async throws {
            if storage.offline { throw Offline() }
            storage.vaults[root] = nil
            storage.claimed.remove(root)
            storage.deleted.append(root)
        }
    }

    private let account = "YWNjdC1vd25lZA=="
    private let exportKey = Data(repeating: 0x11, count: 64)
    private let newExportKey = Data(repeating: 0x12, count: 64)
    private let serverHalf = Data(repeating: 0x55, count: 32)
    private let credentialId = Data([4, 3, 2, 1])
    private let prf = Data(repeating: 0x33, count: 32)

    private final class Device {
        var pendingSecret: Data?
        var carry: OwnedChannels.ResetCarry?
        var lineage: Identity
        // The owed steps (item C), in memory.
        var retakeDone: Set<String>?
        var handleRekeys: [String: Data] = [:]
        var passkeyLocks = OwnedChannelsOwed.PasskeyLocks()
        /// What the channels service answers the owner change of an entry.
        var recover: (FfiOwnedChannel) -> ChannelReturnRuntime.RecoveryOutcome = { _ in .owned }
        var recovered: [Data] = []
        init(_ lineage: Identity) { self.lineage = lineage }
    }

    private func owned(_ storage: Storage, _ device: Device) -> OwnedChannels {
        let owned = OwnedChannels()
        owned.ports = OwnedChannels.Ports(
            vault: { FakeVault(root: $0, storage: storage) },
            lineage: { _, _ in device.lineage },
            exportKey: { _ in nil },
            pendingSecret: { _ in device.pendingSecret },
            clearPendingSecret: { _ in device.pendingSecret = nil },
            resetCarry: { _ in device.carry },
            clearResetCarry: { _ in device.carry = nil },
            recover: { entry, _, _, _, _, _ in
                device.recovered.append(entry.channelId)
                return device.recover(entry)
            },
            retakeDone: { _ in device.retakeDone },
            startRetake: { _ in if device.retakeDone == nil { device.retakeDone = [] } },
            noteRetakeDone: { id, _ in device.retakeDone = (device.retakeDone ?? []).union([id]) },
            finishRetake: { _ in device.retakeDone = nil },
            oweHandleRekey: { id, seed, _ in device.handleRekeys[id] = seed },
            passkeyLocks: { _ in device.passkeyLocks },
            setPasskeyLocks: { owed, _ in device.passkeyLocks = owed })
        return owned
    }

    private func identity() throws -> Identity {
        try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
    }

    private func list(_ storage: Storage, _ lineage: Identity) throws -> FfiOwnedChannelsList {
        let keys = try decodeOwnedKeysLock(
            head: try XCTUnwrap(storage.vaults[ownedKeysLockRoot(lineageIdentity: lineage)]).plaintext)
        return try decodeOwnedChannelsList(bytes: try XCTUnwrap(storage.vaults[keys.listKey]).plaintext)
    }

    // MARK: - The entries

    func testAnEntryKeepsTheReadKeyTheNewestEightAdmissionSeedsAndFourEpochSeeds() throws {
        let id = Data(repeating: 0xC1, count: 16)
        let seed = { (byte: UInt8) in Data(repeating: byte, count: 32) }
        let held = OwnedChannelsRules.Held(
            channelId: id, readKey: seed(0xAA),
            admission: (1...10).map { .init(version: UInt32($0), seed: seed(UInt8($0))) }
                + [.init(version: 10, seed: seed(10)), .init(version: 11, seed: Data([1, 2]))],
            epochs: (1...6).map { .init(epoch: UInt32($0), seed: seed(UInt8(0x40 + $0))) })
        let entry = try XCTUnwrap(OwnedChannelsRules.entry(held))
        XCTAssertEqual(entry.channelId, id)
        XCTAssertEqual(entry.readKey, seed(0xAA))
        XCTAssertEqual(entry.admission.map(\.version), Array(3...10), "the newest eight, each seed once")
        XCTAssertEqual(entry.epochs.map(\.epochId), [3, 4, 5, 6], "the ADR-0031 retention")
        // The entry is one the core's codec takes.
        let encoded = try encodeOwnedChannelsList(list: FfiOwnedChannelsList(
            passwordLockRoot: seed(1), passkeyLocks: [], channels: [entry]))
        XCTAssertEqual(try decodeOwnedChannelsList(bytes: encoded).channels, [entry])

        XCTAssertNil(OwnedChannelsRules.entry(
            .init(channelId: Data([1, 2, 3]), readKey: nil, admission: [], epochs: [])), "a malformed id")
        // A private channel has no read key; a key of the wrong length is none.
        XCTAssertNil(OwnedChannelsRules.entry(
            .init(channelId: id, readKey: Data([1]), admission: [], epochs: []))?.readKey)
    }

    func testTheDesiredListKeepsTheLocksAndListsWhatThisDeviceOwns() {
        let root = Data(repeating: 9, count: 32)
        let lock = FfiOwnedPasskeyLock(tag: Data(repeating: 1, count: 16), root: Data(repeating: 2, count: 32))
        let a = Data(repeating: 0xA0, count: 16), b = Data(repeating: 0xB0, count: 16)
        let gone = Data(repeating: 0xD0, count: 16)
        let current = FfiOwnedChannelsList(
            passwordLockRoot: root, passkeyLocks: [lock],
            channels: [FfiOwnedChannel(channelId: gone, readKey: nil, admission: [], epochs: [])])
        let held = [b, a].map { OwnedChannelsRules.Held(channelId: $0, readKey: nil, admission: [], epochs: []) }
        let desired = OwnedChannelsRules.desired(current: current, held: held)
        XCTAssertEqual(desired.passwordLockRoot, root)
        XCTAssertEqual(desired.passkeyLocks, [lock])
        XCTAssertEqual(desired.channels.map(\.channelId), [a, b], "sorted by id, whatever order they were held in")
        XCTAssertEqual(OwnedChannelsRules.deleted(current: current, held: held), [gone])
        XCTAssertTrue(OwnedChannelsRules.same(desired, OwnedChannelsRules.desired(current: desired, held: held)))
        XCTAssertFalse(OwnedChannelsRules.same(desired, current))

        // At most 64 channels: the ones rotated most recently.
        let many = (0..<70).map { index in
            OwnedChannelsRules.Held(
                channelId: Data([UInt8(index)] + [UInt8](repeating: 0, count: 15)), readKey: nil, admission: [],
                epochs: [.init(epoch: UInt32(index), seed: Data(repeating: 7, count: 32))])
        }
        let capped = OwnedChannelsRules.desired(current: current, held: many)
        XCTAssertEqual(capped.channels.count, OwnedChannelsRules.maxChannels)
        XCTAssertFalse(capped.channels.contains { $0.channelId.first == 0 }, "the oldest are left out")
        XCTAssertTrue(capped.channels.contains { $0.channelId.first == 69 })
    }

    func testThePasskeyLocksOfAListAreSetReplacedRemovedAndCappedAtEight() {
        let list = FfiOwnedChannelsList(passwordLockRoot: Data(repeating: 9, count: 32), passkeyLocks: [], channels: [])
        let tag = Data(repeating: 1, count: 16)
        let one = OwnedChannelsRules.settingPasskeyLock(list, tag: tag, root: Data(repeating: 2, count: 32))
        XCTAssertEqual(one.passkeyLocks.map(\.tag), [tag])
        let replaced = OwnedChannelsRules.settingPasskeyLock(one, tag: tag, root: Data(repeating: 3, count: 32))
        XCTAssertEqual(replaced.passkeyLocks.map(\.root), [Data(repeating: 3, count: 32)])
        XCTAssertTrue(OwnedChannelsRules.settingPasskeyLock(replaced, tag: tag, root: nil).passkeyLocks.isEmpty)
        var full = list
        for index in 0..<8 {
            full = OwnedChannelsRules.settingPasskeyLock(
                full, tag: Data(repeating: UInt8(0x10 + index), count: 16), root: Data(repeating: 5, count: 32))
        }
        XCTAssertEqual(
            OwnedChannelsRules.settingPasskeyLock(full, tag: tag, root: Data(repeating: 6, count: 32)).passkeyLocks.count,
            8)
        XCTAssertEqual(
            OwnedChannelsRules.settingPasswordLock(list, root: Data(repeating: 7, count: 32)).passwordLockRoot,
            Data(repeating: 7, count: 32))
    }

    /// What sealed a reset decides which lock opens: the two roots are the
    /// core's, and a wrong secret names another vault.
    func testTheLockOfWhatSealedTheReset() throws {
        XCTAssertEqual(
            OwnedChannelsRules.lockRoot(.password(exportKey: exportKey), serverHalf: serverHalf),
            try ownedPasswordLockRoot(exportKey: exportKey, serverHalf: serverHalf))
        XCTAssertEqual(
            OwnedChannelsRules.lockRoot(.passkey(prfOutput: prf), serverHalf: serverHalf),
            try ownedPasskeyLockRoot(prfOutput: prf, serverHalf: serverHalf))
        XCTAssertNotEqual(
            OwnedChannelsRules.lockRoot(.password(exportKey: newExportKey), serverHalf: serverHalf),
            OwnedChannelsRules.lockRoot(.password(exportKey: exportKey), serverHalf: serverHalf))
        // Without the server half the password alone opens nothing.
        XCTAssertNotEqual(
            OwnedChannelsRules.lockRoot(.password(exportKey: exportKey), serverHalf: Data(repeating: 0, count: 32)),
            OwnedChannelsRules.lockRoot(.password(exportKey: exportKey), serverHalf: serverHalf))
    }

    // MARK: - The lifecycle

    func testSignUpMakesTheListThePasswordLockAndTheKeysLock() async throws {
        let storage = Storage()
        let device = Device(try identity())
        device.pendingSecret = serverHalf
        let owned = owned(storage, device)

        let made = await owned.create(
            accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        XCTAssertTrue(made)
        XCTAssertNil(device.pendingSecret, "the server half is not kept once the locks exist")

        // The keys lock holds R and S; the password lock holds R; the list
        // names the password lock and no channel yet.
        let keysRoot = ownedKeysLockRoot(lineageIdentity: device.lineage)
        let keys = try decodeOwnedKeysLock(head: try XCTUnwrap(storage.vaults[keysRoot]).plaintext)
        XCTAssertEqual(keys.serverHalf, serverHalf)
        let passwordRoot = try ownedPasswordLockRoot(exportKey: exportKey, serverHalf: serverHalf)
        XCTAssertEqual(
            try decodeOwnedListKeyLock(head: try XCTUnwrap(storage.vaults[passwordRoot]).plaintext), keys.listKey)
        let list = try self.list(storage, device.lineage)
        XCTAssertEqual(list.passwordLockRoot, passwordRoot)
        XCTAssertTrue(list.channels.isEmpty)
        XCTAssertTrue(list.passkeyLocks.isEmpty)
        // Nothing stored names the account, and no vault holds a secret in
        // the clear that opens another by itself.
        XCTAssertEqual(storage.vaults.count, 3)

        // A second device of the account (or a retry) keeps the list key.
        let again = self.owned(storage, device)
        let second = await again.create(
            accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        XCTAssertTrue(second)
        XCTAssertEqual(
            try decodeOwnedKeysLock(head: try XCTUnwrap(storage.vaults[keysRoot]).plaintext).listKey, keys.listKey)
        XCTAssertEqual(storage.vaults.count, 3)
    }

    func testALockThatCouldNotBeWrittenLeavesTheServerHalfPendingForTheNextTry() async throws {
        let storage = Storage()
        storage.offline = true
        let device = Device(try identity())
        device.pendingSecret = serverHalf
        let owned = owned(storage, device)
        let made = await owned.create(
            accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        XCTAssertFalse(made)
        XCTAssertEqual(device.pendingSecret, serverHalf)
        XCTAssertTrue(storage.vaults.isEmpty)
    }

    func testAPasswordChangeMakesThePasswordLockAgainAndDeletesTheOldOne() async throws {
        let storage = Storage()
        let device = Device(try identity())
        let owned = owned(storage, device)
        await owned.create(accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        let oldRoot = try ownedPasswordLockRoot(exportKey: exportKey, serverHalf: serverHalf)
        let newRoot = try ownedPasswordLockRoot(exportKey: newExportKey, serverHalf: serverHalf)

        // The same password: nothing to do.
        let same = await owned.passwordProven(accountIdB64: account, currentSeed: Data(), exportKey: exportKey)
        XCTAssertTrue(same)
        XCTAssertTrue(storage.deleted.isEmpty)

        let changed = await owned.passwordProven(accountIdB64: account, currentSeed: Data(), exportKey: newExportKey)
        XCTAssertTrue(changed)
        XCTAssertEqual(try list(storage, device.lineage).passwordLockRoot, newRoot)
        XCTAssertNotNil(storage.vaults[newRoot])
        XCTAssertNil(storage.vaults[oldRoot], "the old password opens nothing any more")
        XCTAssertEqual(storage.deleted, [oldRoot])

        // A device that could not reach the storage service changed nothing.
        storage.offline = true
        let offline = await self.owned(storage, device).passwordProven(
            accountIdB64: account, currentSeed: Data(), exportKey: exportKey)
        XCTAssertFalse(offline)
    }

    func testAPasskeyLockFollowsItsPasskey() async throws {
        let storage = Storage()
        let device = Device(try identity())
        let owned = owned(storage, device)
        await owned.create(accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        let root = try ownedPasskeyLockRoot(prfOutput: prf, serverHalf: serverHalf)
        let tag = resetSealPasskeyTag(credentialId: credentialId)

        await owned.passkeyAdded(accountIdB64: account, currentSeed: Data(), credentialId: credentialId, prfOutput: prf)
        XCTAssertEqual(try list(storage, device.lineage).passkeyLocks, [FfiOwnedPasskeyLock(tag: tag, root: root)])
        let keys = try decodeOwnedKeysLock(
            head: try XCTUnwrap(storage.vaults[ownedKeysLockRoot(lineageIdentity: device.lineage)]).plaintext)
        XCTAssertEqual(try decodeOwnedListKeyLock(head: try XCTUnwrap(storage.vaults[root]).plaintext), keys.listKey)

        // Removing a passkey that has no lock changes nothing.
        await owned.passkeyRemoved(accountIdB64: account, currentSeed: Data(), credentialId: Data([9, 9]))
        XCTAssertEqual(try list(storage, device.lineage).passkeyLocks.count, 1)

        await owned.passkeyRemoved(accountIdB64: account, currentSeed: Data(), credentialId: credentialId)
        XCTAssertTrue(try list(storage, device.lineage).passkeyLocks.isEmpty)
        XCTAssertNil(storage.vaults[root])
        XCTAssertEqual(storage.deleted, [root])
    }

    /// Two devices of the account write the list at once: the second reads
    /// again, merges and writes once more; neither's change is lost.
    func testAConflictingWriteIsMergedAndNeitherChangeIsLost() async throws {
        let storage = Storage()
        let device = Device(try identity())
        let owned = owned(storage, device)
        await owned.create(accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        let keys = try decodeOwnedKeysLock(
            head: try XCTUnwrap(storage.vaults[ownedKeysLockRoot(lineageIdentity: device.lineage)]).plaintext)
        // "Another device" lists a channel just before this one's write.
        let channel = FfiOwnedChannel(
            channelId: Data(repeating: 0xC7, count: 16), readKey: nil, admission: [], epochs: [])
        let theirs = FfiOwnedChannelsList(
            passwordLockRoot: try ownedPasswordLockRoot(exportKey: exportKey, serverHalf: serverHalf),
            passkeyLocks: [], channels: [channel])
        storage.conflictOnce[keys.listKey] = try encodeOwnedChannelsList(list: theirs)

        await owned.passkeyAdded(accountIdB64: account, currentSeed: Data(), credentialId: credentialId, prfOutput: prf)

        let merged = try list(storage, device.lineage)
        XCTAssertEqual(merged.channels, [channel], "the other device's channel")
        XCTAssertEqual(merged.passkeyLocks.map(\.tag), [resetSealPasskeyTag(credentialId: credentialId)], "this one's lock")
    }

    func testDeletingTheAccountDeletesTheListAndEveryLock() async throws {
        let storage = Storage()
        let device = Device(try identity())
        device.pendingSecret = serverHalf
        let owned = owned(storage, device)
        await owned.create(accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        await owned.passkeyAdded(accountIdB64: account, currentSeed: Data(), credentialId: credentialId, prfOutput: prf)
        XCTAssertEqual(storage.vaults.count, 4)

        await owned.deleteAll(accountIdB64: account, currentSeed: Data())
        XCTAssertTrue(storage.vaults.isEmpty, "the list, the password lock, the passkey lock and the keys lock")
        XCTAssertNil(device.pendingSecret)
    }

    // MARK: - After a key reset

    private func resetWorld() async throws -> (Storage, Device, OwnedChannels, Data) {
        let storage = Storage()
        let device = Device(try identity())
        let owned = owned(storage, device)
        await owned.create(accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        await owned.passkeyAdded(accountIdB64: account, currentSeed: Data(), credentialId: credentialId, prfOutput: prf)
        let listKey = try decodeOwnedKeysLock(
            head: try XCTUnwrap(storage.vaults[ownedKeysLockRoot(lineageIdentity: device.lineage)]).plaintext).listKey
        return (storage, device, owned, listKey)
    }

    private let channels = ChannelsUiState()
    private let client = ChannelsClient(baseURL: "https://example.test/channels")

    /// The reset was sealed by the password: the password lock opens with
    /// the export key and S, the new account keys get a keys lock of their
    /// own, and the passkey locks go (the reset deleted the passkeys).
    func testAfterAPasswordSealedResetTheListOpensAndTheNewKeysGetTheirLock() async throws {
        let (storage, device, _, listKey) = try await resetWorld()
        let oldLineage = device.lineage
        let passkeyRoot = try ownedPasskeyLockRoot(prfOutput: prf, serverHalf: serverHalf)
        // The reset: a new account seed on this device, nothing cached.
        device.lineage = try identity()
        let after = owned(storage, device)

        let result = await after.retake(
            carry: .init(serverHalf: serverHalf, lock: .password(exportKey: exportKey), exportKey: exportKey),
            channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(result, .done(0), "an account that owns no channel")

        let newKeys = try decodeOwnedKeysLock(
            head: try XCTUnwrap(storage.vaults[ownedKeysLockRoot(lineageIdentity: device.lineage)]).plaintext)
        XCTAssertEqual(newKeys.listKey, listKey, "the same list, under the new keys")
        XCTAssertEqual(newKeys.serverHalf, serverHalf)
        XCTAssertNotNil(
            storage.vaults[ownedKeysLockRoot(lineageIdentity: oldLineage)],
            "the old keys lock is left: only the old seed opens it")
        XCTAssertTrue(try list(storage, device.lineage).passkeyLocks.isEmpty)
        XCTAssertNil(storage.vaults[passkeyRoot], "the passkey was deleted by the reset")
    }

    /// The reset was sealed by a passkey: its lock opens, and the password
    /// lock is made again under the password just proven when the list
    /// names another.
    func testAfterAPasskeySealedResetThePasswordLockIsMadeAgainUnderThePasswordJustProven() async throws {
        let (storage, device, _, _) = try await resetWorld()
        let oldPasswordRoot = try ownedPasswordLockRoot(exportKey: exportKey, serverHalf: serverHalf)
        let newPasswordRoot = try ownedPasswordLockRoot(exportKey: newExportKey, serverHalf: serverHalf)
        device.lineage = try identity()
        let after = owned(storage, device)

        let result = await after.retake(
            carry: .init(serverHalf: serverHalf, lock: .passkey(prfOutput: prf), exportKey: newExportKey),
            channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(result, .done(0))
        XCTAssertEqual(try list(storage, device.lineage).passwordLockRoot, newPasswordRoot)
        XCTAssertNotNil(storage.vaults[newPasswordRoot])
        XCTAssertNil(storage.vaults[oldPasswordRoot])
    }

    /// A password changed without the keys (a mailed code) is not the one
    /// the lock is under: nothing opens, and the owned channels do not come
    /// back. A storage service that cannot be reached is tried again.
    func testALockThatDoesNotOpenAndAStorageServiceThatIsOut() async throws {
        let (storage, device, _, _) = try await resetWorld()
        device.lineage = try identity()
        let after = owned(storage, device)
        let wrong = OwnedChannels.ResetCarry(
            serverHalf: serverHalf, lock: .password(exportKey: newExportKey), exportKey: newExportKey)
        let closed = await after.retake(
            carry: wrong, channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(closed, .lockDidNotOpen)
        XCTAssertNil(storage.vaults[ownedKeysLockRoot(lineageIdentity: device.lineage)])

        storage.offline = true
        let out = await after.retake(
            carry: .init(serverHalf: serverHalf, lock: .password(exportKey: exportKey), exportKey: exportKey),
            channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(out, .unreachable)
    }

    // MARK: - Alignment items H, M, C, B

    /// Item H: a list entry carries the channel's handle write seed (32 bytes
    /// or none), and the core's codec takes it.
    func testAnEntryCarriesTheHandleWriteSeed() throws {
        let id = Data(repeating: 0xC2, count: 16)
        let handleSeed = Data(repeating: 0x5A, count: 32)
        let entry = try XCTUnwrap(OwnedChannelsRules.entry(
            .init(channelId: id, readKey: nil, admission: [], epochs: [], handleSeed: handleSeed)))
        XCTAssertEqual(entry.handleSeed, handleSeed)
        let encoded = try encodeOwnedChannelsList(list: FfiOwnedChannelsList(
            passwordLockRoot: Data(repeating: 1, count: 32), passkeyLocks: [], channels: [entry]))
        XCTAssertEqual(encoded.count, Int(ownedChannelsListLen()), "always the one size")
        XCTAssertEqual(try decodeOwnedChannelsList(bytes: encoded).channels.first?.handleSeed, handleSeed)
        XCTAssertNil(OwnedChannelsRules.entry(
            .init(channelId: id, readKey: nil, admission: [], epochs: [], handleSeed: Data([1])))?.handleSeed)
    }

    /// Item H: what an owed rekey does with what this device holds.
    func testAnOwedHandleRekeyWaitsForTheRecordAndEndsOnAFinalRefusal() {
        XCTAssertEqual(HandleRekeyStep.decide(metaKnown: false, handle: nil, isPublic: false), .wait)
        XCTAssertEqual(HandleRekeyStep.decide(metaKnown: true, handle: nil, isPublic: false), .drop)
        XCTAssertEqual(HandleRekeyStep.decide(metaKnown: true, handle: "", isPublic: true), .drop)
        XCTAssertEqual(
            HandleRekeyStep.decide(metaKnown: true, handle: "news", isPublic: true),
            .rekey(handle: "news", isPublic: true))
        XCTAssertTrue(HandleRekeyStep.isFinal(DiscoveryClient.HandleWriteError(status: 404, code: "no-row")))
        XCTAssertTrue(HandleRekeyStep.isFinal(DiscoveryClient.HandleWriteError(status: 401, code: "wrong-key")))
        XCTAssertFalse(HandleRekeyStep.isFinal(DiscoveryClient.HandleWriteError(status: 429, code: "limited")))
        XCTAssertFalse(HandleRekeyStep.isFinal(URLError(.notConnectedToInternet)))
    }

    /// Item M (the web rule): a private channel whose entry lists admission
    /// seeds is judged by its tickets; everything else keeps every listed
    /// reader.
    func testARetakenPrivateChannelIsJudgedByItsTickets() {
        let id = Data(repeating: 0xC3, count: 16)
        let seed = FfiOwnedAdmissionSeed(version: 1, seed: Data(repeating: 7, count: 32))
        XCTAssertEqual(OwnedChannelsRules.retakenAdmission(
            FfiOwnedChannel(channelId: id, readKey: nil, admission: [seed], epochs: [])), .tickets)
        XCTAssertEqual(OwnedChannelsRules.retakenAdmission(
            FfiOwnedChannel(channelId: id, readKey: Data(repeating: 2, count: 32), admission: [seed], epochs: [])),
            .everyoneListed, "a public channel")
        XCTAssertEqual(OwnedChannelsRules.retakenAdmission(
            FfiOwnedChannel(channelId: id, readKey: nil, admission: [], epochs: [])),
            .everyoneListed, "nothing to judge a row with")
    }

    private func listWith(_ storage: Storage, _ listKey: Data, channels: [FfiOwnedChannel]) throws {
        let current = try decodeOwnedChannelsList(bytes: try XCTUnwrap(storage.vaults[listKey]).plaintext)
        storage.vaults[listKey] = (
            (storage.vaults[listKey]?.version ?? 0) + 1,
            try encodeOwnedChannelsList(list: FfiOwnedChannelsList(
                passwordLockRoot: current.passwordLockRoot, passkeyLocks: current.passkeyLocks,
                channels: channels)))
    }

    /// Item C: a retake records each channel as it settles, keeps the carry
    /// while one waits, continues after a relaunch without asking for a
    /// channel twice, drops a refused one from the list, and owes the
    /// `@name` rekey of each channel taken back (item H).
    func testARetakeContinuesAfterARelaunchAndOwesTheNameOfEachChannelBack() async throws {
        let (storage, device, _, listKey) = try await resetWorld()
        let a = Data(repeating: 0xA1, count: 16), b = Data(repeating: 0xB1, count: 16)
        let c = Data(repeating: 0xC1, count: 16)
        let handleSeed = Data(repeating: 0x5B, count: 32)
        try listWith(storage, listKey, channels: [
            FfiOwnedChannel(channelId: a, readKey: nil, handleSeed: handleSeed, admission: [], epochs: []),
            FfiOwnedChannel(channelId: b, readKey: nil, admission: [], epochs: []),
            FfiOwnedChannel(channelId: c, readKey: nil, admission: [], epochs: []),
        ])
        device.lineage = try identity()
        device.recover = { entry in
            switch entry.channelId {
            case a: return .owned
            case b: return .refused
            default: return .retry
            }
        }
        let carry = OwnedChannels.ResetCarry(
            serverHalf: serverHalf, lock: .password(exportKey: exportKey), exportKey: exportKey)
        let first = await owned(storage, device).retake(
            carry: carry, channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(first, .unreachable, "a channel waits: the carry stays")
        XCTAssertEqual(device.handleRekeys, [hexOf(a): handleSeed], "the name of the channel taken back is owed")
        XCTAssertEqual(device.retakeDone, [hexOf(a), hexOf(b)])
        XCTAssertEqual(try list(storage, device.lineage).channels.map(\.channelId), [a, c], "the refused one is dropped")

        // A relaunch: a new runtime; the new keys lock opens again and only
        // the waiting channel is asked for.
        device.recovered = []
        device.recover = { _ in .owned }
        let second = await owned(storage, device).retake(
            carry: carry, channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(second, .done(1))
        XCTAssertEqual(device.recovered, [c])
        XCTAssertNil(device.retakeDone, "the progress goes once everything settled")
    }

    private func hexOf(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    /// Item B: a recall device opens the list through the OLD keys lock (its
    /// root kept in the recall record), makes the keys lock of its new
    /// lineage, and makes the password lock again under the new password.
    func testARecallTakesTheOwnedChannelsBackThroughTheOldKeysLock() async throws {
        let (storage, device, _, listKey) = try await resetWorld()
        let oldKeysRoot = ownedKeysLockRoot(lineageIdentity: device.lineage)
        let channel = Data(repeating: 0xD1, count: 16)
        try listWith(storage, listKey, channels: [
            FfiOwnedChannel(channelId: channel, readKey: nil, admission: [], epochs: [])])
        device.lineage = try identity()
        let result = await owned(storage, device).retake(
            carry: .init(serverHalf: nil, lock: .keys(root: oldKeysRoot), exportKey: newExportKey),
            channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(result, .done(1))
        let newKeys = try decodeOwnedKeysLock(
            head: try XCTUnwrap(storage.vaults[ownedKeysLockRoot(lineageIdentity: device.lineage)]).plaintext)
        XCTAssertEqual(newKeys.listKey, listKey)
        XCTAssertEqual(newKeys.serverHalf, serverHalf, "S came from the old keys lock")
        XCTAssertEqual(
            try list(storage, device.lineage).passwordLockRoot,
            try ownedPasswordLockRoot(exportKey: newExportKey, serverHalf: serverHalf))
        XCTAssertEqual(device.recovered, [channel])

        // A keys root that opens nothing.
        let other = try await resetWorld()
        other.1.lineage = try identity()
        let closed = await owned(other.0, other.1).retake(
            carry: .init(serverHalf: nil, lock: .keys(root: Data(repeating: 3, count: 32)), exportKey: nil),
            channels: channels, client: client, seed: Data(), accountIdB64: account)
        XCTAssertEqual(closed, .lockDidNotOpen)
    }

    /// Item C: a passkey lock that could not be written stays owed and is
    /// made at the next pass.
    func testAPasskeyLockThatCouldNotBeWrittenIsMadeLater() async throws {
        let storage = Storage()
        let device = Device(try identity())
        let owned = owned(storage, device)
        await owned.create(accountIdB64: account, currentSeed: Data(), exportKey: exportKey, serverHalf: serverHalf)
        storage.offline = true
        await owned.passkeyAdded(accountIdB64: account, currentSeed: Data(), credentialId: credentialId, prfOutput: prf)
        XCTAssertEqual(device.passkeyLocks.add.map(\.credentialId), [credentialId], "owed")
        storage.offline = false
        await owned.flushPasskeyLocks(accountIdB64: account, currentSeed: Data())
        XCTAssertTrue(device.passkeyLocks.isEmpty)
        XCTAssertEqual(try list(storage, device.lineage).passkeyLocks.count, 1)
    }

    // MARK: - What the device keeps between the steps

    func testTheServerHalfAndTheResetCarryAreKeptUnderTheKeychainUntilUsed() {
        let id = "YWNjdC1rZXljaGFpbi1vd25lZA=="
        addTeardownBlock {
            OwnedChannelsPendingSecret.delete(accountIdB64: id)
            OwnedChannelsResetCarryStore.delete(accountIdB64: id)
        }
        XCTAssertNil(OwnedChannelsPendingSecret.get(accountIdB64: id))
        OwnedChannelsPendingSecret.put(Data([1, 2, 3]), accountIdB64: id)
        XCTAssertNil(OwnedChannelsPendingSecret.get(accountIdB64: id), "not a server half")
        OwnedChannelsPendingSecret.put(serverHalf, accountIdB64: id)
        XCTAssertEqual(OwnedChannelsPendingSecret.get(accountIdB64: id), serverHalf)
        OwnedChannelsPendingSecret.delete(accountIdB64: id)
        XCTAssertNil(OwnedChannelsPendingSecret.get(accountIdB64: id))

        for carry in [
            OwnedChannels.ResetCarry(serverHalf: serverHalf, lock: .password(exportKey: exportKey), exportKey: exportKey),
            OwnedChannels.ResetCarry(serverHalf: serverHalf, lock: .passkey(prfOutput: prf), exportKey: nil),
            // Item B: a recall's carry has no S (the old keys lock holds it).
            OwnedChannels.ResetCarry(
                serverHalf: nil, lock: .keys(root: Data(repeating: 0x77, count: 32)), exportKey: newExportKey),
        ] {
            OwnedChannelsResetCarryStore.put(carry, accountIdB64: id)
            XCTAssertEqual(OwnedChannelsResetCarryStore.get(accountIdB64: id), carry)
        }
        OwnedChannelsResetCarryStore.delete(accountIdB64: id)
        XCTAssertNil(OwnedChannelsResetCarryStore.get(accountIdB64: id))
    }

    /// Since 2026-11-26 a channel's recovery key comes from the list key
    /// alone (no server evaluation): the same key on every device that
    /// opens the list, another for another channel or another list.
    func testARecoveryKeyComesFromTheListKeyAndTheChannelId() throws {
        let listKey = generateOwnedChannelsListKey()
        let a = Data(repeating: 0xA0, count: 16), b = Data(repeating: 0xB0, count: 16)
        let first = try ChannelRecoveryKey.fromListKey(listKey: listKey, channelId: a).verifyingKey()
        XCTAssertEqual(try ChannelRecoveryKey.fromListKey(listKey: listKey, channelId: a).verifyingKey(), first)
        XCTAssertNotEqual(try ChannelRecoveryKey.fromListKey(listKey: listKey, channelId: b).verifyingKey(), first)
        XCTAssertNotEqual(
            try ChannelRecoveryKey.fromListKey(listKey: generateOwnedChannelsListKey(), channelId: a).verifyingKey(),
            first)
    }
}
