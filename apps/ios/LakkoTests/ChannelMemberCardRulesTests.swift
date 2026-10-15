// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0060 section 3 / 4 — the channel member-card rules and the
/// introductions relay, aligned with web (`channels/member-cards.ts`,
/// `channels/member-card-rules.ts`, `reach/ScopedDiscoveryRuntime.tsx`).
@MainActor
final class ChannelMemberCardRulesTests: XCTestCase {

    private let author = Data(repeating: 0xA1, count: 32)
    private let own = Data(repeating: 0x0B, count: 32)
    private let channelHex = String(repeating: "cd", count: 16)

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

    private func channelGrant(version: UInt32 = 3) throws -> FfiReachGrant {
        try deriveReachGrant(
            accountSeed: Data(repeating: 0x44, count: 32), class: .channel,
            scopeId: Data(repeating: 0xCD, count: 16), version: version)
    }

    // MARK: - Rule 4: the directory lookup

    func testACardIsKeptWhenTheDirectoryPublishesNothing() {
        XCTAssertTrue(ChannelMemberCards.cardKept(cardKey: author, publishedKey: nil))
        XCTAssertTrue(ChannelMemberCards.cardKept(cardKey: author, publishedKey: author))
        XCTAssertFalse(ChannelMemberCards.cardKept(cardKey: author, publishedKey: own),
                       "a different published key names someone else's account")
    }

    // MARK: - Rules 5 and 6: file after the reach check, withdraw without a grant

    func testAGrantIsFiledOnlyAfterTheReachCheckAccepts() {
        func decide(_ verified: Bool?) -> ChannelMemberCards.CardGrantDecision {
            ChannelMemberCards.cardGrantDecision(
                isOwnCard: false, authorAccountKey: author, ownAccountKey: own,
                carriesChannelGrant: true, grantVerified: verified)
        }
        XCTAssertEqual(decide(true), .file)
        XCTAssertEqual(decide(false), .skip, "a refused grant is not a narrowed setting")
        XCTAssertEqual(decide(nil), .skip, "an unanswered check proves nothing")
    }

    func testACardWithoutTheGrantWithdrawsOneHeld() {
        XCTAssertEqual(
            ChannelMemberCards.cardGrantDecision(
                isOwnCard: false, authorAccountKey: author, ownAccountKey: own,
                carriesChannelGrant: false, grantVerified: nil),
            .withdraw)
    }

    func testOwnCardsAndUnresolvedAuthorsLeaveTheStoreAlone() {
        XCTAssertEqual(
            ChannelMemberCards.cardGrantDecision(
                isOwnCard: true, authorAccountKey: author, ownAccountKey: own,
                carriesChannelGrant: true, grantVerified: true),
            .skip)
        XCTAssertEqual(
            ChannelMemberCards.cardGrantDecision(
                isOwnCard: false, authorAccountKey: own, ownAccountKey: own,
                carriesChannelGrant: false, grantVerified: nil),
            .skip, "another device of this account")
        XCTAssertEqual(
            ChannelMemberCards.cardGrantDecision(
                isOwnCard: false, authorAccountKey: nil, ownAccountKey: own,
                carriesChannelGrant: false, grantVerified: nil),
            .skip)
    }

    func testWithdrawingRemovesOnlyThatChannelsGrant() throws {
        let c = try center()
        // ADR-0078 — a kept grant's mailbox binding verifies under its author.
        let realAuthor = try Identity.fromSeed(seed: Data(repeating: 0xA1, count: 32))
        let keyB64 = realAuthor.signaturePublicKey().base64EncodedString()
        let other = String(repeating: "ef", count: 16)
        let grant = try deriveReachGrant(
            accountSeed: realAuthor.ed25519Seed(), class: .channel,
            scopeId: Data(repeating: 0xCD, count: 16), version: 3)
        XCTAssertTrue(c.keep(grant, carrier: .channelMemberCard,
                             fromAccountKeyB64: keyB64, scopeHex: channelHex))
        XCTAssertTrue(c.keep(grant, carrier: .channelMemberCard,
                             fromAccountKeyB64: keyB64, scopeHex: other))
        XCTAssertTrue(c.holdsGrant(.channel, fromAccountKeyB64: keyB64, scopeHex: channelHex))
        XCTAssertTrue(c.withdrawHeld(.channel, fromAccountKeyB64: keyB64, scopeHex: channelHex))
        XCTAssertFalse(c.holdsGrant(.channel, fromAccountKeyB64: keyB64, scopeHex: channelHex))
        XCTAssertTrue(c.holdsGrant(.channel, fromAccountKeyB64: keyB64, scopeHex: other))
        XCTAssertFalse(c.withdrawHeld(.channel, fromAccountKeyB64: keyB64, scopeHex: channelHex),
                       "nothing left to withdraw")
    }

    // MARK: - Rule 7: the username goes with the permission

    func testAUsernameShowsOnlyWhileTheGrantIsCarriedAndKept() {
        XCTAssertEqual(
            ChannelMemberCards.shownUsername(username: "anna_w", carriesChannelGrant: true, grantHeld: true),
            "anna_w")
        XCTAssertNil(ChannelMemberCards.shownUsername(
            username: "anna_w", carriesChannelGrant: true, grantHeld: false))
        XCTAssertNil(ChannelMemberCards.shownUsername(
            username: "anna_w", carriesChannelGrant: false, grantHeld: true))
    }

    // MARK: - The relay

    func testTheRelayedBundleIsCappedAtTheCoreMaximum() {
        let cap = Int(maxBundleRecords())
        let records = (0..<(cap + 5)).map { Data([UInt8($0 % 256), UInt8($0 / 256)]) }
        XCTAssertEqual(ChatRepository.relayBundleRecords(records).count, cap)
        XCTAssertEqual(ChatRepository.relayBundleRecords(Array(records.prefix(2))).count, 2)
    }

    func testTheBundleDigestIgnoresOrder() {
        let a = Data([1, 2, 3]), b = Data([4, 5])
        XCTAssertEqual(ChatRepository.introductionsBundleDigest([a, b]),
                       ChatRepository.introductionsBundleDigest([b, a]))
        XCTAssertNotEqual(ChatRepository.introductionsBundleDigest([a]),
                          ChatRepository.introductionsBundleDigest([a, b]))
    }

    /// The offer and the bundle are tracked apart: marking one sent does not
    /// mark the other, and a failed send (not marked) is retried.
    func testOfferAndBundleAreTrackedSeparately() throws {
        let c = try center()
        let g = "aa"
        XCTAssertTrue(c.offerNeedsSending(groupIdHex: g, key: "o1"))
        XCTAssertTrue(c.bundleNeedsSending(groupIdHex: g, digest: "d1"))
        c.markOfferSent(groupIdHex: g, key: "o1")
        XCTAssertFalse(c.offerNeedsSending(groupIdHex: g, key: "o1"))
        XCTAssertTrue(c.bundleNeedsSending(groupIdHex: g, digest: "d1"), "the bundle is still owed")
        c.markBundleSent(groupIdHex: g, digest: "d1")
        XCTAssertFalse(c.bundleNeedsSending(groupIdHex: g, digest: "d1"))
        XCTAssertTrue(c.bundleNeedsSending(groupIdHex: g, digest: "d2"))
        XCTAssertFalse(c.offerNeedsSending(groupIdHex: g, key: "o1"), "a new bundle leaves the offer alone")
    }
}
