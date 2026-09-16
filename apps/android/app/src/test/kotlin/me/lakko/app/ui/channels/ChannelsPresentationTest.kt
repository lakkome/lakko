// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.PostView
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/** docs/ux/client-redesign.md section 5.4 — the channel row and the status lines, all pure. */
class ChannelsPresentationTest {

    private val zone: ZoneId = ZoneId.of("Europe/Berlin")
    private val en: Locale = Locale.UK

    /** 2026-09-16 14:05 in Berlin (CEST, UTC+2). */
    private val now: Long = Instant.parse("2026-09-16T12:05:00Z").toEpochMilli()

    private fun at(iso: String): Long = Instant.parse(iso).toEpochMilli()

    private fun channel(name: String = "Lakko news", handle: String = "", addedAtMs: Long = at("2026-09-01T09:00:00Z")) =
        ChannelHandle(id = "00ff".repeat(8), localName = name, localDescription = "", addedAtMs = addedAtMs.toULong(), handle = handle)

    private fun post(cursor: Long, iso: String) =
        PostView(cursor = cursor.toULong(), serverTsMs = at(iso).toULong(), payloadB64 = "x")

    // --- row model -------------------------------------------------------------

    @Test
    fun `row carries name, handle, subscriber count and the newest post time`() {
        val posts = listOf(post(1, "2026-09-16T07:03:00Z"), post(2, "2026-09-16T09:30:00Z"))
        val row = channelRowModel(channel(handle = "news"), posts, 12, now, zone, use24h = true, locale = en)
        assertEquals("Lakko news", row.title)
        assertEquals("@news", row.handle)
        assertEquals("12 subscribers", row.subscriberLine)
        assertEquals("11:30", row.time)
        assertEquals(at("2026-09-16T09:30:00Z"), row.sortMs)
    }

    @Test
    fun `row without posts, handle or count leaves those slots empty and sorts by added time`() {
        val added = at("2026-09-10T10:00:00Z")
        val row = channelRowModel(channel(addedAtMs = added), null, null, now, zone, use24h = true, locale = en)
        assertNull(row.handle)
        assertNull(row.subscriberLine)
        assertNull(row.time)
        assertEquals(added, row.sortMs)
    }

    @Test
    fun `an unnamed channel is titled Channel, never its id`() {
        val row = channelRowModel(channel(name = "  "), null, null, now, zone, true, en)
        assertEquals("Channel", row.title)
    }

    @Test
    fun `handle label normalises a typed at sign and the singular count reads naturally`() {
        assertEquals("@news", channelHandleLabel("@news"))
        assertEquals("@news", channelHandleLabel(" news "))
        assertNull(channelHandleLabel(""))
        assertNull(channelHandleLabel("@"))
        assertEquals("1 subscriber", subscriberCountLine(1))
        assertEquals("0 subscribers", subscriberCountLine(0))
        assertNull(subscriberCountLine(null))
    }

    @Test
    fun `filter matches title or handle and orders newest first`() {
        val a = channelRowModel(channel(name = "Alpha", handle = "alpha"), listOf(post(1, "2026-09-15T10:00:00Z")), null, now, zone, true, en)
        val b = channelRowModel(channel(name = "Beta", handle = "daily"), listOf(post(1, "2026-09-16T10:00:00Z")), null, now, zone, true, en)
        assertEquals(listOf(b, a), filterChannelRows(listOf(a, b), ""))
        assertEquals(listOf(a), filterChannelRows(listOf(a, b), "ALP"))
        assertEquals(listOf(b), filterChannelRows(listOf(a, b), "@dai"))
        assertEquals(emptyList<ChannelRowModel>(), filterChannelRows(listOf(a, b), "zzz"))
    }

    // --- relative time -----------------------------------------------------------

    @Test
    fun `relative time steps from just now through minutes, hours and days to a date`() {
        assertEquals("just now", relativeTimeLabel(now - 30_000L, now, zone, en))
        assertEquals("just now", relativeTimeLabel(now + 60_000L, now, zone, en))
        assertEquals("5 min ago", relativeTimeLabel(now - 5 * 60_000L, now, zone, en))
        assertEquals("2 h ago", relativeTimeLabel(now - 2 * 3_600_000L, now, zone, en))
        assertEquals("3 d ago", relativeTimeLabel(now - 3 * 86_400_000L, now, zone, en))
        assertEquals("1 Sept 2026", relativeTimeLabel(now - 15 * 86_400_000L, now, zone, en))
    }

    @Test
    fun `rotation line names a time only for a rotation in this session`() {
        assertEquals("Keys rotate automatically.", rotationStatusLine(null, now, zone, en))
        assertEquals("Keys rotated 2 h ago.", rotationStatusLine(now - 2 * 3_600_000L, now, zone, en))
    }

    // --- post time ----------------------------------------------------------------

    @Test
    fun `post time is the clock today and the day plus clock otherwise`() {
        assertEquals("09:03", formatPostTime(at("2026-09-16T07:03:00Z"), now, zone, true, en))
        assertEquals("14 Sept, 09:03", formatPostTime(at("2026-09-14T07:03:00Z"), now, zone, true, en))
        assertEquals("14 Sept 2025, 09:03", formatPostTime(at("2025-09-14T07:03:00Z"), now, zone, true, en))
    }

    @Test
    fun `unreadable reason is one plain sentence with no protocol words`() {
        for (line in listOf(unreadableReasonLine(true), unreadableReasonLine(false))) {
            for (banned in listOf("epoch", "seed", "cursor", "key window", "hex")) {
                org.junit.Assert.assertFalse("$line contains $banned", line.contains(banned, ignoreCase = true))
            }
        }
    }
}
