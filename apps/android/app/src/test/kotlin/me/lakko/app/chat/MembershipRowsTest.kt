// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.json.Json
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.ui.chat.TimelineItem
import me.lakko.app.ui.chat.buildTimeline
import me.lakko.app.ui.chat.isHiddenSystemRow
import me.lakko.app.ui.chat.previewSourceRow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * `docs/ux/second-device.md` section 4, "Membership rows (decided
 * 2026-10-29)": no generic "Members changed"; a group's batch that changes
 * the people names them on its last commit row; nothing else is drawn.
 */
class MembershipRowsTest {
    init {
        EnglishForTests.install()
    }

    private val ana = "QUFB" // person keys, standard base64
    private val bea = "QkJC"
    private val own = "T1dO"
    private val names = mapOf(ana to "Ana", bea to "Bea")
    private val nameOf: (MemberIdentity) -> String = { names[it] ?: "Member" }
    private val noAccount: (String) -> String? = { null }

    private fun commit(id: Long) = IncomingChatEvent.System(id, SystemKind.Commit, "2026-10-29T10:00:0${id % 10}Z")

    private fun commitRow(id: Long, joined: List<String> = emptyList(), left: List<String> = emptyList()) =
        ChatMessage(
            messageId = id,
            direction = Direction.System,
            text = COMMIT_ROW_TEXT,
            createdAt = "2026-10-29T10:00:00Z",
            membershipJoined = joined,
            membershipLeft = left,
        )

    private fun text(id: Long, body: String) = ChatMessage(
        messageId = id,
        direction = Direction.Incoming,
        text = body,
        createdAt = "2026-10-29T10:00:00Z",
        authorKey = ana,
    )

    private fun shown(messages: List<ChatMessage>, isGroup: Boolean): List<Long> =
        buildTimeline(
            messages,
            Instant.parse("2026-10-29T12:00:00Z").toEpochMilli(),
            ZoneId.of("UTC"),
            use24h = true,
            locale = Locale.UK,
            isGroup = isGroup,
        ).filterIsInstance<TimelineItem.Row>().map { it.message.messageId }

    @Test
    fun `a batch adding a person records joined on its last commit row and draws the name`() {
        val change = membershipChangeOf(listOf(own, ana), listOf(own, ana, bea), noAccount, false, own)
        assertEquals(MembershipChange(joined = listOf(bea), left = emptyList()), change)
        val batch = listOf(commit(1), IncomingChatEvent.System(2, SystemKind.ExternalJoinProposal, "t"), commit(3))
        assertEquals(2, membershipRowIndex(batch, change, commitRowsSuppressed = false))
        val row = commitRow(3, joined = change!!.joined)
        assertTrue(isMembershipRow(row))
        assertEquals("Bea joined", membershipRowText(row, nameOf))
        assertEquals(listOf(1L, 3L), shown(listOf(text(1, "hi"), row), isGroup = true))
    }

    @Test
    fun `a person gone draws that they are no longer in the group, several people one sentence each with joined first`() {
        val change = membershipChangeOf(listOf(own, ana, bea), listOf(own, ana), noAccount, false, own)
        assertEquals(MembershipChange(joined = emptyList(), left = listOf(bea)), change)
        assertEquals("Bea is no longer in the group", membershipRowText(commitRow(4, left = change!!.left), nameOf))
        val both = commitRow(5, joined = listOf("Q0ND"), left = listOf(bea))
        assertEquals("Member joined Bea is no longer in the group", membershipRowText(both, nameOf))
    }

    @Test
    fun `a device added or removed records nothing and draws nothing`() {
        // The same people before and after (a device leaf came or went).
        assertNull(membershipChangeOf(listOf(own, ana), listOf(ana, own), noAccount, false, own))
        // A reset's devices swapping: the old key and its successor are one person.
        val successor = "QU5B"
        val accountOf = { k: String -> if (k == ana || k == successor) "acct-ana" else null }
        assertNull(membershipChangeOf(listOf(own, ana), listOf(own, successor), accountOf, false, own))
        // The device-swap verdict suppressed the batch.
        assertNull(membershipChangeOf(listOf(own, ana), listOf(own), noAccount, swapSuppressed = true, ownPersonKey = own))
        // No change: no row carries anything.
        assertEquals(-1, membershipRowIndex(listOf(commit(1)), null, false))
        assertEquals(-1, membershipRowIndex(listOf(commit(1)), MembershipChange(listOf(bea), emptyList()), true))
        // A torn read records nothing.
        assertNull(membershipChangeOf(null, listOf(own), noAccount, false, own))
    }

