// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0074 part 3, third pass — a device paired AFTER a rotation of the
/// account keys, and the durable sent-requests list of section 3.5a rule 5.
///
///   * the pairing state bundle, format 2: round trip, format 1 refused, each
///     bound;
///   * the receive rule: what is stored, the read window counted from the
///     sender's adoption time, a contact's predecessors recorded as proven;
///   * the device binding: a device paired after a rotation derives the SAME
///     conversation account key as the device that paired it;
///   * the pure decisions of the sent-requests store.
@MainActor
final class PairedAfterRotationTests: XCTestCase {

    private var defaults: UserDefaults!
    private let suiteName = "lakko.tests.paired-after-rotation"
    private let account = "paired-after-rotation-test-account"
    private let peerAccount = Data(repeating: 0xB0, count: 16).base64EncodedString()

    override func setUp() {
        super.setUp()
        UserDefaults().removePersistentDomain(forName: suiteName)
        defaults = UserDefaults(suiteName: suiteName)
        cleanKeychain()
    }

    override func tearDown() {
        cleanKeychain()
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func cleanKeychain() {
        DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: account)
        KeychainJSONItem.delete(service: DeviceIdentityStore.bindingServiceTag, account: account)
        KeychainSentRequestsStore().clear(accountIdB64: account)
    }

    private func seed(_ byte: UInt8) -> Data { Data(repeating: byte, count: StateBundle.seedLength) }
    private func key(_ byte: UInt8) -> String { Data(repeating: byte, count: 32).base64EncodedString() }
    private func publicKey(_ seed: Data) throws -> Data { try Identity.fromSeed(seed: seed).signaturePublicKey() }

