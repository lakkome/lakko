// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.chat.ConversationKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/navigation.md sections 5.1 and 6.2: every call this device was in is
 * one row of Calls, and the index's list and its filed call row keys agree.
 *
 * Found on the Android emulator 2026-11-24: a group call the device took part
 * in (its conversation shows "Incoming call, 2:34") was missing from Calls. The
 * stored list was empty while the filed set still held that call's key: the
 * group had been wiped and joined again (ADR-0087 section 9), the wipe took the
 * list entry and left the key, and every later filing of the row returned to
 * the conversation was skipped as a repeat.
 */
class CallIndexStateTest {
    private val walk = "ab".repeat(32)
    private val other = "cd".repeat(32)

    /** The group call as the live path files it: under its start time. */
    private val live = CallHistoryEntry(
        conversationIdHex = walk,
        kind = ConversationKind.Group,
        outcome = CallOutcome.INCOMING,
        video = true,
        startedAtMs = 1_000_000L,
        durationSecs = 154L,
    )

    /** The same call as its transcript row reads: at its end. */
    private val transcript = live.copy(startedAtMs = 1_160_000L)

    private val rowId = callRowMessageId("0123456789abcdef0123456789abcdef")!!
    private val rowKey = callRowFiledKey(walk, rowId)

    private fun empty() = CallIndexState(emptyList(), LinkedHashSet())

    /** What the live record leaves: the entry, and its row key marked filed. */
    private fun afterLiveRecord() = CallIndexState(
        callHistoryWith(emptyList(), live),
        linkedSetOf(rowKey),
    )

    @Test
    fun `a wiped conversation takes its filed keys with its calls`() {
        val withOther = CallIndexState(
            afterLiveRecord().entries + live.copy(conversationIdHex = other),
            linkedSetOf(rowKey, callRowFiledKey(other, 7L)),
        )
        val next = callIndexWithoutConversation(withOther, walk)
        assertEquals(listOf(other), next.entries.map { it.conversationIdHex })
        assertEquals(linkedSetOf(callRowFiledKey(other, 7L)), next.filed)
    }

    @Test
    fun `a call row that comes back after a wipe is listed again`() {
        val wiped = callIndexWithoutConversation(afterLiveRecord(), walk)
        val refiled = callIndexFiled(wiped, listOf(rowKey to transcript))
        assertEquals(listOf(transcript), refiled.entries)
        assertTrue(rowKey in refiled.filed)
    }

    @Test
    fun `a filed row is not filed twice`() {
        val held = afterLiveRecord()
        val again = callIndexFiled(held, listOf(rowKey to transcript))
        assertEquals(held, again)
    }

    @Test
    fun `the past fill never lists a live call a second time`() {
        // A snapshot taken after the live row landed still holds that row; its
        // key is filed, so the call stays one row, under its start time.
        val next = callIndexFiled(afterLiveRecord(), listOf(rowKey to transcript))
        assertEquals(listOf(live), next.entries)
    }

    @Test
    fun `the repair lists the call an older build emptied out`() {
        // The emulator's stored state: an empty list and one filed key.
        val damaged = CallIndexState(emptyList(), linkedSetOf(rowKey))
        val repaired = callIndexRepaired(damaged, listOf(rowKey to transcript))
        assertEquals(listOf(transcript), repaired.entries)
        assertEquals(linkedSetOf(rowKey), repaired.filed)
    }

    @Test
    fun `the repair leaves a conversation that is listed alone`() {
        val held = afterLiveRecord()
        val older = transcript.copy(startedAtMs = 500L)
        val olderKey = callRowFiledKey(walk, 3L)
        // Even an unfiled row of a listed conversation stays out: the repair
        // is only for what a wipe took, never a second fill.
        assertSame(held, callIndexRepaired(held, listOf(rowKey to transcript, olderKey to older)))
    }

    @Test
    fun `the repair files every lost conversation once and keeps the rest`() {
        val otherEntry = live.copy(conversationIdHex = other, startedAtMs = 2_000_000L)
        val otherKey = callRowFiledKey(other, 9L)
        val first = transcript
        val secondKey = callRowFiledKey(walk, 11L)
        val second = transcript.copy(startedAtMs = 3_000_000L, outcome = CallOutcome.MISSED, durationSecs = null)
        val damaged = CallIndexState(listOf(otherEntry), linkedSetOf(otherKey, rowKey, secondKey))
        val repaired = callIndexRepaired(
            damaged,
            listOf(otherKey to otherEntry, rowKey to first, secondKey to second),
        )
        assertEquals(listOf(second, otherEntry, first), repaired.entries)
        // Running it again changes nothing.
        assertEquals(repaired, callIndexRepaired(repaired, listOf(otherKey to otherEntry, rowKey to first, secondKey to second)))
    }

    @Test
    fun `nothing held repairs nothing`() {
        assertEquals(empty(), callIndexRepaired(empty(), emptyList()))
    }

    @Test
    fun `a call with no conversation survives a conversation wipe`() {
        val link = noConversationCallEntry("aa".repeat(16), outgoing = false, video = true, startedAtMs = 5L, durationSecs = 60L)
        val held = CallIndexState(listOf(live, link), linkedSetOf(rowKey))
        val next = callIndexWithoutConversation(held, walk)
        assertEquals(listOf(link), next.entries)
        assertTrue(next.filed.isEmpty())
    }

    @Test
    fun `a key of another conversation sharing a prefix stays filed`() {
        // Keys are "<hex>:<id>": a prefix of the hex alone must not match.
        val longer = walk + "00"
        val key = callRowFiledKey(longer, 1L)
        val next = callIndexWithoutConversation(CallIndexState(emptyList(), linkedSetOf(key)), walk)
        assertEquals(linkedSetOf(key), next.filed)
    }
}
