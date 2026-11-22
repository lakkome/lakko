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

    // 2026-11-05 — a link stored on the contact row is a hint, never the
    // answer by itself (seen on web: Message opened a group the person is in).

    func testALinkedGroupIsNotTheirChat() {
        let group = Conv(id: "g", people: [me, anna, "boris"], oneToOne: false)
        XCTAssertNil(find("g", [group]))
        let twoPersonGroup = Conv(id: "g2", people: [me, anna], oneToOne: false)
        XCTAssertNil(find("g2", [twoPersonGroup]))
    }

    func testALinkedOneToOneWithSomeoneElseIsNotTheirChat() {
        let boris = Conv(id: "gb", people: [me, "boris"], oneToOne: true)
        XCTAssertNil(find("gb", [boris]))
    }

    func testAWrongLinkGivesWayToTheirOneToOne() {
        let group = Conv(id: "g", people: [me, anna, "boris"], oneToOne: false)
        let mine = Conv(id: "g1", people: [me, anna], oneToOne: true)
        XCTAssertEqual(find("g", [group, mine]), mine)
    }

    func testAProvenRotationIsOnePerson() {
        let conv = Conv(id: "g1", people: [me, "anna-new"], oneToOne: true)
        let found = ContactConversation.find(
            contactKey: anna, linkedId: "g1", conversations: [conv], idOf: { $0.id }, ownKey: me,
            peopleOf: { $0.people }, isOneToOne: { $0.oneToOne },
            samePerson: { $0 == $1 || Set([$0, $1]) == Set([anna, "anna-new"]) })
        XCTAssertEqual(found, conv)
    }

    func testOnlyTheLinkedConversationAnswersBeforeASessionKnowsItsOwnKey() {
        let conv = Conv(id: "g1", people: [me, anna], oneToOne: true)
        XCTAssertNil(find(nil, [conv], own: nil))
        XCTAssertEqual(find("g1", [conv], own: nil), conv)
        // Even then a linked group, or a one-to-one with someone else, is not.
        XCTAssertNil(find("g1", [Conv(id: "g1", people: [me, anna], oneToOne: false)], own: nil))
        XCTAssertNil(find("g1", [Conv(id: "g1", people: [me, "boris"], oneToOne: true)], own: nil))
    }

    /// People are counted, not device leaves (ADR-0050): the readers hand the
    /// rule person keys, so a third PERSON is what makes it not a match.
    func testAThirdPersonIsNoMatch() {
        XCTAssertNil(find(nil, [Conv(id: "g", people: [me, anna, "boris"], oneToOne: true)]))
    }
}

/// One person, one one-to-one row (walk of 2026-11-07). Mirror of
/// `apps/web/src/contacts/duplicate-one-to-one.test.ts`.
final class DuplicateOneToOneTests: XCTestCase {
    private let me = "me-key"
    private let vg3 = "vg3-key"
    private let anna = "anna-key"

    private func chat(
        _ groupHex: String, _ founder: String?, peer: String? = nil, conversation: String? = nil
    ) -> DuplicateOneToOne.Held {
        DuplicateOneToOne.Held(
            groupHex: groupHex, conversationHex: conversation ?? groupHex, isOneToOne: true,
            people: [me, peer ?? vg3], founderKeyB64: founder)
    }

    private func fold(_ successor: String, _ root: String) -> DuplicateOneToOne.Fold {
        DuplicateOneToOne.Fold(successorHex: successor, rootHex: root)
    }

