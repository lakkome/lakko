// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// ADR-0074 part 3, Stage A — the ROTATING device: the owed record and its
// state machine, the envelope-after-removal seam with its inverted companion,
// and the runner over scripted ports.

// MARK: - The state machine and the owed record

final class KeyRotationPlanTests: XCTestCase {

    private var suites: [String] = []

    override func tearDown() {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        super.tearDown()
    }

    private func defaults() -> UserDefaults {
        let suite = "key-rotation-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private func record(_ phase: KeyRotationRecord.Phase) -> KeyRotationRecord {
        var record = KeyRotationRecord(targetDeviceIdB64: "ZGV2", startedAtMs: 1)
        record.phase = phase
        return record
    }

    func testNothingIsOwedBeforeTheCommitAndEveryStepAfterItInOrder() {
        XCTAssertEqual(KeyRotationPlan.remaining(record(.removing)), [])
        XCTAssertEqual(KeyRotationPlan.remaining(record(.removed)), [])
        XCTAssertEqual(KeyRotationPlan.remaining(record(.announced)), [])
        XCTAssertEqual(
            KeyRotationPlan.remaining(record(.committed)),
            [.discovery, .keyPackage, .deviceGroupRoot, .journals, .channels, .reach, .profile, .grants, .backup])
    }

    func testAStepThatLandedIsNotOwedAgain() {
        var committed = record(.committed)
        for step in KeyRotationPlan.Step.allCases {
            XCTAssertTrue(KeyRotationPlan.remaining(committed).contains(step))
            committed = KeyRotationPlan.marking(committed, done: step)
            XCTAssertFalse(KeyRotationPlan.remaining(committed).contains(step))
        }
        XCTAssertEqual(KeyRotationPlan.remaining(committed), [])
    }

    func testTheWordsAreWithheldUntilTheDirectoryTheKeyPackageAndTheDeviceListKnowTheNewKeys() {
        var committed = record(.committed)
        XCTAssertTrue(KeyRotationPlan.publishOwed(committed))
        committed = KeyRotationPlan.marking(committed, done: .discovery)
        committed = KeyRotationPlan.marking(committed, done: .keyPackage)
        XCTAssertTrue(KeyRotationPlan.publishOwed(committed), "the device list is still owed")
        committed = KeyRotationPlan.marking(committed, done: .deviceGroupRoot)
        XCTAssertFalse(KeyRotationPlan.publishOwed(committed), "the rest finishes in the background")
        XCTAssertFalse(KeyRotationPlan.remaining(committed).isEmpty)
        // Before the commit nothing was changed, so nothing is "owed".
        XCTAssertFalse(KeyRotationPlan.publishOwed(record(.announced)))
    }

    func testTheRecordIsCompleteOnlyWhenEveryStepLandedAndTheWordsWereConfirmed() {
        var committed = record(.committed)
        for step in KeyRotationPlan.Step.allCases { committed = KeyRotationPlan.marking(committed, done: step) }
        XCTAssertFalse(KeyRotationPlan.isComplete(committed), "the words were not confirmed yet")
        committed.phraseAcknowledged = true
        XCTAssertTrue(KeyRotationPlan.isComplete(committed))
    }

    func testTheStepListFollowsThePhase() {
        XCTAssertEqual(KeyRotationPlan.stage(of: record(.removing)), .removingDevice)
        XCTAssertEqual(KeyRotationPlan.stage(of: record(.removed)), .changingKeys)
        XCTAssertEqual(KeyRotationPlan.stage(of: record(.announced)), .changingKeys)
        XCTAssertEqual(KeyRotationPlan.stage(of: record(.committed)), .updatingDevices)
    }

    // "Removed but not rotated" is a state of its own.
    func testARecordThatNeverCommittedReadsAsRemovedButNotRotated() {
        let held = Data(repeating: 1, count: 32)
        XCTAssertEqual(KeyRotationPlan.resume(nil, heldAccountKey: held), .nothing)
        XCTAssertEqual(KeyRotationPlan.resume(record(.removing), heldAccountKey: held), .removedNotRotated)
        XCTAssertEqual(KeyRotationPlan.resume(record(.removed), heldAccountKey: held), .removedNotRotated)
        var announced = record(.announced)
        announced.newSignaturePubkey = Data(repeating: 2, count: 32)
        XCTAssertEqual(
            KeyRotationPlan.resume(announced, heldAccountKey: held), .removedNotRotated,
            "the device still holds the old key: the commit did not land")
    }

    func testAnAnnouncedRecordWhoseKeyThisDeviceNowHoldsMeansTheCommitLanded() {
        let newKey = Data(repeating: 2, count: 32)
        var announced = record(.announced)
        announced.newSignaturePubkey = newKey
        XCTAssertEqual(KeyRotationPlan.resume(announced, heldAccountKey: newKey), .commitLandedUnseen)
    }

    func testACommittedRecordFinishesItsStepsThenShowsTheWords() {
        var committed = record(.committed)
        XCTAssertEqual(KeyRotationPlan.resume(committed, heldAccountKey: nil), .finishOwed)
        for step in KeyRotationPlan.Step.allCases { committed = KeyRotationPlan.marking(committed, done: step) }
        XCTAssertEqual(KeyRotationPlan.resume(committed, heldAccountKey: nil), .showWords)
    }

