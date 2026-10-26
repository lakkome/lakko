// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.contacts.InvitePreviewResult
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The signed-out invite page (`docs/ux/contact-invite-link.md` section 3,
 * "Signed out", amended 2026-10-26; ADR-0079 amendment of 2026-10-26).
 */
class SignedOutInviteStateTest {

    private val owner = ByteArray(16) { it.toByte() }

    private fun preview(displayName: String?, username: String?, label: String = "") =
        InvitePreviewResult.Preview(displayName, username, label, owner)

    @Test
    fun liveLinkShowsTheInviterCardAndKeepsTheLinkParked() {
        val state = signedOutInviteState(preview("Anna", "anna", "from the conference"))
        val live = state as SignedOutInviteState.Live
        assertEquals("Anna", live.statedName)
        assertEquals("@anna", live.usernameLine)
        assertEquals("from the conference", live.label)
        assertEquals(me.lakko.app.ds.hexOf(owner), live.avatarSeed)
        assertTrue(state.offersAccount)
        assertTrue(state.keepsLinkParked)
    }

    @Test
    fun liveLinkWithOnlyAUsernameTitlesWithItAndShowsNoSecondLine() {
        val live = signedOutInviteState(preview(null, "anna")) as SignedOutInviteState.Live
        assertEquals("@anna", live.statedName)
        assertNull(live.usernameLine)
    }

    @Test
    fun liveLinkWithNoNameLeavesTheTitleToTheUnnamedText() {
        val live = signedOutInviteState(preview("  ", null)) as SignedOutInviteState.Live
        assertNull(live.statedName)
        assertNull(live.usernameLine)
        assertEquals("", live.label)
    }

    @Test
    fun deadLinkOffersBothAccountsAndDropsTheLink() {
        val state = signedOutInviteState(InvitePreviewResult.Dead)
        assertEquals(SignedOutInviteState.Dead, state)
        assertTrue(state.offersAccount)
        assertFalse(state.keepsLinkParked)
    }

    @Test
    fun rateLimitAndNetworkFailureReuseTheRetryStates() {
        val limited = signedOutInviteState(InvitePreviewResult.RateLimited)
        val failed = signedOutInviteState(InvitePreviewResult.Failed)
        assertEquals(SignedOutInviteState.RateLimited, limited)
        assertEquals(SignedOutInviteState.OpenFailed, failed)
        assertFalse(limited.offersAccount)
        assertFalse(failed.offersAccount)
        assertFalse(limited.keepsLinkParked)
        assertFalse(SignedOutInviteState.Loading.offersAccount)
    }
}
