// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0065 R.5 - one thread per conversation, the stores read through any
 *  segment, and the read position across segments. */
class ConversationFoldTest {
    private val lineage = buildLineage(
        listOf(
            LineageEntry("s1", "root", 10UL),
            LineageEntry("s2", "root", 20UL),
        ),
    )

    private fun message(id: Long, text: String = "m$id", localId: Long = 0, reply: ReplyRef? = null) = ChatMessage(
        messageId = id,
        direction = Direction.Incoming,
        text = text,
        createdAt = "2026-01-01T00:00:00Z",
        localId = localId,
        reply = reply,
    )

    private fun thread(hex: String, vararg messages: ChatMessage, ended: Boolean = false, unread: Int = 0) =
        ChatThread(groupIdHex = hex, messages = messages.toList(), cursor = 0, membershipEnded = ended, localUnread = unread)

    @Test
    fun `a conversation of one group is its thread untouched`() {
        val alone = thread("solo", message(1))
        val folded = foldConversations(listOf(alone), lineage)
        assertEquals(1, folded.size)
        assertSame(alone, folded[0])
    }

    @Test
    fun `segments fold into one thread whose head is the last active group`() {
        val folded = foldConversations(
            listOf(
                thread("root", message(1), message(2), ended = true, unread = 1),
                thread("other", message(9)),
                thread("s1", message(1), message(3), unread = 2),
            ),
            lineage,
        )
        assertEquals(listOf("s1", "other"), folded.map { it.groupIdHex })
        val c = folded[0]
        assertEquals("root", c.conversationHex)
        assertEquals(listOf("root"), c.olderSegments)
        assertEquals(3, c.localUnread)
        // History first, then the head's rows; every id is distinct although
        // both groups hold a message 1.
        assertEquals(
            listOf(historyRowId(1, 1), historyRowId(2, 1), 1L, 3L),
            c.messages.map { it.messageId },
        )
        assertEquals(listOf("root", "root", null, null), c.messages.map { it.historyOf })
        assertEquals(listOf(1L, 2L, 0L, 0L), c.messages.map { it.historyMessageId })
        assertEquals(4, c.messages.map { it.rowId }.toSet().size)
        assertTrue(c.messages[0].isHistory)
        assertFalse(c.messages[2].isHistory)
    }

    @Test
    fun `a successor this device does not hold leaves the old group as the head`() {
        val folded = foldConversations(listOf(thread("root", message(1))), lineage)
        assertEquals("root", folded.single().groupIdHex)
        assertTrue(folded.single().olderSegments.isEmpty())
    }

    @Test
    fun `three segments keep their order and their distances`() {
        val folded = foldConversations(
            listOf(thread("s2", message(5)), thread("s1", message(5)), thread("root", message(5))),
            lineage,
        ).single()
        assertEquals("s2", folded.groupIdHex)
        assertEquals(listOf("root", "s1"), folded.olderSegments)
        assertEquals(listOf(historyRowId(5, 2), historyRowId(5, 1), 5L), folded.messages.map { it.messageId })
        // The stores resolve a history id back to its group.
        assertEquals(PinTarget("root", 5, older = true), pinTargetOfRow(historyRowId(5, 2), "s2", folded.olderSegments))
        assertEquals(PinTarget("s1", 5, older = true), pinTargetOfRow(historyRowId(5, 1), "s2", folded.olderSegments))
    }

    @Test
    fun `a history row keeps a quote that jumps inside its own segment and a local row keeps an id of its own`() {
        val quoted = historyRowOf(message(7, reply = ReplyRef(target = 4, author = "a", excerpt = "e")), "root", 1)
        assertEquals(historyRowId(4, 1), quoted.reply?.target)
        val noJump = historyRowOf(message(7, reply = ReplyRef(target = 0, author = "a", excerpt = "e")), "root", 1)
        assertEquals(0L, noJump.reply?.target)
        val local = historyRowOf(message(0, localId = -3), "root", 1)
        assertEquals(0L, local.messageId)
        assertTrue(local.rowId < -HISTORY_ID_SPAN)
        assertFalse(canSelectMessage(quoted))
    }

    @Test
    fun `any segment id finds the conversation`() {
        val folded = foldConversations(listOf(thread("root", message(1)), thread("s1", message(2))), lineage)
        assertEquals("s1", conversationThreadOf(folded, "s1")?.groupIdHex)
        assertEquals("s1", conversationThreadOf(folded, "root")?.groupIdHex)
        assertEquals("s1", conversationThreadOf(folded, "ROOT")?.groupIdHex)
        assertNull(conversationThreadOf(folded, "nope"))
        assertEquals(listOf("root", "s1"), conversationDeleteOrder(folded.single()))
    }