    func testTheCommitsAnswersAreToldApart() {
        XCTAssertEqual(KeyRotationPlan.commitOutcome(status: 200, body: "{}"), .committed)
        XCTAssertEqual(
            KeyRotationPlan.commitOutcome(status: 403, body: #"{"error":"fresh-auth-required"}"#),
            .freshAuthRequired)
        XCTAssertEqual(
            KeyRotationPlan.commitOutcome(status: 403, body: #"{"error":"account-key-required"}"#),
            .accountKeyRequired)
        XCTAssertEqual(
            KeyRotationPlan.commitOutcome(
                status: 429, body: #"{"error":"too-many-attempts","retry_after_secs":40}"#),
            .rateLimited(retryAfterSecs: 40))
        XCTAssertEqual(
            KeyRotationPlan.commitOutcome(status: 401, body: #"{"error":"device-revoked"}"#), .sessionEnded)
        XCTAssertEqual(KeyRotationPlan.commitOutcome(status: 400, body: ""), .failed)
        XCTAssertEqual(
            KeyRotationPlan.commitOutcome(
                of: AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))),
            .offline)
        XCTAssertEqual(KeyRotationPlan.commitOutcome(of: nil), .committed)
    }

    func testTheDirectoryFallsBackToAnUploadWithoutTheProofOnlyWhenTheProofWasRefused() {
        XCTAssertEqual(KeyRotationPlan.directoryOutcome(status: 200, body: "", sentProof: true), .done)
        XCTAssertEqual(
            KeyRotationPlan.directoryOutcome(
                status: 409, body: #"{"error":"key-generation-not-newer"}"#, sentProof: true),
            .done, "a retry of an upload that already landed")
        XCTAssertEqual(
            KeyRotationPlan.directoryOutcome(
                status: 400, body: #"{"error":"supersession-proof-invalid"}"#, sentProof: true),
            .retryWithoutProof)
        XCTAssertEqual(
            KeyRotationPlan.directoryOutcome(
                status: 400, body: #"{"error":"supersession-proof-invalid"}"#, sentProof: false),
            .retry, "no proof was sent: there is nothing to drop")
        XCTAssertEqual(
            KeyRotationPlan.directoryOutcome(
                status: 401, body: #"{"error":"key-generation-superseded"}"#, sentProof: true),
            .superseded)
        XCTAssertEqual(KeyRotationPlan.directoryOutcome(status: 503, body: "", sentProof: true), .retry)
    }

    func testTheStoreKeepsTheRecordUntilItIsCompleteAndSaysWhenAPublishIsOwed() {
        let store = KeyRotationStore(defaults: defaults())
        XCTAssertNil(store.load(accountIdB64: "a"))
        XCTAssertFalse(store.publishOwed(accountIdB64: "a"))

        var stored = record(.removing)
        stored.announceEpochFloor = 7
        store.put(stored, accountIdB64: "a")
        XCTAssertEqual(store.load(accountIdB64: "a"), stored)
        XCTAssertFalse(store.publishOwed(accountIdB64: "a"), "nothing was changed yet")
        XCTAssertNil(store.load(accountIdB64: "b"), "per account")

        stored.phase = .committed
        stored.attestation = Data(repeating: 9, count: 177)
        stored.keyGeneration = 4
        store.put(stored, accountIdB64: "a")
        XCTAssertTrue(store.publishOwed(accountIdB64: "a"))

        for step in KeyRotationPlan.Step.allCases { stored = KeyRotationPlan.marking(stored, done: step) }
        store.put(stored, accountIdB64: "a")
        XCTAssertFalse(store.publishOwed(accountIdB64: "a"))
        XCTAssertNotNil(store.load(accountIdB64: "a"), "the words are still to be confirmed")
        stored.phraseAcknowledged = true
        store.put(stored, accountIdB64: "a")
        XCTAssertNil(store.load(accountIdB64: "a"), "complete: the record is gone")
    }

    func testTheRecordHoldsNoSeed() throws {
        var stored = record(.committed)
        stored.newSignaturePubkey = Data(repeating: 2, count: 32)
        stored.supersessionProof = Data(repeating: 3, count: 145)
        stored.attestation = Data(repeating: 4, count: 177)
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(stored)) as? [String: Any])
        XCTAssertEqual(
            Set(json.keys),
            ["target_device_id", "started_at_ms", "phase", "new_signature_pubkey", "supersession_proof",
             "attestation", "discovery_done", "key_package_done", "device_group_root_done",
             "journals_done", "channels_done", "reach_done", "backup_done", "phrase_acknowledged"],
            "public artefacts and flags: no entropy, no seed, no phrase")
    }
}

// MARK: - The envelope-after-removal seam

/// ADR-0074 section 3.2: the `key-rotation` envelope must be sealed at a
/// DeviceGroup epoch the removed device never held. Nothing but the sender
/// can check it, so the test asserts on the EPOCH OF THE EMITTED MESSAGE, and
/// a permanent inverted-order companion shows what the seam prevents.
@MainActor
final class KeyRotationAnnounceTests: XCTestCase {

    /// A DeviceGroup reduced to what the seam reads: an epoch, a roster, and
    /// a send that seals at the epoch it has when called.
    private final class FakeGroup {
        var epoch: UInt64
        var roster: [Data]
        /// The epoch each emitted message was sealed at.
        private(set) var sealedAt: [UInt64] = []
        private(set) var log: [String] = []
        var removeAdvances = true
        var removeDrops = true
        /// A stale handle seals below the live epoch by this much.
        var sealLag: UInt64 = 0

        init(epoch: UInt64, roster: [Data]) {
            self.epoch = epoch
            self.roster = roster
        }

        func remove(_ target: Data) {
            log.append("remove")
            if removeDrops { roster.removeAll { $0 == target } }
            if removeAdvances { epoch += 1 }
        }

        func send(_ envelope: Data) -> UInt64 {
            log.append("send")
            let sealed = epoch - sealLag
            sealedAt.append(sealed)
            return sealed
        }

        func note(_ event: String) { log.append(event) }

        func ports(target: Data) -> KeyRotationAnnounce.Ports {
            KeyRotationAnnounce.Ports(
                epoch: { self.epoch },
                roster: { self.roster },
                remove: { self.remove(target) },
                send: { self.send($0) })
        }
    }

    private let lost = Data(repeating: 0xD1, count: 32)
    private let own = Data(repeating: 0xD2, count: 32)
    private let kept = Data(repeating: 0xD3, count: 32)

    func testTheEnvelopeIsSealedAtTheEpochAfterTheRemoval() async throws {
        let group = FakeGroup(epoch: 10, roster: [own, kept, lost])
        var floors: [UInt64] = []

        let announced = try await KeyRotationAnnounce.removeThenAnnounce(
            target: lost, recordedFloor: nil, onFloor: { floors.append($0) },
            ports: group.ports(target: lost),
            envelope: {
                group.note("envelope")
                return Data([1])
            })

        // The assertion that matters: the epoch of the EMITTED message is
        // past the last epoch the lost device held (10).
        XCTAssertEqual(group.sealedAt, [11])
        XCTAssertEqual(announced.sealedEpoch, 11)
        XCTAssertGreaterThan(announced.sealedEpoch, 10)
        XCTAssertEqual(announced.floor, 11)
        XCTAssertEqual(floors, [11], "the floor is persisted before the removal starts")
        // The new seed does not exist while the lost device can still read
        // the group: the envelope is built only after the removal.
        XCTAssertEqual(group.log, ["remove", "envelope", "send"])
        XCTAssertFalse(group.roster.contains(lost))
    }

    /// The inverted companion: announce FIRST, remove after. The message is
    /// then sealed at the epoch the lost device still holds. This is the order
    /// the seam exists to make impossible; the test runs it by hand so the
    /// assertion above is known to discriminate.
    func testInvertedOrderSealsAtAnEpochTheLostDeviceStillHolds() {
        let group = FakeGroup(epoch: 10, roster: [own, kept, lost])
        let heldByLostDevice = group.epoch

        let sealed = group.send(Data([1]))
        group.remove(lost)

        XCTAssertEqual(group.log, ["send", "remove"])
        XCTAssertEqual(sealed, heldByLostDevice, "sealed where the lost device can read it")
        XCTAssertLessThan(
            sealed,
            KeyRotationAnnounce.floor(recorded: nil, currentEpoch: heldByLostDevice, targetPresent: true),
            "below the floor the seam enforces: the seam would never have emitted it")
    }

    func testNothingIsSentWhileTheDeviceIsStillInTheDeviceGroup() async {
        let group = FakeGroup(epoch: 10, roster: [own, lost])
        group.removeDrops = false

        do {
            _ = try await KeyRotationAnnounce.removeThenAnnounce(
                target: lost, recordedFloor: nil, onFloor: { _ in },
                ports: group.ports(target: lost), envelope: { Data([1]) })
            XCTFail("the seam must refuse")
        } catch {
            XCTAssertEqual(error as? KeyRotationAnnounce.Failure, .deviceStillPresent)
        }
        XCTAssertEqual(group.sealedAt, [], "nothing was emitted")
    }

    func testNothingIsSentWhenTheEpochDidNotAdvance() async {
        let group = FakeGroup(epoch: 10, roster: [own, lost])
        group.removeAdvances = false

        do {
            _ = try await KeyRotationAnnounce.removeThenAnnounce(
                target: lost, recordedFloor: nil, onFloor: { _ in },
                ports: group.ports(target: lost), envelope: { Data([1]) })
            XCTFail("the seam must refuse")
        } catch {
            XCTAssertEqual(
                error as? KeyRotationAnnounce.Failure, .epochNotAdvanced(current: 10, floor: 11))
        }
        XCTAssertEqual(group.sealedAt, [])
    }

    func testAMessageSealedBelowTheFloorStopsTheRotation() async {
        let group = FakeGroup(epoch: 10, roster: [own, lost])
        group.sealLag = 1  // a stale handle

        do {
            _ = try await KeyRotationAnnounce.removeThenAnnounce(
                target: lost, recordedFloor: nil, onFloor: { _ in },
                ports: group.ports(target: lost), envelope: { Data([1]) })
            XCTFail("the seam must refuse")
        } catch {
            XCTAssertEqual(
                error as? KeyRotationAnnounce.Failure, .sealedBeforeRemoval(sealed: 10, floor: 11))
        }
    }

    func testARetryAfterTheRemovalKeepsTheRecordedFloor() async throws {
        // An earlier run removed the device at epoch 10 -> 11 and stopped; the
        // group moved on to 13 since.
        let group = FakeGroup(epoch: 13, roster: [own, kept])
        group.removeAdvances = false
        group.removeDrops = false

        let announced = try await KeyRotationAnnounce.removeThenAnnounce(
            target: lost, recordedFloor: 11, onFloor: { _ in },
            ports: group.ports(target: lost), envelope: { Data([1]) })

        XCTAssertEqual(announced.floor, 11)
        XCTAssertEqual(announced.sealedEpoch, 13)
    }

    func testTheFloorIsOnePastTheEpochTheTargetHoldsOrTheCurrentOneWhenItIsAlreadyOut() {
        XCTAssertEqual(KeyRotationAnnounce.floor(recorded: nil, currentEpoch: 5, targetPresent: true), 6)
        XCTAssertEqual(KeyRotationAnnounce.floor(recorded: nil, currentEpoch: 5, targetPresent: false), 5)
        XCTAssertEqual(KeyRotationAnnounce.floor(recorded: 3, currentEpoch: 5, targetPresent: true), 3)
    }

    // MARK: The same property over a REAL DeviceGroup

    private struct Device {
        let identity: Identity
        let group: DeviceGroup
    }

    private func randomBytes(_ count: Int) -> Data {
        Data((0..<count).map { _ in UInt8.random(in: 0...255) })
    }

    /// A DeviceGroup of three devices: the rotating one (founder), a kept one
    /// and the lost one.
    private func threeDevices(accountId: Data) throws -> (rotating: Device, kept: Device, lost: Device) {
        let founderIdentity = try Identity.fromSeed(seed: randomBytes(32))
        let founder = try DeviceGroup.createFounder(identity: founderIdentity, accountId: accountId)
        let keptIdentity = try Identity.fromSeed(seed: randomBytes(32))
        let addKept = try founder.addDeviceFromWire(
            identity: founderIdentity, keyPackage: try keptIdentity.createKeyPackage())
        let keptGroup = try DeviceGroup.joinFromWelcome(
            welcome: addKept.welcome, identity: keptIdentity, accountId: accountId)
        let lostIdentity = try Identity.fromSeed(seed: randomBytes(32))
        let addLost = try founder.addDeviceFromWire(
            identity: founderIdentity, keyPackage: try lostIdentity.createKeyPackage())
        let lostGroup = try DeviceGroup.joinFromWelcome(
            welcome: addLost.welcome, identity: lostIdentity, accountId: accountId)
        _ = try process(Device(identity: keptIdentity, group: keptGroup), addLost.commit)
        return (
            Device(identity: founderIdentity, group: founder),
            Device(identity: keptIdentity, group: keptGroup),
            Device(identity: lostIdentity, group: lostGroup))
    }

    private func process(_ device: Device, _ message: Data) throws -> ProcessedMessage {
        try device.group.processIncomingVerifyingRemovals(
            identity: device.identity, messageBytes: message, attestations: [],
            attestVerifyingKey: Data(count: 32), nowUnixSecs: Int64(Date().timeIntervalSince1970))
    }

    /// The service envelope an application message carries, or `nil`.
    private func serviceEnvelope(_ processed: ProcessedMessage) throws -> Data? {
        guard case .application(let data, _, _) = processed else { return nil }
        guard case .serviceEnvelope(let envelope) = try parseDevicegroupPayload(plaintext: data).body else {
            return nil
        }
        return envelope
    }

    private func rotationEnvelope(oldSeed: Data) throws -> (envelope: Data, newKey: Data) {
        let mnemonic = RecoveryMnemonic.generate()
        let new = try Identity.fromSeed(seed: mnemonic.toSeed())
        let now = Int64(Date().timeIntervalSince1970)
        let proof = try signSupersessionProof(
            oldEd25519Seed: try Identity.fromSeed(seed: oldSeed).ed25519Seed(),
            newEd25519Seed: new.ed25519Seed(),
            issuedAtUnixSecs: now, expiresAtUnixSecs: now + KeyRotationWire.proofWindowSecs)
        return (
            try composeKeyRotationEnvelope(
                preparedAtMs: UInt64(now) * 1000, newEntropy: mnemonic.toEntropy(), supersessionProof: proof),
            new.signaturePublicKey())
    }

    func testOverARealDeviceGroupTheEmittedMessageIsPastTheRemovalAndTheLostDeviceCannotOpenIt() async throws {
        let accountId = randomBytes(16)
        let oldSeed = randomBytes(64)
        let heldKey = try Identity.fromSeed(seed: oldSeed).signaturePublicKey()
        let devices = try threeDevices(accountId: accountId)
        let lostKey = devices.lost.identity.signaturePublicKey()
        var removalCommit: Data?
        var emitted: Data?
        var newKey: Data?

        let rotating = devices.rotating
        let epochHeldByLostDevice = try rotating.group.dsAccessSigner(identity: rotating.identity).epoch()
        let announced = try await KeyRotationAnnounce.removeThenAnnounce(
            target: lostKey, recordedFloor: nil, onFloor: { _ in },
            ports: KeyRotationAnnounce.Ports(
                epoch: { try rotating.group.dsAccessSigner(identity: rotating.identity).epoch() },
                roster: { try rotating.group.devices().map(\.signaturePublicKey) },
                remove: {
                    let leaf = try XCTUnwrap(
                        try rotating.group.devices().first { $0.signaturePublicKey == lostKey }?.leafIndex)
                    removalCommit = try rotating.group.removeDeviceAt(
                        identity: rotating.identity, leafIndex: leaf)
                },
                send: { envelope in
                    let wire = try rotating.group.createServiceEnvelopeMessage(
                        identity: rotating.identity, envelope: envelope, hlc: 1)
                    emitted = wire
                    return try XCTUnwrap(epochOfMessage(bytes: wire))
                }),
            envelope: {
                let built = try self.rotationEnvelope(oldSeed: oldSeed)
                newKey = built.newKey
                return built.envelope
            })

        // The epoch read off the emitted MLS message itself.
        let wire = try XCTUnwrap(emitted)
        XCTAssertEqual(epochOfMessage(bytes: wire), announced.sealedEpoch)
        XCTAssertGreaterThan(announced.sealedEpoch, epochHeldByLostDevice)

        // The kept device follows the removal, opens the message and verifies
        // the envelope under the account key it holds.
        _ = try process(devices.kept, try XCTUnwrap(removalCommit))
        let received = try XCTUnwrap(try serviceEnvelope(try process(devices.kept, wire)))
        XCTAssertEqual(try dispatchIncomingEnvelope(plaintext: received).kind, .keyRotation)
        let verified = try verifyKeyRotationEnvelope(
            plaintext: received, heldAccountPubkey: heldKey,
            nowUnixSecs: Int64(Date().timeIntervalSince1970))
        XCTAssertEqual(verified.newSignaturePubkey, newKey)

        // The lost device never held that epoch: with or without the removal
        // commit it cannot open the message.
        XCTAssertThrowsError(try process(devices.lost, wire))
        _ = try? process(devices.lost, try XCTUnwrap(removalCommit))
        XCTAssertThrowsError(try process(devices.lost, wire))
    }

    /// The inverted companion over the real group: sealed BEFORE the removal,
    /// the lost device opens the envelope and reads the account's new seed.
    func testOverARealDeviceGroupInvertedOrderHandsTheNewSeedToTheLostDevice() throws {
        let accountId = randomBytes(16)
        let oldSeed = randomBytes(64)
        let devices = try threeDevices(accountId: accountId)
        let rotating = devices.rotating
        let epochHeldByLostDevice = try rotating.group.dsAccessSigner(identity: rotating.identity).epoch()
        let built = try rotationEnvelope(oldSeed: oldSeed)

        let wire = try rotating.group.createServiceEnvelopeMessage(
            identity: rotating.identity, envelope: built.envelope, hlc: 1)

        XCTAssertEqual(epochOfMessage(bytes: wire), epochHeldByLostDevice)
        let leaked = try XCTUnwrap(try serviceEnvelope(try process(devices.lost, wire)))
        XCTAssertEqual(leaked, built.envelope, "the lost device read the new seed's envelope")
    }
}

// MARK: - The runner

@MainActor
final class KeyRotationRunnerTests: XCTestCase {

    private let accountId = Data((0..<16).map { UInt8(0xE0 | $0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private let oldSeed = Data(repeating: 0x5A, count: 64)
    private let target = Data(repeating: 0xB2, count: 32)
    private let own = Data(repeating: 0xA1, count: 32)
    private var suites: [String] = []

    override func tearDown() async throws {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        try await super.tearDown()
    }

    private func store() -> KeyRotationStore {
        let suite = "key-rotation-runner-\(UUID().uuidString)"
        suites.append(suite)
        return KeyRotationStore(defaults: UserDefaults(suiteName: suite)!)
    }

    private struct Down: Error {}

    /// Everything outside the runner, scripted.
    private final class World {
        var seed: Data
        var epoch: UInt64 = 20
        var roster: [Data]
        var grant: String? = "grant"
        var removeResult: DeviceRemovalPlan.Result = .removed
        var commitError: Error?
        var phraseProtected = true
        var failing: Set<String> = []
        var discoveryErrors: [Error] = []
        private(set) var events: [String] = []
        private(set) var sealedAt: [UInt64] = []
        private(set) var pending: [PendingKeyRotation] = []
        private(set) var committedRevokes: [[Data]] = []
        private(set) var committedProofs: [Data] = []
        private(set) var published: [(attestation: Data?, withProof: Bool)] = []
        private(set) var dgRoot: [(attestation: Data?, generation: UInt64?)] = []
        private(set) var freshAuthDropped = 0
        let material: KeyRotationMaterial
        let attestation = Data(repeating: 0xA7, count: KeyResetResult.attestationLength)
        var phrase: String?

        init(seed: Data, roster: [Data]) throws {
            self.seed = seed
            self.roster = roster
            self.material = try KeyRotationMaterial.generate()
        }

        func note(_ event: String) { events.append(event) }
        func sealed(_ epoch: UInt64) { sealedAt.append(epoch) }
        func keep(_ pending: PendingKeyRotation) { self.pending.append(pending) }
        func committed(revokes: [Data], proof: Data) {
            committedRevokes.append(revokes)
            committedProofs.append(proof)
        }
        func publishedIdentity(_ attestation: Data?, _ withProof: Bool) {
            published.append((attestation, withProof))
        }
        func transferred(_ attestation: Data?, _ generation: UInt64?) { dgRoot.append((attestation, generation)) }
        func droppedFreshAuth() { freshAuthDropped += 1 }
        func nextDiscoveryError() -> Error? { discoveryErrors.isEmpty ? nil : discoveryErrors.removeFirst() }
    }

    private func ports(_ world: World) -> KeyRotationRunner.Ports {
        let accountId = self.accountId
        let accountIdB64 = self.accountIdB64
        let own = self.own
        let target = self.target
        func step(_ name: String) throws {
            world.note(name)
            if world.failing.contains(name) { throw Down() }
        }
        return KeyRotationRunner.Ports(
            session: {
                KeyRotationRunner.Session(
                    accountId: accountId, accountIdB64: accountIdB64,
                    accountIdentitySeed: world.seed, ownDeviceId: own)
            },
            sessionToken: { "tok" },
            freshGrant: { world.grant },
            removeDevice: { deviceId, _ in
                world.note("remove")
                if world.removeResult == .removed {
                    world.roster.removeAll { $0 == deviceId }
                    world.epoch += 1
                }
                return world.removeResult
            },
            deviceGroupEpoch: { world.epoch },
            deviceGroupRoster: { world.roster },
            sendToDeviceGroup: { _ in
                world.note("announce")
                world.sealed(world.epoch)
                return world.epoch
            },
            generate: { world.material },
            signProof: { old, new, issued, expires in
                XCTAssertEqual(expires - issued, KeyRotationWire.proofWindowSecs, "a 14-day window")
                return try signSupersessionProof(
                    oldEd25519Seed: try Identity.fromSeed(seed: old).ed25519Seed(),
                    newEd25519Seed: try Identity.fromSeed(seed: new).ed25519Seed(),
                    issuedAtUnixSecs: issued, expiresAtUnixSecs: expires)
            },
            composeEnvelope: { preparedAtMs, entropy, proof in
                try composeKeyRotationEnvelope(
                    preparedAtMs: preparedAtMs, newEntropy: entropy, supersessionProof: proof)
            },
            storeOwnPending: { pending, _ in
                world.note("own-pending")
                world.keep(pending)
            },
            commit: { _, _, material, proof, revokes in
                world.note("commit")
                if let error = world.commitError { throw error }
                world.committed(revokes: revokes, proof: proof)
                XCTAssertEqual(material.newSignaturePubkey, world.material.newSignaturePubkey)
                return KeyResetResult(
                    attestation: world.attestation, session: .fixture(access: "tok-new", generation: 4))
            },
            adopt: { material, _ in
                try step("adopt")
                world.seed = material.seed
                world.phrase = material.phrase
            },
            publishIdentity: { _, attestation, withProof in
                world.note(withProof ? "discovery" : "discovery-no-proof")
                world.publishedIdentity(attestation, withProof)
                if let error = world.nextDiscoveryError() { throw error }
                if world.failing.contains("discovery") { throw Down() }
            },
            republishKeyPackage: { try step("keypackage") },
            transferDeviceGroupRoot: { attestation, generation in
                world.transferred(attestation, generation)
                try step("dg-root")
            },
            transferOwnedJournals: { try step("journals") },
            handOverChannels: { _ in try step("channels") },
            rekeyReach: { try step("reach") },
            resendProfile: { try step("profile") },
            commitSavingPhrase: { _, _, material, proof in
                world.note("commit-saving-phrase")
                if let error = world.commitError { throw error }
                world.committed(revokes: [], proof: proof)
                XCTAssertEqual(material.newSignaturePubkey, world.material.newSignaturePubkey)
                return KeyResetResult(
                    attestation: world.attestation, session: .fixture(access: "tok-new", generation: 4))
            },
            phraseWasSaved: { _ in world.note("phrase-saved") },
            phraseProtected: { _ in world.phraseProtected },
            resealBackup: { try step("backup") },
            devicePhrase: { _ in world.phrase },
            dropFreshAuth: { world.droppedFreshAuth() },
            nowMs: { 1_700_000_000_000 }
        )
    }

    private func make(roster: [Data]? = nil) throws -> (KeyRotationRunner, World, KeyRotationStore) {
        let world = try World(seed: oldSeed, roster: roster ?? [own, target])
        let store = store()
        return (KeyRotationRunner(ports: ports(world), store: store), world, store)
    }

    func testTheWholeRotationRunsInOrderAndShowsTheNewWords() async throws {
        let (runner, world, store) = try make()

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase))
        XCTAssertEqual(
            world.events,
            ["remove", "own-pending", "announce", "commit", "adopt", "discovery", "keypackage", "dg-root",
             "journals", "channels", "reach", "profile", "backup"])
        // The envelope left at the epoch the removal produced.
        XCTAssertEqual(world.sealedAt, [21])
        // This device's own copy was kept before the envelope left.
        XCTAssertEqual(world.pending.map(\.newSignaturePubkey), [world.material.newSignaturePubkey])
        XCTAssertEqual(world.pending.first?.newEntropy, world.material.entropy)
        // The removed device is revoked with the commit too.
        XCTAssertEqual(world.committedRevokes, [[target]])
        // The proof is by the OLD key over the new one.
        let proof = try XCTUnwrap(world.committedProofs.first)
        XCTAssertEqual(proof.count, KeyRotationWire.proofLength)
        XCTAssertEqual(
            try verifySupersession(
                proofBytes: proof,
                trustedOldPubkey: try Identity.fromSeed(seed: oldSeed).signaturePublicKey(),
                nowUnixSecs: 1_700_000_000),
            world.material.newSignaturePubkey)
        // The directory upload carries the proof AND the attestation; the
        // DeviceGroup root transfer the attestation and its generation.
        XCTAssertEqual(world.published.count, 1)
        XCTAssertEqual(world.published.first?.attestation, world.attestation)
        XCTAssertEqual(world.published.first?.withProof, true)
        XCTAssertEqual(world.dgRoot.first?.attestation, world.attestation)
        XCTAssertEqual(world.dgRoot.first?.generation, 4)
        XCTAssertEqual(world.freshAuthDropped, 1, "a rotation spends the proof")
        // The words are still to be confirmed.
        XCTAssertEqual(runner.resume, .showWords)
        runner.acknowledgePhrase()
        XCTAssertNil(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(runner.resume, .nothing)
    }

    // MARK: - ADR-0075: new words only for a phrase-protected account

    func testADeviceRotationOnAnAccountWithoutASavedPhraseShowsNoWordsAnywhere() async throws {
        let (runner, world, store) = try make()
        world.phraseProtected = false

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotatedNoWords)
        // The keys were changed and published all the same.
        XCTAssertTrue(world.events.contains("commit"))
        XCTAssertTrue(world.events.contains("backup"))
        XCTAssertFalse(world.events.contains("commit-saving-phrase"), "phrase_saved is the save-phrase flow's")
        XCTAssertNil(runner.wordsToShow())
        XCTAssertEqual(runner.resume, .nothing, "nothing waits for words to be confirmed")
        XCTAssertNil(store.load(accountIdB64: accountIdB64))
    }

    func testNoWithheldWordsStateWithoutASavedPhraseWhileThePublishIsOwed() async throws {
        let (runner, world, store) = try make()
        world.phraseProtected = false
        world.failing = ["discovery"]

        let result = await runner.rotate(deviceId: target, attestation: nil)
        XCTAssertEqual(result, .rotatedNoWords, "not the withheld-words state")
        XCTAssertNil(runner.wordsToShow())
        // The owed steps are still owed, and finish in the background.
        XCTAssertNotNil(store.load(accountIdB64: accountIdB64))
        world.failing = []
        await runner.resumeOwed()
        XCTAssertNil(store.load(accountIdB64: accountIdB64))
        XCTAssertNil(runner.wordsToShow())
        XCTAssertEqual(runner.resume, .nothing)
    }

    func testSavingAPhraseShowsItsWordsEvenThoughTheAccountWasNotPhraseProtected() async throws {
        let (runner, world, _) = try make()
        world.phraseProtected = false
        let result = await runner.savePhrase()
        XCTAssertEqual(result, .rotated(phrase: world.material.phrase))
    }

    // MARK: - ADR-0075 section 3: "Save a recovery phrase"

    func testSavingAPhraseRotatesWithoutRemovingADeviceAndShowsTheNewWordsLast() async throws {
        let (runner, world, store) = try make()

        let result = await runner.savePhrase()

        // The new words come back only after the commit and the publish.
        XCTAssertEqual(result, .rotated(phrase: world.material.phrase))
        XCTAssertEqual(
            world.events,
            ["own-pending", "announce", "commit-saving-phrase", "phrase-saved", "adopt", "discovery",
             "keypackage", "dg-root", "journals", "channels", "reach", "profile", "backup"])
        XCTAssertFalse(world.events.contains("remove"), "no device is removed")
        XCTAssertFalse(world.events.contains("commit"), "the commit carries phrase_saved")
        XCTAssertEqual(world.committedRevokes, [[]], "no device is revoked with it")
        XCTAssertEqual(world.roster, [own, target], "the other devices are kept")
        XCTAssertEqual(world.freshAuthDropped, 1)
        let record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertTrue(record.savesPhrase)
        XCTAssertEqual(record.targetDeviceIdB64, "")
        XCTAssertEqual(runner.resume, .showWords)
        runner.acknowledgePhrase()
        XCTAssertNil(store.load(accountIdB64: accountIdB64))
    }

    func testSavingAPhraseWithholdsTheWordsWhileThePublishIsOwed() async throws {
        let (runner, world, _) = try make()
        world.failing = ["discovery"]

        let saved1 = await runner.savePhrase()
        XCTAssertEqual(saved1, .rotatedPublishOwed)
        XCTAssertNil(runner.wordsToShow(), "the words wait for the directory")

        world.failing = []
        await runner.resumeOwed()
        XCTAssertEqual(runner.wordsToShow(), world.material.phrase)
    }

    func testAPhraseThatCouldNotBeSavedChangesNothingAndOwesNothing() async throws {
        // The commit is refused: nothing changed on the server.
        let (runner, world, store) = try make()
        world.commitError = AuthClientError.http(
            status: 403, endpoint: "/account/keys/rotate/finish", body: #"{"error":"account-key-required"}"#)
        let saved2 = await runner.savePhrase()
        XCTAssertEqual(saved2, .removedNotRotated(.accountKeyRequired))
        XCTAssertNil(store.load(accountIdB64: accountIdB64), "nothing is owed")
        XCTAssertEqual(runner.resume, .nothing, "no line about a removed device")
        XCTAssertFalse(world.events.contains("phrase-saved"))
        XCTAssertNil(world.phrase, "no new words exist on the device")

        // The proof ran out: the block is asked for again, nothing is owed.
        world.commitError = AuthClientError.http(
            status: 403, endpoint: "/account/keys/rotate/start", body: #"{"error":"fresh-auth-required"}"#)
        let saved3 = await runner.savePhrase()
        XCTAssertEqual(saved3, .freshAuthRequired)
        XCTAssertNil(store.load(accountIdB64: accountIdB64))

        // No proof at all: nothing is sent.
        world.commitError = nil
        world.grant = nil
        let before = world.events.count
        let saved4 = await runner.savePhrase()
        XCTAssertEqual(saved4, .freshAuthRequired)
        XCTAssertEqual(world.events.count, before)
    }

    func testAnUnansweredPhraseCommitIsKeptWithoutTheRemovedDeviceLine() async throws {
        let (runner, world, store) = try make()
        world.commitError = AuthClientError.network(
            endpoint: "/account/keys/rotate/finish", underlying: URLError(.timedOut))

        let saved5 = await runner.savePhrase()
        XCTAssertEqual(saved5, .removedNotRotated(.offline))
        // Unknown whether it landed: the record stays, and says nothing.
        XCTAssertEqual(store.load(accountIdB64: accountIdB64)?.phase, .announced)
        XCTAssertEqual(runner.resume, .nothing)

        // It did land: this device adopted its own envelope.
        world.seed = world.material.seed
        world.phrase = world.material.phrase
        runner.reload()
        XCTAssertEqual(runner.resume, .commitLandedUnseen)
        await runner.resumeOwed()
        XCTAssertTrue(world.events.contains("phrase-saved"), "the account is phrase-protected from here")
        XCTAssertEqual(runner.wordsToShow(), world.material.phrase)
    }

    func testTheResumeOfATargetlessRecordNeverReadsAsARemovedDevice() {
        var record = KeyRotationRecord(targetDeviceIdB64: "", startedAtMs: 1)
        record.phraseSaved = true
        record.phase = .removed
        XCTAssertEqual(KeyRotationPlan.resume(record, heldAccountKey: nil), .nothing)
        record.phase = .announced
        record.newSignaturePubkey = Data(repeating: 1, count: 32)
        XCTAssertEqual(KeyRotationPlan.resume(record, heldAccountKey: Data(repeating: 2, count: 32)), .nothing)
        XCTAssertEqual(
            KeyRotationPlan.resume(record, heldAccountKey: Data(repeating: 1, count: 32)), .commitLandedUnseen)
        // A device rotation is unchanged.
        var device = KeyRotationRecord(targetDeviceIdB64: "AAAA", startedAtMs: 1)
        device.phase = .removed
        XCTAssertEqual(KeyRotationPlan.resume(device, heldAccountKey: nil), .removedNotRotated)
        // An older record without the field still decodes, as a device rotation.
        let old = try? JSONDecoder().decode(
            KeyRotationRecord.self,
            from: Data(#"{"target_device_id":"AAAA","started_at_ms":1,"phase":"removed","discovery_done":false,"key_package_done":false,"device_group_root_done":false,"journals_done":false,"channels_done":false,"reach_done":false,"backup_done":false,"phrase_acknowledged":false}"#.utf8))
        XCTAssertEqual(old?.savesPhrase, false)
    }

    func testTheOwedRecordIsWrittenBeforeAnythingIsSent() async throws {
        let (runner, world, store) = try make()
        world.removeResult = .notRevoked(.offline)

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .notRemoved(.notRevoked(.offline)))
        XCTAssertEqual(world.events, ["remove"], "nothing of the rotation ran")
        XCTAssertNil(store.load(accountIdB64: accountIdB64), "nothing was removed: nothing is owed")
    }

    func testACommitRefusedLeavesTheDeviceRemovedAndTheKeysUnchanged() async throws {
        let (runner, world, store) = try make()
        world.commitError = AuthClientError.http(
            status: 403, endpoint: "/account/keys/rotate/finish", body: #"{"error":"account-key-required"}"#)

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .removedNotRotated(.accountKeyRequired))
        XCTAssertEqual(world.events, ["remove", "own-pending", "announce", "commit"])
        XCTAssertEqual(world.seed, oldSeed, "nothing was adopted")
        XCTAssertEqual(world.published.count, 0)
        let record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(record.phase, .removed)
        XCTAssertNil(record.newSignaturePubkey, "the next try makes a new phrase")
        XCTAssertEqual(runner.resume, .removedNotRotated)
        XCTAssertFalse(store.publishOwed(accountIdB64: accountIdB64))

        // `Not now`: the attempt is forgotten; the device stays removed.
        runner.abandonUnstarted()
        XCTAssertNil(store.load(accountIdB64: accountIdB64))
    }

    func testTryAgainAfterARefusedCommitDoesNotRemoveTheDeviceASecondTime() async throws {
        let (runner, world, _) = try make()
        world.commitError = AuthClientError.http(
            status: 429, endpoint: "/account/keys/rotate/finish", body: #"{"error":"too-many-attempts"}"#)
        _ = await runner.rotate(deviceId: target, attestation: nil)
        world.commitError = nil

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase))
        XCTAssertEqual(world.events.filter { $0 == "remove" }.count, 1, "the device was removed once")
        XCTAssertEqual(world.sealedAt, [21, 21], "both envelopes left past the removal")
    }

    func testACommitWithNoAnswerKeepsTheRecordAnnounced() async throws {
        let (runner, world, store) = try make()
        world.commitError = AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .removedNotRotated(.offline))
        let record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(record.phase, .announced, "it may have landed: the next landing finds out")
        XCTAssertEqual(record.newSignaturePubkey, world.material.newSignaturePubkey)
    }

    func testACommitWhoseAnswerWasLostIsFinishedOnceThisDeviceHoldsTheNewKey() async throws {
        let (runner, world, store) = try make()
        world.commitError = AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))
        _ = await runner.rotate(deviceId: target, attestation: nil)
        // The commit had landed; this device adopted its own envelope like a
        // kept device (``AuthRepository/keysSuperseded``).
        world.seed = world.material.seed
        world.phrase = world.material.phrase
        runner.reload()
        XCTAssertEqual(runner.resume, .commitLandedUnseen)

        await runner.resumeOwed()

        let record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(record.phase, .committed)
        XCTAssertNil(record.attestation, "the answer that carried it never arrived")
        XCTAssertEqual(world.published.last?.withProof, true)
        XCTAssertNil(world.published.last?.attestation ?? nil, "the upload goes by proof alone")
        XCTAssertNil(world.dgRoot.last?.attestation ?? nil, "the transfer goes plain")
        XCTAssertEqual(KeyRotationPlan.remaining(record), [])
    }

    func testAnOwedPublishWithholdsTheWordsAndResumesWithTheSameAttestation() async throws {
        let (runner, world, store) = try make()
        world.failing = ["discovery"]

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotatedPublishOwed)
        XCTAssertNil(runner.wordsToShow(), "the words are withheld while the publish is owed")
        XCTAssertTrue(store.publishOwed(accountIdB64: accountIdB64))
        XCTAssertFalse(world.events.contains("keypackage"), "a row published before the upload would be deleted by it")
        XCTAssertEqual(runner.resume, .finishOwed)

        world.failing = []
        await runner.resumeOwed()

        XCTAssertFalse(store.publishOwed(accountIdB64: accountIdB64))
        XCTAssertEqual(runner.wordsToShow(), world.material.phrase)
        XCTAssertEqual(world.published.map(\.attestation), [world.attestation, world.attestation])
        XCTAssertEqual(world.events.filter { $0 == "commit" }.count, 1, "never a second rotation")
        XCTAssertEqual(world.events.filter { $0 == "remove" }.count, 1)
    }

    func testAStepThatDidNotLandDoesNotHoldTheLaterOnesBack() async throws {
        let (runner, world, store) = try make()
        world.failing = ["journals", "channels"]

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase), "the words wait only for the publish")
        let record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(KeyRotationPlan.remaining(record), [.journals, .channels])
        XCTAssertTrue(world.events.contains("reach"))
        XCTAssertTrue(world.events.contains("backup"))
    }

    func testABackupThatDidNotUploadStaysOwedAndIsRetriedAlone() async throws {
        let (runner, world, store) = try make()
        world.failing = ["backup"]

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase), "the words wait only for the publish")
        var record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertFalse(record.backupDone)
        XCTAssertEqual(KeyRotationPlan.remaining(record), [.backup])
        XCTAssertFalse(KeyRotationPlan.isComplete(record), "a rotation whose backup did not upload is not done")

        // Still failing: still owed, and nothing else is run again.
        let before = world.events.count
        await runner.resumeOwed()
        XCTAssertEqual(Array(world.events[before...]), ["backup"])
        record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(KeyRotationPlan.remaining(record), [.backup])

        world.failing = []
        let beforeRetry = world.events.count
        await runner.resumeOwed()

        XCTAssertEqual(Array(world.events[beforeRetry...]), ["backup"], "only the owed step is retried")
        // A record that became complete is dropped by the store.
        if let after = store.load(accountIdB64: accountIdB64) {
            XCTAssertTrue(after.backupDone)
            XCTAssertEqual(KeyRotationPlan.remaining(after), [])
        }
        XCTAssertEqual(world.events.filter { $0 == "commit" }.count, 1, "never a second rotation")
    }

