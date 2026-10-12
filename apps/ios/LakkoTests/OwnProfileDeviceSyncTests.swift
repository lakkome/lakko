// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0048 section 4a — the own display name carried between the account's
/// devices as a DeviceGroup `ServiceEnvelope` (the ADR-0060 section 7 carrier),
/// against the real core codecs and dispatcher.
@MainActor
final class OwnProfileDeviceSyncTests: XCTestCase {

    private final class MemoryPersistence: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }

    private let accountId = Data(repeating: 0x42, count: 16)
    private var accountIdB64: String { accountId.base64EncodedString() }
    private var suites: [String] = []

    override func tearDown() {
        for suite in suites {
            UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite)
        }
        suites = []
        super.tearDown()
    }

    private func freshDefaults() throws -> UserDefaults {
        let suite = "lakko.tests.own-profile-device-sync.\(UUID().uuidString)"
        suites.append(suite)
        return try XCTUnwrap(UserDefaults(suiteName: suite))
    }

    private func directory(defaults: UserDefaults? = nil) throws -> ProfileDirectory {
        let defaults = try defaults ?? freshDefaults()
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: MemoryPersistence(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: UsernameVerifier(resolve: { _ in .notRegistered }, defaults: defaults),
            ownStore: OwnProfileStore(defaults: defaults))
        directory.verifiesOnResolve = false
        directory.setRenames(from: [])
        directory.activate(accountIdB64: accountIdB64, username: "valentin")
        return directory
    }

    private func envelope(_ name: String?, stamp: UInt64, accountId: Data? = nil) throws -> Data {
        try composeProfileEnvelope(profile: FfiProfile(
            displayName: name, username: nil, updatedAtMs: stamp,
            accountId: accountId ?? self.accountId, usernameCertificate: nil, grants: []))
    }

    // MARK: - Settings > Profile draft

    func testTheUneditedDraftFollowsANameFromAnotherDeviceAndAnEditedOneDoesNot() throws {
        let d = try directory()
        let vm = ProfileSettingsViewModel(directory: d, publishToAllGroups: {})
        XCTAssertEqual(vm.draftName, "")

        vm.followSavedName("Alice")
        XCTAssertEqual(vm.draftName, "Alice", "an unedited field shows the name that arrived")

        vm.draftName = "Alicia"
        vm.followSavedName("Alice B")
        XCTAssertEqual(vm.draftName, "Alicia", "what the person is typing is never replaced")
    }

    // MARK: - Receive

    func testAStrictlyNewerProfileIsAdoptedWithItsStamp() throws {
        let defaults = try freshDefaults()
        let d = try directory(defaults: defaults)
        let before = d.revision
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Alice", stamp: 1_000)), .adopted)
        XCTAssertEqual(d.ownProfile, OwnProfile(displayName: "Alice", updatedAtMs: 1_000))
        XCTAssertGreaterThan(d.revision, before, "the UI showing the own name re-renders")
        XCTAssertEqual(OwnProfileStore(defaults: defaults).load(accountIdB64: accountIdB64),
                       OwnProfile(displayName: "Alice", updatedAtMs: 1_000),
                       "kept on disk with the received stamp, not re-stamped")

        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Alice B", stamp: 1_001)), .adopted)
        XCTAssertEqual(d.ownProfile?.displayName, "Alice B")
    }

    func testAnEqualOrOlderProfileIsIgnored() throws {
        let d = try directory()
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Alice", stamp: 1_000)), .adopted)
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Mallory", stamp: 1_000)), .notNewer,
                       "an equal stamp is not newer")
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Mallory", stamp: 999)), .notNewer)
        XCTAssertEqual(d.ownProfile, OwnProfile(displayName: "Alice", updatedAtMs: 1_000))

        // A name saved on this device outranks an older one from another.
        let saved = try d.setOwnDisplayName("Local")
        XCTAssertEqual(
            d.adoptOwnProfileFromDevice(envelope: try envelope("Remote", stamp: saved.updatedAtMs)),
            .notNewer)
        XCTAssertEqual(d.ownProfile?.displayName, "Local")
    }

    func testAProfileOfAnotherAccountIsDropped() throws {
        let d = try directory()
        let other = Data(repeating: 0x07, count: 16)
        XCTAssertEqual(
            d.adoptOwnProfileFromDevice(envelope: try envelope("Stranger", stamp: 5_000, accountId: other)),
            .foreignAccount)
        XCTAssertNil(d.ownProfile)
    }

    func testAClearedNameIsAdoptedWhenNewer() throws {
        let d = try directory()
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Alice", stamp: 1_000)), .adopted)
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope(nil, stamp: 900)), .notNewer)
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope(nil, stamp: 2_000)), .adopted)
        XCTAssertEqual(d.ownProfile, OwnProfile(displayName: nil, updatedAtMs: 2_000))
    }

    func testNothingIsAdoptedWithoutAnActiveAccountOrFromNonProfileBytes() throws {
        let d = try directory()
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: composeCommitterBeacon()), .notAProfile)
        d.deactivate()
        XCTAssertEqual(d.adoptOwnProfileFromDevice(envelope: try envelope("Alice", stamp: 1)), .inactive)
    }

    // MARK: - Send

    /// A save hands the envelope to the DeviceGroup with the stamp it is kept
    /// under, the display name, this account's id and nothing else; a later
    /// re-compose carries the same stamp; an adopted profile is not echoed.
    func testTheComposeSideUsesTheStoredStamp() throws {
        let d = try directory()
        var sent: [Data] = []
        d.onOwnDeviceEnvelope = { sent.append($0) }
        XCTAssertNil(d.ownDeviceEnvelope(), "a name never saved (stamp 0) sends nothing")

        let saved = try d.setOwnDisplayName("  Valentin  ")
        XCTAssertEqual(sent.count, 1)
        let decoded = try decodeProfileEnvelope(plaintext: sent[0])
        XCTAssertEqual(decoded.displayName, "Valentin")
        XCTAssertEqual(decoded.updatedAtMs, saved.updatedAtMs)
        XCTAssertEqual(decoded.accountId, accountId)
        XCTAssertNil(decoded.username, "the receiver keeps its own session's username")
        XCTAssertNil(decoded.usernameCertificate)
        XCTAssertTrue(decoded.grants.isEmpty, "grants are scoped to a conversation, never sent here")
        XCTAssertEqual(d.ownDeviceEnvelope(), sent[0], "re-composed later: the same stamp, byte for byte")

        try d.setOwnDisplayName("")
        XCTAssertEqual(sent.count, 2)
        let cleared = try decodeProfileEnvelope(plaintext: sent[1])
        XCTAssertNil(cleared.displayName, "a cleared name is sent as no name")
        XCTAssertGreaterThan(cleared.updatedAtMs, saved.updatedAtMs)

        XCTAssertEqual(
            d.adoptOwnProfileFromDevice(envelope: try envelope("From elsewhere", stamp: cleared.updatedAtMs + 1)),
            .adopted)
        XCTAssertEqual(sent.count, 2, "an adopted profile is not sent back")
        XCTAssertEqual(try decodeProfileEnvelope(plaintext: XCTUnwrap(d.ownDeviceEnvelope())).updatedAtMs,
                       cleared.updatedAtMs + 1, "re-sent later with the stamp it was adopted under")
    }

    /// Two devices of one account: what the first sends, the second adopts.
    func testASavedNameReachesTheOtherDevice() throws {
        let first = try directory()
        let second = try directory()
        var sent: [Data] = []
        first.onOwnDeviceEnvelope = { sent.append($0) }
        let saved = try first.setOwnDisplayName("Valentin")
        XCTAssertEqual(second.adoptOwnProfileFromDevice(envelope: try XCTUnwrap(sent.first)), .adopted)
        XCTAssertEqual(second.ownProfile, saved)
    }

    // MARK: - Pairing hand-over and routing

    private func reachCenter() throws -> ReachCenter {
        let c = ReachCenter(
            persistence: InMemoryReachPersistence(),
            // Unreachable on purpose: a unit test never registers anything.
            ds: Offline.ds())
        c.activate(
            accountIdB64: accountIdB64,
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32)))
        return c
    }

    func testThePairingHandOverIncludesTheProfileOnceANameWasSaved() throws {
        let d = try directory()
        let reach = try reachCenter()
        XCTAssertEqual(
            DeviceGroupSyncRepository.handOverEnvelopes(accountIdB64: accountIdB64, reach: reach, profiles: d),
            [], "nothing ever set: nothing handed over")

        try d.setOwnDisplayName("Valentin")
        let handed = DeviceGroupSyncRepository.handOverEnvelopes(
            accountIdB64: accountIdB64, reach: reach, profiles: d)
        let kinds = handed.compactMap { try? dispatchIncomingEnvelope(plaintext: $0).kind }
        XCTAssertEqual(kinds, [.profile])
        XCTAssertEqual(try decodeProfileEnvelope(plaintext: handed[0]).updatedAtMs, d.ownProfile?.updatedAtMs)
        XCTAssertEqual(
            DeviceGroupSyncRepository.handOverEnvelopes(accountIdB64: "another", reach: reach, profiles: d),
            [], "nothing for an account that is not active")
    }

    func testTheServiceEnvelopeIntakeRoutesAProfileToTheDirectory() throws {
        let d = try directory()
        let reach = try reachCenter()
        reach.ownProfileAdopter = { d.adoptOwnProfileFromDevice(envelope: $0) }
        XCTAssertEqual(reach.adoptDeviceEnvelope(try envelope("Alice", stamp: 1_000)), .adoptedOwnProfile)
        XCTAssertEqual(d.ownProfile?.displayName, "Alice")
        XCTAssertEqual(reach.adoptDeviceEnvelope(try envelope("Alice", stamp: 1_000)), .notNewer)
        XCTAssertEqual(
            reach.adoptDeviceEnvelope(try envelope("Stranger", stamp: 9_000, accountId: Data(repeating: 1, count: 16))),
            .foreign)
        XCTAssertEqual(d.ownProfile?.displayName, "Alice")
        XCTAssertEqual(reach.adoptDeviceEnvelope(composeCommitterBeacon()), .foreign,
                       "every other kind is still dropped")
    }

    func testPendingSendsGiveTheProfileItsOwnSlot() throws {
        XCTAssertEqual(
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try envelope("A", stamp: 1)),
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try envelope("B", stamp: 2)))
        XCTAssertNotEqual(
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try envelope("A", stamp: 1)),
            DeviceGroupSyncRepository.serviceEnvelopeSlot(
                try composeVisibilityEnvelope(setting: defaultVisibilitySetting())))
    }
}
