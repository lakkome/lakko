// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.PostView
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * docs/ux/client-redesign.md section 5.4 + docs/ux/group-and-channel-names.md
 * section 3.2 — the channel row, the reader header and the status lines, all
 * pure.
 */
class ChannelsPresentationTest {

    private val zone: ZoneId = ZoneId.of("Europe/Berlin")
    private val en: Locale = Locale.UK
    private val strings = EnglishForTests.strings

    /** 2026-09-16 14:05 in Berlin (CEST, UTC+2). */
    private val now: Long = Instant.parse("2026-09-16T12:05:00Z").toEpochMilli()

    private fun at(iso: String): Long = Instant.parse(iso).toEpochMilli()

    private fun channel(
        name: String = "Lakko news",
        handle: String = "",
        addedAtMs: Long = at("2026-09-01T09:00:00Z"),
        description: String = "",
        sharedName: String = "",
        sharedDescription: String = "",
        sharedStamp: ULong? = null,
    ) = ChannelHandle(
        id = "00ff".repeat(8),
        localName = name,
        localDescription = description,
        addedAtMs = addedAtMs.toULong(),
        handle = handle,
        sharedName = sharedName,
        sharedDescription = sharedDescription,
        sharedUpdatedAtMs = sharedStamp,
    )

    private fun post(cursor: Long, iso: String) =
        PostView(cursor = cursor.toULong(), serverTsMs = at(iso).toULong(), payloadB64 = "x")

    private fun row(
        channel: ChannelHandle,
        posts: List<PostView>? = null,
        count: Int? = null,
        role: ChannelViewerRole = ChannelViewerRole.SUBSCRIBER,
    ) = channelRowModel(channel, posts, count, role, now, zone, use24h = true, locale = en, strings = strings)

    // --- row model -------------------------------------------------------------

    @Test
    fun `row carries name, handle plus subscriber count and the newest post time`() {
        val posts = listOf(post(1, "2026-09-16T07:03:00Z"), post(2, "2026-09-16T09:30:00Z"))
        val r = row(channel(handle = "news"), posts, count = 12, role = ChannelViewerRole.OWNER)
        assertEquals("Lakko news", r.title)
        assertEquals("@news", r.handle)
        assertEquals("@news · 12 subscribers", r.subtitle)
        assertEquals("11:30", r.time)
        assertEquals(at("2026-09-16T09:30:00Z"), r.sortMs)
    }

    @Test
    fun `row without posts, handle or count falls back to the relation line and sorts by added time`() {
        val added = at("2026-09-10T10:00:00Z")
        assertEquals("Subscribed", row(channel(addedAtMs = added)).subtitle)
        assertEquals("Your channel", row(channel(addedAtMs = added), role = ChannelViewerRole.OWNER).subtitle)
        assertEquals("Publisher", row(channel(addedAtMs = added), role = ChannelViewerRole.PUBLISHER).subtitle)
        val r = row(channel(addedAtMs = added))
        assertNull(r.handle)
        assertNull(r.time)
        assertEquals(added, r.sortMs)
    }

    @Test
    fun `title precedence is shared name, then local name, then handle, then Channel`() {
        assertEquals("Lakko News", channelTitle(channel(name = "local", handle = "h", sharedName = "Lakko News", sharedStamp = 1uL), strings))
        assertEquals("local", channelTitle(channel(name = "local", handle = "h"), strings))
        assertEquals("lakkonews", channelTitle(channel(name = "  ", handle = "@lakkonews"), strings))
        assertEquals("Channel", channelTitle(channel(name = "  "), strings))
    }

    @Test
    fun `a channel titled by its handle never shows the handle twice`() {
        val byHandle = channel(name = "", handle = "lakkonews")
        assertEquals("lakkonews", row(byHandle).title)
        assertEquals("Subscribed", row(byHandle).subtitle)
        assertEquals("3 subscribers", row(byHandle, count = 3, role = ChannelViewerRole.OWNER).subtitle)
        assertNull(channelHeaderModel(byHandle, role = ChannelViewerRole.SUBSCRIBER, strings).handleLine)
        // With a name the handle is informative and shown once, in the subtitle.
        val named = channel(name = "", handle = "lakkonews", sharedName = "Lakko News", sharedStamp = 1uL)
        assertEquals("@lakkonews", row(named).subtitle)
        assertEquals("@lakkonews", channelHeaderModel(named, role = ChannelViewerRole.SUBSCRIBER, strings).handleLine)
    }

    @Test
    fun `header carries the shared description over the local one and the relation line`() {
        val h = channelHeaderModel(
            channel(name = "local", description = "old", sharedName = "Lakko News", sharedDescription = "Daily notes", sharedStamp = 5uL),
            role = ChannelViewerRole.OWNER,
            strings,
        )
        assertEquals("Lakko News", h.title)
        assertEquals("Daily notes", h.description)
        assertEquals("Your channel", h.statusLine.en)
        assertNull(channelHeaderModel(channel(description = "  "), role = ChannelViewerRole.SUBSCRIBER, strings).description)
        assertEquals("Subscribed", channelHeaderModel(channel(), role = ChannelViewerRole.SUBSCRIBER, strings).statusLine.en)
    }

