// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/call-links.md` section 3 - `Invite contacts` is in the
/// `Participants` menu for every account, with or without conversations; only
/// a guest account does not get it.
///
/// Found live on web 2026-10-02: an account with no conversations saw only
/// `Copy invite link`. An item that comes and goes with the chat list reads
/// as a missing feature, so the list it opens says when it is empty.
@MainActor
final class CallInviteContactsItemTests: XCTestCase {

    private func menu(_ offer: CallInviteOffer, hasOthers: Bool = false)
        -> [CallScreenPresentation.ParticipantsMenuItem]
    {
        CallScreenPresentation.participantsMenu(hasOthers: hasOthers, guest: offer.guest)
    }

    func testAnAccountWithNoContactsOrConversationsStillGetsTheItem() {
        let offer = CallInviteOffer(mint: { "https://lakko.me/c/x#s" }, contacts: { [] })
        XCTAssertTrue(offer.targets().isEmpty)
        XCTAssertFalse(offer.guest)
        XCTAssertEqual(menu(offer), [.copyLink, .inviteContacts])
        XCTAssertEqual(menu(offer, hasOthers: true), [.participants, .copyLink, .inviteContacts])
    }

    func testAGuestDoesNotGetTheItem() {
        let offer = CallInviteOffer(mint: { "https://lakko.me/c/x#s" }, isGuest: { true })
        XCTAssertTrue(offer.guest)
        XCTAssertEqual(menu(offer), [.copyLink])
        XCTAssertEqual(menu(offer, hasOthers: true), [.participants, .copyLink])
    }

    func testTheItemFollowsTheSessionNotTheMomentTheCallStarted() {
        // A guest who keeps the account during a call is an account from then
        // on: the offer asks each time the menu is drawn.
        var guest = true
        let offer = CallInviteOffer(mint: { "https://lakko.me/c/x#s" }, isGuest: { guest })
        XCTAssertEqual(menu(offer), [.copyLink])
        guest = false
        XCTAssertEqual(menu(offer), [.copyLink, .inviteContacts])
    }

    /// The host hands its session's answer to the offer of a call it founds.
    func testTheCallHostTellsTheOfferWhetherTheSessionIsAGuest() async {
        for guest in [false, true] {
            let host = LinkCallHost(ports: LinkCallHost.Ports(
                session: { (Data(repeating: 1, count: 64), "token") },
                sfuBaseUrl: { "wss://sfu.test" },
                microphoneGranted: { true },
                cameraGranted: { _ in true },
                startCall: { _, _, _, _, _, _ in FakeLinkCallHandle() },
                ownIdentity: { (Data(repeating: 2, count: 16), Data(repeating: 3, count: 32)) },
                isGuest: { guest }
            ))
            let outcome = await host.startNewCall()
            XCTAssertEqual(outcome, .started)
            XCTAssertEqual(host.invite?.guest, guest)
            host.reset()
        }
    }

    // MARK: - The list's empty state

    func testAPersonWithNoContactsIsToldSoAndPointedAtTheLink() {
        let own = Localization.text("call.invite.picker_empty")
        XCTAssertEqual(CallInviteContactsScreen.placeholder(hasRows: false), own)
        XCTAssertNotEqual(own, "call.invite.picker_empty", "the key is in the embedded catalog")
        XCTAssertNotEqual(own, Localization.text("chat.forward.empty"))
    }

    func testASearchThatMatchesNobodySaysSo() {
        XCTAssertEqual(
            CallInviteContactsScreen.placeholder(hasRows: true),
            Localization.text("chats.search.no_results"))
    }
}
