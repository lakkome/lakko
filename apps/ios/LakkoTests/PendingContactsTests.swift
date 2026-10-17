// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0079 sections 4 and 5 — the rules of a person added through their
/// invite link before their app answered, and the list rule over the synced
/// link-record register.
final class PendingContactsTests: XCTestCase {

    private func contact(_ hex: String, key: UInt8, at: Int64 = 1_000, name: String = "Anna",
                         username: String = "anna") -> PendingContact {
        PendingContact(
            accountIdHex: hex, signaturePubkey: Data(repeating: key, count: 32),
            x25519Pubkey: Data(repeating: key &+ 1, count: 32), displayName: name,
            username: username, addedAtMs: at)
    }

    func testASecondAddKeepsTheFirstAddTime() {
        let first = contact("aa", key: 1, at: 1_000)
        let again = contact("aa", key: 1, at: 9_000, name: "Anna B")
        let list = PendingContacts.adding(again, to: [first])
        XCTAssertEqual(list.count, 1)
        XCTAssertEqual(list[0].addedAtMs, 1_000)
        XCTAssertEqual(list[0].displayName, "Anna B")
    }

    func testTheLineChangesAfterSevenDays() {
        let c = contact("aa", key: 1, at: 0)
        XCTAssertEqual(PendingContacts.line(c, nowMs: PendingContacts.staleMs - 1), .waiting)
        XCTAssertEqual(PendingContacts.line(c, nowMs: PendingContacts.staleMs), .stale)
    }

    func testAContactWithAOneToOneIsNotListedTwice() {
        let a = contact("aa", key: 1)
        let b = contact("bb", key: 2)
        let shown = PendingContacts.shown([a, b], oneToOnePeerKeys: [Data(repeating: 1, count: 32)])
        XCTAssertEqual(shown.map(\.accountIdHex), ["bb"])
    }

    func testOnlyAOneToOneWithThePersonTakesTheirMessages() {
        let a = contact("aa", key: 1)
        let key = Data(repeating: 1, count: 32)
        XCTAssertEqual(PendingContacts.forConversation([a], oneToOne: true, personKeys: [key])?.accountIdHex, "aa")
        XCTAssertNil(PendingContacts.forConversation([a], oneToOne: false, personKeys: [key]))
        XCTAssertNil(PendingContacts.forConversation(
            [a], oneToOne: true, personKeys: [Data(repeating: 9, count: 32)]))
    }

    func testTheNameIsTheDisplayNameThenTheUsername() {
        XCTAssertEqual(PendingContacts.name(contact("aa", key: 1)), "Anna")
        XCTAssertEqual(PendingContacts.name(contact("aa", key: 1, name: " ")), "@anna")
        XCTAssertNil(PendingContacts.name(contact("aa", key: 1, name: "", username: "")))
    }

    private func record(id: UInt8, uses: UInt32 = 0, maxUses: UInt32 = 0, expiry: UInt64 = 0,
                        revoked: Bool = false, minted: UInt64 = 1, ledger: Bool = true) -> LinkRecord {
        LinkRecord(
            hlc: 0, linkId: Data(repeating: id, count: 16), linkSecret: Data(repeating: 7, count: 12),
            kind: .contact, ledgerId: ledger ? Data(repeating: id, count: 6) : Data(),
            expiryMs: expiry, maxUses: maxUses, uses: uses, revoked: revoked,
            mintedAtMs: minted, label: "", answered: [])
    }

    func testTheListShowsLiveContactLinksNewestFirst() {
        let records = [
            record(id: 1, minted: 1),
            record(id: 2, minted: 3),
            record(id: 3, revoked: true),
            record(id: 4, uses: 1),
            record(id: 5, uses: 1, maxUses: 5, minted: 2),
            record(id: 6, expiry: 10),
            record(id: 7, ledger: false),
        ]
        let listed = LinkRecords.listedContactLinks(records, nowMs: 100)
        XCTAssertEqual(listed.map { $0.linkId.first }, [2, 5, 1])
        XCTAssertNil(LinkRecords.effectiveLimit(maxUses: UInt32.max))
        XCTAssertEqual(LinkRecords.effectiveLimit(maxUses: 0), 1)
    }
}
