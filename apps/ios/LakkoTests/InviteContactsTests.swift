// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

// `docs/ux/call-links.md` section 3 - `Invite contacts` lists the person's
// CONTACTS, not their conversations, and sends the invitation into the
// one-to-one with the chosen contact, founding it first when this device
// holds none (2026-10-02). The web client is the reference
// (`contacts/contact-conversation.ts`, `calls/invite-contacts.ts` and their
// tests, ported below, and `sendLinkCallInvite`).

// MARK: - Which conversation is the one-to-one with a contact

final class ContactConversationTests: XCTestCase {

    private struct Conv: Equatable {
        let id: String
        let people: [String]
        let oneToOne: Bool
    }

    private let me = "me"
    private let anna = "anna"

    private func find(_ linked: String?, _ conversations: [Conv], own: String? = "me") -> Conv? {
        ContactConversation.find(
            contactKey: anna,
            linkedId: linked,
            conversations: conversations,
            idOf: { $0.id },
            ownKey: own,
            peopleOf: { $0.people },
            isOneToOne: { $0.oneToOne })
    }

    func testAnswersTheConversationLinkedOnTheContactWhileTheRegistryHoldsIt() {
        let linked = Conv(id: "g1", people: [me, anna], oneToOne: true)
        XCTAssertEqual(find("g1", [linked]), linked)
    }

    func testALinkedIdWhoseChatIsGoneIsNotAnAddress() {
        XCTAssertNil(find("gone", []))
    }

    func testFallsBackToTheOneToOneWhoseTwoPeopleAreThisAccountAndTheContact() {
        let other = Conv(id: "g0", people: [me, "boris"], oneToOne: true)
        let mine = Conv(id: "g2", people: [anna, me], oneToOne: true)
        XCTAssertEqual(find("gone", [other, mine]), mine)
        XCTAssertEqual(find(nil, [other, mine]), mine)
    }

    func testNeverAnswersAGroupThatHappensToHoldTheSameTwoPeople() {
        // Writing there is not writing to the contact.
        XCTAssertNil(find(nil, [Conv(id: "g3", people: [me, anna], oneToOne: false)]))
    }

    func testNeverAnswersAOneToOneWithSomeoneElseOrOneThisAccountIsNotIn() {
        XCTAssertNil(find(nil, [Conv(id: "g", people: [me, "boris"], oneToOne: true)]))
        XCTAssertNil(find(nil, [Conv(id: "g", people: [anna, "boris"], oneToOne: true)]))
    }

    func testATornConversationIsNoMatch() {
        XCTAssertNil(find(nil, [Conv(id: "g", people: [], oneToOne: true)]))
    }

    func testOnlyTheLinkedConversationAnswersBeforeASessionKnowsItsOwnKey() {
        let conv = Conv(id: "g1", people: [me, anna], oneToOne: true)
        XCTAssertNil(find(nil, [conv], own: nil))
        XCTAssertEqual(find("g1", [conv], own: nil), conv)
    }

    /// People are counted, not device leaves (ADR-0050): the readers hand the
    /// rule person keys, so a third PERSON is what makes it not a match.
    func testAThirdPersonIsNoMatch() {
        XCTAssertNil(find(nil, [Conv(id: "g", people: [me, anna, "boris"], oneToOne: true)]))
    }
}

// MARK: - The rows and the filter

final class InviteContactRowsTests: XCTestCase {

    private struct Contact {
        let key: String
        let name: String
        let username: String?
    }

    private let contacts = [
        Contact(key: "k-zoe", name: "Zoe", username: "zoe_k"),
        Contact(key: "k-anna", name: "Anna", username: nil),
        Contact(key: "k-boris", name: "boris", username: "b0ris"),
    ]

    private func rows(_ list: [Contact]? = nil) -> [InviteContactRow] {
        InviteContacts.rows(list ?? contacts) {
            InviteContactRow(key: Data($0.key.utf8), name: $0.name, username: $0.username)
        }
    }

    func testListsEveryContactByName() {
        XCTAssertEqual(rows().map(\.name), ["Anna", "boris", "Zoe"])
    }

    func testCarriesTheKeyTheInvitationIsAddressedToAndTheUsernameWhenKnown() {
        XCTAssertEqual(rows()[0], InviteContactRow(key: Data("k-anna".utf8), name: "Anna", username: nil))
        XCTAssertEqual(rows()[2], InviteContactRow(key: Data("k-zoe".utf8), name: "Zoe", username: "zoe_k"))
        XCTAssertNil(rows()[0].usernameLine)
        XCTAssertEqual(rows()[2].usernameLine, "@zoe_k")
        XCTAssertEqual(rows()[2].id, Data("k-zoe".utf8).base64EncodedString())
    }

