// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.Direction
import me.lakko.app.chat.LocalUnread
import me.lakko.app.chat.LineageEntry
import me.lakko.app.chat.LocalUnreadCounter
import me.lakko.app.chat.TimelinePosition
import me.lakko.app.chat.buildLineage
import me.lakko.app.chat.combinedUnread
import me.lakko.app.chat.encodeReadPosition
import me.lakko.app.chat.foldConversations
import me.lakko.app.devices.sync.ReadCursorState
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The chat list's unread badge.
 *
 * ## The defect behind this
 *
 * The badge came only from the cross-device read cursor, and a chat with no
 * cursor counted every message as read: a message arriving for a chat that was
 * not open updated the preview line and showed no badge. The local counter
 * counts what this device's polls applied while the chat was not on screen.
 */
class UnreadCountTest {

    private val group = "cc".repeat(16)

    private fun thread(
        messages: List<ChatMessage>,
        localUnread: Int = 0,
        viewed: Boolean = false,
    ) = ChatThread(
        groupIdHex = group,
        messages = messages,
        cursor = 0,
        localUnread = localUnread,
        unreadViewedThisSession = viewed,
    )

    private fun incoming(id: Long) = ChatMessage(id, Direction.Incoming, "hi", createdAt = "t")

    private fun outgoing(id: Long) = ChatMessage(id, Direction.Outgoing, "yo", createdAt = "t")

    private fun cursorAt(id: Long): ReadCursorState {
        val bytes = ByteArray(8)
        var v = id
        for (i in 7 downTo 0) {
            bytes[i] = (v and 0xff).toByte()
            v = v ushr 8
        }
        return ReadCursorState(upToMessageId = bytes, readAtMs = 0uL, updatedAtHlc = 0uL)
    }

    @Test
    fun `messages applied while the chat is not on screen count, with no read cursor at all`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied(group, listOf(1, 2))
        counter.onIncomingApplied(group, listOf(3))
        val local = counter.snapshot(group)
        assertEquals(LocalUnread(count = 3, viewedThisSession = false), local)

