// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/message-actions.md` section 9 — a notification already on screen
 * for a deleted message is withdrawn; nothing else is.
 */
class DeletedMessageNotificationsTest {
    private val group = "ab".repeat(32)
    private val other = "cd".repeat(32)

    private fun shown(id: Int, groupHex: String? = group, messageId: Long? = 10L, author: String? = "anna") =
        ShownNotification(tag = null, id = id, groupHex = groupHex, messageId = messageId, authorKeyB64 = author)

    @Test
    fun `the notification of a deleted message is withdrawn`() {
        val n = shown(1)
        assertEquals(listOf(n), notificationsToWithdraw(listOf(n), group, listOf(AppliedDelete(10))))
    }

    @Test
    fun `a notification of another message or another group stays`() {
        val active = listOf(shown(1, messageId = 11L), shown(2, groupHex = other))
        assertTrue(notificationsToWithdraw(active, group, listOf(AppliedDelete(10))).isEmpty())
    }

    @Test
    fun `a generic or legacy notification without the extras is left alone`() {
        val active = listOf(shown(1, groupHex = null, messageId = null, author = null), shown(2, messageId = null))
        assertTrue(notificationsToWithdraw(active, group, listOf(AppliedDelete(10))).isEmpty())
    }

    @Test
    fun `an unchecked delete withdraws only when the deleter is the recorded author`() {
        val n = shown(1, author = "anna")
        assertEquals(listOf(n), notificationsToWithdraw(listOf(n), group, listOf(AppliedDelete(10, "anna"))))
        assertTrue(notificationsToWithdraw(listOf(n), group, listOf(AppliedDelete(10, "bob"))).isEmpty())
        // No recorded author: an unchecked delete cannot be matched to it.
        val noAuthor = shown(2, author = null)
        assertTrue(notificationsToWithdraw(listOf(noAuthor), group, listOf(AppliedDelete(10, "anna"))).isEmpty())
        // A delete the running app applied passed the author gate already.
        assertEquals(listOf(noAuthor), notificationsToWithdraw(listOf(noAuthor), group, listOf(AppliedDelete(10))))
    }

    @Test
    fun `group ids are compared case-insensitively and no deletes withdraw nothing`() {
        val n = shown(1, groupHex = group.uppercase())
        assertEquals(listOf(n), notificationsToWithdraw(listOf(n), group, listOf(AppliedDelete(10))))
        assertTrue(notificationsToWithdraw(listOf(n), group, emptyList()).isEmpty())
    }
}
