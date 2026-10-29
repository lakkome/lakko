// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0050 client rules on iOS: the re-admission planner (a port of the web
/// `device-readmission.test.ts`), people rather than leaves in a roster, and
/// the pairing state bundle codec and row tag (a port of the web
/// `state-bundle.test.ts`, whose wire the iOS side must match byte for byte).
final class DeviceLeavesTests: XCTestCase {

    // MARK: - planReadmission

    private func kp(_ tag: String) -> DeviceReadmission.DirectoryDevice {
        DeviceReadmission.DirectoryDevice(deviceKeyB64: tag, keyPackage: Data("kp:\(tag)".utf8))
    }

    private func leaf(_ key: String, _ person: String) -> DeviceReadmission.RosterLeafKeys {
        DeviceReadmission.RosterLeafKeys(leafKeyB64: key, personKeyB64: person)
    }

    func testAdmitsAListedDeviceThatHoldsNoLeafForAnyAccountInTheChat() {
        let plan = DeviceReadmission.plan(
            roster: [leaf("me-1", "me"), leaf("bob-1", "bob")],
            ownPersonKeyB64: "me",
            accounts: [
                .init(personKeyB64: "me", devices: [kp("me-1"), kp("me-2")]),
                .init(personKeyB64: "bob", devices: [kp("bob-1"), kp("bob-2")]),
            ],
            removed: [],
            ownDeviceGroupRoster: ["me-1", "me-2"])
        XCTAssertEqual(plan.admit.map { $0.deviceKeyB64 }, ["me-2", "bob-2"])
        XCTAssertEqual(plan.retire, [])
    }

    func testNeverReadmitsADeviceThisClientSawRemoved() {
        let plan = DeviceReadmission.plan(
            roster: [leaf("bob-1", "bob")],
            ownPersonKeyB64: "me",
            accounts: [.init(personKeyB64: "bob", devices: [kp("bob-1"), kp("bob-old")])],
            removed: ["bob-old"],
            ownDeviceGroupRoster: nil)
        XCTAssertEqual(plan.admit, [])
    }

    func testRetiresAnOwnLeafWhoseDirectoryRowIsGoneAndNobodyElses() {
        let plan = DeviceReadmission.plan(
            roster: [leaf("me-1", "me"), leaf("me-forgotten", "me"), leaf("bob-gone", "bob")],
            ownPersonKeyB64: "me",
            accounts: [
                .init(personKeyB64: "me", devices: [kp("me-1")]),
                .init(personKeyB64: "bob", devices: []),
            ],
            removed: [],
            ownDeviceGroupRoster: nil)
        XCTAssertEqual(plan.retire, ["me-forgotten"])
        XCTAssertEqual(plan.admit, [])
    }

    func testDecidesNothingForAnAccountWhoseDirectoryFetchFailed() {
        let plan = DeviceReadmission.plan(
            roster: [leaf("me-1", "me"), leaf("me-2", "me")],
            ownPersonKeyB64: "me",
            accounts: [.init(personKeyB64: "me", devices: nil)],
            removed: [],
            ownDeviceGroupRoster: nil)
        XCTAssertEqual(plan, DeviceReadmission.Plan(admit: [], retire: []))
    }

    func testProposesEachDeviceOnceEvenWhenTwoAccountsListIt() {
        let plan = DeviceReadmission.plan(
            roster: [],
            ownPersonKeyB64: "me",
            accounts: [
                .init(personKeyB64: "a", devices: [kp("x")]),
                .init(personKeyB64: "b", devices: [kp("x")]),
            ],
            removed: [],
            ownDeviceGroupRoster: nil)
        XCTAssertEqual(plan.admit.count, 1)
    }