    func testAContactWhoseRecordIsDoubledIsListedOnce() {
        let anna = Contact(key: "k-anna", name: "Anna", username: nil)
        XCTAssertEqual(rows([anna, anna]).count, 1)
    }

    func testIsEmptyForAPersonWithNoContacts() {
        XCTAssertEqual(rows([]), [])
    }

    /// The Contacts list's own order: a person nothing is known about sorts
    /// after every named one, and a name that is a username shows no second
    /// `@username` line.
    func testFollowsTheContactsListForPlaceholdersAndUsernameOnlyNames() {
        let listed = InviteContacts.rows(["m", "a", "u"]) { key -> InviteContactRow in
            switch key {
            case "m":
                return InviteContactRow(key: Data("m".utf8), name: "Member", username: nil, isPlaceholder: true)
            case "u":
                return InviteContactRow(
                    key: Data("u".utf8), name: "@mira", username: "mira", nameIsUsername: true)
            default:
                return InviteContactRow(key: Data("a".utf8), name: "Zed", username: nil)
            }
        }
        XCTAssertEqual(listed.map(\.name), ["@mira", "Zed", "Member"])
        XCTAssertNil(listed[0].usernameLine)
    }

    func testAnEmptyQueryKeepsEverything() {
        XCTAssertEqual(InviteContacts.filter(rows(), query: "  ").count, 3)
    }

    func testMatchesByNameOrByUsernameIgnoringCaseAndATypedAt() {
        func keys(_ query: String) -> [String] {
            InviteContacts.filter(rows(), query: query).map { String(decoding: $0.key, as: UTF8.self) }
        }
        XCTAssertEqual(keys("ann"), ["k-anna"])
        XCTAssertEqual(keys("@ZOE_"), ["k-zoe"])
        XCTAssertEqual(keys("b0"), ["k-boris"])
    }

    func testKeepsNothingWhenNobodyMatches() {
        XCTAssertEqual(InviteContacts.filter(rows(), query: "nobody"), [])
    }
}

// MARK: - Sending the invitation

@MainActor
final class InviteContactSendTests: XCTestCase {

    private struct Boom: Error {}

    private let annaKey = Data(repeating: 0xA1, count: 32)

    private func contact(accountId: Data? = Data(repeating: 0x0A, count: 16), linked: String? = nil)
        -> AcceptedContact
    {
        AcceptedContact(
            request: IncomingContactRequest(
                messageId: 7,
                fromPubkey: annaKey,
                fromAccountId: accountId,
                fromPseudonym: Data(repeating: 0, count: 32),
                validAfterMs: 1,
                intro: nil,
                createdAt: "2026-10-02T00:00:00Z"),
            groupIdHex: linked)
    }

    /// The ports over one ordered log.
    private final class World {
        var log: [String] = []
        var contacts: [AcceptedContact] = []
        var existing: String?
        var foundError: Error?
        var sendError: Error?

        var ports: InviteContactSend.Ports {
            InviteContactSend.Ports(
                contact: { [self] key in contacts.first { $0.request.fromPubkey == key } },
                conversationWith: { [self] _ in existing },
                found: { [self] _ in
                    log.append("found")
                    if let foundError { throw foundError }
                    existing = "new-1x1"
                    return "new-1x1"
                },
                send: { [self] groupIdHex, text in
                    log.append("send \(groupIdHex) \(text)")
                    if let sendError { throw sendError }
                })
        }
    }

    func testAnExistingConversationIsUsed() async throws {
        let world = World()
        world.contacts = [contact(linked: "aa11")]
        world.existing = "aa11"
        try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(world.log, ["send aa11 link"], "nothing is founded")
    }

    func testWithNoConversationOneIsFoundedOnceThenTheInvitationIsSent() async throws {
        let world = World()
        world.contacts = [contact()]
        try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(world.log, ["found", "send new-1x1 link"])

        // The next invitation to the same contact finds the conversation.
        try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(world.log, ["found", "send new-1x1 link", "send new-1x1 link"])
    }

    func testAContactRecordWithoutAnAccountIdFailsAndFoundsNothing() async {
        let world = World()
        world.contacts = [contact(accountId: nil)]
        do {
            try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
            XCTFail("there is nothing to found a conversation with")
        } catch {
            XCTAssertEqual(error as? InviteContactSendError, .contactHasNoAccountId)
        }
        XCTAssertEqual(world.log, [])
    }

    func testAContactWithoutAnAccountIdStillGetsTheInvitationInAnExistingConversation() async throws {
        let world = World()
        world.contacts = [contact(accountId: nil, linked: "aa11")]
        world.existing = "aa11"
        try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(world.log, ["send aa11 link"])
    }

    func testSomeoneWhoIsNoLongerAContactIsNotWrittenTo() async {
        let world = World()
        do {
            try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
            XCTFail("not a contact")
        } catch {
            XCTAssertEqual(error as? InviteContactSendError, .unknownContact)
        }
        XCTAssertEqual(world.log, [])
    }

