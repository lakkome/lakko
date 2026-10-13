// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 R.7 case 1 - the stored held messages (mirror of the web
 *  `held-send-queue` codec cases). */
class HeldSendQueueTest {
    private fun row(text: String = "hello", at: Long = 10, conversation: String = "ab12", group: String = "ab12") =
        PersistedHeldSend(conversationHex = conversation, groupIdHex = group, heldAtMs = at, text = text)

    @Test
    fun `the queue round-trips in order with its quote and its forward`() {
        val rows = listOf(
            row("one", 1),
            row("two", 2).copy(reply = PersistedReply(target = 7, author = "A", excerpt = "quoted")),
            row("three", 3).copy(forward = PersistedForward(author = "B", originGroupIdHex = "cd", originMessageId = 4)),
        )
        assertEquals(rows, decodeHeldSends(encodeHeldSends(rows)))
    }

    @Test
    fun `a text that is not the schema is an empty queue`() {
        assertTrue(decodeHeldSends(null).isEmpty())
        assertTrue(decodeHeldSends("").isEmpty())
        assertTrue(decodeHeldSends("not json").isEmpty())
        assertTrue(decodeHeldSends("{\"a\":1}").isEmpty())
    }

    @Test
    fun `a row that cannot be sent again is left out and the rest keep their order`() {
        val rows = listOf(
            row("kept", 1),
            row("", 2),
            row("bad conversation", 3, conversation = "XYZ"),
            row("bad group", 4, group = ""),
            row("negative time", -1),
            row("also kept", 5),
        )
        assertEquals(listOf("kept", "also kept"), decodeHeldSends(encodeHeldSends(rows)).map { it.text })
        assertFalse(heldRowUsable(row("")))
        assertTrue(heldRowUsable(row("x")))
    }

    @Test
    fun `unknown fields are ignored`() {
        val raw = """[{"conversationHex":"aa","groupIdHex":"aa","heldAtMs":5,"text":"t","future":true}]"""
        assertEquals(listOf(row("t", 5, "aa", "aa")), decodeHeldSends(raw))
    }

    @Test
    fun `the stored queue is bounded and keeps the newest`() {
        val rows = (0 until MAX_STORED_HELD_SENDS + 3).map { row("m$it", it.toLong()) }
        val back = decodeHeldSends(encodeHeldSends(rows))
        assertEquals(MAX_STORED_HELD_SENDS, back.size)
        assertEquals("m3", back.first().text)
    }

    @Test
    fun `a restored message waits from now`() {
        val back = restoredHeldSends(listOf(row("a", 1), row("b", 2)), nowMs = 900)
        assertEquals(listOf(900L, 900L), back.map { it.heldAtMs })
        assertEquals(listOf("a", "b"), back.map { it.text })
    }

    @Test
    fun `the in-memory backend keeps each account's queue apart`() {
        val backend = InMemoryHeldSendBackend()
        assertTrue(backend.save("acct-1", listOf(row("x"))))
        assertEquals(listOf("x"), backend.load("acct-1").map { it.text })
        assertTrue(backend.load("acct-2").isEmpty())
        assertTrue(backend.save("acct-1", emptyList()))
        assertTrue(backend.load("acct-1").isEmpty())
    }

    @Test
    fun `a held reply keeps its quote and carries target zero into another segment`() {
        // Rule 7 and the web addendum: 0 is "no jump" on the wire.
        assertEquals(REPLY_NO_JUMP_TARGET, replyTargetForSend(7, "old", "new"))
        assertEquals(0L, REPLY_NO_JUMP_TARGET)
        assertEquals(7L, replyTargetForSend(7, "same", "same"))
    }
}
