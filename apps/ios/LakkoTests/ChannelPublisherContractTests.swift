// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0087 sections 7 and 15, amended 2026-11-08 (UX 15.2.1, 15.2.2): the
/// earlier channel seeds a new publisher gets, the rotate request a
/// co-publisher sends instead of rotating, and the publisher card's waiting
/// states. Web and Android implement the same rules.
final class ChannelPublisherContractTests: XCTestCase {

    private func seed(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    // MARK: - A. Earlier seeds: what the owner sends

    func testTheOwnerSendsTheNewestSeedsNotSentYetInAscendingOrder() {
        let held: [UInt32: Data] = [0: seed(0), 1: seed(1), 2: seed(2), 3: seed(3)]
        let first = ChannelAdmissionSeedShare.epochSeedsToSend(held: held, sent: [], max: 64)
        XCTAssertEqual(first.map(\.epoch), [0, 1, 2, 3])
        XCTAssertEqual(first.first?.seed, seed(0))
        // Recorded as sent: only what is new goes next time.
        let next = ChannelAdmissionSeedShare.epochSeedsToSend(
            held: held.merging([4: seed(4)]) { a, _ in a }, sent: [0, 1, 2, 3], max: 64)
        XCTAssertEqual(next.map(\.epoch), [4])
        XCTAssertTrue(ChannelAdmissionSeedShare.epochSeedsToSend(held: held, sent: [0, 1, 2, 3], max: 64).isEmpty)
    }

    func testAtMostTheNewestSixtyFourAreSent() {
        var held: [UInt32: Data] = [:]
        for epoch in UInt32(0)..<100 { held[epoch] = seed(UInt8(epoch)) }
        let sent = ChannelAdmissionSeedShare.epochSeedsToSend(held: held, sent: [], max: 64)
        XCTAssertEqual(sent.count, 64)
        XCTAssertEqual(sent.first?.epoch, 36)
        XCTAssertEqual(sent.last?.epoch, 99)
        XCTAssertEqual(Int(channelEpochSeedsMax()), 64)
    }

    // MARK: - A. Earlier seeds: what the publisher keeps

    func testAReceivedSeedIsAddedOnlyForAnEpochWithNoSeedHeld() {
        let keep = ChannelAdmissionSeedShare.epochSeedsToKeep(
            received: [(0, seed(9)), (1, seed(1)), (2, seed(9)), (1, seed(8))],
            held: [0: seed(0), 2: seed(2)])
        XCTAssertEqual(keep.map(\.epoch), [1])
        XCTAssertEqual(keep.first?.seed, seed(1))
    }

    /// The envelope opens only for the account it names, with the seeds in
    /// ascending order.
    func testTheEarlierSeedsOpenOnlyForTheirPublisher() throws {
        let publisher = try Identity.fromSeed(seed: Data(repeating: 0x21, count: 32))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x22, count: 32))
        let channelId = Data(repeating: 0x33, count: 16)
        let envelope = try composeChannelEpochSeedsEnvelope(
            channelId: channelId, memberAccountKey: publisher.signaturePublicKey(),
            memberX25519PublicKey: publisher.x25519PublicKey(),
            seeds: [FfiChannelEpochSeed(epoch: 0, seed: seed(0)), FfiChannelEpochSeed(epoch: 1, seed: seed(1))])
        let decoded = try decodeChannelEpochSeedsEnvelope(plaintext: envelope)
        XCTAssertEqual(decoded.channelId, channelId)
        XCTAssertEqual(decoded.memberAccountKey, publisher.signaturePublicKey())
        let opened = try openChannelEpochSeedsEnvelope(plaintext: envelope, accountIdentity: publisher)
        XCTAssertEqual(opened.map(\.epoch), [0, 1])
        XCTAssertEqual(opened.map(\.seed), [seed(0), seed(1)])
        XCTAssertThrowsError(try openChannelEpochSeedsEnvelope(plaintext: envelope, accountIdentity: stranger))
        let kind = try dispatchIncomingEnvelope(plaintext: envelope).kind
        XCTAssertEqual(kind, .channelEpochSeeds)
    }

    // MARK: - B. The rotate request

    func testTheOwnerRotatesOnlyForACurrentPublishersRequest() {
        let channel = Data(repeating: 1, count: 16)
        let owner = Data(repeating: 0xA0, count: 32)
        let publisher = Data(repeating: 0xB0, count: 32)
        let members: Set<Data> = [owner, publisher]
        func honors(
            request: Data? = channel, sender: Data? = publisher, sentAt: Int64? = 2_000, last: UInt64? = 1_000
        ) -> Bool {
            ChannelPublisherLoops.honorsRotateRequest(
                requestChannelId: request, channelId: channel, sender: sender, ownerAccountKey: owner,
                publisherAccountKeys: members, sentAtMs: sentAt, lastRotationMs: last)
        }
        XCTAssertTrue(honors())
        XCTAssertFalse(honors(request: Data(repeating: 2, count: 16)), "another channel")
        XCTAssertFalse(honors(request: nil), "not a rotate request")
        XCTAssertFalse(honors(sender: Data(repeating: 0xC0, count: 32)), "not a current publisher")
        XCTAssertFalse(honors(sender: owner), "the owner's own")
        XCTAssertFalse(honors(sender: nil), "a sender that did not resolve")
        XCTAssertFalse(honors(sentAt: 900), "answered already by the last rotation")
        XCTAssertFalse(honors(sentAt: nil), "no time to judge it by")
        XCTAssertTrue(honors(sentAt: nil, last: nil), "never rotated yet")
    }

    func testTheRotateRequestEnvelopeNamesItsChannel() throws {
        let channelId = Data(repeating: 0x44, count: 16)
        let envelope = try composeChannelRotateRequestEnvelope(channelId: channelId)
        XCTAssertEqual(envelope.count, Int(channelRotateRequestEnvelopeLen()))
        XCTAssertEqual(try decodeChannelRotateRequestEnvelope(plaintext: envelope), channelId)
        XCTAssertEqual(try dispatchIncomingEnvelope(plaintext: envelope).kind, .channelRotateRequest)
    }

    /// The co-publisher's rotate request is owed before it is sent: a send
    /// that fails keeps it owed for the next pass, one that publishes clears
    /// it, and nothing is sent when nothing is owed.
    @MainActor
    func testARotateRequestStaysOwedUntilItPublishes() async {
        let channels = ChannelsUiState()
        let id = String(repeating: "e", count: 32)
        var sends = 0
        await ChannelPublisherLoops.sendOwedRotateRequest(channelId: id, channels: channels) {
            sends += 1
            return true
        }
        XCTAssertEqual(sends, 0, "nothing owed, nothing sent")

        channels.updateExtras(id) { $0.rotateRequestOwed = true }
        await ChannelPublisherLoops.sendOwedRotateRequest(channelId: id, channels: channels) {
            sends += 1
            return false
        }
        XCTAssertEqual(sends, 1)
        XCTAssertEqual(channels.extras(id).rotateRequestOwed, true, "a failed send stays owed")

        await ChannelPublisherLoops.sendOwedRotateRequest(channelId: id, channels: channels) {
            sends += 1
            return true
        }
        XCTAssertEqual(sends, 2)
        XCTAssertNil(channels.extras(id).rotateRequestOwed, "cleared once it published")
    }

    /// The owed mark is part of the stored extras (behind the account store
    /// gate, deleted with them), and a row written before it decodes.
    func testTheOwedRotateRequestIsStoredAndOldRowsDecode() throws {
        var extras = ChannelExtras()
        extras.rotateRequestOwed = true
        let decoded = try JSONDecoder().decode(ChannelExtras.self, from: JSONEncoder().encode(extras))
        XCTAssertEqual(decoded.rotateRequestOwed, true)
        var json = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(ChannelExtras())) as? [String: Any])
        json.removeValue(forKey: "rotateRequestOwed")
        let old = try JSONDecoder().decode(ChannelExtras.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertNil(old.rotateRequestOwed)
    }

    @MainActor
    func testTheOwedRotateRequestGoesWithTheAccountsStoredExtras() {
        let defaults = UserDefaults(suiteName: "lakko-test-\(UUID().uuidString)")!
        let account = "acct-\(UUID().uuidString)"
        var extras = ChannelExtras()
        extras.rotateRequestOwed = true
        ChannelExtrasStore.save(["c": extras], account, defaults)
        XCTAssertEqual(ChannelExtrasStore.load(account, defaults)["c"]?.rotateRequestOwed, true)
        ChannelExtrasStore.delete(account, defaults)
        XCTAssertNil(ChannelExtrasStore.load(account, defaults)["c"])
    }

    // MARK: - D. The waiting states on the card

    private let day: Int64 = 24 * 60 * 60 * 1000

    @MainActor
    private func state(
        publisher: Bool = false, phase: CoPublisherAcceptance.Phase? = nil, sentAt: Int64? = nil, now: Int64 = 0
    ) -> CoPublisherAcceptance.CardState {
        CoPublisherAcceptance.cardState(isPublisher: publisher, phase: phase, requestedAtMs: sentAt, nowMs: now)
    }

    @MainActor
    func testTheCardWaitsThenOffersAskAgainAfterADay() {
        XCTAssertEqual(state(), .accept)
        XCTAssertEqual(state(phase: .sending), .sending)
        XCTAssertEqual(state(phase: .waiting, sentAt: 0, now: 1), .waiting)
        XCTAssertEqual(state(sentAt: 0, now: day - 1), .waiting, "after a restart")
        XCTAssertEqual(state(sentAt: 0, now: day), .noAnswer)
        XCTAssertEqual(state(phase: .waiting, sentAt: 0, now: 2 * day), .noAnswer)
        XCTAssertEqual(state(publisher: true, sentAt: 0, now: 8 * day), .open, "approved")
    }

    @MainActor
    func testADeadLinkOnAskAgainKeepsTheRequestAndSaysSo() {
        let dead = CoPublisherJoin.Failure(message: .key("invite.link.invalid"), offersRetry: false, kind: .linkDead)
        XCTAssertEqual(state(phase: .linkDead, sentAt: 0, now: 8 * day), .linkDead)
        XCTAssertEqual(state(phase: .failed(dead), sentAt: 0, now: 8 * day), .linkDead)
        // A first Accept on a dead link: the line, no button.
        XCTAssertEqual(state(phase: .failed(dead)), .failed(dead, askAgain: false))
        let offline = CoPublisherJoin.Failure(message: .key("common.error.offline"), offersRetry: true, kind: .offline)
        XCTAssertEqual(state(phase: .failed(offline), sentAt: 0, now: 8 * day), .failed(offline, askAgain: true))
    }

    func testNoConnectionReadsAsOffline() {
        XCTAssertTrue(CoPublisherJoin.Failure.isOffline(URLError(.notConnectedToInternet)))
        XCTAssertTrue(CoPublisherJoin.Failure.isOffline(
            DsClientError.network(endpoint: "x", underlying: URLError(.timedOut))))
        struct Other: Error {}
        XCTAssertFalse(CoPublisherJoin.Failure.isOffline(Other()))
    }
}