    @Test
    fun `this device's own joining or removal writes no row`() {
        assertNull(membershipChangeOf(listOf(ana), listOf(ana, own), noAccount, false, own))
        assertNull(membershipChangeOf(listOf(ana, own), listOf(ana), noAccount, false, own))
    }

    @Test
    fun `a stored commit row without keys is not drawn, in a group or anywhere`() {
        val old = commitRow(2)
        assertFalse(isMembershipRow(old))
        assertTrue(isHiddenSystemRow(old, isGroup = true))
        assertNull(membershipRowText(old, nameOf))
        assertEquals(listOf(1L), shown(listOf(text(1, "hi"), old), isGroup = true))
    }

    @Test
    fun `a one-to-one draws no membership row, even one that carries keys`() {
        val row = commitRow(2, joined = listOf(bea))
        assertTrue(isHiddenSystemRow(row, isGroup = false))
        assertEquals(listOf(1L), shown(listOf(text(1, "hi"), row), isGroup = false))
    }

    @Test
    fun `the chat-list preview skips a membership row`() {
        val messages = listOf(text(1, "hi"), commitRow(2, joined = listOf(bea)))
        assertEquals(1L, previewSourceRow(messages)?.messageId)
        assertNull(previewSourceRow(listOf(commitRow(2, left = listOf(bea)))))
    }

    @Test
    fun `the person keys survive the stored log, and an old row reads back with none`() {
        val row = commitRow(7, joined = listOf(bea), left = listOf(ana))
        val back = PersistedMessage.from(row).toChatMessage()
        assertEquals(listOf(bea), back?.membershipJoined)
        assertEquals(listOf(ana), back?.membershipLeft)
        val json = Json { ignoreUnknownKeys = true }
        val oldRow = json.decodeFromString(
            PersistedMessage.serializer(),
            """{"messageId":9,"direction":"system","text":"[mls commit]","attachments":[],"createdAt":"2026-10-29T10:00:00Z"}""",
        ).toChatMessage()
        assertEquals(emptyList<String>(), oldRow?.membershipJoined)
        assertEquals(emptyList<String>(), oldRow?.membershipLeft)
        assertTrue(oldRow != null && isHiddenSystemRow(oldRow, isGroup = true))
    }

    @Test
    fun `the committing device writes the named row for its own merged sweep at the commit's id`() {
        // The DS never returns this device's own commit, so the sweep's merge
        // writes the row a receiver would: people before the sweep against
        // people after the merge, at the commit route's message id.
        val change = membershipChangeOf(listOf(own, ana), listOf(own, ana, bea), noAccount, false, own)!!
        val held = listOf(text(10, "hi"), text(12, "later"))
        val at = ownCommitRowInsertIndex(held, 11)
        assertEquals(1, at)
        val timeline = held.toMutableList().apply { add(at!!, ownCommitRow(11, "2026-10-29T10:00:00Z", change)) }
        assertEquals(listOf(10L, 11L, 12L), timeline.map { it.messageId })
        assertEquals("Bea joined", membershipRowText(timeline[1], nameOf))
        assertEquals(listOf(10L, 11L, 12L), shown(timeline, isGroup = true))
        // Drawn only in a group.
        assertEquals(listOf(10L, 12L), shown(timeline, isGroup = false))
        // Written once: a second merge report for the same id adds nothing.
        assertNull(ownCommitRowInsertIndex(timeline, 11))
        // The newest row: appended at the end.
        assertEquals(3, ownCommitRowInsertIndex(timeline, 20))
        // A device-only own commit records nothing to write.
        assertNull(membershipChangeOf(listOf(own, ana), listOf(own, ana), noAccount, false, own))
    }
}
