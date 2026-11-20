// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import LakkoCore
import XCTest
@testable import Lakko

/// G-56 (ADR-0074 "Joining after a rotation") — the pure parts on iOS: the
/// shared verdict vectors through the core, the own-link rule, the recovery
/// row v3 through the core (golden vector and refusals), the confirming
/// device's predecessor list, the chat-layer refusal and the DeviceGroup
/// lineage guard.
@MainActor
final class EarlierKeysGateTests: XCTestCase {

    private var accountsToClean: [String] = []

    override func tearDown() async throws {
        for b64 in accountsToClean {
            KeychainOwnPredecessorStore().clear(accountIdB64: b64)
            KeychainOwnConversationKeyStore().clear(accountIdB64: b64)
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: b64)
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: b64)
            KnownGroupsStore().clear(accountIdB64: b64)
        }
        accountsToClean = []
        try await super.tearDown()
    }

    private func newAccount() -> String {
        let b64 = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))
        accountsToClean.append(b64)
        return b64
    }

    // MARK: - The shared truth table (contract 2.1)

    private func key(_ fill: UInt8) -> Data { Data(repeating: fill, count: 32) }

    private enum Bound { case none, cur, k0, k1, foreign }
    private enum Link { case absent, k0, unknown }
    private enum Expected: Equatable { case keep, keepUnderCurrentKey, wait, mintCur, mintK0 }

    func testTheSeventeenSharedVectors() {
        let cur = key(0xCC), k0 = key(0xA0), k1 = key(0xA1), foreign = key(0xF0)
        func bound(_ b: Bound) -> Data? {
            switch b {
            case .none: return nil
            case .cur: return cur
            case .k0: return k0
            case .k1: return k1
            case .foreign: return foreign
            }
        }
        func link(_ l: Link) -> FfiOwnLink {
            switch l {
            case .absent: return .absent
            case .k0: return .key(key: k0)
            case .unknown: return .unknown
            }
        }
        func expected(_ e: Expected) -> FfiBindingVerdict {
            switch e {
            case .keep: return .keep
            case .keepUnderCurrentKey: return .keepUnderCurrentKey
            case .wait: return .wait
            case .mintCur: return .mint(key: cur)
            case .mintK0: return .mint(key: k0)
            }
        }
        let vectors: [(Bound, [Data], Link, Bool, Expected)] = [
            (.none, [], .absent, false, .mintCur),
            (.none, [k0], .absent, false, .mintK0),
            (.none, [k0], .k0, false, .mintK0),
            (.none, [], .k0, false, .wait),
            (.none, [k1], .k0, false, .wait),
            (.none, [k0, k1], .k0, false, .mintK0),
            (.none, [], .unknown, false, .wait),
            (.cur, [], .absent, false, .keep),
            (.cur, [], .k0, false, .wait),
            (.cur, [k0], .k0, false, .mintK0),
            (.cur, [k0], .k0, true, .keepUnderCurrentKey),
            (.cur, [], .k0, true, .keepUnderCurrentKey),
            (.k0, [k0], .k0, true, .keep),
            (.k1, [k0, k1], .k0, true, .keep),
            (.foreign, [], .absent, false, .mintCur),
            (.foreign, [], .k0, false, .wait),
            (.foreign, [k0], .unknown, false, .wait),
        ]
        XCTAssertEqual(vectors.count, 17)
        for (index, vector) in vectors.enumerated() {
            let inputs = EarlierKeysGate.Inputs(
                boundKey: bound(vector.0), currentKey: cur, predecessorKeys: vector.1,
                holdsConversations: vector.3)
            XCTAssertEqual(
                EarlierKeysGate.verdict(inputs, link: link(vector.2)), expected(vector.4), "vector \(index + 1)")
        }
    }

    func testOnlyTheTwoKeptAnswersAreKept() {
        XCTAssertTrue(EarlierKeysGate.isKept(.keep))
        XCTAssertTrue(EarlierKeysGate.isKept(.keepUnderCurrentKey))
        XCTAssertFalse(EarlierKeysGate.isKept(.wait))
        XCTAssertFalse(EarlierKeysGate.isKept(.mint(key: key(1))))
    }

    func testAMalformedInputWaits() {
        let inputs = EarlierKeysGate.Inputs(
            boundKey: nil, currentKey: Data([1, 2, 3]), predecessorKeys: [], holdsConversations: false)
        XCTAssertEqual(EarlierKeysGate.verdict(inputs, link: .absent), .wait)
    }

    // MARK: - The own link (contract 1.1)

    private struct Keys {
        let currentSeed: Data
        let oldSeed: Data
        var current: Data { try! Identity.fromSeed(seed: currentSeed).signaturePublicKey() }
        var old: Data { try! Identity.fromSeed(seed: oldSeed).signaturePublicKey() }
    }

    private func keys() -> Keys {
        Keys(currentSeed: RecoveryMnemonic.generate().toSeed(), oldSeed: RecoveryMnemonic.generate().toSeed())
    }

    private func bundle(current: Data, link: Data?) -> IdentityFetchResponse {
        IdentityFetchResponse(
            signaturePubkey: base64Encode(current), x25519Pubkey: "",
            conversationLink: link.map { $0.base64EncodedString() })
    }

    func testTheOwnLinkRule() throws {
        let k = keys()
        let link = try XCTUnwrap(OwnAccountKeys.conversationLink(
            currentSeed: k.currentSeed, predecessorSeeds: [k.oldSeed], issuedAtUnixSecs: 1_700_000_000))
        // Absent and empty.
        XCTAssertEqual(EarlierKeysGate.linkAnswer(bundle: nil, currentKey: k.current), .absent)
        XCTAssertEqual(EarlierKeysGate.linkAnswer(bundle: bundle(current: k.current, link: nil), currentKey: k.current), .absent)
        XCTAssertEqual(
            EarlierKeysGate.linkAnswer(bundle: bundle(current: k.current, link: Data()), currentKey: k.current), .absent)
        // Verifies against this device's current key: names the old key.
        XCTAssertEqual(
            EarlierKeysGate.linkAnswer(bundle: bundle(current: k.current, link: link), currentKey: k.current),
            .key(key: k.old))
        // Does not verify (minted before a reset: another current key).
        let other = keys()
        XCTAssertEqual(
            EarlierKeysGate.linkAnswer(bundle: bundle(current: other.current, link: link), currentKey: other.current),
            .absent)
        // Damaged bytes.
        var damaged = link
        damaged[damaged.count - 1] ^= 0x01
        XCTAssertEqual(
            EarlierKeysGate.linkAnswer(bundle: bundle(current: k.current, link: damaged), currentKey: k.current),
            .absent)
        // A link from a key to itself is never minted, and the core answers none.
        XCTAssertNil(OwnAccountKeys.conversationLink(
            currentSeed: k.currentSeed, predecessorSeeds: [k.currentSeed], issuedAtUnixSecs: 1))
        XCTAssertNil(try ownConversationKey(link: nil, currentKey: k.current))
    }

    func testTheRecordOfALinkAnswer() {
        XCTAssertEqual(EarlierKeysGate.record(for: .absent, nowMs: 5), OwnConversationKeyRecord(conversationKey: nil, readAtMs: 5))
        XCTAssertEqual(
            EarlierKeysGate.record(for: .key(key: key(7)), nowMs: 6),
            OwnConversationKeyRecord(conversationKey: key(7), readAtMs: 6))
        XCTAssertNil(EarlierKeysGate.record(for: .unknown, nowMs: 7), "an unread link records nothing")
        XCTAssertEqual(EarlierKeysGate.linkFromRecord(nil), .absent)
        XCTAssertEqual(EarlierKeysGate.linkFromRecord(OwnConversationKeyRecord(conversationKey: key(7), readAtMs: 1)), .key(key: key(7)))
    }

    func testTheRecordStoreRoundTrips() {
        let account = newAccount()
        let store = KeychainOwnConversationKeyStore()
        XCTAssertNil(store.load(accountIdB64: account))
        store.save(OwnConversationKeyRecord(conversationKey: key(3), readAtMs: 9), accountIdB64: account)
        XCTAssertEqual(store.load(accountIdB64: account)?.conversationKey, key(3))
        store.clear(accountIdB64: account)
        XCTAssertNil(store.load(accountIdB64: account))
    }

    // MARK: - The recovery row v3 through the core (contract 3.1)

    func testTheGoldenPlaintextVector() throws {
        let plaintext = try composeRecoveryPayload(
            grant: Data(repeating: 0x01, count: 32),
            phraseEntropy: Data(repeating: 0x02, count: 16),
            predecessors: [
                FfiRecoveryPredecessor(seed: Data(repeating: 0x03, count: 64), adoptedAtMs: 1_700_000_000_000),
                FfiRecoveryPredecessor(seed: Data(repeating: 0x04, count: 64), adoptedAtMs: 1_700_000_100_000),
            ])
        XCTAssertEqual(plaintext.count, 193)
        let digest = SHA256.hash(data: plaintext).map { String(format: "%02x", $0) }.joined()
        XCTAssertEqual(digest, "43260742d3c1e705179d7ac8baf1f0566b5fd9621eb1df08bb9a48e34dd56a41")
        let decoded = try decodeRecoveryPayload(plaintext: plaintext)
        XCTAssertEqual(decoded.grant, Data(repeating: 0x01, count: 32))
        XCTAssertEqual(decoded.phraseEntropy, Data(repeating: 0x02, count: 16))
        XCTAssertEqual(decoded.predecessors.map(\.adoptedAtMs), [1_700_000_000_000, 1_700_000_100_000])
        XCTAssertEqual(decoded.predecessors.map(\.seed), [Data(repeating: 0x03, count: 64), Data(repeating: 0x04, count: 64)])
    }

    func testTheLabelsAndBounds() {
        XCTAssertEqual(recoveryExporterLabel(), "lakko/device-recovery/v3")
        XCTAssertEqual(recoveryExporterKeyLen(), 32)
        XCTAssertEqual(recoveryPredecessorsMax(), 8)
    }

    private func refusalCode(_ body: () throws -> Void) -> String? {
        do {
            try body()
            return nil
        } catch FfiError.RecoveryPayload(let message) {
            return message.components(separatedBy: ":").first
        } catch {
            return "other: \(error)"
        }
    }

    func testEveryRefusal() throws {
        let entropy = RecoveryMnemonic.generate().toEntropy()
        let currentSeed = try RecoveryMnemonic.fromEntropy(entropy: entropy).toSeed()
        let grant = Data(repeating: 0x01, count: 32)
        func seed(_ fill: UInt8) -> FfiRecoveryPredecessor {
            FfiRecoveryPredecessor(seed: Data(repeating: fill, count: 64), adoptedAtMs: 1)
        }
        // Nine predecessors.
        XCTAssertEqual(refusalCode {
            _ = try composeRecoveryPayload(grant: grant, phraseEntropy: entropy, predecessors: (1...9).map { seed(UInt8($0)) })
        }, "too-many-predecessors")
        // A duplicate.
        XCTAssertEqual(refusalCode {
            _ = try composeRecoveryPayload(grant: grant, phraseEntropy: entropy, predecessors: [seed(3), seed(3)])
        }, "duplicate-predecessor")
        // The current seed.
        XCTAssertEqual(refusalCode {
            _ = try composeRecoveryPayload(
                grant: grant, phraseEntropy: entropy,
                predecessors: [FfiRecoveryPredecessor(seed: currentSeed, adoptedAtMs: 1)])
        }, "current-key-predecessor")
        // Wrong lengths on decode: a v2 48-byte payload, and one byte short.
        let good = try composeRecoveryPayload(grant: grant, phraseEntropy: entropy, predecessors: [seed(3)])
        XCTAssertEqual(refusalCode { _ = try decodeRecoveryPayload(plaintext: good.prefix(48)) }, "bad-length")
        XCTAssertEqual(refusalCode { _ = try decodeRecoveryPayload(plaintext: good.dropLast()) }, "bad-length")
        // A count of nine on the wire.
        var nine = Data(grant + entropy)
        nine.append(9)
        for fill in 1...9 { nine.append(Data(repeating: UInt8(fill), count: 64)); nine.append(Data(count: 8)) }
        // The core checks the length first: nine does not fit the 625-byte
        // ceiling, so it is refused as a length.
        let nineCode = refusalCode { _ = try decodeRecoveryPayload(plaintext: nine) }
        XCTAssertTrue(["bad-length", "too-many-predecessors"].contains(nineCode ?? ""), "\(String(describing: nineCode))")
    }

    func testTheRowFramingRefusesV2AndSkipsOtherRows() throws {
        let sealed = Data([0x7F, 0x01, 0x02])
        let row = wrapRecoveryRow(sealed: sealed)
        XCTAssertEqual(try unwrapRecoveryRow(row: row), sealed)
        XCTAssertTrue(RecoveryPayload.isRecoveryRow(row))
        if case .received(let blob)? = RecoveryPayload.outcome(of: row) {
            XCTAssertEqual(blob, sealed)
        } else {
            XCTFail("a v3 row ends the poll with its blob")
        }
        // A v2 row: recognised as a recovery row, refused.
        let v2 = Data("lakko/recovery-payload/v2".utf8) + sealed
        XCTAssertEqual(refusalCode { _ = try unwrapRecoveryRow(row: v2) }, "unsupported-row")
        XCTAssertTrue(RecoveryPayload.isRecoveryRow(v2), "the history poll skips it")
        if case .refused? = RecoveryPayload.outcome(of: v2) {} else { XCTFail("a v2 row is refused") }
        // Not a recovery row at all (a history or state bundle).
        XCTAssertNil(try unwrapRecoveryRow(row: Data(count: 64)))
        XCTAssertFalse(RecoveryPayload.isRecoveryRow(Data(count: 64)))
        XCTAssertNil(RecoveryPayload.outcome(of: Data(count: 64)))
    }

    // MARK: - The confirming device's list (contract 3.2)

    private final class FailingPredecessorStore: OwnPredecessorStoring {
        func load(accountIdB64: String) -> [Data] { [] }
        func add(_ seed: Data, accountIdB64: String) throws {}
        func replace(_ seeds: [Data], accountIdB64: String) throws {}
        func clear(accountIdB64: String) {}
        func loadChecked(accountIdB64: String) throws -> [Data] { throw KeychainJSONItemError.keychain(-25_300) }
    }

    func testTheRowCarriesThePredecessorsOldestFirstWithoutTheCurrentSeed() throws {
        let account = newAccount()
        let store = InMemoryOwnPredecessorStore()
        let a = RecoveryMnemonic.generate().toSeed(), b = RecoveryMnemonic.generate().toSeed()
        let current = RecoveryMnemonic.generate().toSeed()
        try store.replace([a, b, current], accountIdB64: account)
        let defaults = UserDefaults(suiteName: "g56-\(UUID().uuidString)")!
        let rows = try PairingOrchestrator.rowPredecessors(
            accountIdB64: account, currentSeed: current, store: store,
            adoptions: PredecessorMailbox.AdoptionStore(defaults: defaults), nowMs: 42)
        XCTAssertEqual(rows.map(\.seed), [a, b])
        XCTAssertEqual(rows.map(\.adoptedAtMs), [42, 42])
        // An empty slot carries none.
        XCTAssertEqual(try PairingOrchestrator.rowPredecessors(
            accountIdB64: newAccount(), currentSeed: current, store: InMemoryOwnPredecessorStore()).count, 0)
    }

    func testAnUnreadableSlotIsNotNone() {
        XCTAssertThrowsError(try PairingOrchestrator.rowPredecessors(
            accountIdB64: newAccount(), currentSeed: Data(count: 64), store: FailingPredecessorStore()))
    }

    // MARK: - The chat-layer refusal (contract 2.2, defence in depth)

    func testTheChatLayerMintsNothingWhileWaiting() throws {
        let account = newAccount()
        let k = keys()
        let current = try Identity.fromSeed(seed: k.currentSeed)
        XCTAssertThrowsError(try ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: current, predecessorSeeds: [], holdsConversations: false,
            link: .key(key: k.old))) { error in
            XCTAssertEqual(error as? EarlierKeysMissing, EarlierKeysMissing(accountIdB64: account))
        }
        XCTAssertNil(DeviceIdentityStore.getDeviceBinding(accountIdB64: account), "no binding is stored")
        XCTAssertNil(try DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: account), "nor a device seed")

        // Once the earlier seed is held, the binding is minted under it.
        let bound = try XCTUnwrap(try ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: current, predecessorSeeds: [k.oldSeed], holdsConversations: false,
            link: .key(key: k.old)))
        XCTAssertEqual(bound.accountPublicKey(), k.old)
    }

    func testTheChatLayerKeepsTheOldRuleWithNoLink() throws {
        let account = newAccount()
        let k = keys()
        let current = try Identity.fromSeed(seed: k.currentSeed)
        let bound = try XCTUnwrap(try ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: current, predecessorSeeds: [], holdsConversations: false))
        XCTAssertEqual(bound.accountPublicKey(), k.current)
    }

    // MARK: - Pairing a device that is already in the DeviceGroup (follow-up 2)

    private func random(_ count: Int) -> Data { Data((0..<count).map { _ in UInt8.random(in: 0...255) }) }

    /// The core refuses to add a KeyPackage under the leaf key of a device
    /// already in the DeviceGroup: re-pairing a device that came in by
    /// pairing (and parked) under its old device identity cannot work.
    func testTheCoreRefusesToAddADeviceAlreadyInTheDeviceGroup() throws {
        let accountId = random(16)
        let founderIdentity = try Identity.fromSeed(seed: random(32))
        let founder = try DeviceGroup.createFounder(
            identity: founderIdentity, accountId: accountId, accountIdentity: try Identity.fromSeed(seed: random(32)))
        let joiner = try Identity.fromSeed(seed: random(32))
        let first = try founder.addDeviceFromWire(identity: founderIdentity, keyPackage: try joiner.createKeyPackage())
        _ = try DeviceGroup.joinFromWelcome(welcome: first.welcome, identity: joiner, accountId: accountId)

        XCTAssertThrowsError(
            try founder.addDeviceFromWire(identity: founderIdentity, keyPackage: try joiner.createKeyPackage())
        ) { error in
            XCTAssertTrue(String(describing: error).contains("DuplicateSignatureKey"), "\(error)")
        }
    }

    /// G-56 — "Another device" on the parked screen of a device already in
    /// the DeviceGroup: it keeps its device identity and carries a
    /// self-removal proof; the confirming device replaces its leaf in one
    /// commit (no extra leaf), and the join succeeds. A proof for another
    /// epoch is refused.
    func testAParkedMemberPairsAgainAndItsLeafIsReplaced() async throws {
        let accountIdB64 = newAccount()
        let accountId = try XCTUnwrap(base64Decode(accountIdB64))
        let founderIdentity = try Identity.fromSeed(seed: random(32))
        let founder = try DeviceGroup.createFounder(
            identity: founderIdentity, accountId: accountId, accountIdentity: try Identity.fromSeed(seed: random(32)))
        // This device came in by pairing: its seed, its DeviceGroup state and
        // the id it joined.
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
        let old = try Identity.fromSeed(seed: seed)
        DeviceGroupLifecycle.noteHandle(old, accountIdB64: accountIdB64)
        let first = try founder.addDeviceFromWire(identity: founderIdentity, keyPackage: try old.createKeyPackage())
        let joined = try DeviceGroup.joinFromWelcome(welcome: first.welcome, identity: old, accountId: accountId)
        let snapshots = DeviceGroupSnapshotStore()
        _ = try DeviceGroupLifecycle.persistSnapshotOrThrow(
            identity: old, accountIdB64: accountIdB64, snapshotStore: snapshots)
        let ids = JoinedDeviceGroupIdStore()
        ids.save(try joined.groupId(), accountIdB64: accountIdB64)
        defer {
            snapshots.deleteSnapshot(accountIdB64: accountIdB64)
            ids.clear(accountIdB64: accountIdB64)
        }
        let loginHandle = random(32)
        let founderEnsured = DeviceGroupLifecycle.Ensured(
            identity: founderIdentity, group: founder, bootstrapped: false, generation: 0)

        // A proof for another epoch: refused, nothing committed.
        let stale = try await PairingOrchestrator.prepareRejoinPairingFlow(
            loginHandle: loginHandle, accountIdBytes: accountId, accountIdB64: accountIdB64,
            snapshotStore: snapshots, joinedGroupIds: ids, latestEpoch: { _ in try founder.epoch() + 3 })
        let staleDecoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: stale.qrPayload, expectedLoginHandle: loginHandle)
        XCTAssertNotNil(staleDecoded.selfRemovalProof)
        let epochBefore = try founder.epoch()
        XCTAssertThrowsError(try PairingOrchestrator.replacementCommit(ensured: founderEnsured, decoded: staleDecoded)) {
            XCTAssertEqual($0 as? PairingOrchestrator.SelfRemovalProofNotCovering, .init())
        }
        XCTAssertEqual(try founder.epoch(), epochBefore)

        // The probe names the DeviceGroup's epoch: the leaf is replaced.
        let session = try await PairingOrchestrator.prepareRejoinPairingFlow(
            loginHandle: loginHandle, accountIdBytes: accountId, accountIdB64: accountIdB64,
            snapshotStore: snapshots, joinedGroupIds: ids, latestEpoch: { _ in try founder.epoch() })
        let decoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: session.qrPayload, expectedLoginHandle: loginHandle)
        XCTAssertEqual(try keyPackageSignatureKey(keyPackage: decoded.keyPackage), old.signaturePublicKey(),
                       "the device keeps its identity")
        let built = try XCTUnwrap(try PairingOrchestrator.replacementCommit(ensured: founderEnsured, decoded: decoded))
        XCTAssertEqual(built.kind, .rejoinReplace)
        XCTAssertEqual(try founder.devices().count, 2, "no extra leaf")
        // The proof rides in the journal envelope's removal slot.
        let envelope = try DeviceGroupJournalEnvelope.decode(built.row)
        XCTAssertEqual(envelope.attestation, decoded.selfRemovalProof)

        _ = try await session.accept(welcomeBytes: try XCTUnwrap(built.welcome))
        XCTAssertEqual(try DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: accountIdB64), seed)
        let restored = try Identity.fromSeed(seed: seed)
        try restored.restoreStorage(snapshot: try XCTUnwrap(snapshots.getSnapshot(accountIdB64: accountIdB64)))
        let reloaded = try XCTUnwrap(try loadDeviceGroupById(
            identity: restored, accountId: accountId, groupId: try founder.groupId()))
        XCTAssertEqual(try reloaded.epoch(), try founder.epoch())
        XCTAssertEqual(try reloaded.devices().count, 2)

        // Without a proof nothing changes: the ordinary add is attempted
        // (and the core refuses a second leaf under the same key).
        let plain = try PairingOrchestrator.prepareJoinFlow(
            loginHandle: loginHandle, accountIdBytes: accountId, accountIdB64: accountIdB64)
        let plainDecoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: plain.qrPayload, expectedLoginHandle: loginHandle)
        XCTAssertNil(plainDecoded.selfRemovalProof)
        XCTAssertNil(try PairingOrchestrator.replacementCommit(ensured: founderEnsured, decoded: plainDecoded))
    }

    // MARK: - The DeviceGroup lineage guard (contract 2.4)

    func testTheLineageIsNoneWhileTheConversationKeyIsNotHeld() throws {
        let account = newAccount()
        let k = keys()
        let predecessors = InMemoryOwnPredecessorStore()
        let records = InMemoryOwnConversationKeyStore()
        // No record: the fallback to the current seed is unchanged.
        XCTAssertEqual(DeviceGroupLifecycle.lineageSeed(
            accountIdB64: account, currentSeed: k.currentSeed, predecessors: predecessors,
            conversationKeys: records), k.currentSeed)
        // A record naming a key that no held seed derives: none.
        records.save(OwnConversationKeyRecord(conversationKey: k.old, readAtMs: 1), accountIdB64: account)
        XCTAssertNil(DeviceGroupLifecycle.lineageSeed(
            accountIdB64: account, currentSeed: k.currentSeed, predecessors: predecessors,
            conversationKeys: records))
        // The guard skipped (the local wipe): the fallback.
        XCTAssertEqual(DeviceGroupLifecycle.lineageSeed(
            accountIdB64: account, currentSeed: k.currentSeed, predecessors: predecessors,
            conversationKeys: nil), k.currentSeed)
        // Once the earlier seed is held, the lineage is it.
        try predecessors.replace([k.oldSeed], accountIdB64: account)
        XCTAssertEqual(DeviceGroupLifecycle.lineageSeed(
            accountIdB64: account, currentSeed: k.currentSeed, predecessors: predecessors,
            conversationKeys: records), k.oldSeed)
        // A record of "no link" changes nothing.
        records.save(OwnConversationKeyRecord(conversationKey: nil, readAtMs: 2), accountIdB64: account)
        XCTAssertEqual(DeviceGroupLifecycle.lineageSeed(
            accountIdB64: account, currentSeed: k.currentSeed, predecessors: InMemoryOwnPredecessorStore(),
            conversationKeys: records), k.currentSeed)
    }
}

/// G-56 — for test fixtures of devices that never bootstrapped a chat (no
/// device binding): the gate reads no inputs and every landing goes on, as
/// before G-56. The G-56 tests use the real reader.
let skipEarlierKeysGate: @MainActor (String, Data) -> EarlierKeysGate.Inputs? = { _, _ in nil }
