// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What `Invite` is doing (`docs/ux/call-links.md` sections 3 and 5). The test
 * list is the web client's `in-call-invite.test.ts`.
 */
class InCallInviteTest {
    init {
        EnglishForTests.install()
    }

    private val url = "https://lakko.me/c/AAAA#secret"
    private val copied = InCallInvite.Ready(url, LinkDelivery.Copied)

    // ---- nextInvitePress -----------------------------------------------------

    @Test
    fun `it mints once and then re-offers, so a second use costs no round trip`() {
        assertEquals(InvitePress.MINT, nextInvitePress(InCallInvite.Idle))
        assertEquals(InvitePress.REOFFER, nextInvitePress(copied))
    }

    @Test
    fun `a use while a mint is in flight is ignored`() {
        assertEquals(InvitePress.IGNORE, nextInvitePress(InCallInvite.Minting))
    }

    @Test
    fun `a failed mint can be tried again`() {
        assertEquals(InvitePress.MINT, nextInvitePress(InCallInvite.Failed(CallInviteFailure.OTHER)))
        assertEquals(InvitePress.MINT, nextInvitePress(InCallInvite.Failed(CallInviteFailure.NO_SESSION)))
        assertEquals(InvitePress.MINT, nextInvitePress(InCallInvite.Failed(CallInviteFailure.NETWORK)))
    }

    @Test
    fun `a failed send does not mint again`() {
        val state = InCallInvite.Failed(CallInviteFailure.NOT_SENT, url)
        assertEquals(InvitePress.REOFFER, nextInvitePress(state))
        assertEquals(url, mintedInviteUrl(state))
        assertNull(mintedInviteUrl(InCallInvite.Failed(CallInviteFailure.OTHER)))
        assertNull(mintedInviteUrl(InCallInvite.Idle))
        assertNull(mintedInviteUrl(InCallInvite.Minting))
    }

    // ---- inviteBusy ----------------------------------------------------------

    @Test
    fun `it is busy only while minting`() {
        assertFalse(inviteBusy(InCallInvite.Idle))
        assertTrue(inviteBusy(InCallInvite.Minting))
        assertFalse(inviteBusy(copied))
        assertFalse(inviteBusy(InCallInvite.Failed(CallInviteFailure.OTHER)))
    }

    // ---- inviteStatusLine ----------------------------------------------------

    @Test
    fun `nothing is said before the first use`() {
        assertNull(inviteStatusLine(InCallInvite.Idle))
    }

    @Test
    fun `the wait says what it is, not only that there is one`() {
        val line = inviteStatusLine(InCallInvite.Minting)!!.en
        assertTrue(line.contains("link"))
        assertTrue(line.contains("few seconds"))
    }

    @Test
    fun `the line names where the link went`() {
        assertTrue(inviteStatusLine(InCallInvite.Ready(url, LinkDelivery.Sent("Anna")))!!.en.contains("Anna"))
        assertTrue(inviteStatusLine(copied)!!.en.contains("copied"))
        assertTrue(inviteStatusLine(InCallInvite.Ready(url, LinkDelivery.Shown))!!.en.contains(url))
    }

    @Test
    fun `a failure says what happened and what to do, and never ends the call`() {
        val failed = inviteStatusLine(InCallInvite.Failed(CallInviteFailure.OTHER))!!.en
        assertTrue(failed.contains("could not"))
        assertTrue(failed.contains("try again"))
        assertTrue(failed.contains("call is unaffected"))
        assertTrue(inviteStatusLine(InCallInvite.Failed(CallInviteFailure.NO_SESSION))!!.en.contains("Sign in again"))
        assertEquals(
            "The invitation was not sent. Try again.",
            inviteStatusLine(InCallInvite.Failed(CallInviteFailure.NOT_SENT, url))!!.en,
        )
    }

    @Test
    fun `no line uses protocol vocabulary or an exclamation mark`() {
        val states = listOf(
            InCallInvite.Idle,
            InCallInvite.Minting,
            InCallInvite.Ready(url, LinkDelivery.Sent("Anna")),
            InCallInvite.Failed(CallInviteFailure.NOT_SENT, url),
            copied,
            InCallInvite.Ready(url, LinkDelivery.Shown),
            InCallInvite.Failed(CallInviteFailure.NO_SESSION),
            InCallInvite.Failed(CallInviteFailure.NETWORK),
            InCallInvite.Failed(CallInviteFailure.OTHER),
        )
        val banned = listOf(
            "MLS", "OPRF", "KeyPackage", "pseudonym", "epoch", "capability",
            "proof of work", "proof-of-work", "by design",
        )
        for (state in states) {
            val line = inviteStatusLine(state)?.en ?: continue
            assertFalse("$state", line.contains("!"))
            for (word in banned) {
                assertFalse("$state: $word", line.lowercase().contains(word.lowercase()))
            }
        }
    }

    // ---- inviteStatusHoldMs --------------------------------------------------

    @Test
    fun `a finished line is held for a few seconds`() {
        assertEquals(INVITE_STATUS_HOLD_MS, inviteStatusHoldMs(copied))
        assertEquals(INVITE_STATUS_HOLD_MS, inviteStatusHoldMs(InCallInvite.Failed(CallInviteFailure.OTHER)))
        assertEquals(INVITE_STATUS_HOLD_MS, inviteStatusHoldMs(InCallInvite.Ready(url, LinkDelivery.Sent("Anna"))))
    }

    @Test
    fun `a wait, an empty line and a link being copied by hand never time out`() {
        assertNull(inviteStatusHoldMs(InCallInvite.Minting))
        assertNull(inviteStatusHoldMs(InCallInvite.Idle))
        assertNull(inviteStatusHoldMs(InCallInvite.Ready(url, LinkDelivery.Shown)))
    }

    // ---- copyCallLink --------------------------------------------------------

    @Test
    fun `the link goes to the clipboard`() {
        val copies = ArrayList<String>()
        assertEquals(LinkDelivery.Copied, copyCallLink(url) { copies.add(it) })
        assertEquals(listOf(url), copies)
    }

    @Test
    fun `the link is shown when the clipboard is not reachable`() {
        assertEquals(LinkDelivery.Shown, copyCallLink(url, null))
        assertEquals(LinkDelivery.Shown, copyCallLink(url) { throw SecurityException("denied") })
    }
}
