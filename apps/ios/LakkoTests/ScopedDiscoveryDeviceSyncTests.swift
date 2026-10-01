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
            ds: DsClient(baseURL: "http://127.0.0.1:1"))
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

    // MARK: - ReachState

    func testANewerReachStateIsAdoptedAndChangesTheGrants() throws {
        let c = try center()
        c.updateSetting(setContactsOfContacts(setting: c.setting, on: true))
        let before = c.fofGrant()
        XCTAssertEqual(c.adoptDeviceEnvelope(try reachStateEnvelope(fofVersion: 7, stamp: 500)),
                       .adoptedReachState)
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
        XCTAssertEqual(c.fofGrant()?.version, 2)
        let localStamp = try decodeReachStateEnvelope(plaintext: try XCTUnwrap(sent.last)).updatedAtMs
        sent.removeAll()

        // The other device rotated this account's `member` key for group A,
        // not knowing about the `fof` rotation, stamped later.
        let theirs = try composeReachStateEnvelope(state: FfiReachState(
            entries: [FfiReachStateEntry(class: .member, scopeId: groupA, version: 2)],
            updatedAtMs: localStamp + 10))
        XCTAssertEqual(c.adoptDeviceEnvelope(theirs), .mergedReachState)
        XCTAssertEqual(c.fofGrant()?.version, 2, "this device's rotation survives")
        XCTAssertEqual(
            c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .member }?.version, 2,
            "the other device's rotation is kept")

        XCTAssertEqual(sent.count, 1, "the merge goes back to the other devices")
        let merged = try decodeReachStateEnvelope(plaintext: sent[0])
        XCTAssertGreaterThan(merged.updatedAtMs, localStamp + 10)
        XCTAssertEqual(ReachRules.version(of: .fof, scope: Data(), in: merged), 2)
        XCTAssertEqual(ReachRules.version(of: .member, scope: groupA, in: merged), 2)
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
        XCTAssertEqual(sent.count, 2)
        XCTAssertEqual(try decodeVisibilityEnvelope(plaintext: sent[0]).updatedAtMs, c.setting.updatedAtMs)
        XCTAssertEqual(c.deviceEnvelopes(accountIdB64: "acct"), sent)
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
