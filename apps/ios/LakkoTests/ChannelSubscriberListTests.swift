// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Base walk 2026-11-13 (scenario 27): `docs/ux/group-and-channel-privacy.md`
/// 15.1 / 7.3. A private channel's owner saw a nameless card as "Member", and
/// both subscribers again as anonymous rows, so it could not tell whom
/// `Remove` took. The list is built by the shared rule over the real core.
@MainActor
final class ChannelSubscriberListTests: XCTestCase {
    private let iphonePk = Data(repeating: 0xAA, count: 32)
    private let pixelPk = Data(repeating: 0xBB, count: 32)

    private func member(_ pk: Data, account: UInt8, name: String?, username: String? = nil)
        -> ChannelMemberCards.Member {
        ChannelMemberCards.Member(
            subscriberPk: pk,
            claim: ProfileClaim(displayName: name, username: username, updatedAtMs: 1),
            accountKey: Data(repeating: account, count: 32),
            subscriberPks: [pk])
    }

    private func row(_ pk: Data, seq: UInt64) -> ChannelsClient.SubscriberRow {
        ChannelsClient.SubscriberRow(seq: seq, pubkeyHex: ChannelsComposer.bytesToHex(pk), sealedTicket: nil)
    }

    func testANamelessCardIsNotANamedRow() {
        let list = SubscriberList.build(
            members: [member(iphonePk, account: 1, name: nil)],
            rows: [row(iphonePk, seq: 1), row(pixelPk, seq: 2)], count: 2, canRemove: true)
        XCTAssertTrue(list.people.isEmpty)
        XCTAssertEqual(list.others.map(\.seq), [1, 2])
        XCTAssertEqual(list.othersCount, 2)
    }

    func testANamedPersonIsNotListedAgainAndCanBeRemoved() {
        let list = SubscriberList.build(
            members: [member(pixelPk, account: 2, name: "Pixel Four"), member(iphonePk, account: 1, name: nil)],
            rows: [row(iphonePk, seq: 1), row(pixelPk, seq: 2)], count: 2, canRemove: true)
        XCTAssertEqual(list.people.map(\.member.claim.displayName), ["Pixel Four"])
        XCTAssertEqual(list.people.first?.pubkeysHex, [ChannelsComposer.bytesToHex(pixelPk).lowercased()])
        XCTAssertEqual(list.people.first?.removable, true)
        XCTAssertEqual(list.others.map(\.seq), [1])
        XCTAssertEqual(list.othersCount, 1)
    }

    func testAPublicChannelRemovesNobodyAndCountsTheOthers() {
        let list = SubscriberList.build(
            members: [member(pixelPk, account: 2, name: "Pixel Four"), member(iphonePk, account: 1, name: nil)],
            rows: [], count: 5, canRemove: false)
        XCTAssertEqual(list.people.count, 1)
        XCTAssertEqual(list.people.first?.removable, false)
        XCTAssertTrue(list.others.isEmpty)
        XCTAssertEqual(list.othersCount, 4)
    }

    func testAUsernameAloneNamesTheRow() {
        let list = SubscriberList.build(
            members: [member(iphonePk, account: 1, name: nil, username: "qa_ip")],
            rows: [], count: 1, canRemove: false)
        XCTAssertEqual(list.people.count, 1)
        XCTAssertEqual(list.othersCount, 0)
    }

    func testTheViewersOwnKeysAreOneYouRowNeverRemovedNorListed() {
        let ownPk = Data(repeating: 0xCC, count: 32)
        var you = member(ownPk, account: 9, name: nil)
        you.isSelf = true
        let list = SubscriberList.build(
            members: [you, member(pixelPk, account: 2, name: "Pixel Four")],
            rows: [row(iphonePk, seq: 1), row(pixelPk, seq: 2), row(ownPk, seq: 3)], count: 3, canRemove: true)
        XCTAssertEqual(list.people.map(\.member.isSelf), [true, false])
        XCTAssertEqual(list.people.first?.removable, false)
        XCTAssertEqual(list.others.map(\.seq), [1])
        XCTAssertEqual(list.othersCount, 1)
    }

    func testGoingPrivateKeepsEveryoneSubscribedAndNoTicketAloneLetsNobodyIn() {
        func v(_ seq: UInt64, _ live: Bool, _ mark: UInt64, _ set: Bool, _ own: Bool = false) -> SubscriberAdmission.LetIn {
            SubscriberAdmission.letInVerdict(seq: seq, ticketLive: live, mark: mark, markSet: set, isOwnKey: own)
        }
        // UX 4.1 — a row from before the channel went private stays.
        XCTAssertEqual(v(4, false, 5, true), .admit)
        // Above the mark with no valid ticket: removed on the device that set it.
        XCTAssertEqual(v(9, false, 5, true), .remove)
        XCTAssertEqual(v(10, true, 5, true), .admit)
        // A device that did not set the mark: neither let in nor removed.
        XCTAssertEqual(v(9, false, 0, false), .leave)
        XCTAssertEqual(v(10, true, 0, false), .admit)
        // This device's own card key is never removed.
        XCTAssertEqual(v(9, false, 5, true, true), .leave)
    }

    func testTheMarkMovesOnlyPastRowsLetInWhereThisDeviceDidNotSetIt() {
        let judged: [(seq: UInt64, verdict: SubscriberAdmission.LetIn)] = [(7, .admit), (9, .leave), (12, .admit)]
        XCTAssertEqual(SubscriberAdmission.advancedMark(5, markSet: true, judged: judged), 12)
        XCTAssertEqual(SubscriberAdmission.advancedMark(5, markSet: false, judged: judged), 7)
        XCTAssertEqual(SubscriberAdmission.advancedMark(20, markSet: true, judged: judged), 20)
        XCTAssertEqual(SubscriberAdmission.advancedMark(5, markSet: true, judged: []), 5)
    }

    /// The rotation reseal resealed the new seed to EVERY listed key,
    /// including a row with no valid ticket a server could have added.
    func testAPrivateRotationResealsOnlyToTheRowsLetIn() {
        XCTAssertEqual(ChannelReseal.resealThrough(isPublic: false, mark: 8), 8)
        // A private channel on a device that set no mark: the readers only.
        XCTAssertEqual(ChannelReseal.resealThrough(isPublic: false, mark: nil), 0)
        XCTAssertNil(ChannelReseal.resealThrough(isPublic: true, mark: 8))
        XCTAssertNil(ChannelReseal.resealThrough(isPublic: nil, mark: 8))
        let rows = [row(iphonePk, seq: 3), row(pixelPk, seq: 8), row(Data(repeating: 0xEE, count: 32), seq: 9)]
        XCTAssertEqual(
            ChannelReseal.resealKeys(rows: rows, throughSeq: 8),
            [ChannelsComposer.bytesToHex(iphonePk), ChannelsComposer.bytesToHex(pixelPk)])
    }
}