        val t = thread(listOf(incoming(1), incoming(2), incoming(3)), local.count, local.viewedThisSession)
        assertEquals(3, unreadCount(t, cursor = null))
    }

    @Test
    fun `messages applied while the chat is on screen do not count, and showing it clears the count`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied(group, listOf(1, 2, 3, 4))
        counter.setOnScreen(group, true)
        assertEquals(0, counter.snapshot(group).count)
        counter.onIncomingApplied(group, listOf(5, 6))
        assertEquals(0, counter.snapshot(group).count)

        counter.setOnScreen(group, false)
        counter.onIncomingApplied(group, listOf(7))
        assertEquals(LocalUnread(count = 1, viewedThisSession = true), counter.snapshot(group))
    }

    @Test
    fun `counts are per chat`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied(group, listOf(1, 2))
        counter.setOnScreen("dd".repeat(16), true)
        assertEquals(2, counter.snapshot(group).count)
        counter.clear()
        assertEquals(0, counter.snapshot(group).count)
    }

    @Test
    fun `the cursor count still applies when this process has not shown the chat`() {
        // Five incoming rows, the cursor says two were read, and nothing arrived
        // since the process started: the cursor's three stand.
        val messages = (1L..5L).map { incoming(it) } + outgoing(6)
        assertEquals(3, unreadCount(thread(messages), cursorAt(2)))
        // Two of them arrived this session: still three, not five.
        assertEquals(3, unreadCount(thread(messages, localUnread = 2), cursorAt(2)))
    }

    @Test
    fun `a system row past the cursor is not unread`() {
        // Two commit rows the timeline does not draw put "2" on a chat whose
        // timeline showed nothing new (seen live 2026-11-22).
        val system = { id: Long -> ChatMessage(id, Direction.System, "[mls commit]", createdAt = "t") }
        assertEquals(0, unreadCount(thread(listOf(system(3), system(4))), cursorAt(2)))
        assertEquals(1, unreadCount(thread(listOf(system(3), incoming(4))), cursorAt(2)))
    }

    @Test
    fun `once shown here, a stale cursor does not resurrect read messages`() {
        // The DeviceGroup broadcast of this device's read cursor failed, so the
        // cursor still says three unread; this device showed the chat since.
        val messages = (1L..5L).map { incoming(it) }
        assertEquals(0, unreadCount(thread(messages, localUnread = 0, viewed = true), cursorAt(2)))
        // Another device read further than this one knows: the cursor caps it.
        assertEquals(1, unreadCount(thread(messages, localUnread = 3, viewed = true), cursorAt(4)))
    }

    @Test
    fun `combinedUnread table`() {
        assertEquals(0, combinedUnread(null, LocalUnread(0, false)))
        assertEquals(4, combinedUnread(null, LocalUnread(4, true)))
        assertEquals(6, combinedUnread(6, LocalUnread(4, false)))
        assertEquals(4, combinedUnread(6, LocalUnread(4, true)))
    }

    // ---- ADR-0073 R.5: the badge across a conversation's segments ---------

    private fun folded(): ChatThread {
        val lineage = me.lakko.app.chat.buildLineage(listOf(me.lakko.app.chat.LineageEntry("s1", "root", 1uL)))
        return me.lakko.app.chat.foldConversations(
            listOf(
                ChatThread(groupIdHex = "root", messages = listOf(incoming(1), incoming(2), incoming(3)), cursor = 0, membershipEnded = true),
                ChatThread(groupIdHex = "s1", messages = listOf(incoming(1), outgoing(2), incoming(3)), cursor = 0),
            ),
            lineage,
        ).single()
    }

    private fun cursorAt(segment: Int, id: Long) = ReadCursorState(
        upToMessageId = me.lakko.app.chat.encodeReadPosition(me.lakko.app.chat.TimelinePosition(segment, id)),
        readAtMs = 0uL,
        updatedAtHlc = 0uL,
    )

    @Test
    fun `unread rows of an older segment count toward the badge`() {
        // Read up to message 1 of the old group: its messages 2 and 3, and
        // the two incoming rows of the successor.
        assertEquals(4, unreadCount(folded(), cursorAt(0, 1)))
        // The 8-byte form every earlier build wrote reads the same.
        assertEquals(4, unreadCount(folded(), cursorAt(1)))
    }

    @Test
    fun `a position in the successor reads the whole older segment`() {
        assertEquals(1, unreadCount(folded(), cursorAt(1, 1)))
        assertEquals(0, unreadCount(folded(), cursorAt(1, 3)))
    }

    @Test
    fun `a position that only reached the old group reads nothing of the successor`() {
        // Message ids restart in a successor: "read up to 3" in the old group
        // must not read the successor's messages 1 to 3.
        assertEquals(2, unreadCount(folded(), cursorAt(0, 3)))
    }

    @Test
    fun `an undecodable cursor counts nothing from the cursor`() {
        val broken = ReadCursorState(upToMessageId = ByteArray(3), readAtMs = 0uL, updatedAtHlc = 0uL)
        assertEquals(0, unreadCount(folded(), broken))
    }

    // ----- "History is not news" (docs/ux/second-device.md section 2) ------

    private fun withFloor(messages: List<ChatMessage>, floor: Long, localUnread: Int = 0) =
        thread(messages, localUnread).copy(syncedThrough = mapOf(group to floor))

    @Test
    fun `a message of the history bundle is not unread with no read cursor`() {
        val messages = (1L..5L).map { incoming(it) }
        assertEquals(0, unreadCount(withFloor(messages, floor = 5), cursor = null))
    }

    @Test
    fun `a read cursor behind the floor does not bring the history back, and later messages count`() {
        val messages = (1L..7L).map { incoming(it) }
        // A cursor that arrives after the bundle, from a device that read two.
        assertEquals(2, unreadCount(withFloor(messages, floor = 5), cursorAt(2)))
        // The same with no cursor at all: the two came through this device's polls.
        assertEquals(2, unreadCount(withFloor(messages, floor = 5, localUnread = 2), cursor = null))
        // A cursor past the floor says what it always said.
        assertEquals(1, unreadCount(withFloor(messages, floor = 5), cursorAt(6)))
    }

    @Test
    fun `a polled message at or below the floor is not counted, one above it is`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied(group, listOf(4, 5, 6), syncedThrough = 5)
        assertEquals(1, counter.snapshot(group).count)
    }

    @Test
    fun `messages counted before the bundle came stop counting when it carries them`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied(group, listOf(8, 9, 12))
        counter.onSyncedThrough(group, floor = 9)
        assertEquals(1, counter.snapshot(group).count)
        // Polled again after a held batch: still one row each.
        counter.onIncomingApplied(group, listOf(12))
        assertEquals(1, counter.snapshot(group).count)
    }

    @Test
    fun `the feed takes the rows it showed out of the local count by id`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied(group, listOf(1, 2, 3))
        counter.onSeenElsewhere(group, listOf(2, 99))
        assertEquals(2, counter.snapshot(group).count)
    }

    @Test
    fun `each segment of a conversation has its own floor, since ids restart in every group`() {
        val older = "aa".repeat(16)
        val head = "bb".repeat(16)
        val lineage = buildLineage(listOf(LineageEntry(older, "root", 10UL), LineageEntry(head, "root", 20UL)))
        val threads = listOf(
            ChatThread(
                groupIdHex = older,
                messages = listOf(incoming(1), incoming(2), incoming(3)),
                cursor = 0,
                syncedThrough = mapOf(older to 2L),
            ),
            ChatThread(groupIdHex = head, messages = listOf(incoming(1), incoming(2)), cursor = 0),
        )
        val folded = foldConversations(threads, lineage).single()

        // Position at the very start: every row is past it. The older segment's
        // floor takes its first two rows, and the head's rows of the same ids stay.
        val nothingRead = encodeReadPosition(TimelinePosition(0, 0))
        val cursor = ReadCursorState(upToMessageId = nothingRead, readAtMs = 0uL, updatedAtHlc = 0uL)
        assertEquals(3, unreadCount(folded, cursor))
    }
}