    func testABackupThatUploadedIsRecorded() async throws {
        let (runner, world, store) = try make()

        _ = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(world.events.filter { $0 == "backup" }.count, 1)
        if let record = store.load(accountIdB64: accountIdB64) {
            XCTAssertTrue(record.backupDone)
            XCTAssertFalse(KeyRotationPlan.remaining(record).contains(.backup))
        }
    }

    // ADR-0074 section 3.5a rule 3 — the profile re-send is a step of the
    // owed record: a send that did not land keeps it owed and is retried.

    func testAProfileReSendThatDidNotLandStaysOwedAndIsRetriedAlone() async throws {
        let (runner, world, store) = try make()
        world.failing = ["profile"]

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase), "the words do not wait for it")
        var record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertNotEqual(record.profileDone, true)
        XCTAssertEqual(KeyRotationPlan.remaining(record), [.profile])
        XCTAssertFalse(KeyRotationPlan.isComplete(record), "a rotation whose profile was not re-sent is not done")
        XCTAssertTrue(world.events.contains("backup"), "a later step is not held back")

        let before = world.events.count
        await runner.resumeOwed()
        XCTAssertEqual(Array(world.events[before...]), ["profile"], "still failing: only it is tried again")
        record = try XCTUnwrap(store.load(accountIdB64: accountIdB64))
        XCTAssertEqual(KeyRotationPlan.remaining(record), [.profile])

        world.failing = []
        let beforeRetry = world.events.count
        await runner.resumeOwed()
        XCTAssertEqual(Array(world.events[beforeRetry...]), ["profile"])
        if let after = store.load(accountIdB64: accountIdB64) {
            XCTAssertEqual(after.profileDone, true)
            XCTAssertEqual(KeyRotationPlan.remaining(after), [])
        }
    }

    func testTheProfileIsNotReSentBeforeTheReachKeysAreOnTheNewMailbox() async throws {
        let (runner, world, store) = try make()
        world.failing = ["reach"]

        _ = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertFalse(world.events.contains("profile"), "a grant must name a key the mailbox already knows")
        XCTAssertEqual(
            KeyRotationPlan.remaining(try XCTUnwrap(store.load(accountIdB64: accountIdB64))),
            [.reach, .profile, .grants])

        world.failing = []
        let before = world.events.count
        await runner.resumeOwed()
        XCTAssertEqual(Array(world.events[before...]), ["reach", "profile"])
    }

    func testARecordWrittenBeforeTheProfileStepExistedOwesIt() throws {
        var record = KeyRotationRecord(targetDeviceIdB64: "ZGV2", startedAtMs: 1)
        record.phase = .committed
        record.discoveryDone = true
        record.keyPackageDone = true
        record.deviceGroupRootDone = true
        record.journalsDone = true
        record.channelsDone = true
        record.reachDone = true
        record.grantsDone = true
        record.backupDone = true
        var json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(record)) as? [String: Any])
        json["profile_done"] = nil
        let old = try JSONDecoder().decode(
            KeyRotationRecord.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertEqual(KeyRotationPlan.remaining(old), [.profile])
    }

    func testWithNoSnapshotTheBackupStaysOwedAndNothingIsUploaded() async throws {
        let world = try World(seed: oldSeed, roster: [own, target])
        let store = store()
        var snapshot: Data?
        var bootstrapped = true
        var uploads: [(snapshot: Data, groups: [String])] = []
        var all = ports(world)
        all.resealBackup = {
            try await KeyRotationBackup.reseal(
                chatBootstrapped: bootstrapped,
                snapshot: { snapshot },
                knownGroupIdsHex: { ["aa"] },
                upload: { taken, groups in uploads.append((taken, groups)) })
        }
        let runner = KeyRotationRunner(ports: all, store: store)

        _ = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertTrue(uploads.isEmpty, "an empty snapshot must never overwrite the server's copy")
        XCTAssertEqual(KeyRotationPlan.remaining(try XCTUnwrap(store.load(accountIdB64: accountIdB64))), [.backup])

        // An empty snapshot is no snapshot.
        snapshot = Data()
        await runner.resumeOwed()
        XCTAssertTrue(uploads.isEmpty)
        XCTAssertEqual(KeyRotationPlan.remaining(try XCTUnwrap(store.load(accountIdB64: accountIdB64))), [.backup])

        // A snapshot from a chat layer that has not bootstrapped is not used.
        snapshot = Data([1, 2, 3])
        bootstrapped = false
        await runner.resumeOwed()
        XCTAssertTrue(uploads.isEmpty)
        XCTAssertEqual(KeyRotationPlan.remaining(try XCTUnwrap(store.load(accountIdB64: accountIdB64))), [.backup])

        bootstrapped = true
        await runner.resumeOwed()
        XCTAssertEqual(uploads.count, 1)
        XCTAssertEqual(uploads.first?.snapshot, Data([1, 2, 3]))
        XCTAssertEqual(uploads.first?.groups, ["aa"])
        if let after = store.load(accountIdB64: accountIdB64) {
            XCTAssertTrue(after.backupDone)
        }
    }

    func testTheBackupStepThrowsWhatTheUploadThrewAndNamesWhyItDidNotRun() async {
        do {
            try await KeyRotationBackup.reseal(
                chatBootstrapped: true, snapshot: { Data([9]) }, knownGroupIdsHex: { [] },
                upload: { _, _ in throw Down() })
            XCTFail("a failed upload must throw")
        } catch { XCTAssertTrue(error is Down) }
        do {
            try await KeyRotationBackup.reseal(
                chatBootstrapped: false, snapshot: { Data([9]) }, knownGroupIdsHex: { [] },
                upload: { _, _ in XCTFail("no upload before the chat layer is ready") })
            XCTFail("must throw")
        } catch { XCTAssertEqual(error as? KeyRotationBackup.NotReady, .chatNotBootstrapped) }
        do {
            try await KeyRotationBackup.reseal(
                chatBootstrapped: true, snapshot: { nil }, knownGroupIdsHex: { [] },
                upload: { _, _ in XCTFail("no upload without a snapshot") })
            XCTFail("must throw")
        } catch { XCTAssertEqual(error as? KeyRotationBackup.NotReady, .noSnapshot) }
    }

    func testTheDirectoryRefusingTheProofFallsBackToTheAttestationAlone() async throws {
        let (runner, world, _) = try make()
        world.discoveryErrors = [
            DiscoveryClientError.http(
                status: 400, endpoint: "/discovery/identity", body: #"{"error":"supersession-proof-invalid"}"#),
        ]

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase))
        XCTAssertEqual(world.published.map(\.withProof), [true, false])
        XCTAssertEqual(world.published.last?.attestation, world.attestation)
    }

    func testAnUploadThatAlreadyLandedCountsAsDone() async throws {
        let (runner, world, _) = try make()
        world.discoveryErrors = [
            DiscoveryClientError.http(
                status: 409, endpoint: "/discovery/identity", body: #"{"error":"key-generation-not-newer"}"#),
        ]

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .rotated(phrase: world.material.phrase))
        XCTAssertEqual(world.published.count, 1)
    }

    func testWithoutAFreshProofNothingStarts() async throws {
        let (runner, world, _) = try make()
        world.grant = nil

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .freshAuthRequired)
        XCTAssertEqual(world.events, [])
    }

    func testWhenTheDeviceGroupCannotBeReadNothingIsRemovedAndNothingIsOwed() async throws {
        let world = try World(seed: oldSeed, roster: [own, target])
        let store = store()
        var failing = ports(world)
        failing.deviceGroupEpoch = { throw Down() }
        let runner = KeyRotationRunner(ports: failing, store: store)

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .notRemoved(.notRevoked(.failed)))
        XCTAssertEqual(world.events, [], "the removal never ran")
        XCTAssertNil(store.load(accountIdB64: accountIdB64), "no record claims a removal that did not happen")
        XCTAssertEqual(runner.resume, .nothing)
    }

    func testADeviceStillInTheDeviceGroupStopsTheRotationBeforeAnySeedExists() async throws {
        let (runner, world, store) = try make()
        // Signed out at the server; the DeviceGroup step is still owed.
        world.removeResult = .removedPending

        let result = await runner.rotate(deviceId: target, attestation: nil)

        XCTAssertEqual(result, .removedNotRotated(nil))
        XCTAssertEqual(world.events, ["remove"], "no phrase, no envelope, no commit")
        XCTAssertEqual(world.pending.count, 0)
        XCTAssertEqual(store.load(accountIdB64: accountIdB64)?.phase, .removed)
    }
}

