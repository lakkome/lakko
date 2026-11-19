// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * docs/ux/client-redesign.md sections 5.1 and 5.2 — the row time, the bubble
 * time, the date separators, the preview line and the run grouping, all pure.
 */
class TimelinePresentationTest {
    init {
        EnglishForTests.install()
    }


    private val zone: ZoneId = ZoneId.of("Europe/Berlin")
    private val en: Locale = Locale.UK

    /** 2026-09-16 14:05 in Berlin (CEST, UTC+2). */
    private val now: Long = Instant.parse("2026-09-16T12:05:00Z").toEpochMilli()

    private fun at(iso: String): Long = Instant.parse(iso).toEpochMilli()

    private fun msg(
        id: Long,
        direction: Direction = Direction.Incoming,
        author: String? = "anna",
        createdAt: String = "2026-09-16T12:00:00Z",
        text: String = "hi",
    ) = ChatMessage(
        messageId = id,
        direction = direction,
        text = text,
        createdAt = createdAt,
        authorKey = author,
    )

    // --- created_at parsing -------------------------------------------------

    @Test
    fun `parses an instant and an offset timestamp, refuses garbage`() {
        assertEquals(at("2026-09-16T12:00:00Z"), parseMessageTimeMs("2026-09-16T12:00:00Z"))
        assertEquals(at("2026-09-16T12:00:00Z"), parseMessageTimeMs("2026-09-16T14:00:00+02:00"))
        assertNull(parseMessageTimeMs("yesterday"))
        assertNull(parseMessageTimeMs(""))
        assertNull(parseMessageTimeMs(null))
    }

    // --- row time -----------------------------------------------------------

    @Test
    fun `today shows the clock time in the device format`() {
        val t = at("2026-09-16T07:03:00Z") // 09:03 Berlin
        assertEquals("09:03", formatListTime(t, now, zone, use24h = true, locale = en))
        assertEquals("9:03 am", formatListTime(t, now, zone, use24h = false, locale = en).lowercase())
    }

    @Test
    fun `this week shows the short weekday`() {
        val monday = at("2026-09-14T10:00:00Z")
        assertEquals("Mon", formatListTime(monday, now, zone, use24h = true, locale = en))
        val sixDaysAgo = at("2026-09-10T10:00:00Z")
        assertEquals("Thu", formatListTime(sixDaysAgo, now, zone, use24h = true, locale = en))
    }

    @Test
    fun `older in the same year shows day and short month`() {
        // Seven days back is already past the weekday band.
        val sevenDaysAgo = at("2026-09-09T10:00:00Z")
        val sevenDaysLabel = formatListTime(sevenDaysAgo, now, zone, use24h = true, locale = en)
        assertTrue(sevenDaysLabel, sevenDaysLabel.startsWith("9 Sep"))
        // "Aug" is spelled the same in every CLDR version (September is
        // "Sep" or "Sept" depending on the JDK's CLDR).
        val august = at("2026-08-09T10:00:00Z")
        assertEquals("9 Aug", formatListTime(august, now, zone, use24h = true, locale = en))
    }

    @Test
    fun `another year shows the locale short numeric date`() {
        val lastYear = at("2025-09-16T10:00:00Z")
        // The order and separators of the DEVICE's region (localization.md 5).
        assertEquals("16/09/2025", formatListTime(lastYear, now, zone, use24h = true, locale = en, region = Locale.UK))
        assertEquals("16.09.25", formatListTime(lastYear, now, zone, use24h = true, locale = en, region = Locale.GERMANY))
    }

    @Test
    fun `a message just before midnight and one just after fall on different days`() {
        val before = at("2026-09-15T21:59:00Z") // 23:59 Berlin, yesterday
        val after = at("2026-09-15T22:01:00Z") // 00:01 Berlin, today
        assertEquals("Tue", formatListTime(before, now, zone, use24h = true, locale = en))
        assertEquals("00:01", formatListTime(after, now, zone, use24h = true, locale = en))
    }

    // --- date separators ----------------------------------------------------

