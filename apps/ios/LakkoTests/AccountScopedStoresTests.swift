// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// Account data that used to sit under device-wide keys, now filed per account
/// (2026-09-30, the same class as the accepted-contact list in
/// ``ContactsPerAccountTests``): two accounts signed in on one phone never see
/// each other's rows, a read with no account is empty and a write with none is
/// dropped, and forget-device removes only the forgotten account's rows.
///
/// The lock-screen mirrors have their own cases in `MutedCallRingTests` and
/// `CallKitIncomingTests`.
@MainActor
final class AccountScopedStoresTests: XCTestCase {

    private var alice = ""
    private var bob = ""
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        let run = UUID().uuidString
        alice = "test-alice-\(run)"
        bob = "test-bob-\(run)"
        suiteName = "account-scoped-stores-\(run)"
        defaults = UserDefaults(suiteName: suiteName)!
        AcceptedContactsStore.bind(accountIdB64: nil)
    }

    override func tearDown() {
        AcceptedContactsStore.bind(accountIdB64: nil)
        ChannelsUiState.forget(accountIdB64: alice)
        ChannelsUiState.forget(accountIdB64: bob)
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    // MARK: - Pairing known group ids ("Synchronizing...")

    func testKnownGroupIdsBelongToTheAccountThatWasPaired() {
        let groupB64 = base64Encode(Data(repeating: 0xAB, count: 32))
        StateBundle.saveKnownGroupIds([groupB64], accountIdB64: alice, defaults: defaults)

        XCTAssertEqual(StateBundle.loadKnownGroupIds(accountIdB64: alice, defaults: defaults), [groupB64])
        XCTAssertTrue(StateBundle.loadKnownGroupIds(accountIdB64: bob, defaults: defaults).isEmpty,
                      "another account never sees the first one's groups")
        XCTAssertEqual(StateBundle.pendingKnownGroups(
            joinedGroupIdHexes: [], accountIdB64: alice, defaults: defaults), 1)
        XCTAssertEqual(StateBundle.pendingKnownGroups(
            joinedGroupIdHexes: [], accountIdB64: bob, defaults: defaults), 0,
            "the other account is never shown Synchronizing... for them")
    }

    func testKnownGroupIdsWithNoAccountAreEmptyAndNotWritten() {
        StateBundle.saveKnownGroupIds(["AAAA"], accountIdB64: nil, defaults: defaults)
        XCTAssertTrue(StateBundle.loadKnownGroupIds(accountIdB64: nil, defaults: defaults).isEmpty)
        XCTAssertEqual(StateBundle.pendingKnownGroups(
            joinedGroupIdHexes: [], accountIdB64: nil, defaults: defaults), 0)
        XCTAssertTrue(defaults.dictionaryRepresentation().keys
            .filter { $0.hasPrefix("lakko.pairing.known-groups") }.isEmpty)
    }

    func testKnownGroupIdsFollowTheBoundAccountByDefault() {
        StateBundle.saveKnownGroupIds(["AAAA"], accountIdB64: alice, defaults: defaults)
        AcceptedContactsStore.bind(accountIdB64: bob)
        XCTAssertTrue(StateBundle.loadKnownGroupIds(defaults: defaults).isEmpty)
        AcceptedContactsStore.bind(accountIdB64: alice)
        XCTAssertEqual(StateBundle.loadKnownGroupIds(defaults: defaults), ["AAAA"])
    }

    func testForgetClearsOnlyThatAccountsKnownGroupIds() {
        StateBundle.saveKnownGroupIds(["AAAA"], accountIdB64: alice, defaults: defaults)
        StateBundle.saveKnownGroupIds(["BBBB"], accountIdB64: bob, defaults: defaults)
        defaults.set(["OLD"], forKey: StateBundle.legacyKnownGroupsKey)
        StateBundle.clearKnownGroupIds(accountIdB64: alice, defaults: defaults)
        XCTAssertTrue(StateBundle.loadKnownGroupIds(accountIdB64: alice, defaults: defaults).isEmpty)
        XCTAssertEqual(StateBundle.loadKnownGroupIds(accountIdB64: bob, defaults: defaults), ["BBBB"])
        XCTAssertNil(defaults.object(forKey: StateBundle.legacyKnownGroupsKey))
    }

    // MARK: - Channel keys (Keychain, one item per account)

    func testChannelKeysOfOneAccountAreNeverLoadedForAnother() throws {
        let channel = "0102030405060708090a0b0c0d0e0f10"
        let secret = Data(repeating: 0x07, count: 32)
        let pair = try ChannelSubscriberKeypair.fromSecret(secret: secret)
        let subscriberKeys = ChannelSubscriberKeys(secret: secret, publicKey: pair.`public`())
        let publisherKeys = ChannelPublisherKeys(
            seed: Data(repeating: 0x11, count: 32), signingKeySeed: Data(repeating: 0x22, count: 32))

        let asAlice = ChannelsUiState(accountIdB64: alice)
        asAlice.setSubscriberKeys(channelId: channel, keys: subscriberKeys)
        asAlice.setKeys(channelId: channel, keys: publisherKeys)
        XCTAssertEqual(try asAlice.nextCounter(channelId: channel), 0)
        XCTAssertEqual(try asAlice.nextCounter(channelId: channel), 1)

        // A second account on the same phone: nothing of the first one's. A
        // reused subscriber key would link the two accounts at the server.
        let asBob = ChannelsUiState(accountIdB64: bob)
        XCTAssertTrue(asBob.subscriberKeysByChannel.isEmpty)
        XCTAssertNil(asBob.persistedPublisherSigningSeed(channelId: channel))
        XCTAssertTrue(asBob.counterByChannel.isEmpty)

        // The first account comes back with its own (logout keeps them).
        let aliceAgain = ChannelsUiState(accountIdB64: alice)
        XCTAssertEqual(aliceAgain.subscriberKeysByChannel[channel], subscriberKeys)
        XCTAssertEqual(aliceAgain.persistedPublisherSigningSeed(channelId: channel),
                       publisherKeys.signingKeySeed)
        XCTAssertEqual(aliceAgain.counterByChannel[channel], 2,
                       "the publish counter stays a monotone floor for its own account")

        // Rebinding one instance swaps every map, it never merges.
        aliceAgain.bind(accountIdB64: bob)
        XCTAssertTrue(aliceAgain.subscriberKeysByChannel.isEmpty)
        XCTAssertTrue(aliceAgain.counterByChannel.isEmpty)
    }

    func testAnUnboundChannelsStateWritesNoKeys() throws {
        let channel = "1112131415161718191a1b1c1d1e1f20"
        let secret = Data(repeating: 0x09, count: 32)
        let pair = try ChannelSubscriberKeypair.fromSecret(secret: secret)
        let unbound = ChannelsUiState()
        unbound.setSubscriberKeys(
            channelId: channel, keys: ChannelSubscriberKeys(secret: secret, publicKey: pair.`public`()))
        // Unbound: no counter is handed out (nowhere to persist it).
        XCTAssertThrowsError(try unbound.nextCounter(channelId: channel))
        XCTAssertTrue(ChannelsUiState(accountIdB64: alice).subscriberKeysByChannel.isEmpty)
        XCTAssertTrue(ChannelsUiState(accountIdB64: alice).counterByChannel.isEmpty)
    }

    func testForgetDropsOnlyThatAccountsChannelKeys() throws {
        let channel = "2122232425262728292a2b2c2d2e2f30"
        let secret = Data(repeating: 0x05, count: 32)
        let pair = try ChannelSubscriberKeypair.fromSecret(secret: secret)
        let keys = ChannelSubscriberKeys(secret: secret, publicKey: pair.`public`())
        ChannelsUiState(accountIdB64: alice).setSubscriberKeys(channelId: channel, keys: keys)
        ChannelsUiState(accountIdB64: bob).setSubscriberKeys(channelId: channel, keys: keys)

        ChannelsUiState.forget(accountIdB64: alice)

        XCTAssertTrue(ChannelsUiState(accountIdB64: alice).subscriberKeysByChannel.isEmpty)
        XCTAssertEqual(ChannelsUiState(accountIdB64: bob).subscriberKeysByChannel[channel], keys)
    }

    // MARK: - Channel rotation state

    func testRotationStateIsPerAccount() {
        let state = ChannelRotationState(lastRotationMs: 42, postsSinceRotation: 3)
        ChannelRotationStateStore.save(accountIdB64: alice, channelId: "c1", state: state, defaults: defaults)

        XCTAssertEqual(ChannelRotationStateStore.load(
            accountIdB64: alice, channelId: "c1", defaults: defaults)?.lastRotationMs, 42)
        XCTAssertNil(ChannelRotationStateStore.load(accountIdB64: bob, channelId: "c1", defaults: defaults))
        XCTAssertNil(ChannelRotationStateStore.load(accountIdB64: nil, channelId: "c1", defaults: defaults))

        // No account: a genesis state comes back, and nothing is stored.
        _ = ChannelRotationStateStore.loadOrSeed(
            accountIdB64: nil, channelId: "c2", nowMs: 7, defaults: defaults)
        XCTAssertNil(ChannelRotationStateStore.load(accountIdB64: alice, channelId: "c2", defaults: defaults))

        ChannelRotationStateStore.save(accountIdB64: bob, channelId: "c1", state: state, defaults: defaults)
        ChannelRotationStateStore.clear(accountIdB64: alice, defaults: defaults)
        XCTAssertNil(ChannelRotationStateStore.load(accountIdB64: alice, channelId: "c1", defaults: defaults))
        XCTAssertNotNil(ChannelRotationStateStore.load(accountIdB64: bob, channelId: "c1", defaults: defaults))
    }

    // MARK: - Co-published journals

    func testCoPublisherMarksArePerAccount() {
        var current: String? = alice
        let marks = CoPublishedJournals(defaults: defaults, accountIdB64: { current })
        marks.mark(journalIdHex: "AB12", accountIdB64: current)
        XCTAssertTrue(marks.contains(journalIdHex: "ab12"))

        // The channel's owner signed in on the same phone still claims.
        current = bob
        XCTAssertFalse(marks.contains(journalIdHex: "ab12"))

        current = nil
        marks.mark(journalIdHex: "cd34", accountIdB64: current)
        XCTAssertFalse(marks.contains(journalIdHex: "cd34"), "no account, nothing marked")

        current = bob
        marks.mark(journalIdHex: "ab12", accountIdB64: current)
        marks.clear(accountIdB64: alice)
        current = alice
        XCTAssertFalse(marks.contains(journalIdHex: "ab12"))
        current = bob
        XCTAssertTrue(marks.contains(journalIdHex: "ab12"))
    }

    // MARK: - Played marks on voice messages and video notes

    func testPlayedMarksArePerAccount() {
        var current: String? = alice
        let prefs = MediaPrefs(defaults: defaults, accountIdB64: { current })
        prefs.markPlayed("blob-1", accountIdB64: current)
        XCTAssertEqual(prefs.playedKeys(), ["blob-1"])

        current = bob
        XCTAssertTrue(prefs.playedKeys().isEmpty)
        prefs.markPlayed("blob-2", accountIdB64: current)

        current = nil
        XCTAssertTrue(prefs.playedKeys().isEmpty)
        prefs.markPlayed("blob-3", accountIdB64: current)

        prefs.clearPlayed(accountIdB64: alice)
        current = alice
        XCTAssertTrue(prefs.playedKeys().isEmpty)
        current = bob
        XCTAssertEqual(prefs.playedKeys(), ["blob-2"], "forget of one account leaves the other's")
    }

    /// The device preferences beside the played set stay device-wide: they
    /// describe this phone, not an account.
    func testDevicePreferencesStayDeviceWide() {
        var current: String? = alice
        let prefs = MediaPrefs(defaults: defaults, accountIdB64: { current })
        prefs.autoplayVideoNotes = true
        current = bob
        XCTAssertTrue(prefs.autoplayVideoNotes)
    }
}