    @Test
    fun `a forward jumps to the head row or to the history row of an older segment`() {
        val c = foldConversations(listOf(thread("root", message(4)), thread("s1", message(4))), lineage).single()
        assertEquals(4L, forwardJumpRow(c, "s1", 4))
        assertEquals(historyRowId(4, 1), forwardJumpRow(c, "root", 4))
        assertNull(forwardJumpRow(c, "elsewhere", 4))
        assertNull(forwardJumpRow(c, "root", 0))
        val ref = ForwardRef(author = "x", originGroupIdHex = "root", originMessageId = 4)
        assertEquals("s1" to historyRowId(4, 1), forwardJumpTarget(ref, listOf(c)))
        assertNull(forwardJumpTarget(ref.copy(originMessageId = 9), listOf(c)))
    }

    @Test
    fun `a store keyed by conversation answers under every segment`() {
        val entries = listOf(LineageEntry("s1", "root", 10UL), LineageEntry("s2", "root", 20UL))
        val out = aliasAcrossSegments(mapOf("root" to "R", "solo" to "S", "s1" to "stale"), entries, lineage)
        assertEquals(mapOf("root" to "R", "solo" to "S", "s1" to "R", "s2" to "R"), out)
        // No register: the map is returned as it is.
        val plain = mapOf("a" to 1)
        assertSame(plain, aliasAcrossSegments(plain, emptyList(), NO_LINEAGE))
        // A conversation with no value gives its segments none.
        assertEquals(mapOf("solo" to "S"), aliasAcrossSegments(mapOf("solo" to "S"), entries, lineage))
    }

    @Test
    fun `the read position reads as each segment's own message id`() {
        val inFirst = encodeReadPosition(TimelinePosition(0, 42))
        assertEquals(8, inFirst.size)
        assertArrayEquals(inFirst, readCursorBytesForSegment(inFirst, 0))
        assertNull(readCursorBytesForSegment(inFirst, 1))
        val inSecond = encodeReadPosition(TimelinePosition(1, 7))
        assertEquals(12, inSecond.size)
        assertArrayEquals(byteArrayOf(0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1), inSecond)
        assertArrayEquals(
            java.nio.ByteBuffer.allocate(8).putLong(Long.MAX_VALUE).array(),
            readCursorBytesForSegment(inSecond, 0),
        )
        assertArrayEquals(java.nio.ByteBuffer.allocate(8).putLong(7).array(), readCursorBytesForSegment(inSecond, 1))
        assertNull(readCursorBytesForSegment(inSecond, 2))
        assertNull(readCursorBytesForSegment(ByteArray(3), 0))
    }

    @Test
    fun `a read position is written with its segment and never moves back across segments`() {
        val id9 = java.nio.ByteBuffer.allocate(8).putLong(9).array()
        assertArrayEquals(encodeReadPosition(TimelinePosition(0, 9)), readCursorBytesForWrite(id9, 0, null))
        assertArrayEquals(encodeReadPosition(TimelinePosition(2, 9)), readCursorBytesForWrite(id9, 2, null))
        val heldLater = encodeReadPosition(TimelinePosition(1, 3))
        // Marking an older segment read after the head was read: dropped.
        assertNull(readCursorBytesForWrite(id9, 0, heldLater))
        // Moving forward to a later segment: written.
        assertArrayEquals(encodeReadPosition(TimelinePosition(2, 1)), readCursorBytesForWrite(
            java.nio.ByteBuffer.allocate(8).putLong(1).array(), 2, heldLater,
        ))
        // The same segment is the caller's own monotone rule.
        assertArrayEquals(encodeReadPosition(TimelinePosition(1, 9)), readCursorBytesForWrite(id9, 1, heldLater))
        // "Everything" read back from an older segment is never written.
        assertNull(readCursorBytesForWrite(java.nio.ByteBuffer.allocate(8).putLong(Long.MAX_VALUE).array(), 0, null))
        assertNull(readCursorBytesForWrite(ByteArray(4), 0, null))
    }

    @Test
    fun `a history row offers unpin alone`() {
        assertTrue(me.lakko.app.ui.chat.historyRowActions(rowIsPinned = true, pinLabel = "Unpin").let {
            it.canPin && !it.canCopy && !it.canEdit && !it.canDelete && !it.canReply && !it.canForward && !it.canSelect
        })
        assertFalse(me.lakko.app.ui.chat.historyRowActions(rowIsPinned = false, pinLabel = "Pin").any)
    }
}
