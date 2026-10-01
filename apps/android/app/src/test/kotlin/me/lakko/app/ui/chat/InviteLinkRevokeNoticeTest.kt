// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Test

/** The notice after Revoke on a group invite link, the web rule. */
class InviteLinkRevokeNoticeTest {
    @Test
    fun aLedgerLinkThatWasRevokedSaysRevoked() {
        assertEquals(UiText("chat.invite_link.revoked"), inviteLinkRevokeNotice(revoked = true))
    }

    @Test
    fun anInlineLinkWithNothingToRevokeSaysCleared() {
        assertEquals(UiText("chat.invite_link.cleared"), inviteLinkRevokeNotice(revoked = false))
    }
}