    /// The desktop founded aa (VG3 never opened it); VG3 founded bb in answer
    /// to the desktop's link answer (seen live 2026-11-07).
    func testContinuesTheOwnFoundedOneToOneWithThePeerFoundedOne() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [chat("aa", me), chat("bb", vg3)], ownKeyB64: me),
            [fold("bb", "aa")])
    }

    func testIsOrderIndependentAndWritesNothingOnceFolded() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [chat("bb", vg3), chat("aa", me)], ownKeyB64: me),
            [fold("bb", "aa")])
        XCTAssertEqual(
            DuplicateOneToOne.folds(
                held: [chat("aa", me), chat("bb", vg3, conversation: "aa")], ownKeyB64: me),
            [])
    }

    // Base scenario 9 (2026-11-22) — ordered by when this device first held them.

    private func at(_ held: DuplicateOneToOne.Held, _ ms: Int64) -> DuplicateOneToOne.Held {
        var out = held
        out.heldSinceMs = ms
        return out
    }

    func testTheNewerOneToOneTheSamePersonFoundedContinuesTheOlderOne() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [at(chat("aa", vg3), 1), at(chat("bb", vg3), 2)], ownKeyB64: me),
            [fold("bb", "aa")])
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [at(chat("bb", vg3), 2), at(chat("aa", vg3), 1)], ownKeyB64: me),
            [fold("bb", "aa")])
    }

    func testAOneToOneASiblingFoundedContinuesAnOlderOneThePersonFounded() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [at(chat("aa", vg3), 1), at(chat("bb", me), 2)], ownKeyB64: me),
            [fold("bb", "aa")])
    }

    func testEqualOrUnknownTimesKeepTheRuleAsItWas() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [at(chat("aa", vg3), 1), at(chat("bb", vg3), 1)], ownKeyB64: me), [])
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [chat("aa", vg3), at(chat("bb", vg3), 2)], ownKeyB64: me), [])
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [at(chat("aa", me), 1), at(chat("bb", vg3), 1)], ownKeyB64: me),
            [fold("bb", "aa")])
    }

    func testTheHeldSinceRecordKeepsFirstTimesAndDropsGroupsNoLongerHeld() {
        let first = ConversationHeldSince.next(stored: [:], groupHexes: ["aa"], nowMs: 100)
        XCTAssertEqual(first, ["aa": 100])
        let second = ConversationHeldSince.next(stored: first, groupHexes: ["aa", "bb"], nowMs: 200)
        XCTAssertEqual(second, ["aa": 100, "bb": 200])
        XCTAssertEqual(ConversationHeldSince.next(stored: second, groupHexes: ["bb"], nowMs: 300), ["bb": 200])
    }

    func testLeavesOneToOnesWithDifferentPeopleApart() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [chat("aa", me, peer: anna), chat("bb", vg3)], ownKeyB64: me),
            [])
    }

    func testDoesNothingForOneSideOrAnUnknownFounder() {
        XCTAssertEqual(DuplicateOneToOne.folds(held: [chat("aa", me), chat("bb", me)], ownKeyB64: me), [])
        XCTAssertEqual(DuplicateOneToOne.folds(held: [chat("aa", vg3), chat("bb", vg3)], ownKeyB64: me), [])
        XCTAssertEqual(DuplicateOneToOne.folds(held: [chat("aa", nil), chat("bb", vg3)], ownKeyB64: me), [])
        XCTAssertEqual(DuplicateOneToOne.folds(held: [chat("aa", me), chat("bb", vg3)], ownKeyB64: nil), [])
    }

    func testNeverFoldsAGroupNorIntoOne() {
        let group = DuplicateOneToOne.Held(
            groupHex: "aa", conversationHex: "aa", isOneToOne: false, people: [me, vg3], founderKeyB64: me)
        XCTAssertEqual(DuplicateOneToOne.folds(held: [group, chat("bb", vg3)], ownKeyB64: me), [])
    }

    /// bb is already a successor of cc.
    func testDoesNotFoldAGroupThatAlreadyContinuesOrIsContinued() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(
                held: [chat("aa", me), chat("bb", vg3, conversation: "cc"), chat("cc", vg3)], ownKeyB64: me),
            [])
    }

    func testPicksTheSameRootOnEveryDeviceWhenSeveralOwnOneToOnesExist() {
        XCTAssertEqual(
            DuplicateOneToOne.folds(held: [chat("cc", me), chat("aa", me), chat("bb", vg3)], ownKeyB64: me),
            [fold("bb", "aa")])
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
        var route: ContactRequestRoute = .inbox
        var waiting = false

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
                },
                route: { [self] _ in route },
                hasWaitingRequest: { [self] _ in waiting },
                requestInGroup: { [self] _, groupIdHex in
                    log.append("request in \(groupIdHex)")
                    waiting = true
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
                return try await InviteContactSend.run(contactKey: key, text: text, ports: world.ports)
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

    // MARK: The order Message follows (`docs/ux/who-can-find-me.md` 6.4)

    func testWithAGrantTheChatIsFoundedAndTheLinkSent() async throws {
        let world = World()
        world.contacts = [contact()]
        world.route = .inbox
        let outcome = try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(outcome, .sent)
        XCTAssertEqual(world.log, ["found", "send new-1x1 link"])
    }

    func testWithoutAGrantARequestGoesInsideTheSharedGroupAndNoLink() async throws {
        let world = World()
        world.contacts = [contact()]
        world.route = .group(groupIdHex: "shared")
        let outcome = try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(outcome, .requestSent)
        XCTAssertEqual(world.log, ["request in shared"], "nothing founded, no call link sent")
    }

    func testAWaitingRequestSendsNothingNew() async throws {
        let world = World()
        world.contacts = [contact()]
        world.route = .group(groupIdHex: "shared")
        world.waiting = true
        let outcome = try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(outcome, .requestSent)
        XCTAssertEqual(world.log, [])
    }

    func testWithNeitherNothingIsSent() async throws {
        let world = World()
        world.contacts = [contact()]
        world.route = .none
        let outcome = try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(outcome, .cannotMessage)
        XCTAssertEqual(world.log, [])
    }

    func testAHeldChatIsUsedWhateverTheRoute() async throws {
        let world = World()
        world.contacts = [contact(linked: "aa11")]
        world.existing = "aa11"
        world.route = .none
        let outcome = try await InviteContactSend.run(contactKey: annaKey, text: "link", ports: world.ports)
        XCTAssertEqual(outcome, .sent)
        XCTAssertEqual(world.log, ["send aa11 link"])
    }

    func testThroughTheOfferARequestMarksTheRowRequestSentAndRefusesASecondPick() async {
        let world = World()
        world.contacts = [contact()]
        world.route = .group(groupIdHex: "shared")
        var mints = 0
        let offer = offer(world) { mints += 1 }
        offer.openPicker()

        let sent = await offer.sendInvite(to: target)

        XCTAssertFalse(sent, "no call invitation went out")
        XCTAssertEqual(offer.requestSent, [target.id])
        XCTAssertTrue(offer.invited.isEmpty)
        XCTAssertNil(offer.pickerError)
        XCTAssertNil(offer.statusLine, "no call-invite line")
        XCTAssertEqual(offer.state, .ready(url: "https://lakko.me/c/one#secret", delivery: .kept))
        XCTAssertTrue(offer.pickerOpen)

        let again = await offer.sendInvite(to: target)
        XCTAssertFalse(again)
        XCTAssertEqual(world.log, ["request in shared"], "one request per person")
        XCTAssertEqual(mints, 1)
    }

    func testThroughTheOfferNothingToSendSaysTheyCannotBeMessagedYet() async {
        let world = World()
        world.contacts = [contact()]
        world.route = .none
        let offer = offer(world) {}
        offer.openPicker()

        let sent = await offer.sendInvite(to: target)

        XCTAssertFalse(sent)
        XCTAssertEqual(
            offer.pickerError,
            Localization.text("contacts.detail.cannot_message", texts: ["name": "Anna"]))
        XCTAssertTrue(offer.requestSent.isEmpty)
        XCTAssertTrue(offer.invited.isEmpty, "the row stays pressable")
        XCTAssertEqual(world.log, [])
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

    func testLinkingKeepsTheContactsEarlierKeys() {
        var held = contact(linked: "group")
        held.previousKeysB64 = ["old"]
        let linked = ContactsRepository.linking([held], signatureKey: annaKey, groupIdHex: "new-1x1")
        XCTAssertEqual(linked?.first?.previousKeysB64, ["old"])
        XCTAssertEqual(linked?.first?.groupIdHex, "new-1x1")
    }
}