// MARK: - The sheet's second choice

final class RotateSheetRulesTests: XCTestCase {

    func testTheMainButtonWaitsForAReason() {
        XCTAssertFalse(RotateSheetRules.canConfirm(reason: nil, busy: false, blocked: false, proofReady: true))
        XCTAssertTrue(RotateSheetRules.canConfirm(reason: .unused, busy: false, blocked: false, proofReady: true))
        XCTAssertTrue(RotateSheetRules.canConfirm(reason: .stolen, busy: false, blocked: false, proofReady: true))
        XCTAssertFalse(RotateSheetRules.canConfirm(reason: .stolen, busy: true, blocked: false, proofReady: true))
        XCTAssertFalse(RotateSheetRules.canConfirm(reason: .stolen, busy: false, blocked: true, proofReady: true))
        XCTAssertFalse(RotateSheetRules.canConfirm(reason: .stolen, busy: false, blocked: false, proofReady: false))
    }

    func testTheButtonNamesWhatItDoes() {
        XCTAssertEqual(RotateSheetRules.confirmKey(.unused), "settings.devices.remove.confirm")
        XCTAssertEqual(RotateSheetRules.confirmKey(nil), "settings.devices.remove.confirm")
        XCTAssertEqual(RotateSheetRules.confirmKey(.stolen), "devices.rotate.confirm")
    }

