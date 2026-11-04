// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0060 section 7 — the "who can find me" setting and the reach-key
/// version counters carried between the account's devices as DeviceGroup
/// `ServiceEnvelope`s, and section 3's channel unsubscribe request. Against
/// the real core codecs and dispatcher.
@MainActor
final class ScopedDiscoveryDeviceSyncTests: XCTestCase {

    private func center() throws -> ReachCenter {
        let c = ReachCenter(
            persistence: InMemoryReachPersistence(),
            // Unreachable on purpose: a unit test never registers anything.
            ds: Offline.ds())
        c.activate(
            accountIdB64: "acct",
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32)))
        return c
    }

    private func settingEnvelope(_ mode: FfiVisibilityMode, stamp: UInt64) throws -> Data {
        var s = chooseVisibilityMode(setting: defaultVisibilitySetting(), mode: mode)
        s.updatedAtMs = stamp
        return try composeVisibilityEnvelope(setting: s)
    }

    private func reachStateEnvelope(fofVersion: UInt32, stamp: UInt64) throws -> Data {
        try composeReachStateEnvelope(state: FfiReachState(
            entries: [FfiReachStateEntry(class: .fof, scopeId: Data(), version: fofVersion)],
            updatedAtMs: stamp))
    }

    // MARK: - Visibility

    func testANewerSettingIsAdopted() throws {
        let c = try center()
        let envelope = try settingEnvelope(.nobody, stamp: 1_000)
        XCTAssertEqual(c.adoptDeviceEnvelope(envelope), .adoptedSetting)
        XCTAssertEqual(c.setting.mode, .nobody)
        XCTAssertEqual(c.setting.updatedAtMs, 1_000)
    }

    func testAnOlderOrEqualSettingIsRefused() throws {
        let c = try center()
        XCTAssertEqual(c.adoptDeviceEnvelope(try settingEnvelope(.nobody, stamp: 1_000)), .adoptedSetting)
        XCTAssertEqual(c.adoptDeviceEnvelope(try settingEnvelope(.everyone, stamp: 1_000)), .notNewer,
                       "an equal stamp is not newer")
        XCTAssertEqual(c.adoptDeviceEnvelope(try settingEnvelope(.everyone, stamp: 999)), .notNewer)
        XCTAssertEqual(c.setting.mode, .nobody)
        XCTAssertEqual(c.setting.updatedAtMs, 1_000)
    }

    // MARK: - The backup (D37, D38)

    func testABackupCarriesTheChosenSettingAndNotTheUntouchedDefault() throws {
        let c = try center()
        XCTAssertNil(c.backupSettingEnvelope(accountIdB64: "acct"), "the default at stamp 0")
        let envelope = try settingEnvelope(.everyone, stamp: 1_000)
        c.adoptDeviceEnvelope(envelope)
        XCTAssertEqual(c.backupSettingEnvelope(accountIdB64: "acct"), envelope)
    }

    func testARestoredSettingForTheActiveAccountTakesTheDevicePath() throws {
        let c = try center()
        XCTAssertTrue(c.adoptRestoredSetting(
            envelope: try settingEnvelope(.everyone, stamp: 1_000), accountIdB64: "acct"))
        XCTAssertEqual(c.setting.mode, .everyone)
        XCTAssertFalse(c.adoptRestoredSetting(
            envelope: try settingEnvelope(.nobody, stamp: 900), accountIdB64: "acct"), "nothing older")
        XCTAssertEqual(c.setting.mode, .everyone)
    }

    func testARestoredSettingBeforeTheSessionIsWhatActivationReads() throws {
        let persistence = InMemoryReachPersistence()
        let c = ReachCenter(persistence: persistence, ds: Offline.ds())
        XCTAssertTrue(c.adoptRestoredSetting(
            envelope: try settingEnvelope(.everyone, stamp: 1_000), accountIdB64: "acct"))
        c.activate(
            accountIdB64: "acct",
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32)))
        XCTAssertEqual(c.setting.mode, .everyone)
        XCTAssertEqual(c.setting.updatedAtMs, 1_000)
    }

    func testAnUndecodableRestoredSettingChangesNothing() throws {
        let persistence = InMemoryReachPersistence()
        let c = ReachCenter(persistence: persistence, ds: Offline.ds())
        XCTAssertFalse(c.adoptRestoredSetting(envelope: Data([9, 9]), accountIdB64: "acct"))
        XCTAssertNil(persistence.load(accountIdB64: "acct"))
    }

    func testARestoreDropsTheNoteThatEveryoneWasPublished() throws {
        let c = try center()
        c.setPublishedEveryone(true)
        c.forgetPublishedEveryone(accountIdB64: "acct")
        XCTAssertNil(c.publishedEveryone)

        let persistence = InMemoryReachPersistence()
        var held = ReachRecord()
        held.publishedEveryone = true
        persistence.save(held, accountIdB64: "other")
        let idle = ReachCenter(persistence: persistence, ds: Offline.ds())
        idle.forgetPublishedEveryone(accountIdB64: "other")
        XCTAssertNil(persistence.load(accountIdB64: "other")?.publishedEveryone)
    }

    /// Everyone chosen with nothing to publish leaves the person unfindable:
    /// the setting's screen says so, it is not only a log line.
    func testEveryoneWithNothingToPublishIsNotASilentStop() async throws {
        let c = try center()
        c.adoptDeviceEnvelope(try settingEnvelope(.everyone, stamp: 1_000))
        c.sessionTokenProvider = { "token" }
        c.ownNameProvider = { (nil, nil, nil) }
        await c.applyEveryone(discovery: DiscoveryClient())
        XCTAssertEqual(c.status, .failed)
        XCTAssertNil(c.publishedEveryone)
    }

    /// A rotation keeps the name and moves the key and the mailbox the public
    /// record names, so what was published no longer stands.
    func testAMovedKeyOrMailboxIsANewPublication() {
        func key(_ account: UInt8, _ binding: UInt8) -> String {
            ReachRules.everyonePublishKey(
                displayName: "Nora", username: "nora", certificate: Data([1]),
                accountKey: Data([account]), mailboxBinding: Data([binding]))
        }
        XCTAssertEqual(key(1, 1), key(1, 1))
        XCTAssertNotEqual(key(1, 1), key(2, 1))
        XCTAssertNotEqual(key(1, 1), key(1, 2))
    }

    func testChoosingASettingAsksForABackup() throws {
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: Offline.ds())
        let account = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))
        c.activate(
            accountIdB64: account,
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32)))
        defer { AutoBackupStore.clear(accountIdB64: account) }
        XCTAssertNil(AutoBackupStore.load(accountIdB64: account).dirtySinceMs)
        c.updateSetting(chooseVisibilityMode(setting: c.setting, mode: .everyone))
        XCTAssertNotNil(AutoBackupStore.load(accountIdB64: account).dirtySinceMs)
    }

    // MARK: - ReachState

    func testANewerReachStateIsAdoptedAndChangesTheGrants() throws {
        let c = try center()
        c.updateSetting(setContactsOfContacts(setting: c.setting, on: true))
        let before = c.fofGrant()
        // ADR-0078 section 9: the setting change rotated this device's group
        // and channel keys, which the incoming state lacks, so the two are
        // merged and the merge goes back to the other devices.
        XCTAssertEqual(c.adoptDeviceEnvelope(try reachStateEnvelope(fofVersion: 7, stamp: 500)),
                       .mergedReachState)
        XCTAssertEqual(c.fofGrant()?.version, 7)
        XCTAssertNotEqual(c.fofGrant()?.seed, before?.seed)
    }

    /// An older state, or the same one again, changes nothing and sends
    /// nothing.
    func testAnOlderOrEqualReachStateChangesNothing() throws {
        let c = try center()
        var sent: [Data] = []
        c.onDeviceEnvelope = { sent.append($0) }
        XCTAssertEqual(c.adoptDeviceEnvelope(try reachStateEnvelope(fofVersion: 7, stamp: 500)),
                       .adoptedReachState)
        XCTAssertEqual(c.adoptDeviceEnvelope(try reachStateEnvelope(fofVersion: 7, stamp: 500)), .notNewer)
        XCTAssertEqual(c.adoptDeviceEnvelope(try reachStateEnvelope(fofVersion: 5, stamp: 499)), .notNewer)
        XCTAssertEqual(c.fofGrant()?.version, 7)
        XCTAssertTrue(sent.isEmpty)
        let kept = c.deviceEnvelopes(accountIdB64: "acct")
            .compactMap { try? decodeReachStateEnvelope(plaintext: $0) }
        XCTAssertEqual(kept.first?.updatedAtMs, 500)
    }

    /// Two devices rotate at the same time, each a different key: the merge
    /// keeps both rotations, stamps after both, and is sent back so the other
    /// device gets the one it lacked.
    func testAConcurrentRotationOnEachSideKeepsBoth() throws {
        let c = try center()
        let groupA = Data(repeating: 0xA1, count: 32)
        var sent: [Data] = []
        c.onDeviceEnvelope = { sent.append($0) }
        c.rotate(.fof)
        XCTAssertEqual(c.fofGrant()?.version, 1)
        let localStamp = try decodeReachStateEnvelope(plaintext: try XCTUnwrap(sent.last)).updatedAtMs
        sent.removeAll()

        // The other device rotated this account's `member` key (one for the
        // account, ADR-0078 section 9), not knowing about the `fof` rotation,
        // stamped later.
        let theirs = try composeReachStateEnvelope(state: FfiReachState(
            entries: [FfiReachStateEntry(class: .member, scopeId: Data(), version: 2)],
            updatedAtMs: localStamp + 10))
        XCTAssertEqual(c.adoptDeviceEnvelope(theirs), .mergedReachState)
        XCTAssertEqual(c.fofGrant()?.version, 1, "this device's rotation survives")
        XCTAssertEqual(
            c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .member }?.version, 2,
            "the other device's rotation is kept")

        XCTAssertEqual(sent.count, 1, "the merge goes back to the other devices")
        let merged = try decodeReachStateEnvelope(plaintext: sent[0])
        XCTAssertGreaterThan(merged.updatedAtMs, localStamp + 10)
        XCTAssertEqual(ReachRules.version(of: .fof, scope: Data(), in: merged), 1)
        XCTAssertEqual(ReachRules.version(of: .member, scope: Data(), in: merged), 2)
        XCTAssertEqual(c.deviceEnvelopes(accountIdB64: "acct").last, sent[0],
                       "kept exactly as sent, stamp included")

        // The merge echoed back from the other device changes nothing more.
        XCTAssertEqual(c.adoptDeviceEnvelope(sent[0]), .notNewer)
        XCTAssertEqual(sent.count, 1)
    }

    // MARK: - Re-send

    /// What is re-sent to a newly admitted device is the adopted envelope,
    /// byte for byte: never re-stamped.
    func testAReSendCarriesTheOriginalStamp() throws {
        let c = try center()
        let setting = try settingEnvelope(.nobody, stamp: 1_234)
        let state = try reachStateEnvelope(fofVersion: 3, stamp: 567)
        c.adoptDeviceEnvelope(setting)
        c.adoptDeviceEnvelope(state)
        let resend = c.deviceEnvelopes(accountIdB64: "acct")
        XCTAssertEqual(resend, [setting, state])
        XCTAssertEqual(try decodeVisibilityEnvelope(plaintext: resend[0]).updatedAtMs, 1_234)
        XCTAssertEqual(try decodeReachStateEnvelope(plaintext: resend[1]).updatedAtMs, 567)
        XCTAssertEqual(c.deviceEnvelopes(accountIdB64: "another"), [],
                       "nothing for an account that is not active")
    }

    /// A local change is handed to the DeviceGroup once, with the stamp it is
    /// kept under; an adopted one is not echoed back.
    func testALocalChangeIsSentAndAnAdoptedOneIsNot() throws {
        let c = try center()
        var sent: [Data] = []
        c.onDeviceEnvelope = { sent.append($0) }
        c.adoptDeviceEnvelope(try settingEnvelope(.nobody, stamp: 10))
        XCTAssertTrue(sent.isEmpty)
        c.updateSetting(chooseVisibilityMode(setting: c.setting, mode: .everyone))
        c.rotate(.fof)
        // The setting, the state after the setting's group and channel
        // rotation (ADR-0078 section 9), and the state after the fof rotation.
        XCTAssertEqual(sent.count, 3)
        XCTAssertEqual(try decodeVisibilityEnvelope(plaintext: sent[0]).updatedAtMs, c.setting.updatedAtMs)
        // A new device gets the setting and the newest state only.
        XCTAssertEqual(c.deviceEnvelopes(accountIdB64: "acct"), [sent[0], sent[2]])
    }

    /// The untouched default (stamp 0) is not sent to a new device: it holds
    /// the same and would refuse it.
    func testTheUntouchedDefaultIsNotSent() throws {
        let c = try center()
        XCTAssertEqual(c.deviceEnvelopes(accountIdB64: "acct"), [])
    }

    // MARK: - Foreign envelopes

    func testAForeignEnvelopeInsideAServiceEnvelopeIsDropped() throws {
        let c = try center()
        let before = c.setting
        XCTAssertEqual(c.adoptDeviceEnvelope(composeProfileRequest()), .foreign)
        XCTAssertEqual(c.adoptDeviceEnvelope(composeCommitterBeacon()), .foreign)
        XCTAssertEqual(c.adoptDeviceEnvelope(Data("not an envelope".utf8)), .foreign)
        XCTAssertEqual(c.setting, before)
    }

    /// The DeviceGroup receive path hands a `ServiceEnvelope`'s bytes on
    /// verbatim; the typed bodies keep their own handling.
    func testTheSyncRepositoryRoutesAServiceEnvelope() throws {
        let repo = DeviceGroupSyncRepository()
        var routed: [Data] = []
        repo.onServiceEnvelope = { routed.append($0) }
        let envelope = try settingEnvelope(.nobody, stamp: 42)
        repo.applyParsed(ParsedDeviceGroupPayload(
            hlc: 1, senderLeaf: 1, body: .serviceEnvelope(envelope: envelope)))
        XCTAssertEqual(routed, [envelope])
    }

    func testPendingSendsAreSlottedByKind() throws {
        XCTAssertEqual(
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try settingEnvelope(.nobody, stamp: 1)),
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try settingEnvelope(.everyone, stamp: 2)))
        XCTAssertNotEqual(
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try settingEnvelope(.nobody, stamp: 1)),
            DeviceGroupSyncRepository.serviceEnvelopeSlot(try reachStateEnvelope(fofVersion: 1, stamp: 1)))
    }

    // MARK: - Channel unsubscribe (ADR-0060 section 3)

    func testTheUnsubscribeRequestShape() throws {
        let kp = ChannelSubscriberKeypair.generate()
        let keys = ChannelSubscriberKeys(secret: kp.secret(), publicKey: kp.`public`())
        let channelId = String(repeating: "ab", count: 16)
        let request = try ChannelUnsubscribe.request(
            keys: keys, channelId: channelId, issuedAtMs: 1_700_000_000_123,
            client: ChannelsClient(baseURL: "https://example.test/channels"))
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(
            request.url?.absoluteString,
            "https://example.test/channels/v1/channels/\(channelId)/subscribers/"
                + ChannelsComposer.bytesToHex(kp.`public`()))
        XCTAssertEqual(ChannelsClient.unsubscribeSigHeader, unsubscribeSigHeaderName())
        let header = try XCTUnwrap(request.value(forHTTPHeaderField: "x-lakko-unsubscribe-sig"))
        let parts = header.split(separator: ".", omittingEmptySubsequences: false)
        XCTAssertEqual(parts.count, 2)
        XCTAssertEqual(parts.first, "1700000000123")
        XCTAssertEqual(base64UrlDecodeNoPad(String(parts[1]))?.count, 64,
                       "an Ed25519 signature, base64url without padding")
        XCTAssertNil(request.httpBody)
    }

    func testAnUnsubscribeRefusesAChannelIdThatIsNotHex() {
        let kp = ChannelSubscriberKeypair.generate()
        let keys = ChannelSubscriberKeys(secret: kp.secret(), publicKey: kp.`public`())
        XCTAssertThrowsError(try ChannelUnsubscribe.request(
            keys: keys, channelId: "not-hex", issuedAtMs: 1))
    }
}