    func testAFailedFoundingSendsNothing() async {
        let world = World()
        world.contacts = [contact()]
        world.foundError = Boom()
        do {
            try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
            XCTFail("the conversation was not founded")
        } catch {
            XCTAssertTrue(error is Boom)
        }
        XCTAssertEqual(world.log, ["found"])
    }

    // MARK: Through the offer: the outcome the call screen shows

    private func offer(_ world: World, mints: @escaping () -> Void) -> CallInviteOffer {
        CallInviteOffer(
            mint: {
                mints()
                return "https://lakko.me/c/one#secret"
            },
            send: { contactId, text in
                guard let key = Data(base64Encoded: contactId) else { throw Boom() }
                try await InviteContactSend.run(contactKey: key, text: text, ports: world.ports)
            },
            hold: { _ in })
    }

    private var target: CallInviteTarget {
        CallInviteTarget(id: annaKey.base64EncodedString(), title: "Anna")
    }

    func testTheInvitationIsSentIntoTheFoundedConversationAndTheRowIsMarked() async {
        let world = World()
        world.contacts = [contact()]
        var mints = 0
        let offer = offer(world) { mints += 1 }
        offer.openPicker()

        let sent = await offer.sendInvite(to: target)

        XCTAssertTrue(sent)
        XCTAssertEqual(world.log, ["found", "send new-1x1 https://lakko.me/c/one#secret"])
        XCTAssertEqual(offer.invited, [target.id])
        XCTAssertTrue(offer.pickerOpen, "the list stays open for the next person")
        XCTAssertEqual(
            offer.state, .ready(url: "https://lakko.me/c/one#secret", delivery: .sent(name: "Anna")))
        XCTAssertEqual(mints, 1)
    }

    func testAContactWithoutAnAccountIdIsTheSendFailedOutcomeAndNothingIsFounded() async {
        let world = World()
        world.contacts = [contact(accountId: nil)]
        var mints = 0
        let offer = offer(world) { mints += 1 }
        offer.openPicker()

        let sent = await offer.sendInvite(to: target)

        XCTAssertFalse(sent)
        XCTAssertEqual(world.log, [], "nothing founded, nothing sent")
        XCTAssertEqual(offer.pickerError, Localization.text("call.invite.send_failed"))
        XCTAssertEqual(offer.state, .failed(failure: .notSent, url: "https://lakko.me/c/one#secret"))
        XCTAssertTrue(offer.invited.isEmpty, "the row stays pressable")
        XCTAssertTrue(offer.pickerOpen)
    }

    func testAFailedSendKeepsTheMintedLink() async {
        let world = World()
        world.contacts = [contact()]
        world.sendError = Boom()
        var mints = 0
        let offer = offer(world) { mints += 1 }
        offer.openPicker()

        let first = await offer.sendInvite(to: target)
        XCTAssertFalse(first)
        XCTAssertEqual(offer.state, .failed(failure: .notSent, url: "https://lakko.me/c/one#secret"))
        XCTAssertEqual(offer.pickerError, Localization.text("call.invite.send_failed"))
        XCTAssertEqual(mints, 1)

        // The retry reuses the link and the conversation the first attempt
        // founded.
        world.sendError = nil
        let second = await offer.sendInvite(to: target)
        XCTAssertTrue(second)
        XCTAssertEqual(mints, 1, "a failed send does not mint a second link")
        XCTAssertEqual(world.log.filter { $0 == "found" }.count, 1, "and does not found a second conversation")
        XCTAssertEqual(offer.invited, [target.id])
    }

    // MARK: Linking the founded conversation on the contact record

    func testTheFoundedConversationIsLinkedOnTheContactAndReplacesAStaleLink() {
        let stale = contact(linked: "gone")
        let linked = ContactsRepository.linking([stale], signatureKey: annaKey, groupIdHex: "new-1x1")
        XCTAssertEqual(linked?.first?.groupIdHex, "new-1x1")
        XCTAssertEqual(linked?.first?.request, stale.request)

        XCTAssertNil(
            ContactsRepository.linking(linked ?? [], signatureKey: annaKey, groupIdHex: "new-1x1"),
            "already linked: nothing changes")
        XCTAssertNil(
            ContactsRepository.linking([stale], signatureKey: Data(repeating: 9, count: 32), groupIdHex: "x"),
            "not a contact: nothing changes")
    }

    func testLinkingKeepsTheContactsOwnName() {
        var named = contact()
        named.displayName = "Anya"
        let linked = ContactsRepository.linking([named], signatureKey: annaKey, groupIdHex: "new-1x1")
        XCTAssertEqual(linked?.first?.displayName, "Anya")
    }
}
