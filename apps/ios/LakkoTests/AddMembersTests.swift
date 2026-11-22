// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/conversation-info.md` sections 2.4, 3 and 3.1 — who the Add
/// members screen lists and lets be picked, how an Add runs, and where each
/// back goes once the group's info is a pushed screen.
@MainActor
final class AddMembersTests: XCTestCase {

    private func key(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }
    private func id(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    private func contact(_ byte: UInt8, name: String, username: String? = nil, accountId: Bool = true)
        -> AddMembersRules.Contact
    {
        AddMembersRules.Contact(
            personKey: key(byte), accountId: accountId ? id(byte) : nil, name: name, username: username)
    }

    private func resolved(_ byte: UInt8, name: String?, username: String?, verified: Bool = true)
        -> ResolvedDisplayName
    {
        resolveDisplayName(
            signatureKey: key(byte),
            localRename: nil,
            profile: ProfileClaim(displayName: name, username: username, updatedAtMs: 1),
            verification: verified ? .verified : nil)
    }

    // MARK: - Row text (seen live 2026-11-22)

    /// The picker row read "G53Person · @w6g53x2201" with "@w6g53x2201" again
    /// under it, and its avatar "GW". The row is the name, the username once
    /// under it, initials from the name.
    func testAPickerRowIsTheNameWithTheUsernameOnceUnderIt() {
        let row = AddMembersRules.contact(
            personKey: key(1), accountId: id(1),
            resolved: resolved(1, name: "G53Person", username: "w6g53x2201"))
        XCTAssertEqual(row.name, "G53Person")
        XCTAssertEqual(row.username, "w6g53x2201")
        XCTAssertEqual(avatarInitials(name: row.name), "G5")
        XCTAssertEqual(AddMembersRules.pick(AddMembersRules.ContactRow(contact: row))?.name, "G53Person",
                       "the chip and a failure name the person, not the label")
        // Still found by the username.
        XCTAssertTrue(AddMembersRules.matches(query: "w6g", name: row.name, username: row.username))
    }

    func testAPickerRowWithoutANameIsTheUsernameOnce() {
        let row = AddMembersRules.contact(
            personKey: key(2), accountId: id(2), resolved: resolved(2, name: nil, username: "ann"))
        XCTAssertEqual(row.name, "@ann")
        XCTAssertNil(row.username, "not repeated as the second line")
    }

    func testAnUnverifiedUsernameIsNotShownOnAPickerRow() {
        let row = AddMembersRules.contact(
            personKey: key(3), accountId: id(3),
            resolved: resolved(3, name: "Bob", username: "bob", verified: false))
        XCTAssertEqual(row.name, "Bob")
        XCTAssertNil(row.username)
    }

    /// The member row read "G53Person · @w6g53x2201", wrapped to two lines.
    func testAMemberRowIsTheNameWithTheUsernameUnderIt() {
        let named = PersonRowText(resolved(4, name: "G53Person", username: "w6g53x2201"))
        XCTAssertEqual(named, PersonRowText(title: "G53Person", subtitle: "@w6g53x2201"))
        XCTAssertEqual(
            PersonRowText(resolved(5, name: nil, username: "ann")),
            PersonRowText(title: "@ann", subtitle: nil))
        XCTAssertEqual(
            PersonRowText(resolved(6, name: "Bob", username: "bob", verified: false)),
            PersonRowText(title: "Bob", subtitle: nil))
        XCTAssertEqual(PersonRowText.own("You"), PersonRowText(title: "You", subtitle: nil))
    }

    // MARK: - Contacts

    func testMembersAndPendingAddsAreNotListed() {
        let rows = AddMembersRules.contactRows(
            contacts: [contact(1, name: "Ann"), contact(2, name: "Bob"), contact(3, name: "Cat")],
            memberKeys: [key(1)], pendingKeys: [key(2)], query: "")
        XCTAssertEqual(rows.map(\.contact.name), ["Cat"])
    }

    func testAProvenPredecessorKeyNamesTheSameMember() {
        // The contact row still holds the old key; the roster holds the new.
        let rows = AddMembersRules.contactRows(
            contacts: [contact(1, name: "Ann")],
            memberKeys: [key(9)], pendingKeys: [], query: "",
            current: { $0 == self.key(1) ? self.key(9) : $0 })
        XCTAssertTrue(rows.isEmpty)
    }

    func testAContactWithoutAnAccountIdIsListedButNotPickable() {
        let rows = AddMembersRules.contactRows(
            contacts: [contact(1, name: "Ann", accountId: false), contact(2, name: "Bob")],
            memberKeys: [], pendingKeys: [], query: "")
        XCTAssertEqual(rows.map(\.pickable), [false, true])
        XCTAssertNil(AddMembersRules.pick(rows[0]))
        XCTAssertEqual(AddMembersRules.pick(rows[1]),
                       AddMembersPick(personKey: key(2), accountId: id(2), name: "Bob"))
    }

    func testTypingFiltersLooselyFromTheFirstLetter() {
        let all = [contact(1, name: "Zoë Adams", username: "zoe"), contact(2, name: "Bob", username: "robert")]
        func names(_ q: String) -> [String] {
            AddMembersRules.contactRows(contacts: all, memberKeys: [], pendingKeys: [], query: q)
                .map(\.contact.name)
        }
        XCTAssertEqual(names("z"), ["Zoë Adams"])
        XCTAssertEqual(names("ZOE"), ["Zoë Adams"], "case and accents are ignored")
        XCTAssertEqual(names("adam"), ["Zoë Adams"], "anywhere in the name")
        XCTAssertEqual(names("@rob"), ["Bob"], "the username, a leading @ ignored")
        XCTAssertEqual(names("  "), ["Zoë Adams", "Bob"])
        XCTAssertEqual(names("xyz"), [])
    }

    // MARK: - People found by the network

    private func found(_ byte: UInt8, accountId: Bool = true, reach: PersonReach = .open) -> FoundPerson {
        FoundPerson(
            accountKey: key(byte), accountId: accountId ? id(byte) : nil,
            displayName: "P\(byte)", username: nil, writeSeed: nil, reach: reach)
    }

    func testPeopleSkipMembersContactsPendingAndThisAccount() {
        let rows = AddMembersRules.networkRows(
            found: [found(1), found(2), found(3), found(4), found(5), found(6, reach: .contact), found(7), found(7)],
            contactKeys: [key(2)], memberKeys: [key(1)], pendingKeys: [key(3)],
            ownAccountKey: key(4))
        XCTAssertEqual(rows.map(\.accountKey), [key(5), key(7)])
    }

    func testAResultWithoutAnAccountIdIsShownButNotPickable() {
        XCTAssertFalse(AddMembersRules.isPickable(found(1, accountId: false)))
        XCTAssertNil(AddMembersRules.pick(found(1, accountId: false), fallbackName: "Someone"))
        XCTAssertFalse(AddMembersRules.isPickable(found(2, reach: .none)), "shown by name only")
        XCTAssertTrue(AddMembersRules.isPickable(found(3, reach: .open)))
        XCTAssertEqual(AddMembersRules.pick(found(3), fallbackName: "Someone")?.accountId, id(3))
    }

    // MARK: - Picks and the button

    func testTheButtonCountsFromTwo() {
        XCTAssertEqual(AddMembersRules.submitLabel(count: 0).key, "chat.members.add.submit")
        XCTAssertEqual(AddMembersRules.submitLabel(count: 1).key, "chat.members.add.submit")
        XCTAssertNil(AddMembersRules.submitLabel(count: 1).count)
        XCTAssertEqual(AddMembersRules.submitLabel(count: 3).key, "chat.members.add.submit_count")
        XCTAssertEqual(AddMembersRules.submitLabel(count: 3).count, 3)
    }

    func testTogglingKeepsPickOrder() {
        let a = AddMembersPick(personKey: key(1), accountId: id(1), name: "A")
        let b = AddMembersPick(personKey: key(2), accountId: id(2), name: "B")
        var picks = AddMembersRules.toggling(a, in: [])
        picks = AddMembersRules.toggling(b, in: picks)
        XCTAssertEqual(picks, [a, b])
        XCTAssertEqual(AddMembersRules.toggling(a, in: picks), [b])
    }

    func testPendingAddsAreTheOwnIntentsStillPending() {
        let leafA = key(10).base64EncodedString()
        let leafB = key(11).base64EncodedString()
        let intents = [
            MembershipIntent(request: .add(personKeyB64: key(1).base64EncodedString(),
                                           accountIdB64: id(1).base64EncodedString(),
                                           leafKeysB64: [leafA]), epoch: 3),
            // Landed already: its leaf is no longer pending.
            MembershipIntent(request: .add(personKeyB64: key(2).base64EncodedString(),
                                           accountIdB64: id(2).base64EncodedString(),
                                           leafKeysB64: [key(12).base64EncodedString()]), epoch: 3),
        ]
        let pending = AddMembersRules.pendingAddPersonKeys(
            pendingAddLeafKeysB64: [leafA, leafB], intents: intents)
        XCTAssertTrue(pending.contains(key(1)))
        XCTAssertFalse(pending.contains(key(2)))
        XCTAssertTrue(pending.contains(key(11)), "a pending add's own leaf key")
    }

    // MARK: - The Add run

    func testEveryPickIsAddedInOrderAndFailuresDoNotStopTheOthers() async {
        let picks = (1...3).map { AddMembersPick(personKey: key($0), accountId: id($0), name: "P\($0)") }
        var calls: [String] = []
        struct Refused: Error {}
        let failed = await AddMembersRun.run(
            picks: picks,
            add: { pick in
                calls.append("add \(pick.name)")
                if pick.name == "P2" { throw Refused() }
            },
            sweep: { calls.append("sweep") })
        XCTAssertEqual(calls, ["add P1", "add P2", "add P3", "sweep"], "one after another, then one sweep")
        XCTAssertEqual(failed.map(\.name), ["P2"])
    }

    func testNothingLandedMeansNoSweep() async {
        let picks = [AddMembersPick(personKey: key(1), accountId: id(1), name: "P1")]
        var swept = false
        struct Refused: Error {}
        let failed = await AddMembersRun.run(
            picks: picks, add: { _ in throw Refused() }, sweep: { swept = true })
        XCTAssertEqual(failed, picks)
        XCTAssertFalse(swept)
    }

    // MARK: - Back destinations (sections 3 and 3.1)

    func testTheInfoIsAPushOverItsConversation() {
        let path: [TabRoute] = [.chat(groupIdHex: "g")]
        let opened = GroupInfoNavigation.openInfo(path, groupIdHex: "g")
        XCTAssertEqual(opened, [.chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g")])
        XCTAssertEqual(GroupInfoNavigation.openInfo(opened, groupIdHex: "g"), opened, "not pushed twice")
        // The back is the route below: the conversation, which stays.
        XCTAssertEqual(opened.dropLast().last, .chat(groupIdHex: "g"))
    }

    func testAddReturnsToTheInfoAtOnce() {
        let path: [TabRoute] = [.chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g")]
        let adding = GroupInfoNavigation.openAddMembers(path, groupIdHex: "g")
        XCTAssertEqual(adding.last, .addMembers(groupIdHex: "g"))
        XCTAssertEqual(GroupInfoNavigation.afterAdd(adding, groupIdHex: "g"), path)
    }

    /// ADR-0087 section 10 — `Add members` in a group of one opens the Add
    /// members screen (it opened the info), with the info under it so its
    /// back and Add return there.
    func testAddMembersInAGroupOfOneOpensThePickerOverTheInfo() {
        let path: [TabRoute] = [.chat(groupIdHex: "g")]
        let adding = GroupInfoNavigation.openAddMembersFromConversation(path, groupIdHex: "g")
        XCTAssertEqual(adding, [.chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g"), .addMembers(groupIdHex: "g")])
        XCTAssertEqual(
            GroupInfoNavigation.afterAdd(adding, groupIdHex: "g"),
            [.chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g")])
        XCTAssertEqual(
            GroupInfoNavigation.openAddMembersFromConversation(adding, groupIdHex: "g"), adding,
            "nothing pushed twice")
    }

    func testAMembersContactScreenBacksOntoTheInfo() {
        let path: [TabRoute] = [.chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g"), .contactDetail(messageId: 7)]
        XCTAssertEqual(path.dropLast().last, .groupInfo(groupIdHex: "g"))
    }

    func testDeletingTheChatLeavesBothTheInfoAndTheConversation() {
        let path: [TabRoute] = [.newGroup, .chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g")]
        XCTAssertEqual(GroupInfoNavigation.afterDelete(path, groupIdHex: "g"), [.newGroup])
    }

    func testAScreenOverTheInfoIsAPushAndClosesBackToTheInfo() {
        let info: [TabRoute] = [.chat(groupIdHex: "g"), .groupInfo(groupIdHex: "g")]
        for screen in [GroupInfoSubscreen.inviteLinks, .reactions, .groupType, .transferOwnership] {
            let opened = GroupInfoNavigation.openSubscreen(info, groupIdHex: "g", screen)
            XCTAssertEqual(opened.last, .groupInfoSubscreen(groupIdHex: "g", screen: screen))
            XCTAssertEqual(GroupInfoNavigation.openSubscreen(opened, groupIdHex: "g", screen), opened, "not pushed twice")
            XCTAssertEqual(GroupInfoNavigation.closeSubscreen(opened, groupIdHex: "g", screen), info)
            // Closing one that is not open changes nothing.
            XCTAssertEqual(GroupInfoNavigation.closeSubscreen(info, groupIdHex: "g", screen), info)
        }
    }
}