    @MainActor
    func testRemovedMemoryRecordsOnlyLeavesThatLeftAndIgnoresATornRead() {
        let suite = "DeviceLeavesTests.\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suite) else { return XCTFail("no suite") }
        defer { defaults.removePersistentDomain(forName: suite) }
        DeviceReadmissionMemory.recordRemoved(
            accountIdB64: "acct", groupIdHex: "g", before: ["a", "b"], after: nil, defaults: defaults)
        XCTAssertEqual(DeviceReadmissionMemory.removed(accountIdB64: "acct", groupIdHex: "g", defaults: defaults), [])
        DeviceReadmissionMemory.recordRemoved(
            accountIdB64: "acct", groupIdHex: "g", before: ["a", "b"], after: ["b", "c"], defaults: defaults)
        XCTAssertEqual(DeviceReadmissionMemory.removed(accountIdB64: "acct", groupIdHex: "g", defaults: defaults), ["a"])
    }

    // MARK: - people, not leaves

    private func member(_ leaf: UInt32, _ key: UInt8, account: UInt8?) -> GroupMember {
        GroupMember(
            leafIndex: leaf,
            signaturePublicKey: Data(repeating: key, count: 32),
            accountKey: account.map { Data(repeating: $0, count: 32) } ?? Data())
    }

    func testCollapseYieldsOneRowPerPersonKeyedOnTheAccountKey() {
        let roster = [
            member(0, 0x01, account: 0xA0),
            member(1, 0x02, account: 0xB0),
            member(2, 0x03, account: 0xA0),
            member(3, 0x04, account: nil),
        ]
        let rows = PersonKey.collapse(roster, ownLeafIndex: 2)
        XCTAssertEqual(rows.count, 3)
        XCTAssertEqual(rows[0].signaturePublicKey, Data(repeating: 0xA0, count: 32))
        // This device's own leaf represents its own person.
        XCTAssertEqual(rows[0].leafIndex, 2)
        XCTAssertEqual(rows[1].signaturePublicKey, Data(repeating: 0xB0, count: 32))
        // An unresolved leaf falls back to its own key.
        XCTAssertEqual(rows[2].signaturePublicKey, Data(repeating: 0x04, count: 32))
        XCTAssertEqual(PersonKey.people(roster).count, 3)
    }

    func testLeavesOfSamePersonNameEveryDeviceOfTheAccountNamedLeafFirst() {
        let roster = [
            member(0, 0x01, account: 0xA0),
            member(1, 0x02, account: 0xB0),
            member(2, 0x03, account: 0xA0),
        ]
        XCTAssertEqual(PersonKey.leavesOfSamePerson(roster, leafIndex: 2), [2, 0])
        XCTAssertEqual(PersonKey.leavesOfSamePerson(roster, leafIndex: 1), [1])
        XCTAssertEqual(PersonKey.leavesOfSamePerson(roster, leafIndex: 9), [9])
    }

    // MARK: - state bundle

    private let bundle = StateBundle.V3(
        contacts: [
            StateBundle.CarriedContact(
                signaturePubkeyB64: "c2ln",
                displayName: "Bob",
                acceptedAtMs: 1_700_000_000_000,
                accountIdHex: "00112233445566778899aabbccddeeff")
        ],
        pins: [
            StateBundle.CarriedPin(
                accountIdB64: "ABEiM0RVZneImaq7zN3u/w==",
                pinnedSignaturePubkeyB64: "c2ln",
                rotationMarker: 0)
        ],
        knownGroupIdsB64: ["Z3JvdXA="])

    func testStateBundleRoundTrips() {
        XCTAssertEqual(StateBundle.decode(StateBundle.encode(bundle)), bundle)
    }

    func testStateBundleDecodesTheContractEncoding() {
        // The JSON shape of the cross-client contract (format 3): iOS must
        // read it whichever client wrote it.
        let wire = """
        {"v":3,"contacts":[{"signaturePubkeyB64":"c2ln","displayName":"Bob",\
        "acceptedAtMs":1700000000000,"accountIdHex":"00112233445566778899aabbccddeeff"}],\
        "pins":[{"accountIdB64":"ABEiM0RVZneImaq7zN3u/w==","pinnedSignaturePubkeyB64":"c2ln",\
        "rotationMarker":0,"provenPredecessorsB64":[]}],"knownGroupIdsB64":["Z3JvdXA="],\
        "ownPredecessors":[],"groupNames":[],"profiles":[],"pendingContacts":[]}
        """
        XCTAssertEqual(StateBundle.decode(Data(wire.utf8)), bundle)
    }