    func testEveryEndOfTheRunHasItsState() {
        XCTAssertEqual(RotateSheetRules.state(for: .rotated(phrase: "a b")), .words("a b"))
        XCTAssertEqual(RotateSheetRules.state(for: .rotatedPublishOwed), .publishOwed)
        XCTAssertEqual(RotateSheetRules.state(for: .removedNotRotated(nil)), .notStarted(line: nil))
        XCTAssertEqual(
            RotateSheetRules.state(for: .removedNotRotated(.offline)),
            .notStarted(line: "common.error.offline"))
        XCTAssertEqual(
            RotateSheetRules.state(for: .removedNotRotated(.rateLimited(retryAfterSecs: 3))),
            .notStarted(line: "common.error.too_many_attempts"))
        XCTAssertNil(RotateSheetRules.state(for: .freshAuthRequired), "the proof block comes back")
        XCTAssertNil(
            RotateSheetRules.state(for: .notRemoved(.notRevoked(.offline))),
            "nothing was removed: the plain removal's line")
    }

    func testOneStepIsActiveAtATime() {
        XCTAssertEqual(RotateSheetRules.mark(of: .removingDevice, active: .removingDevice), .active)
        XCTAssertEqual(RotateSheetRules.mark(of: .changingKeys, active: .removingDevice), .waiting)
        XCTAssertEqual(RotateSheetRules.mark(of: .removingDevice, active: .changingKeys), .done)
        XCTAssertEqual(RotateSheetRules.mark(of: .changingKeys, active: .updatingDevices), .done)
        XCTAssertEqual(RotateSheetRules.mark(of: .updatingDevices, active: .updatingDevices), .active)
        XCTAssertEqual(RotateSheetRules.mark(of: .updatingDevices, active: nil), .waiting)
    }

    func testTheReasonPairLocksOnceAnythingRuns() {
        XCTAssertFalse(RotateSheetRules.reasonLocked(busy: false, rotate: .idle))
        XCTAssertTrue(RotateSheetRules.reasonLocked(busy: true, rotate: .idle))
        XCTAssertTrue(RotateSheetRules.reasonLocked(busy: false, rotate: .notStarted(line: nil)))
        XCTAssertTrue(RotateSheetRules.reasonLocked(busy: false, rotate: .publishOwed))
    }

    func testEveryKeyTheSurfacesUseIsInTheCatalog() {
        for key in RotateDeviceCopy.all + KeysRotatedCopy.all + ["common.close", "common.dismiss"] {
            XCTAssertNotEqual(
                Localization.text(key, texts: ["username": "@alice"]), key,
                "\(key) is missing from the catalog")
        }
    }
}
