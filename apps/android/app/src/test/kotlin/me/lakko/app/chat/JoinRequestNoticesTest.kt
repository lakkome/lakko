// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** `docs/ux/group-and-channel-privacy.md` section 8.1 — the join-request signals. */
class JoinRequestNoticesTest {

    private val a = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE="
    private val b = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI="

    @Test
    fun `only an owner or admin whose membership holds gets the signals`() {
        assertTrue(joinRequestSignalsApply(ownerOrAdmin = true, membershipEnded = false))
        assertFalse(joinRequestSignalsApply(ownerOrAdmin = false, membershipEnded = false))
        assertFalse(joinRequestSignalsApply(ownerOrAdmin = true, membershipEnded = true))
    }

    @Test
    fun `the bar is gone for a member and when nothing waits`() {
        assertNull(joinRequestBanner(signalsApply = false, pendingCount = 3, queueFull = false))
        assertNull(joinRequestBanner(signalsApply = true, pendingCount = 0, queueFull = false))
        // A full queue with nothing retained is no count of decisions owed.
        assertNull(joinRequestBanner(signalsApply = true, pendingCount = 0, queueFull = true))
    }

    @Test
    fun `the bar counts one and many through the plural key`() {
        assertEquals(JoinRequestBannerText(JOIN_REQUESTS_BANNER_KEY, 1), joinRequestBanner(true, 1, false))
        assertEquals(JoinRequestBannerText(JOIN_REQUESTS_BANNER_KEY, 3), joinRequestBanner(true, 3, false))
        assertEquals(JoinRequestBannerText(JOIN_REQUESTS_BANNER_KEY, 63), joinRequestBanner(true, 63, false))
    }

    @Test
    fun `the bar reads 64+ when the heading does`() {
        assertEquals(JoinRequestBannerText(JOIN_REQUESTS_BANNER_FULL_KEY, null), joinRequestBanner(true, 64, false))
        assertEquals(JoinRequestBannerText(JOIN_REQUESTS_BANNER_FULL_KEY, null), joinRequestBanner(true, 5, true))
    }

    @Test
    fun `a request is announced once`() {
        val first = onJoinRequestArrived(JoinRequestMarks(), a, openAndVisible = false, notificationAllowed = true)
        assertTrue(first.notify)
        assertEquals(setOf(a), first.marks.announced)
        assertEquals(setOf(a), first.marks.unshown)
        // Asked again the next day: the same request, not announced again.
        val again = onJoinRequestArrived(first.marks, a, openAndVisible = false, notificationAllowed = true)
        assertFalse(again.notify)
        assertEquals(first.marks, again.marks)
    }

    @Test
    fun `no notification while the conversation is open in front`() {
        val r = onJoinRequestArrived(JoinRequestMarks(), a, openAndVisible = true, notificationAllowed = true)
        assertFalse(r.notify)
        assertEquals(setOf(a), r.marks.announced)
        assertTrue(r.marks.unshown.isEmpty())
        // Later, out of view, the same request does not notify either.
        assertFalse(onJoinRequestArrived(r.marks, a, openAndVisible = false, notificationAllowed = true).notify)
    }

    @Test
    fun `a muted group counts the request unshown but posts nothing`() {
        val r = onJoinRequestArrived(JoinRequestMarks(), a, openAndVisible = false, notificationAllowed = false)
        assertFalse(r.notify)
        assertEquals(setOf(a), r.marks.unshown)
    }

    @Test
    fun `drawing the bar shows every request`() {
        val marks = JoinRequestMarks(announced = setOf(a, b), unshown = setOf(a, b))
        val shown = markJoinRequestsShown(marks)
        assertTrue(shown.unshown.isEmpty())
        assertEquals(setOf(a, b), shown.announced)
    }

    @Test
    fun `a request no longer waiting leaves both sets`() {
        val marks = JoinRequestMarks(announced = setOf(a, b), unshown = setOf(a, b))
        val pruned = pruneJoinRequestMarks(marks, pending = setOf(b))
        assertEquals(setOf(b), pruned.announced)
        assertEquals(setOf(b), pruned.unshown)
        assertTrue(pruneJoinRequestMarks(marks, emptySet()).isEmpty)
        // Nothing to drop returns the same marks.
        assertTrue(pruneJoinRequestMarks(marks, setOf(a, b)) === marks)
    }

    @Test
    fun `each unshown request adds one to the row`() {
        assertEquals(5, joinRequestRowUnread(messageUnread = 3, unshownCount = 2))
        assertEquals(2, joinRequestRowUnread(messageUnread = 0, unshownCount = 2))
        assertEquals(3, joinRequestRowUnread(messageUnread = 3, unshownCount = 0))
        assertEquals(Int.MAX_VALUE, joinRequestRowUnread(Int.MAX_VALUE, 5))
    }

    @Test
    fun `the notification names the person or says someone`() {
        assertEquals(
            JoinRequestNoticeText(JOIN_REQUEST_NOTICE_BODY_KEY, mapOf("name" to "Anna", "group" to "Hikers")),
            joinRequestNotice("Anna", "Hikers"),
        )
        assertEquals(
            JoinRequestNoticeText(JOIN_REQUEST_NOTICE_BODY_UNNAMED_KEY, mapOf("group" to "Hikers")),
            joinRequestNotice(null, "Hikers"),
        )
        assertEquals(JOIN_REQUEST_NOTICE_BODY_UNNAMED_KEY, joinRequestNotice("  ", "Hikers").bodyKey)
    }

    @Test
    fun `one notification tag per group and request`() {
        assertFalse(joinRequestNotificationTag("AB", a) == joinRequestNotificationTag("ab", b))
        assertEquals(joinRequestNotificationTag("AB", a), joinRequestNotificationTag("ab", a))
    }

    @Test
    fun `the stored sets round trip`() {
        assertNull(encodeJoinRequestKeys(emptySet()))
        assertEquals(setOf(a, b), decodeJoinRequestKeys(encodeJoinRequestKeys(setOf(b, a))))
        assertTrue(decodeJoinRequestKeys(null).isEmpty())
    }
}