    func testStateBundleRefusesAnotherVersionOrAMalformedContact() {
        // Format 1 is refused outright (flag day, no dual accept), with or
        // without the new fields.
        let v1 = Data(#"{"v":1,"contacts":[],"pins":[],"knownGroupIdsB64":[]}"#.utf8)
        XCTAssertNil(StateBundle.decode(v1))
        let v1WithNewFields = Data(
            #"{"v":1,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[]}"#.utf8)
        XCTAssertNil(StateBundle.decode(v1WithNewFields))
        // Format 2 is refused too (bundle v3 is a flag day).
        let v2 = Data(#"{"v":2,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[]}"#.utf8)
        XCTAssertNil(StateBundle.decode(v2))
        let v2WithNewFields = Data(
            (#"{"v":2,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"#
                + #""groupNames":[],"profiles":[],"pendingContacts":[]}"#).utf8)
        XCTAssertNil(StateBundle.decode(v2WithNewFields))
        let v3MissingArrays = Data(
            #"{"v":3,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[]}"#.utf8)
        XCTAssertNil(StateBundle.decode(v3MissingArrays), "the three arrays are required")
        let empty = Data(
            (#"{"v":3,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"#
                + #""groupNames":[],"profiles":[],"pendingContacts":[]}"#).utf8)
        XCTAssertNotNil(StateBundle.decode(empty))
        let broken = Data(
            (#"{"v":3,"contacts":[{"displayName":1}],"pins":[],"knownGroupIdsB64":[],"#
                + #""ownPredecessors":[],"groupNames":[],"profiles":[],"pendingContacts":[]}"#).utf8)
        XCTAssertNil(StateBundle.decode(broken))
        XCTAssertNil(StateBundle.decode(Data([0x7B])))
    }

    func testStateBundleRowIsTaggedSoTheOtherPollsCanSkipIt() {
        let sealed = Data([1, 2, 3, 4])
        let row = StateBundle.wrapRow(sealed)
        XCTAssertTrue(StateBundle.isStateRow(row))
        XCTAssertEqual(StateBundle.unwrapRow(row), sealed)
        XCTAssertFalse(StateBundle.isStateRow(sealed))
        XCTAssertNil(StateBundle.unwrapRow(sealed))
        // The magic is the web one, byte for byte.
        XCTAssertEqual(row.prefix(26), Data("lakko/state-bundle-row/v1\u{0}".utf8))
    }

    func testCarriedContactMapsBackToAnAcceptedContact() {
        let key = Data(repeating: 0x11, count: 32)
        let carried = StateBundle.CarriedContact(
            signaturePubkeyB64: base64Encode(key),
            displayName: "",
            acceptedAtMs: 0,
            accountIdHex: "00112233445566778899aabbccddeeff")
        let contact = StateBundle.accepted(carried)
        XCTAssertEqual(contact?.request.fromPubkey, key)
        XCTAssertEqual(contact?.request.fromAccountId?.count, 16)
        XCTAssertNil(contact?.displayName)
        XCTAssertLessThan(contact?.request.messageId ?? 0, 0)
    }
}

/// Pairing state bundle v3 (2026-10-28): the shared group names, the stored
/// profiles and the pending contacts reach a newly paired device.
@MainActor
final class StateBundleV3Tests: XCTestCase {

    private final class MemoryPersistence: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }

    private let account = Data(repeating: 0x5A, count: 16).base64EncodedString()
    private let groupIdHex = String(repeating: "ab", count: 32)
    private let alice = Data(repeating: 0xA1, count: 32)
    private let own = Data(repeating: 0x0E, count: 32)
    private var suites: [String] = []
    private var dirs: [URL] = []

    override func tearDown() {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        for dir in dirs { try? FileManager.default.removeItem(at: dir) }
        AcceptedContactsStore.save([], accountIdB64: account)
        StateBundle.clearKnownGroupIds(accountIdB64: account)
        suites = []
        dirs = []
        super.tearDown()
    }

    private func freshDefaults() throws -> UserDefaults {
        let suite = "lakko.tests.state-bundle-v3.\(UUID().uuidString)"
        suites.append(suite)
        return try XCTUnwrap(UserDefaults(suiteName: suite))
    }

    private func copies() -> PendingDisplayCopies {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        dirs.append(dir)
        return PendingDisplayCopies(
            ownerAccountIdB64: account,
            sealer: HeldSendQueue.Sealer(seal: { $0 }, open: { $0 }),
            baseDirectory: dir)
    }

    private func directory() throws -> ProfileDirectory {
        let defaults = try freshDefaults()
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: MemoryPersistence(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: UsernameVerifier(resolve: { _ in .notRegistered }, defaults: defaults),
            ownStore: OwnProfileStore(defaults: defaults))
        directory.verifiesOnResolve = false
        directory.setRenames(from: [])
        directory.activate(accountIdB64: account, username: "me")
        return directory
    }

    private func pending(_ byte: UInt8, name: String = "VG2") -> PendingContact {
        PendingContact(
            accountIdHex: String(repeating: String(format: "%02x", byte), count: 16),
            signaturePubkey: Data(repeating: byte, count: 32),
            x25519Pubkey: Data(repeating: byte &+ 1, count: 32),
            displayName: name, username: "vg2", addedAtMs: 1_000,
            linkId: Data(repeating: 0x11, count: 16))
    }

    private func bundleJson(groupNames: String = "[]", profiles: String = "[]", pending: String = "[]") -> Data {
        Data(("{\"v\":3,\"contacts\":[],\"pins\":[],\"knownGroupIdsB64\":[],\"ownPredecessors\":[],"
            + "\"groupNames\":\(groupNames),\"profiles\":\(profiles),\"pendingContacts\":\(pending)}").utf8)
    }

    // MARK: - Round trip

    func testARoundTripCarriesAGroupNameAProfileAndAPendingContact() throws {
        // The confirming device.
        let senderDefaults = try freshDefaults()
        let senderNames = GroupNameStore(defaults: senderDefaults)
        senderNames.record(
            accountIdB64: account, groupIdHex: groupIdHex,
            value: StoredGroupName(name: "Test VG's", updatedAtMs: 1_234))
        PendingContactStore.save([pending(0xC2)], accountIdB64: account, defaults: senderDefaults)
        let built = StateBundle.build(
            accountIdB64: account,
            pinStore: PeerPinStore(defaults: senderDefaults, ownerAccountIdB64: account),
            ownPredecessors: InMemoryOwnPredecessorStore(),
            adoptions: PredecessorMailbox.AdoptionStore(defaults: senderDefaults),
            provenStore: ProvenPredecessorStore(ownerAccountIdB64: account, defaults: senderDefaults),
            groupNameStore: senderNames,
            knownGroupIdHexes: [groupIdHex],
            profiles: [
                base64Encode(alice): ProfileClaim(displayName: "Alice", username: nil, updatedAtMs: 77),
                base64Encode(own): ProfileClaim(displayName: "Me", username: nil, updatedAtMs: 1),
            ],
            ownKeysB64: [base64Encode(own)],
            pendingDefaults: senderDefaults,
            heldMessages: { _ in [
                StateBundle.CarriedHeldMessage(text: "hello", heldAtMs: 5_000),
                StateBundle.CarriedHeldMessage(text: "again", heldAtMs: 6_000),
            ] })
        XCTAssertEqual(built.v, 3)
        XCTAssertEqual(built.groupNames.count, 1)
        XCTAssertEqual(built.profiles.map(\.personKeyB64), [base64Encode(alice)], "never this account's own")
        XCTAssertEqual(built.pendingContacts.count, 1)

        let wire = StateBundle.encode(built)
        XCTAssertTrue(String(decoding: wire, as: UTF8.self).contains("\"replyTo\":null"))
        let decoded = try XCTUnwrap(StateBundle.decode(wire))
        XCTAssertEqual(decoded, built)

        // The new device.
        let receiverDefaults = try freshDefaults()
        let receiverNames = GroupNameStore(defaults: receiverDefaults)
        let receiverCopies = copies()
        StateBundle.apply(
            decoded, accountIdB64: account,
            pinStore: PeerPinStore(defaults: receiverDefaults, ownerAccountIdB64: account),
            ownPredecessors: InMemoryOwnPredecessorStore(),
            adoptions: PredecessorMailbox.AdoptionStore(defaults: receiverDefaults),
            provenStore: ProvenPredecessorStore(ownerAccountIdB64: account, defaults: receiverDefaults),
            pendingDefaults: receiverDefaults,
            displayCopies: receiverCopies,
            applyNamesOnMain: false)
        let directory = try directory()
        let applied = StateBundle.applyNames(
            decoded, accountIdB64: account, groupNameStore: receiverNames, directory: directory,
            ownKeysB64: [base64Encode(own)])

        XCTAssertEqual(applied.groupNames, 1)
        XCTAssertEqual(applied.profiles, 1)
        XCTAssertEqual(
            receiverNames.load(accountIdB64: account, groupIdHex: groupIdHex),
            StoredGroupName(name: "Test VG's", updatedAtMs: 1_234), "the original stamp, not a fresh one")
        XCTAssertEqual(directory.resolve(signatureKey: alice).name, "Alice")
        XCTAssertEqual(directory.storedProfile(signatureKeyB64: base64Encode(alice))?.updatedAtMs, 77)
        XCTAssertEqual(
            PendingContactStore.load(accountIdB64: account, defaults: receiverDefaults).map(\.accountIdHex),
            [pending(0xC2).accountIdHex])
        XCTAssertEqual(
            receiverCopies.list(key: pending(0xC2).queueKey, nowMs: 7_000).map(\.text), ["hello", "again"])
    }

    // MARK: - Refusals

    func testEachNewBoundAndMalformedEntryRefusesTheWholeBundle() {
        let key = base64Encode(alice)
        let envelope = base64Encode(Data([1, 2, 3]))
        XCTAssertNotNil(StateBundle.decode(bundleJson()))

        let names = (0..<1001).map { #"{"groupIdB64":"\#(base64Encode(Data([UInt8($0 % 256), UInt8($0 / 256)])))","envelopeB64":"\#(envelope)"}"# }
        XCTAssertNotNil(StateBundle.decode(bundleJson(groupNames: "[" + names.prefix(1000).joined(separator: ",") + "]")))
        XCTAssertNil(StateBundle.decode(bundleJson(groupNames: "[" + names.joined(separator: ",") + "]")), "over 1000")
        XCTAssertNil(StateBundle.decode(bundleJson(groupNames: "[" + [names[0], names[0]].joined(separator: ",") + "]")),
                     "a repeated group id")
        XCTAssertNil(StateBundle.decode(bundleJson(groupNames: #"[{"groupIdB64":"!!","envelopeB64":"AQ=="}]"#)))

        let profiles = (0..<2001).map { i -> String in
            var bytes = Data(repeating: 0, count: 32)
            bytes[0] = UInt8(i % 256); bytes[1] = UInt8(i / 256)
            return #"{"personKeyB64":"\#(base64Encode(bytes))","envelopeB64":"\#(envelope)"}"#
        }
        XCTAssertNil(StateBundle.decode(bundleJson(profiles: "[" + profiles.joined(separator: ",") + "]")), "over 2000")
        XCTAssertNil(
            StateBundle.decode(bundleJson(
                profiles: #"[{"personKeyB64":"\#(base64Encode(Data(count: 31)))","envelopeB64":"\#(envelope)"}]"#)),
            "a person key that is not 32 bytes")
        XCTAssertNotNil(StateBundle.decode(bundleJson(profiles: #"[{"personKeyB64":"\#(key)","envelopeB64":"\#(envelope)"}]"#)))

        func pendingJson(_ hex: String, held: Int = 0, link: Int = 16) -> String {
            let messages = (0..<held).map { #"{"text":"m\#($0)","attachments":[],"replyTo":null,"heldAtMs":\#($0)}"# }
            return #"{"accountIdHex":"\#(hex)","signaturePubkeyB64":"\#(key)","x25519PubkeyB64":"\#(key)","#
                + #""displayName":"VG2","username":"","addedAtMs":1,"linkIdB64":"\#(base64Encode(Data(count: link)))","#
                + #""heldMessages":[\#(messages.joined(separator: ","))]}"#
        }
        let hex = String(repeating: "c2", count: 16)
        XCTAssertNotNil(StateBundle.decode(bundleJson(pending: "[" + pendingJson(hex, held: 200) + "]")))
        XCTAssertNil(StateBundle.decode(bundleJson(pending: "[" + pendingJson(hex, held: 201) + "]")), "over 200 held")
        XCTAssertNil(StateBundle.decode(bundleJson(pending: "[" + pendingJson(hex, link: 15) + "]")), "a short link id")
        XCTAssertNil(StateBundle.decode(bundleJson(pending: "[" + pendingJson(hex.uppercased()) + "]")),
                     "the account id is lower-case hex")
        let many = (0..<501).map { pendingJson(String(format: "%032x", $0)) }
        XCTAssertNil(StateBundle.decode(bundleJson(pending: "[" + many.joined(separator: ",") + "]")), "over 500")
    }

    // MARK: - The strictly-newer rule and the own key

    func testANewerStoredNameAndProfileAreNotOverwrittenByOlderCarriedOnes() throws {
        let defaults = try freshDefaults()
        let names = GroupNameStore(defaults: defaults)
        names.record(
            accountIdB64: account, groupIdHex: groupIdHex,
            value: StoredGroupName(name: "Renamed", updatedAtMs: 9_000))
        let directory = try directory()
        XCTAssertEqual(
            directory.acceptReceived(
                ProfileClaim(displayName: "Alice 2", username: nil, updatedAtMs: 500),
                signatureKeyB64: base64Encode(alice), accountIdB64: account),
            .stored)

        let bundle = StateBundle.V3(
            contacts: [], pins: [], knownGroupIdsB64: [],
            groupNames: StateBundle.carriedGroupNames(groupIdHexes: [groupIdHex]) { _ in
                StoredGroupName(name: "Old name", updatedAtMs: 1_000)
            },
            profiles: StateBundle.carriedProfiles(
                [base64Encode(alice): ProfileClaim(displayName: "Alice 1", username: nil, updatedAtMs: 100)],
                ownKeysB64: []))
        let applied = StateBundle.applyNames(
            bundle, accountIdB64: account, groupNameStore: names, directory: directory, ownKeysB64: [])

        XCTAssertEqual(applied.groupNames, 0)
        XCTAssertEqual(applied.profiles, 0)
        XCTAssertEqual(names.load(accountIdB64: account, groupIdHex: groupIdHex)?.name, "Renamed")
        XCTAssertEqual(directory.resolve(signatureKey: alice).name, "Alice 2")
    }

    func testAProfileUnderThisAccountsOwnKeyIsNeverStoredFromABundle() throws {
        let directory = try directory()
        let carried = StateBundle.carriedProfiles(
            [base64Encode(own): ProfileClaim(displayName: "Me", username: nil, updatedAtMs: 5)], ownKeysB64: [])
        XCTAssertEqual(carried.count, 1, "a sender that did not leave it out")
        let bundle = StateBundle.V3(contacts: [], pins: [], knownGroupIdsB64: [], profiles: carried)
        let applied = StateBundle.applyNames(
            bundle, accountIdB64: account, groupNameStore: GroupNameStore(defaults: try freshDefaults()),
            directory: directory, ownKeysB64: [base64Encode(own)])
        XCTAssertEqual(applied.profiles, 0)
        XCTAssertNil(directory.storedProfile(signatureKeyB64: base64Encode(own)))
    }

    func testACarriedPendingContactIsAddedOnlyWhenNotAlreadyHeld() throws {
        let defaults = try freshDefaults()
        let held = pending(0xC2, name: "Kept here")
        PendingContactStore.save([held], accountIdB64: account, defaults: defaults)
        let accepted = StateBundle.accepted(StateBundle.CarriedContact(
            signaturePubkeyB64: base64Encode(Data(repeating: 0xC3, count: 32)), displayName: "Bob",
            acceptedAtMs: 1))!
        let carried = StateBundle.carriedPendingContacts(
            [pending(0xC2, name: "From bundle"), pending(0xC3), pending(0xC4)], held: { _ in [] })

        let added = StateBundle.storePendingContacts(
            carried, accountIdB64: account, accepted: [accepted], defaults: defaults)

        XCTAssertEqual(added.map(\.contact.accountIdHex), [pending(0xC4).accountIdHex])
        let list = PendingContactStore.load(accountIdB64: account, defaults: defaults)
        XCTAssertEqual(list.map(\.accountIdHex), [held.accountIdHex, pending(0xC4).accountIdHex])
        XCTAssertEqual(list.first?.displayName, "Kept here")
    }

    // MARK: - The size cap

    func testABundleOverTheCapLosesProfilesThenGroupNamesThenHeldMessages() throws {
        let big = String(repeating: "A", count: 400)
        func profile(_ i: Int) -> StateBundle.CarriedProfile {
            StateBundle.CarriedProfile(personKeyB64: "p\(i)", envelopeB64: big)
        }
        func name(_ i: Int) -> StateBundle.CarriedGroupName {
            StateBundle.CarriedGroupName(groupIdB64: "g\(i)", envelopeB64: big)
        }
        let held = (0..<4).map { StateBundle.CarriedHeldMessage(text: big, heldAtMs: Int64($0)) }
        let contact = StateBundle.CarriedContact(signaturePubkeyB64: "c2ln", displayName: big, acceptedAtMs: 1)
        let bundle = StateBundle.V3(
            contacts: [contact], pins: [], knownGroupIdsB64: ["Z3JvdXA="],
            groupNames: (0..<3).map(name),
            profiles: (0..<5).map(profile),
            pendingContacts: [pending(0xC2)].map { p in
                StateBundle.CarriedPendingContact(
                    accountIdHex: p.accountIdHex, signaturePubkeyB64: base64Encode(p.signaturePubkey),
                    x25519PubkeyB64: base64Encode(p.x25519Pubkey), displayName: "", username: "",
                    addedAtMs: 1, linkIdB64: base64Encode(p.linkId), heldMessages: held)
            })
        let full = StateBundle.encode(bundle).count
        XCTAssertEqual(StateBundle.fittingCap(bundle, cap: full), bundle, "a bundle that fits is untouched")

        let lessProfiles = StateBundle.fittingCap(bundle, cap: full - 800)
        XCTAssertLessThanOrEqual(StateBundle.encode(lessProfiles).count, full - 800)
        XCTAssertEqual(lessProfiles.profiles.map(\.personKeyB64), ["p0", "p1", "p2"], "from the end")
        XCTAssertEqual(lessProfiles.groupNames.count, 3)

        let lessNames = StateBundle.fittingCap(bundle, cap: full - 2_400)
        XCTAssertTrue(lessNames.profiles.isEmpty)
        XCTAssertEqual(lessNames.groupNames.map(\.groupIdB64), ["g0", "g1"])
        XCTAssertEqual(lessNames.pendingContacts.first?.heldMessages.count, 4)

        let lessHeld = StateBundle.fittingCap(bundle, cap: full - 4_000)
        XCTAssertLessThanOrEqual(StateBundle.encode(lessHeld).count, full - 4_000)
        XCTAssertTrue(lessHeld.groupNames.isEmpty)
        XCTAssertEqual(lessHeld.pendingContacts.first?.heldMessages.map(\.heldAtMs), [0, 1])
        XCTAssertEqual(lessHeld.contacts, [contact], "contacts are never trimmed")
        XCTAssertEqual(lessHeld.knownGroupIdsB64, ["Z3JvdXA="])
        XCTAssertEqual(lessHeld.pendingContacts.count, 1)
    }

    // MARK: - Display copies

    private func entry(_ text: String, attachments: Int = 0, id: String) -> HeldSendQueue.Entry {
        HeldSendQueue.Entry(
            localId: id, queuedAtMs: 0, text: text,
            attachments: Array(repeating: Data("{}".utf8), count: attachments))
    }

    func testTheRealMessageReplacesTheFirstCopyWithTheSameText() {
        let list = [entry("hi", id: "a"), entry("other", id: "b"), entry("hi", id: "c")]
        XCTAssertEqual(PendingDisplayCopies.matchIndex(list, text: "hi", attachmentCount: 0), 0, "the first in order")
        XCTAssertNil(PendingDisplayCopies.matchIndex(list, text: "different", attachmentCount: 0))
        XCTAssertNil(PendingDisplayCopies.matchIndex(list, text: "hi", attachmentCount: 1), "the attachment count too")
        XCTAssertEqual(
            PendingDisplayCopies.matchIndex([entry("pic", attachments: 2, id: "p")], text: "pic", attachmentCount: 2), 0)
    }

    func testTwoIdenticalTextsMatchOneCopyEach() {
        let store = copies()
        store.store([
            StateBundle.CarriedHeldMessage(text: "hi", heldAtMs: 1),
            StateBundle.CarriedHeldMessage(text: "hi", heldAtMs: 2),
            StateBundle.CarriedHeldMessage(text: "bye", heldAtMs: 3),
        ], key: "c2c2")
        XCTAssertFalse(store.consumeMatch(key: "c2c2", text: "nope", attachmentCount: 0))
        XCTAssertTrue(store.consumeMatch(key: "c2c2", text: "hi", attachmentCount: 0))
        XCTAssertEqual(store.list(key: "c2c2", nowMs: 10).map(\.queuedAtMs), [2, 3], "the first copy went")
        XCTAssertTrue(store.consumeMatch(key: "c2c2", text: "hi", attachmentCount: 0))
        XCTAssertFalse(store.consumeMatch(key: "c2c2", text: "hi", attachmentCount: 0), "each text matched once")
        XCTAssertEqual(store.list(key: "c2c2", nowMs: 10).map(\.text), ["bye"])
    }

    func testCopiesMoveToTheConversationAndExpireAfterSevenDays() {
        let store = copies()
        store.store([
            StateBundle.CarriedHeldMessage(text: "old", heldAtMs: 0),
            StateBundle.CarriedHeldMessage(text: "new", heldAtMs: PendingDisplayCopies.maxAgeMs),
        ], key: "c2c2")
        XCTAssertEqual(store.store([StateBundle.CarriedHeldMessage(text: "old", heldAtMs: 0)], key: "c2c2"), 2,
                       "a second bundle with the same messages adds nothing")
        XCTAssertTrue(store.move(from: "c2c2", to: "abab"))
        XCTAssertTrue(store.list(key: "c2c2").isEmpty)
        XCTAssertEqual(store.list(key: "abab", nowMs: PendingDisplayCopies.maxAgeMs).map(\.text), ["new"])
    }
}