    @Test
    fun `separator labels`() {
        assertEquals("Today", dateSeparatorLabel(at("2026-09-16T05:00:00Z"), now, zone, en))
        assertEquals("Yesterday", dateSeparatorLabel(at("2026-09-15T05:00:00Z"), now, zone, en))
        assertEquals("Saturday", dateSeparatorLabel(at("2026-09-12T05:00:00Z"), now, zone, en))
        assertEquals("1 September", dateSeparatorLabel(at("2026-09-01T05:00:00Z"), now, zone, en))
        assertEquals("16 September 2025", dateSeparatorLabel(at("2025-09-16T05:00:00Z"), now, zone, en))
    }

    // --- preview line -------------------------------------------------------

    @Test
    fun `media words follow the kind, then the mime`() {
        assertEquals("Voice message", mediaPreviewLabel(PreviewAttachment("voice", "audio/mp4")))
        assertEquals("Video message", mediaPreviewLabel(PreviewAttachment("video_note", "video/mp4")))
        assertEquals("GIF", mediaPreviewLabel(PreviewAttachment("gif", "image/gif")))
        assertEquals("Photo", mediaPreviewLabel(PreviewAttachment(null, "image/JPEG")))
        assertEquals("Video", mediaPreviewLabel(PreviewAttachment(null, "video/mp4")))
        assertEquals("File", mediaPreviewLabel(PreviewAttachment(null, "application/pdf")))
        assertEquals("File", mediaPreviewLabel(PreviewAttachment(null, null)))
    }

    @Test
    fun `preview prefers text, then media, and carries the prefix`() {
        assertEquals("You: hello", previewLine("hello", emptyList(), deleted = false, author = PreviewAuthor.Own))
        assertEquals(
            "Anna: Photo",
            previewLine("", listOf(PreviewAttachment(null, "image/png")), deleted = false, author = PreviewAuthor.Named("Anna")),
        )
        // media-messages section 11 — photos and videos are counted; a file
        // beside them adds no words.
        assertEquals(
            "2 photos",
            previewLine(
                "",
                listOf(PreviewAttachment(null, "image/png"), PreviewAttachment(null, "image/png"), PreviewAttachment(null, null)),
                deleted = false,
                author = null,
            ),
        )
        // A voice message keeps its own word.
        assertEquals(
            "Voice message and 1 more",
            previewLine(
                "",
                listOf(PreviewAttachment("voice", "audio/mp4"), PreviewAttachment(null, "image/png")),
                deleted = false,
                author = null,
            ),
        )
        assertEquals("Message deleted", previewLine("gone", emptyList(), deleted = true, author = null))
        assertEquals("", previewLine("", emptyList(), deleted = false, author = PreviewAuthor.Own))
    }

    @Test
    fun `multi-line text collapses to one line`() {
        assertEquals("a b c", previewLine("a\n b\n\n  c ", emptyList(), deleted = false, author = null))
    }

    @Test
    fun `prefix rule`() {
        assertEquals(PreviewAuthor.Own, previewPrefix(Direction.Outgoing, isGroup = true, senderName = "Anna"))
        assertEquals(PreviewAuthor.Own, previewPrefix(Direction.Outgoing, isGroup = false, senderName = null))
        assertEquals(PreviewAuthor.Named("Anna"), previewPrefix(Direction.Incoming, isGroup = true, senderName = "Anna"))
        assertEquals(null, previewPrefix(Direction.Incoming, isGroup = true, senderName = null))
        assertEquals(null, previewPrefix(Direction.Incoming, isGroup = false, senderName = "Anna"))
        assertEquals(null, previewPrefix(Direction.System, isGroup = true, senderName = "Anna"))
    }

    // --- run grouping -------------------------------------------------------

    @Test
    fun `same author within five minutes continues the run`() {
        val a = msg(1, createdAt = "2026-09-16T12:00:00Z")
        val b = msg(2, createdAt = "2026-09-16T12:04:59Z")
        assertTrue(continuesRun(a, parseMessageTimeMs(a.createdAt), b, parseMessageTimeMs(b.createdAt), zone))
    }

    @Test
    fun `five minutes and one second later starts a new run`() {
        val a = msg(1, createdAt = "2026-09-16T12:00:00Z")
        val b = msg(2, createdAt = "2026-09-16T12:05:01Z")
        assertFalse(continuesRun(a, parseMessageTimeMs(a.createdAt), b, parseMessageTimeMs(b.createdAt), zone))
    }

