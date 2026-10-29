// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.UsernameVerification
import me.lakko.app.profile.avatarName
import me.lakko.app.profile.resolveName
import me.lakko.app.ui.chat.initialsOf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/proposal-based-membership.md` section 2, "What 'tap, done' shows"
 * (2026-10-29, with the same day's amendment): a membership attempt shows no
 * notice when its change is in effect, whichever path committed it; only a
 * plain member's leave that has not landed shows `chat.leave.requested`, which
 * goes by itself when the leave lands.
 */
class RequestedNoticeTest {
    init {
        EnglishForTests.install()
    }

    private val leave = UiText("chat.leave.requested")

    @Test
    fun `no notice when the change is in effect after the attempt, even if a background sweep committed it`() {
        val notice = RequestedNotice()
        // The screen's own sweep found nothing to do: a background sweep had
        // already applied the leave. The group says it is in effect.
        assertNull(notice.afterAttempt(leave) { leaveApplied(isActive = false) })
        assertNull(notice.notice)
        assertNull(notice.onRefresh())
    }

    @Test
    fun `the notice shows when the change is not in effect, and clears itself when it lands`() {
        var active: Boolean? = true
        val notice = RequestedNotice()
        assertEquals(leave, notice.afterAttempt(leave) { leaveApplied(active) })
        assertEquals(leave, notice.notice)
        // A refresh before it lands keeps it.
        assertNull(notice.onRefresh())
        assertEquals(leave, notice.notice)
        // An unreadable membership (`null`) is not a landing.
        active = null
        assertNull(notice.onRefresh())
        // It landed: the refresh names the notice that goes, once.
        active = false
        assertEquals(leave, notice.onRefresh())
        assertNull(notice.notice)
        assertNull(notice.onRefresh())
    }

    @Test
    fun `a group read that throws is not a landing`() {
        val notice = RequestedNotice()
        assertEquals(leave, notice.afterAttempt(leave) { error("torn read") })
        assertNull(notice.onRefresh())
        assertEquals(leave, notice.notice)
        notice.clear()
        assertNull(notice.notice)
    }

    @Test
    fun `leave is in effect only once the membership ended`() {
        assertTrue(leaveApplied(false))
        assertFalse(leaveApplied(true))
        assertFalse(leaveApplied(null))
    }

    @Test
    fun `avatar initials come from the name, never from a label with the username`() {
        val resolved = resolveName(
            localRename = null,
            profile = PeerProfile("VG2", "vg2", 1uL),
            verificationOf = { UsernameVerification.VERIFIED },
            signatureKey = ByteArray(32) { 7 },
        )
        // The list label carries the username; its initials were the defect.
        assertEquals("VG2 · @vg2", resolved.listLabel)
        assertEquals("V·", initialsOf(resolved.listLabel))
        assertEquals("VG2", avatarName(resolved))
        assertEquals("V", initialsOf(avatarName(resolved)))
        val twoWords = resolveName(
            localRename = null,
            profile = PeerProfile("Anna Petrova", "anna", 1uL),
            verificationOf = { UsernameVerification.VERIFIED },
            signatureKey = ByteArray(32) { 8 },
        )
        assertEquals("AP", initialsOf(avatarName(twoWords)))
    }
}
