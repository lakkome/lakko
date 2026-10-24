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
            username: username, addedAtMs: at, linkId: Data(repeating: 7, count: 16))
    }

    /// Add racing a link that went dead: the consume answers dead and nothing
    /// is kept, no pending contact and no request.
    @MainActor
    func testAddOnALinkTheConsumeFindsDeadKeepsNothing() async throws {
        var log: [String] = []
        let outcome = try await InviteAddOrder.run(
            consumeLive: { log.append("consume"); return false },
            recordPending: { log.append("record") },
            sendRequest: { log.append("send") },
            forgetPending: { log.append("forget") })
        XCTAssertEqual(outcome, .dead)
        XCTAssertEqual(log, ["consume"])
    }

    @MainActor
    func testAddOnALiveLinkRecordsBeforeItSendsAndForgetsWhenTheSendFails() async throws {
        struct Offline: Error {}
        var log: [String] = []
        let added = try await InviteAddOrder.run(
            consumeLive: { log.append("consume"); return true },
            recordPending: { log.append("record") },
            sendRequest: { log.append("send") },
            forgetPending: { log.append("forget") })
        XCTAssertEqual(added, .added)
        XCTAssertEqual(log, ["consume", "record", "send"])
        log = []
        do {
            _ = try await InviteAddOrder.run(
                consumeLive: { log.append("consume"); return true },
                recordPending: { log.append("record") },
                sendRequest: { log.append("send"); throw Offline() },
                forgetPending: { log.append("forget") })
            XCTFail("the send failure propagates")
        } catch is Offline {}
        XCTAssertEqual(log, ["consume", "record", "send", "forget"])
    }

    /// Open chat on the page of a person already in the contacts (amended
    /// 2026-10-24): the consume, then the answer, and no pending contact.
    @MainActor
    func testOpenChatOnAKnownContactConsumesThenSends() async throws {
        var log: [String] = []
        let outcome = try await KnownContactAnswerOrder.run(
            consumeLive: { log.append("consume"); return true },
            sendRequest: { log.append("send") })
        XCTAssertEqual(outcome, .sent)
        XCTAssertEqual(log, ["consume", "send"])
    }

    @MainActor
    func testOpenChatOnADeadLinkSendsNothing() async throws {
        var log: [String] = []
        let outcome = try await KnownContactAnswerOrder.run(
            consumeLive: { log.append("consume"); return false },
            sendRequest: { log.append("send") })
        XCTAssertEqual(outcome, .dead)
        XCTAssertEqual(log, ["consume"])
    }

    @MainActor
    func testOpenChatLetsAFailedSendReachThePage() async {
        struct Offline: Error {}
        do {
            _ = try await KnownContactAnswerOrder.run(
                consumeLive: { true },
                sendRequest: { throw Offline() })
            XCTFail("the send failure propagates")
        } catch {
            XCTAssertTrue(error is Offline)
        }
    }

    /// The page's state carries the opened link, so Open chat can answer it;
    /// a page with no link opens the chat as before.
    @MainActor
    func testTheAlreadyContactsStateCarriesTheLink() {
        let state = InviteLandingViewModel.state(for: .knownContact(
            contactMessageId: 41, groupIdHex: "ab", statedName: "Anna", invite: nil))
        XCTAssertEqual(state, .alreadyContacts(.conversation(groupIdHex: "ab"), statedName: "Anna"))
    }

    /// ADR-0079 amendment of 2026-10-19 — only a refusal signed by the key
    /// the link named, over that link and this account, cancels a pending
    /// contact (through the core).
    func testOnlyTheInvitersRefusalCancelsThePendingContact() throws {
        let anna = try Identity.fromSeed(seed: Data(repeating: 1, count: 32))
        let mallory = try Identity.fromSeed(seed: Data(repeating: 2, count: 32))
        let bob = Data(repeating: 0xB0, count: 32)
        let linkId = Data(repeating: 7, count: 16)
        let pending = PendingContact(
            accountIdHex: "aa", signaturePubkey: anna.signaturePublicKey(), x25519Pubkey: anna.x25519PublicKey(),
            displayName: "Anna", username: "anna", addedAtMs: 1, linkId: linkId)
        let verify: (Data, Data, Data, Data) -> Bool = {
            verifyLinkRefusal(wire: $0, inviterKey: $1, expectedLinkId: $2, ownAccountKey: $3)
        }
        let fromAnna = try composeLinkRefusal(inviterSigningSeed: anna.ed25519Seed(), linkId: linkId, requesterAccountKey: bob)
        XCTAssertEqual(
            PendingContacts.refusedBy([pending], refusal: fromAnna, ownAccountKey: bob, verify: verify)?.accountIdHex, "aa")
        let fromMallory = try composeLinkRefusal(
            inviterSigningSeed: mallory.ed25519Seed(), linkId: linkId, requesterAccountKey: bob)
        XCTAssertNil(PendingContacts.refusedBy([pending], refusal: fromMallory, ownAccountKey: bob, verify: verify))
        let otherLink = try composeLinkRefusal(
            inviterSigningSeed: anna.ed25519Seed(), linkId: Data(repeating: 8, count: 16), requesterAccountKey: bob)
        XCTAssertNil(PendingContacts.refusedBy([pending], refusal: otherLink, ownAccountKey: bob, verify: verify))
        XCTAssertNil(PendingContacts.refusedBy(
            [pending], refusal: fromAnna, ownAccountKey: Data(repeating: 0xB1, count: 32), verify: verify))
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
            revocationSeed: ledger ? Data(repeating: 9, count: 32) : Data(),
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