    @Test
    fun `handle label normalises a typed at sign and the singular count reads naturally`() {
        assertEquals("@news", channelHandleLabel("@news"))
        assertEquals("@news", channelHandleLabel(" news "))
        assertNull(channelHandleLabel(""))
        assertNull(channelHandleLabel("@"))
        assertEquals("1 subscriber", subscriberCountLine(1)?.en)
        assertEquals("0 subscribers", subscriberCountLine(0)?.en)
        assertNull(subscriberCountLine(null))
    }

    @Test
    fun `filter matches title or handle and orders newest first`() {
        val a = row(channel(name = "Alpha", handle = "alpha"), listOf(post(1, "2026-09-15T10:00:00Z")))
        val b = row(channel(name = "Beta", handle = "daily"), listOf(post(1, "2026-09-16T10:00:00Z")))
        assertEquals(listOf(b, a), filterChannelRows(listOf(a, b), ""))
        assertEquals(listOf(a), filterChannelRows(listOf(a, b), "ALP"))
        assertEquals(listOf(b), filterChannelRows(listOf(a, b), "@dai"))
        assertEquals(emptyList<ChannelRowModel>(), filterChannelRows(listOf(a, b), "zzz"))
    }

    // --- relative time -----------------------------------------------------------

    @Test
    fun `relative time steps from just now through minutes, hours and days to a date`() {
        assertEquals("just now", relativeTimeLabel(now - 30_000L, now, zone, en, strings))
        assertEquals("just now", relativeTimeLabel(now + 60_000L, now, zone, en, strings))
        assertEquals("5 minutes ago", relativeTimeLabel(now - 5 * 60_000L, now, zone, en, strings))
        assertEquals("2 hours ago", relativeTimeLabel(now - 2 * 3_600_000L, now, zone, en, strings))
        assertEquals("3 days ago", relativeTimeLabel(now - 3 * 86_400_000L, now, zone, en, strings))
        assertEquals("1 Sept 2026", relativeTimeLabel(now - 15 * 86_400_000L, now, zone, en, strings))
    }

    @Test
    fun `rotation line names a time only for a rotation in this session`() {
        assertEquals("Not refreshed yet.", rotationStatusLine(null, now, zone, en, strings))
        assertEquals("Refreshed 2 hours ago.", rotationStatusLine(now - 2 * 3_600_000L, now, zone, en, strings))
    }

    // --- post time ----------------------------------------------------------------

    @Test
    fun `post time is relative while recent, then the date`() {
        assertEquals("just now", postRelativeTimeLabel(now - 20_000L, now, zone, en, strings))
        assertEquals("just now", postRelativeTimeLabel(now + 5_000L, now, zone, en, strings))
        assertEquals("5 minutes ago", postRelativeTimeLabel(now - 5 * 60_000L, now, zone, en, strings))
        assertEquals("2 hours ago", postRelativeTimeLabel(now - 2 * 3_600_000L, now, zone, en, strings))
        assertEquals("14 Sept", postRelativeTimeLabel(at("2026-09-14T07:03:00Z"), now, zone, en, strings))
        assertEquals("14 Sept 2025", postRelativeTimeLabel(at("2025-09-14T07:03:00Z"), now, zone, en, strings))
    }

    @Test
    fun `post time never shows a clock with a meridiem`() {
        val label = postRelativeTimeLabel(at("2026-09-16T07:03:00Z"), now, zone, Locale.US, strings)
        assertFalse(label, label.contains("AM") || label.contains("PM"))
        assertTrue(label, label.endsWith("ago") || label == "just now")
    }

    @Test
    fun `unreadable reason is one plain sentence with no protocol words`() {
        for (line in listOf(unreadableReasonLine(true).en, unreadableReasonLine(false).en)) {
            for (banned in listOf("epoch", "seed", "cursor", "key window", "hex")) {
                assertFalse("$line contains $banned", line.contains(banned, ignoreCase = true))
            }
        }
    }

    @Test
    fun `following needs both the list row and a reader key`() {
        assertTrue(channelIsFollowed(listed = true, readerKeyHeld = true))
        assertFalse(channelIsFollowed(listed = true, readerKeyHeld = false))
        assertFalse(channelIsFollowed(listed = false, readerKeyHeld = true))
        assertFalse(channelIsFollowed(listed = false, readerKeyHeld = false))
    }

    @Test
    fun `opening a channel reads it only for a follower or after a press`() {
        assertFalse(channelPageReads(followed = false, pressed = false))
        assertTrue(channelPageReads(followed = false, pressed = true))
        assertTrue(channelPageReads(followed = true, pressed = false))
    }

    @Test
    fun `publisher keys alone do not make the owner`() {
        assertEquals(ChannelViewerRole.OWNER, channelViewerRole(holdsPublisherKeys = true, joinedAsCoPublisher = false))
        assertEquals(ChannelViewerRole.PUBLISHER, channelViewerRole(holdsPublisherKeys = true, joinedAsCoPublisher = true))
        assertEquals(ChannelViewerRole.SUBSCRIBER, channelViewerRole(holdsPublisherKeys = false, joinedAsCoPublisher = false))
        // A stale co-publisher mark without keys (stepped down, removed) is a subscriber.
        assertEquals(ChannelViewerRole.SUBSCRIBER, channelViewerRole(holdsPublisherKeys = false, joinedAsCoPublisher = true))
        assertEquals("Publisher", channelStatusLine(ChannelViewerRole.PUBLISHER).renderWith(strings))
    }
}