    @Test
    fun `another author, another direction or a system row breaks the run`() {
        val a = msg(1)
        val other = msg(2, author = "ivan")
        val own = msg(3, direction = Direction.Outgoing, author = "me")
        val sys = msg(4, direction = Direction.System, author = null, text = "joined")
        val t = parseMessageTimeMs(a.createdAt)
        assertFalse(continuesRun(a, t, other, t, zone))
        assertFalse(continuesRun(a, t, own, t, zone))
        assertFalse(continuesRun(a, t, sys, t, zone))
        assertFalse(continuesRun(sys, t, a, t, zone))
    }

    @Test
    fun `no previous message never continues`() {
        assertFalse(continuesRun(null, null, msg(1), 0L, zone))
    }

    @Test
    fun `rows without a time group by author but not with a timed row`() {
        val a = msg(1, createdAt = "")
        val b = msg(2, createdAt = "")
        val c = msg(3)
        assertTrue(continuesRun(a, null, b, null, zone))
        assertFalse(continuesRun(b, null, c, parseMessageTimeMs(c.createdAt), zone))
    }

    @Test
    fun `timeline inserts one separator per day and marks run edges`() {
        val messages = listOf(
            msg(1, createdAt = "2026-09-15T10:00:00Z"),
            msg(2, createdAt = "2026-09-15T10:01:00Z"),
            msg(3, direction = Direction.Outgoing, author = "me", createdAt = "2026-09-15T10:02:00Z"),
            msg(4, createdAt = "2026-09-16T08:00:00Z"),
        )
        val items = buildTimeline(messages, now, zone, use24h = true, locale = en)
        val labels = items.map {
            when (it) {
                is TimelineItem.DateSeparator -> "sep:${it.label}"
                is TimelineItem.Row -> "m${it.message.messageId}:${if (it.firstInRun) "F" else "-"}${if (it.lastInRun) "L" else "-"}@${it.time}"
            }
        }
        assertEquals(
            listOf(
                "sep:Yesterday",
                "m1:F-@12:00",
                "m2:-L@12:01",
                "m3:FL@12:02",
                "sep:Today",
                "m4:FL@10:00",
            ),
            labels,
        )
    }

    @Test
    fun `a row without a usable time carries no time and opens no separator`() {
        val items = buildTimeline(listOf(msg(1, createdAt = "")), now, zone, use24h = true, locale = en)
        assertEquals(1, items.size)
        val row = items.single() as TimelineItem.Row
        assertNull(row.time)
    }

    @Test
    fun `timeline keys are unique across separators and rows`() {
        val messages = listOf(
            msg(1, createdAt = "2026-09-15T10:00:00Z"),
            msg(2, createdAt = "2026-09-16T10:00:00Z"),
            msg(3, createdAt = "2026-09-16T11:00:00Z"),
        )
        val keys = buildTimeline(messages, now, zone, use24h = true, locale = en).map { it.key }
        assertEquals(keys.size, keys.toSet().size)
    }

    // --- hidden system rows -------------------------------------------------

    @Test
    fun `a commit row is not drawn and does not break the run around it`() {
        val messages = listOf(
            msg(1, createdAt = "2026-09-16T10:00:00Z"),
            msg(2, direction = Direction.System, author = null, createdAt = "2026-09-16T10:00:30Z", text = "[mls commit]"),
            msg(3, createdAt = "2026-09-16T10:01:00Z"),
            msg(4, direction = Direction.System, author = null, createdAt = "2026-09-16T10:01:30Z", text = "[mls external join]"),
        )
        val rows = buildTimeline(messages, now, zone, use24h = true, locale = en)
            .filterIsInstance<TimelineItem.Row>()
        assertEquals(listOf(1L, 3L), rows.map { it.message.messageId })
        // One run of two: the hidden row between them never opened a boundary.
        assertTrue(rows[0].firstInRun)
        assertFalse(rows[0].lastInRun)
        assertFalse(rows[1].firstInRun)
        assertTrue(rows[1].lastInRun)
    }

    @Test
    fun `a system row that is not bookkeeping is still drawn`() {
        val custom = msg(2, direction = Direction.System, author = null, text = "Alice joined")
        assertFalse(isHiddenSystemRow(custom))
        assertTrue(isHiddenSystemRow(msg(3, direction = Direction.System, author = null, text = "[mls commit]")))
        // The marker text on a non-system row is a message someone typed.
        assertFalse(isHiddenSystemRow(msg(4, text = "[mls commit]")))
    }