    private func json(
        v: Int = 4, own: String = "[]", pinPredecessors: String? = "[]"
    ) -> Data {
        let pinField = pinPredecessors.map { #","provenPredecessorsB64":\#($0)"# } ?? ""
        return Data("""
        {"v":\(v),"contacts":[],"pins":[{"accountIdB64":"\(peerAccount)",\
        "pinnedSignaturePubkeyB64":"\(key(0xC3))","rotationMarker":7\(pinField)}],\
        "knownGroupIdsB64":[],"ownPredecessors":\(own),\
        "groupNames":[],"profiles":[],"pendingContacts":[],"channelSeeds":[]}
        """.utf8)
    }

    private func ownJson(_ entries: [(seed: Data, adoptedAtMs: String)]) -> String {
        "[" + entries.map { #"{"seedB64":"\#($0.seed.base64EncodedString())","adoptedAtMs":\#($0.adoptedAtMs)}"# }
            .joined(separator: ",") + "]"
    }

    // MARK: - The codec

    func testAFormat2BundleRoundTripsWithPredecessors() throws {
        let bundle = StateBundle.V4(
            contacts: [],
            pins: [StateBundle.CarriedPin(
                accountIdB64: peerAccount, pinnedSignaturePubkeyB64: key(0xC3), rotationMarker: 7,
                provenPredecessorsB64: [key(0xC1), key(0xC2)])],
            knownGroupIdsB64: ["Z3JvdXA="],
            ownPredecessors: [
                StateBundle.OwnPredecessor(seedB64: seed(1).base64EncodedString(), adoptedAtMs: 1_000),
                StateBundle.OwnPredecessor(seedB64: seed(2).base64EncodedString(), adoptedAtMs: 2_000),
            ])
        XCTAssertEqual(bundle.v, 4)
        XCTAssertEqual(StateBundle.decode(StateBundle.encode(bundle)), bundle)

        // The contract's field names, read from another client's JSON.
        let wire = json(
            own: ownJson([(seed(1), "1000"), (seed(2), "2000")]),
            pinPredecessors: #"["\#(key(0xC1))","\#(key(0xC2))"]"#)
        let decoded = try XCTUnwrap(StateBundle.decode(wire))
        XCTAssertEqual(decoded.ownPredecessors, bundle.ownPredecessors)
        XCTAssertEqual(decoded.pins, bundle.pins)
    }

    func testFormat1AndEveryOtherVersionIsRefused() {
        XCTAssertNotNil(StateBundle.decode(json()))
        XCTAssertNil(StateBundle.decode(json(v: 1)), "flag day: no dual accept")
        XCTAssertNil(StateBundle.decode(json(v: 2)), "format 2 is refused too (bundle v3 flag day)")
        XCTAssertNil(StateBundle.decode(json(v: 3)), "format 3 is refused too (bundle v4 flag day)")
        XCTAssertNil(StateBundle.decode(json(v: 5)))
        XCTAssertNil(StateBundle.decode(
            Data(#"{"v":1,"contacts":[],"pins":[],"knownGroupIdsB64":[]}"#.utf8)))
    }

    func testEachBoundRefusesTheWholeBundle() {
        let eight = (1...8).map { (seed(UInt8($0)), "1") }
        let nine = (1...9).map { (seed(UInt8($0)), "1") }
        XCTAssertNotNil(StateBundle.decode(json(own: ownJson(eight))))
        XCTAssertNil(StateBundle.decode(json(own: ownJson(nine))), "more than 8 own predecessors")

        XCTAssertNil(
            StateBundle.decode(json(own: ownJson([(Data(repeating: 1, count: 63), "1")]))),
            "a seed that is not 64 bytes")
        XCTAssertNil(
            StateBundle.decode(json(own: ownJson([(Data(repeating: 1, count: 32), "1")]))),
            "a 32-byte key is not a seed")
        XCTAssertNil(
            StateBundle.decode(json(own: #"[{"seedB64":"not base64 !","adoptedAtMs":1}]"#)))
        XCTAssertNil(
            StateBundle.decode(json(own: ownJson([(seed(1), "1.5")]))), "a time that is not an integer")
        XCTAssertNil(
            StateBundle.decode(json(own: ownJson([(seed(1), "\"1\"")]))), "a time that is a string")
        XCTAssertNil(StateBundle.decode(json(own: #"[{"seedB64":"\#(seed(1).base64EncodedString())"}]"#)))

        let eightKeys = "[" + (1...8).map { #""\#(key(UInt8($0)))""# }.joined(separator: ",") + "]"
        let nineKeys = "[" + (1...9).map { #""\#(key(UInt8($0)))""# }.joined(separator: ",") + "]"
        XCTAssertNotNil(StateBundle.decode(json(pinPredecessors: eightKeys)))
        XCTAssertNil(StateBundle.decode(json(pinPredecessors: nineKeys)), "more than 8 per pin")
        XCTAssertNil(
            StateBundle.decode(json(pinPredecessors: #"["\#(Data(repeating: 1, count: 31).base64EncodedString())"]"#)),
            "a proven key that is not 32 bytes")
        XCTAssertNil(StateBundle.decode(json(pinPredecessors: nil)), "the field is part of the format")
        XCTAssertNil(
            StateBundle.decode(Data(#"{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[]}"#.utf8)),
            "ownPredecessors is part of the format")
    }

    // MARK: - The sending side

    func testTheSenderListsItsPredecessorsOldestFirstWithTheirAdoptionTimes() throws {
        let seeds = [seed(1), seed(2)]
        let times = [base64Encode(try publicKey(seed(1))): Int64(111), base64Encode(try publicKey(seed(2))): Int64(222)]
        let carried = StateBundle.carriedOwnPredecessors(seeds: seeds) { times[$0] ?? -1 }
        XCTAssertEqual(carried, [
            StateBundle.OwnPredecessor(seedB64: seed(1).base64EncodedString(), adoptedAtMs: 111),
            StateBundle.OwnPredecessor(seedB64: seed(2).base64EncodedString(), adoptedAtMs: 222),
        ])
        XCTAssertEqual(StateBundle.carriedOwnPredecessors(seeds: []) { _ in 0 }, [])

        // More than the bound: the oldest (the conversation key) and the
        // newest ones are kept, so the bundle is never one the receiver
        // refuses.
        let ten = (1...10).map { seed(UInt8($0)) }
        let kept = StateBundle.carriedOwnPredecessors(seeds: ten) { _ in 0 }.map(\.seedB64)
        XCTAssertEqual(kept.count, StateBundle.maxPredecessors)
        XCTAssertEqual(kept.first, seed(1).base64EncodedString())
        XCTAssertEqual(kept.last, seed(10).base64EncodedString())
        XCTAssertEqual(kept[1], seed(4).base64EncodedString())
    }

    func testTheSenderListsAContactsProvenPredecessorsOldestFirst() {
        var proven = ProvenPredecessors()
        proven.record(oldKeyB64: key(0xC1), newKeyB64: key(0xC2), evidence: .verifiedProof)
        proven.record(oldKeyB64: key(0xC2), newKeyB64: key(0xC3), evidence: .verifiedProof)
        XCTAssertEqual(
            StateBundle.carriedProvenPredecessors(of: key(0xC3), in: proven), [key(0xC1), key(0xC2)])
        XCTAssertEqual(StateBundle.carriedProvenPredecessors(of: key(0xD0), in: proven), [])

        let built = StateBundle.build(
            accountIdB64: account,
            pinStore: PeerPinStore(defaults: defaults, ownerAccountIdB64: account),
            ownPredecessors: InMemoryOwnPredecessorStore(),
            adoptions: PredecessorMailbox.AdoptionStore(defaults: defaults),
            provenStore: ProvenPredecessorStore(ownerAccountIdB64: account, defaults: defaults))
        XCTAssertEqual(built.v, 4)
        XCTAssertEqual(built.ownPredecessors, [], "an account that never rotated sends an empty array")
    }

    func testBuildReadsTheSendersOwnStores() throws {
        let store = InMemoryOwnPredecessorStore()
        try store.add(seed(1), accountIdB64: account)
        try store.add(seed(2), accountIdB64: account)
        let adoptions = PredecessorMailbox.AdoptionStore(defaults: defaults)
        adoptions.recordIfAbsent(
            accountIdB64: account, keyB64: base64Encode(try publicKey(seed(1))), nowMs: 500)

        let built = StateBundle.build(
            accountIdB64: account,
            pinStore: PeerPinStore(defaults: defaults, ownerAccountIdB64: account),
            ownPredecessors: store, adoptions: adoptions,
            provenStore: ProvenPredecessorStore(ownerAccountIdB64: account, defaults: defaults),
            nowMs: 9_000)

        XCTAssertEqual(built.ownPredecessors.map(\.seedB64), [seed(1), seed(2)].map { $0.base64EncodedString() })
        XCTAssertEqual(
            built.ownPredecessors.map(\.adoptedAtMs), [500, 9_000],
            "a key with no recorded adoption is stamped now, as the mailbox poll stamps it")
        XCTAssertNotNil(StateBundle.decode(StateBundle.encode(built)))
    }

    // MARK: - The receive rule

    func testTheReceiverHoldsThePredecessorsAsAKeptDeviceWould() throws {
        let store = InMemoryOwnPredecessorStore()
        let adoptions = PredecessorMailbox.AdoptionStore(defaults: defaults)
        let carried = [
            StateBundle.OwnPredecessor(seedB64: seed(1).base64EncodedString(), adoptedAtMs: 1_000),
            StateBundle.OwnPredecessor(seedB64: seed(2).base64EncodedString(), adoptedAtMs: 2_000),
        ]

        let held = try StateBundle.storeOwnPredecessors(
            carried, accountIdB64: account, store: store, adoptions: adoptions)

        XCTAssertEqual(held, 2)
        XCTAssertEqual(store.load(accountIdB64: account), [seed(1), seed(2)], "oldest first, newest last")
        let k1 = base64Encode(try publicKey(seed(1)))
        let k2 = base64Encode(try publicKey(seed(2)))
        XCTAssertEqual(adoptions.adoptedAtMs(accountIdB64: account, keyB64: k1), 1_000)
        XCTAssertEqual(adoptions.adoptedAtMs(accountIdB64: account, keyB64: k2), 2_000)

        // The read window of each mailbox counts from the SENDER's adoption,
        // not from the pairing.
        let window = PredecessorMailbox.readWindowMs
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: [k1, k2], accountIdB64: account, nowMs: 1_000 + window - 1, adoptions: adoptions),
            [k1, k2])
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: [k1, k2], accountIdB64: account, nowMs: 1_000 + window, adoptions: adoptions),
            [k2], "the older key's 30 days ended; the newer one is still read")
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: [k1, k2], accountIdB64: account, nowMs: 2_000 + window, adoptions: adoptions),
            [])

        // The own-key resolver then answers as the sending device's does.
        let current = try publicKey(seed(3))
        let heldKeys = try store.load(accountIdB64: account).map { try publicKey($0) }
        let resolver = AccountKeyResolver(ownCurrentKey: current, ownPredecessorKeys: heldKeys)
        XCTAssertEqual(resolver.current(of: try publicKey(seed(1))), current)
        XCTAssertTrue(resolver.isOwn(try publicKey(seed(2))))
    }

    func testAnAdoptionTimeThisDeviceHoldsIsNeverMovedAndItsOwnSeedsStay() throws {
        let store = InMemoryOwnPredecessorStore()
        let adoptions = PredecessorMailbox.AdoptionStore(defaults: defaults)
        // This device adopted the rotation that retired seed 2 itself.
        try store.add(seed(2), accountIdB64: account)
        try store.add(seed(9), accountIdB64: account)
        let k2 = base64Encode(try publicKey(seed(2)))
        adoptions.recordIfAbsent(accountIdB64: account, keyB64: k2, nowMs: 50)

        try StateBundle.storeOwnPredecessors(
            [
                StateBundle.OwnPredecessor(seedB64: seed(1).base64EncodedString(), adoptedAtMs: 1_000),
                StateBundle.OwnPredecessor(seedB64: seed(2).base64EncodedString(), adoptedAtMs: 2_000),
            ],
            accountIdB64: account, store: store, adoptions: adoptions)

        XCTAssertEqual(
            store.load(accountIdB64: account), [seed(1), seed(2), seed(9)],
            "the bundle's order first, then what only this device held")
        XCTAssertEqual(adoptions.adoptedAtMs(accountIdB64: account, keyB64: k2), 50)
        XCTAssertEqual(
            StateBundle.mergedOwnPredecessors(held: [seed(2)], carried: [seed(1), seed(2)]), [seed(1), seed(2)])
        XCTAssertEqual(StateBundle.mergedOwnPredecessors(held: [seed(2)], carried: []), [seed(2)])
    }

    func testAnEmptyListStoresNothingAndAFailedWriteThrows() throws {
        let store = InMemoryOwnPredecessorStore()
        let adoptions = PredecessorMailbox.AdoptionStore(defaults: defaults)
        XCTAssertEqual(
            try StateBundle.storeOwnPredecessors([], accountIdB64: account, store: store, adoptions: adoptions), 0)
        XCTAssertEqual(store.load(accountIdB64: account), [])

        store.addError = KeychainJSONItemError.keychain(-1)
        XCTAssertThrowsError(try StateBundle.storeOwnPredecessors(
            [StateBundle.OwnPredecessor(seedB64: seed(1).base64EncodedString(), adoptedAtMs: 1)],
            accountIdB64: account, store: store, adoptions: adoptions))
    }

    func testAContactsPredecessorsAreRecordedAsProven() throws {
        let pins = PeerPinStore(defaults: defaults, ownerAccountIdB64: account)
        let provenStore = ProvenPredecessorStore(ownerAccountIdB64: account, defaults: defaults)
        let bundle = try XCTUnwrap(StateBundle.decode(json(
            own: ownJson([(seed(1), "1000")]),
            pinPredecessors: #"["\#(key(0xC1))","\#(key(0xC2))"]"#)))
        let store = InMemoryOwnPredecessorStore()

        let applied = StateBundle.apply(
            bundle, accountIdB64: account, pinStore: pins,
            ownPredecessors: store,
            adoptions: PredecessorMailbox.AdoptionStore(defaults: defaults),
            provenStore: provenStore)

        XCTAssertEqual(applied.pins, 1)
        XCTAssertEqual(store.load(accountIdB64: account), [seed(1)])
        let proven = provenStore.load()
        XCTAssertEqual(proven.current(of: key(0xC1)), key(0xC3))
        XCTAssertEqual(proven.current(of: key(0xC2)), key(0xC3))
        XCTAssertTrue(proven.samePerson(key(0xC1), key(0xC3)))
        XCTAssertEqual(proven.predecessors(of: key(0xC3)), [key(0xC2), key(0xC1)])
        XCTAssertEqual(
            StateBundle.provenEdges(of: bundle.pins[0]).map(\.oldKeyB64), [key(0xC1), key(0xC2)])
        XCTAssertEqual(
            StateBundle.provenEdges(of: bundle.pins[0]).map(\.newKeyB64), [key(0xC2), key(0xC3)])
    }

    func testAPinThisDeviceRefusesRecordsNoPredecessors() throws {
        let pins = PeerPinStore(defaults: defaults, ownerAccountIdB64: account)
        // This device already holds a LATER pin for the contact.
        XCTAssertEqual(
            pins.setPin(accountIdB64: peerAccount, pinnedSignaturePubkeyB64: key(0xEE), rotationMarker: 99), .set)
        let provenStore = ProvenPredecessorStore(ownerAccountIdB64: account, defaults: defaults)
        let bundle = try XCTUnwrap(StateBundle.decode(json(pinPredecessors: #"["\#(key(0xC1))"]"#)))

        StateBundle.apply(
            bundle, accountIdB64: account, pinStore: pins,
            ownPredecessors: InMemoryOwnPredecessorStore(),
            adoptions: PredecessorMailbox.AdoptionStore(defaults: defaults),
            provenStore: provenStore)

        XCTAssertTrue(provenStore.load().isEmpty)
        XCTAssertEqual(pins.readPin(accountIdB64: peerAccount)?.pinnedSignaturePubkeyB64, key(0xEE))
    }

    // MARK: - The conversation binding

    // The binding verdict table moved to ``EarlierKeysGateTests`` (G-56: the
    // core's `ownBindingVerdict`, the shared 17 vectors).

    func testTheConversationSeedIsTheOldestPredecessor() {
        XCTAssertEqual(
            OwnAccountKeys.conversationSeed(currentSeed: seed(3), predecessorSeeds: [seed(1), seed(2)]), seed(1))
        XCTAssertEqual(OwnAccountKeys.conversationSeed(currentSeed: seed(3), predecessorSeeds: []), seed(3))
    }

    func testAKeyPackageUnderAnotherOwnKeyIsNotAdmitted() {
        let current = Data(repeating: 3, count: 32)
        let old = Data(repeating: 1, count: 32)
        let stranger = Data(repeating: 9, count: 32)
        func wrong(_ keyPackageKey: Data, inConversation: Data) -> Bool {
            OwnAccountKeys.namesWrongOwnKey(
                keyPackageAccountKey: keyPackageKey, ownKeyInConversation: inConversation,
                currentAccountKey: current, predecessorKeys: [old])
        }
        XCTAssertTrue(wrong(current, inConversation: old), "bound under the new key, leaves under the old")
        XCTAssertTrue(wrong(old, inConversation: current))
        XCTAssertFalse(wrong(old, inConversation: old))
        XCTAssertFalse(wrong(current, inConversation: current))
        XCTAssertFalse(wrong(stranger, inConversation: old), "another person's device is not this rule's")
    }

    /// The property the whole change exists for: a device paired after a
    /// rotation, holding only the current seed and what the bundle brought,
    /// binds under the SAME account key as the device that paired it.
    func testADevicePairedAfterARotationDerivesTheSendersConversationAccountKey() throws {
        // The sender was there before the rotation: bound under K0, now
        // holding K1 with K0 in its predecessor slot.
        let k0 = try Identity.fromSeed(seed: seed(0xA0))
        let k1 = try Identity.fromSeed(seed: seed(0xA1))
        let senderDevice = try Identity.fromSeedBound(
            seed: Data(repeating: 0x51, count: DeviceIdentityStore.seedLength),
            accountSeed: k0.ed25519Seed(), issuedAtUnixSecs: 1)
        let senderConversationKey = try XCTUnwrap(senderDevice.accountPublicKey())
        XCTAssertEqual(senderConversationKey, k0.signaturePublicKey())
        let senderStore = InMemoryOwnPredecessorStore()
        try senderStore.add(seed(0xA0), accountIdB64: account)

        // Over the wire.
        let sent = StateBundle.V4(
            contacts: [], pins: [], knownGroupIdsB64: [],
            ownPredecessors: StateBundle.carriedOwnPredecessors(
                seeds: senderStore.load(accountIdB64: account)) { _ in 1_000 })
        let received = try XCTUnwrap(StateBundle.decode(StateBundle.encode(sent)))

        // The new device knows only K1 and the bundle.
        let receiverStore = InMemoryOwnPredecessorStore()
        try StateBundle.storeOwnPredecessors(
            received.ownPredecessors, accountIdB64: account, store: receiverStore,
            adoptions: PredecessorMailbox.AdoptionStore(defaults: defaults))
        let device = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k1,
            predecessorSeeds: receiverStore.load(accountIdB64: account), holdsConversations: false))

        XCTAssertEqual(device.accountPublicKey(), senderConversationKey)
        XCTAssertNotEqual(device.accountPublicKey(), k1.signaturePublicKey())
        // The KeyPackage it publishes names that key, and it is stable
        // across a relaunch.
        XCTAssertEqual(
            try keyPackageAccountKey(keyPackage: device.createKeyPackage()), senderConversationKey)
        let again = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k1,
            predecessorSeeds: receiverStore.load(accountIdB64: account), holdsConversations: true))
        XCTAssertEqual(again.accountPublicKey(), senderConversationKey)
        XCTAssertEqual(again.signaturePublicKey(), device.signaturePublicKey())
    }

    func testABindingMintedBeforeTheBundleIsMintedAgainOnlyWhileNoConversationIsHeld() throws {
        let k0 = try Identity.fromSeed(seed: seed(0xA0))
        let k1 = try Identity.fromSeed(seed: seed(0xA1))
        // Without the bundle the device binds under the only key it knows.
        let early = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k1, predecessorSeeds: [], holdsConversations: false))
        XCTAssertEqual(early.accountPublicKey(), k1.signaturePublicKey())

        // The bundle arrived, but the device already holds conversations
        // under that binding: it is kept.
        let kept = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k1, predecessorSeeds: [seed(0xA0)], holdsConversations: true))
        XCTAssertEqual(kept.accountPublicKey(), k1.signaturePublicKey())

        // With no conversation it is minted again under the conversation key,
        // and the device key does not change.
        let reminted = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k1, predecessorSeeds: [seed(0xA0)], holdsConversations: false))
        XCTAssertEqual(reminted.accountPublicKey(), k0.signaturePublicKey())
        XCTAssertEqual(reminted.signaturePublicKey(), early.signaturePublicKey())
    }

    // MARK: - A device restored from the phrase (section 3.7)

    private let accountBytes = Data(repeating: 0xB1, count: 16)

    private func parsedBackup(predecessors: [BackupOwnPredecessor]) throws -> ParsedBackupPayload {
        try parseBackupPayload(wire: try BackupClient.composePayload(
            accountId: accountBytes,
            createdAtMs: 1_700_000_000_000,
            ed25519Seed: Data(repeating: 0x5E, count: 32),
            contacts: [],
            knownGroupIdsHex: [],
            syncedState: nil,
            ownPredecessors: predecessors))
    }

    func testABackupCarriesTheOwnPredecessorsOldestFirstWithTheirAdoptionTimes() throws {
        let times = [
            base64Encode(try publicKey(seed(1))): Int64(111),
            base64Encode(try publicKey(seed(2))): Int64(222),
        ]
        let records = BackupClient.ownPredecessorRecords(seeds: [seed(1), seed(2)]) { times[$0] ?? -1 }

        let parsed = try parsedBackup(predecessors: records)

        XCTAssertEqual(parsed.ownPredecessors.map(\.seed), [seed(1), seed(2)])
        XCTAssertEqual(parsed.ownPredecessors.map(\.adoptedAtMs), [111, 222])
        // An account that never rotated carries none.
        XCTAssertEqual(try parsedBackup(predecessors: []).ownPredecessors.count, 0)
        // More than the format holds keeps the oldest, the conversation key.
        let ten = BackupClient.ownPredecessorRecords(seeds: (1...10).map { seed(UInt8($0)) }) { _ in 0 }
        XCTAssertEqual(ten.count, StateBundle.maxPredecessors)
        XCTAssertEqual(ten.first?.seed, seed(1))
    }

    /// A restored device stores the predecessors where a kept device does,
    /// and its first binding names the key the conversations are bound to.
    func testARestoredBackupMintsTheDeviceBindingUnderTheOldestPredecessor() throws {
        let k0 = try Identity.fromSeed(seed: seed(0xA0))
        let k2 = try Identity.fromSeed(seed: seed(0xA2))
        let store = InMemoryOwnPredecessorStore()
        let parsed = try parsedBackup(predecessors: [
            BackupOwnPredecessor(seed: seed(0xA0), adoptedAtMs: 1_000),
            BackupOwnPredecessor(seed: seed(0xA1), adoptedAtMs: 2_000),
        ])

        let applied = PhraseRestoreBackup.apply(
            parsed, accountIdB64: account, context: "test", defaults: defaults, ownPredecessors: store)

        XCTAssertEqual(applied.ownPredecessors, 2)
        XCTAssertEqual(store.load(accountIdB64: account), [seed(0xA0), seed(0xA1)])
        let adoptions = PredecessorMailbox.AdoptionStore(defaults: defaults)
        XCTAssertEqual(
            adoptions.adoptedAtMs(accountIdB64: account, keyB64: base64Encode(k0.signaturePublicKey())), 1_000,
            "the mailbox of each is read for its own 30 days from the sender's time")

        let device = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k2,
            predecessorSeeds: store.load(accountIdB64: account), holdsConversations: false))
        XCTAssertEqual(device.accountPublicKey(), k0.signaturePublicKey())
        XCTAssertEqual(try keyPackageAccountKey(keyPackage: device.createKeyPackage()), k0.signaturePublicKey())
    }

    /// The retried restore: the session landed first and bound the device
    /// under the only key it knew.
    func testABackupWithPredecessorsReMintsABindingAlreadyMadeUnderTheNewestKey() throws {
        let k0 = try Identity.fromSeed(seed: seed(0xA0))
        let k2 = try Identity.fromSeed(seed: seed(0xA2))
        let store = InMemoryOwnPredecessorStore()
        let early = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k2, predecessorSeeds: [], holdsConversations: false))
        XCTAssertEqual(early.accountPublicKey(), k2.signaturePublicKey())

        // The chat layer is told, through what the pairing bundle posts.
        let told = expectation(forNotification: .lakkoStateBundleApplied, object: nil) { note in
            note.userInfo?[StateBundle.appliedAccountKey] as? String == self.account
        }
        PhraseRestoreBackup.apply(
            try parsedBackup(predecessors: [BackupOwnPredecessor(seed: seed(0xA0), adoptedAtMs: 1_000)]),
            accountIdB64: account, context: "test", defaults: defaults, ownPredecessors: store)
        wait(for: [told], timeout: 1)

        let reminted = try XCTUnwrap(ChatRepository.installDeviceIdentity(
            accountIdB64: account, account: k2,
            predecessorSeeds: store.load(accountIdB64: account), holdsConversations: false))
        XCTAssertEqual(reminted.accountPublicKey(), k0.signaturePublicKey())
        XCTAssertEqual(reminted.signaturePublicKey(), early.signaturePublicKey(), "the same device")
    }

    func testARestoreKeepsARotationThisDeviceAdoptedAfterTheBackupWasSealed() throws {
        let store = InMemoryOwnPredecessorStore()
        try store.add(seed(0xA3), accountIdB64: account)

        PhraseRestoreBackup.apply(
            try parsedBackup(predecessors: [BackupOwnPredecessor(seed: seed(0xA0), adoptedAtMs: 1)]),
            accountIdB64: account, context: "test", defaults: defaults, ownPredecessors: store)

        XCTAssertEqual(store.load(accountIdB64: account), [seed(0xA0), seed(0xA3)], "oldest first")
    }

    func testABackupWithoutPredecessorsLeavesTheSlotAndTellsNobody() throws {
        let store = InMemoryOwnPredecessorStore()
        let silent = expectation(forNotification: .lakkoStateBundleApplied, object: nil) { note in
            note.userInfo?[StateBundle.appliedAccountKey] as? String == self.account
        }
        silent.isInverted = true

        let applied = PhraseRestoreBackup.apply(
            try parsedBackup(predecessors: []),
            accountIdB64: account, context: "test", defaults: defaults, ownPredecessors: store)

        XCTAssertEqual(applied.ownPredecessors, 0)
        XCTAssertEqual(store.load(accountIdB64: account), [])
        wait(for: [silent], timeout: 0.2)
    }

    /// A slot that cannot be written costs the binding, not the restore.
    func testASlotThatCannotBeWrittenFailsNothingElseOfTheRestore() throws {
        struct Refused: Error {}
        let store = InMemoryOwnPredecessorStore()
        store.addError = Refused()
        let group = Data(repeating: 0x71, count: 32)
        let parsed = try parseBackupPayload(wire: try BackupClient.composePayload(
            accountId: accountBytes, createdAtMs: 1, ed25519Seed: Data(repeating: 0x5E, count: 32),
            contacts: [], knownGroupIdsHex: [PhraseRestoreBackup.hexString(fromBytes: group)],
            syncedState: nil,
            ownPredecessors: [BackupOwnPredecessor(seed: seed(0xA0), adoptedAtMs: 1)]))

        let applied = PhraseRestoreBackup.apply(
            parsed, accountIdB64: account, context: "test", defaults: defaults, ownPredecessors: store)

        XCTAssertEqual(applied.ownPredecessors, 0)
        XCTAssertEqual(applied.knownGroups, 1)
        RestoredKnownGroupsStore.clear(accountIdB64: account, defaults: defaults)
    }

    // MARK: - Sent contact requests (section 3.5a rule 5)

    private func sent(_ name: String, _ byte: UInt8, id: Int64, intro: String = "") -> SentContactRequest {
        SentContactRequest(
            recipientUsername: name,
            recipientAccountIdB64: Data(repeating: byte, count: 16).base64EncodedString(),
            messageId: id, createdAt: "t\(id)", intro: intro)
    }

    func testTheSentListRoundTripsWithItsIntro() throws {
        let rows = [sent("ada", 0xA1, id: 1, intro: "hello, it is me"), sent("bob", 0xB1, id: 2)]
        XCTAssertEqual(SentRequests.decode(try SentRequests.encode(rows)), rows)
        XCTAssertEqual(SentRequests.decode(Data("not json".utf8)), [])

        let store = KeychainSentRequestsStore()
        XCTAssertEqual(store.load(accountIdB64: account), [])
        try store.save(rows, accountIdB64: account)
        XCTAssertEqual(store.load(accountIdB64: account), rows, "survives the process: it is in the Keychain")
        try store.save([], accountIdB64: account)
        XCTAssertNil(KeychainJSONItem.read(service: KeychainSentRequestsStore.serviceTag, account: account))
    }

    func testTheSentListRules() {
        let ada = sent("ada", 0xA1, id: 1, intro: "first")
        let bob = sent("bob", 0xB1, id: 2, intro: "to bob")
        let adaAgain = sent("ada", 0xA1, id: 5, intro: "second")
        XCTAssertEqual(SentRequests.adding(adaAgain, to: [ada, bob]), [bob, adaAgain], "one row per person")
        XCTAssertEqual(SentRequests.answered(recipientAccountIdB64: ada.recipientAccountIdB64, in: [ada, bob]), [bob])
        XCTAssertEqual(SentRequests.intro(for: bob.recipientAccountIdB64, in: [ada, bob]), "to bob")
        XCTAssertEqual(SentRequests.intro(for: "nobody", in: [ada, bob]), "")

        XCTAssertEqual(SentRequests.storedIntro("short"), "short")
        let long = String(repeating: "\u{00E9}", count: SentRequests.maxIntroBytes)
        let cut = SentRequests.storedIntro(long)
        XCTAssertEqual(cut.utf8.count, SentRequests.maxIntroBytes)
        XCTAssertTrue(long.hasPrefix(cut), "cut at a character boundary")
    }

    func testAReSentRequestKeepsTheNameAndTheIntro() {
        let ada = sent("ada", 0xA1, id: 1, intro: "hello")
        let bob = sent("bob", 0xB1, id: 2)
        let replacement = SentContactRequest(
            recipientUsername: "", recipientAccountIdB64: ada.recipientAccountIdB64,
            messageId: 9, createdAt: "t9", intro: "hello")
        XCTAssertEqual(
            SpentGrants.sentList(
                [ada, bob], recipientAccountIdB64: ada.recipientAccountIdB64,
                verdict: .sent, replacement: replacement),
            [bob, SentContactRequest(
                recipientUsername: "ada", recipientAccountIdB64: ada.recipientAccountIdB64,
                messageId: 9, createdAt: "t9", intro: "hello")])
        XCTAssertEqual(
            SpentGrants.sentList(
                [ada, bob], recipientAccountIdB64: ada.recipientAccountIdB64, verdict: .dead, replacement: nil),
            [bob])
    }

    func testTheAddressRecheckIsThrottledToFiveMinutesPerPerson() {
        XCTAssertEqual(PersonAddressRecheck.minIntervalMs, 5 * 60 * 1000)
    }
}
