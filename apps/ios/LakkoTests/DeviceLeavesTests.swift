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

    private let bundle = StateBundle.V1(
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

    func testStateBundleDecodesTheWebEncoding() {
        // The exact JSON the web `encodeStateBundle` produces for the web test
        // fixture: iOS must read it.
        let web = """
        {"v":1,"contacts":[{"signaturePubkeyB64":"c2ln","displayName":"Bob",\
        "acceptedAtMs":1700000000000,"accountIdHex":"00112233445566778899aabbccddeeff"}],\
        "pins":[{"accountIdB64":"ABEiM0RVZneImaq7zN3u/w==","pinnedSignaturePubkeyB64":"c2ln",\
        "rotationMarker":0}],"knownGroupIdsB64":["Z3JvdXA="]}
        """
        XCTAssertEqual(StateBundle.decode(Data(web.utf8)), bundle)
    }

    func testStateBundleRefusesAnotherVersionOrAMalformedContact() {
        let other = Data(#"{"v":2,"contacts":[],"pins":[],"knownGroupIdsB64":[]}"#.utf8)
        XCTAssertNil(StateBundle.decode(other))
        let broken = Data(#"{"v":1,"contacts":[{"displayName":1}],"pins":[],"knownGroupIdsB64":[]}"#.utf8)
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
