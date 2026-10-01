// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The confirmation after revoking a group invite link (the web and Android
/// rule): a ledger pointer link is revoked, an inline link is only cleared.
final class InviteLinkRevokeNoticeTests: XCTestCase {

    func testALedgerLinkReadsRevoked() {
        XCTAssertEqual(ChatViewModel.inviteLinkRevokeNotice(revoked: true), "chat.invite_link.revoked")
    }

    func testAnInlineLinkReadsClearedNotRevoked() {
        XCTAssertEqual(ChatViewModel.inviteLinkRevokeNotice(revoked: false), "chat.invite_link.cleared")
    }

    func testBothKeysExistInTheCatalog() {
        for revoked in [true, false] {
            let key = ChatViewModel.inviteLinkRevokeNotice(revoked: revoked)
            XCTAssertNotEqual(Localization.text(key), key)
        }
    }
}