    // --- one-to-one: no "Members changed", ever (2026-10-29) ---------------

    /** A conversation as its stored log seeds it: messages with commit rows
     *  written between them by an older build. */
    private fun storedLogWithCommits(): List<ChatMessage> = listOf(
        msg(1, createdAt = "2026-10-29T08:00:00Z"),
        msg(2, direction = Direction.System, author = null, createdAt = "2026-10-29T08:01:00Z", text = "[mls commit]"),
        msg(3, direction = Direction.System, author = null, createdAt = "2026-10-29T08:02:00Z", text = "[mls commit]"),
        msg(4, direction = Direction.Outgoing, author = null, createdAt = "2026-10-29T08:03:00Z", text = "yes"),
        msg(5, direction = Direction.System, author = null, createdAt = "2026-10-29T08:04:00Z", text = "[mls commit]"),
    )

    @Test
    fun `a one-to-one's stored commit rows are not in the displayed timeline and stay in the log`() {
        val stored = storedLogWithCommits()
        val rows = buildTimeline(stored, now, zone, use24h = true, locale = en)
            .filterIsInstance<TimelineItem.Row>()
        assertEquals(listOf(1L, 4L), rows.map { it.message.messageId })
        assertTrue(rows.none { it.message.direction == Direction.System })
        // Filtered at display time: the log itself is untouched.
        assertEquals(5, stored.size)
    }

    @Test
    fun `a one-to-one whose last row is a commit previews the message before it`() {
        val stored = storedLogWithCommits()
        assertEquals(4L, previewSourceRow(stored)?.messageId)
        // Only commit rows: no preview row at all ("No messages yet").
        assertNull(previewSourceRow(stored.filter { it.direction == Direction.System }))
    }

    @Test
    fun `a group's stored commit rows without person keys are hidden too`() {
        // UX second-device.md section 4: a commit row is drawn in a group only
        // when it names the people its batch changed; these stored rows name
        // none (`MembershipRowsTest` covers the ones that do).
        val rows = buildTimeline(storedLogWithCommits(), now, zone, use24h = true, locale = en, isGroup = true)
            .filterIsInstance<TimelineItem.Row>()
        assertEquals(listOf(1L, 4L), rows.map { it.message.messageId })
    }

    @Test
    fun `section 11 words for photos, videos and files`() {
        val photo = PreviewAttachment(null, "image/jpeg")
        val video = PreviewAttachment(null, "video/mp4")
        val file = PreviewAttachment(null, "application/pdf")
        assertEquals("Photo", mediaCountWords(listOf(photo)))
        assertEquals("3 photos", mediaCountWords(List(3) { photo }))
        assertEquals("Video", mediaCountWords(listOf(video)))
        assertEquals("2 videos", mediaCountWords(listOf(video, video, file)))
        assertEquals("4 photos and videos", mediaCountWords(listOf(photo, video, photo, video)))
        assertEquals("File", mediaCountWords(listOf(file)))
        assertEquals("3 files", mediaCountWords(List(3) { file }))
        assertNull(mediaCountWords(emptyList()))
        assertNull(mediaCountWords(listOf(photo, PreviewAttachment("gif", "image/gif"))))
    }

    @Test
    fun `section 11 line - the text with photos, the words then the text with files only`() {
        val photo = PreviewAttachment(null, "image/jpeg")
        val file = PreviewAttachment(null, "application/pdf")
        assertEquals("Look at this", messagePreviewBody(" Look\n at  this ", listOf(photo, photo)))
        assertEquals("2 files · Here they are", messagePreviewBody("Here they are", listOf(file, file)))
        assertEquals("Hello", messagePreviewBody("Hello", listOf(photo, file)))
        assertEquals("3 photos", messagePreviewBody("", List(3) { photo }))
        assertEquals("Hi", messagePreviewBody("Hi", emptyList()))
        assertEquals(
            "Anna: 2 photos",
            previewLine("", listOf(photo, photo), deleted = false, author = PreviewAuthor.Named("Anna")),
        )
    }
}
