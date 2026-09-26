// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.Ack
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.media.KIND_GIF
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.KIND_VOICE
import java.time.Instant
import java.time.LocalDate
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeParseException
import java.time.format.FormatStyle
import java.time.format.TextStyle
import java.util.Locale

/*
 * docs/ux/client-redesign.md sections 5.1 and 5.2 — the pure rules behind the
 * conversation rows and the timeline: how a DS `created_at` renders in a row,
 * in a bubble corner and in a date separator; what one line of preview says
 * for a message; and when consecutive messages form one visual run.
 *
 * Nothing here touches Compose or the repository, so every rule is unit-tested
 * on the host JVM. Times come from the DS row's server-assigned `created_at`
 * (ADR-0024's clock rule); a row that cannot be parsed shows no time rather
 * than a made-up one.
 */

/** The ISO-8601 `created_at` as epoch millis, or `null` when unparseable. */
public fun parseMessageTimeMs(createdAt: String?): Long? {
    if (createdAt.isNullOrBlank()) return null
    return try {
        Instant.parse(createdAt).toEpochMilli()
    } catch (_: DateTimeParseException) {
        try {
            OffsetDateTime.parse(createdAt).toInstant().toEpochMilli()
        } catch (_: DateTimeParseException) {
            null
        }
    }
}

private fun dateOf(atMs: Long, zone: ZoneId): LocalDate =
    Instant.ofEpochMilli(atMs).atZone(zone).toLocalDate()

/** `true` when both instants fall on the same calendar day in [zone]. */
public fun sameCalendarDay(aMs: Long, bMs: Long, zone: ZoneId): Boolean =
    dateOf(aMs, zone) == dateOf(bMs, zone)

/** "14:05" or "2:05 PM", per the device's 12/24-hour setting. */
public fun formatClockTime(atMs: Long, zone: ZoneId, use24h: Boolean, locale: Locale): String {
    val pattern = if (use24h) "HH:mm" else "h:mm a"
    return DateTimeFormatter.ofPattern(pattern, locale).format(Instant.ofEpochMilli(atMs).atZone(zone))
}

/**
 * The time slot of a conversation row (UX 5.1): today -> the clock time; the
 * last six days -> the short weekday; older, same year -> "16 Sep"; another
 * year -> the locale's short numeric date ("16.09.25" / "9/16/25").
 */
public fun formatListTime(
    atMs: Long,
    nowMs: Long,
    zone: ZoneId,
    use24h: Boolean,
    locale: Locale,
): String {
    val at = Instant.ofEpochMilli(atMs).atZone(zone)
    val today = dateOf(nowMs, zone)
    val day = at.toLocalDate()
    if (day == today) return formatClockTime(atMs, zone, use24h, locale)
    val daysAgo = today.toEpochDay() - day.toEpochDay()
    if (daysAgo in 1..6) {
        return day.dayOfWeek.getDisplayName(TextStyle.SHORT, locale)
    }
    if (day.year == today.year) {
        return DateTimeFormatter.ofPattern("d MMM", locale).format(at)
    }
    return DateTimeFormatter.ofLocalizedDate(FormatStyle.SHORT).withLocale(locale).format(at)
}

/**
 * The centred date pill between two days of the timeline (UX 5.2): "Today",
 * "Yesterday", the weekday within the last six days, "16 September" in the
 * current year, "16 September 2025" otherwise.
 */
public fun dateSeparatorLabel(atMs: Long, nowMs: Long, zone: ZoneId, locale: Locale): String {
    val at = Instant.ofEpochMilli(atMs).atZone(zone)
    val today = dateOf(nowMs, zone)
    val day = at.toLocalDate()
    val daysAgo = today.toEpochDay() - day.toEpochDay()
    return when {
        daysAgo == 0L -> "Today"
        daysAgo == 1L -> "Yesterday"
        daysAgo in 2..6 -> day.dayOfWeek.getDisplayName(TextStyle.FULL, locale)
        day.year == today.year -> DateTimeFormatter.ofPattern("d MMMM", locale).format(at)
        else -> DateTimeFormatter.ofPattern("d MMMM yyyy", locale).format(at)
    }
}

/** The two sender-chosen fields a preview line reads off an attachment. */
public data class PreviewAttachment(val kind: String?, val mime: String?)

/**
 * One word for a media attachment in a row preview (UX 5.1). The `kind` picks
 * the word the same way it picks the bubble shape (ADR-0047); the MIME type
 * decides "Photo" for a plain image. Nothing here plays or decodes the bytes.
 */
public fun mediaPreviewLabel(attachment: PreviewAttachment): String = when {
    attachment.kind == KIND_VOICE -> "Voice message"
    attachment.kind == KIND_VIDEO_NOTE -> "Video message"
    attachment.kind == KIND_GIF -> "GIF"
    attachment.mime?.lowercase(Locale.ROOT)?.startsWith("image/") == true -> "Photo"
    attachment.mime?.lowercase(Locale.ROOT)?.startsWith("video/") == true -> "Video"
    else -> "File"
}

/**
 * The second line of a conversation row: the text, else the media word, else
 * nothing; a deleted message reads "Message deleted". [prefix] is the
 * sender-name prefix ("Anna: " in groups, "You: " for own messages) and is
 * applied to every non-empty line.
 */
public fun previewLine(
    text: String,
    attachments: List<PreviewAttachment>,
    deleted: Boolean,
    prefix: String,
): String {
    val body = when {
        deleted -> "Message deleted"
        text.isNotBlank() -> text.trim().replace(Regex("\\s+"), " ")
        attachments.isNotEmpty() -> {
            val first = mediaPreviewLabel(attachments.first())
            if (attachments.size > 1) "$first +${attachments.size - 1}" else first
        }
        else -> ""
    }
    return if (body.isEmpty()) body else prefix + body
}

/** The preview prefix for a message: "You: " for own, the sender's name in groups, nothing otherwise. */
public fun previewPrefix(direction: Direction, isGroup: Boolean, senderName: String?): String = when {
    direction == Direction.Outgoing -> "You: "
    direction == Direction.Incoming && isGroup && !senderName.isNullOrBlank() -> "$senderName: "
    else -> ""
}

/** Consecutive messages closer than this, from one sender, render as one run. */
public const val RUN_GAP_MS: Long = 5L * 60L * 1000L

/**
 * `true` when [cur] continues the visual run [prev] started (UX 5.2): the same
 * direction and the same author, both real messages (system rows always stand
 * alone), on the same day and within [RUN_GAP_MS]. Two rows without a
 * parseable time are still grouped by author; one with and one without are not.
 */
public fun continuesRun(
    prev: ChatMessage?,
    prevMs: Long?,
    cur: ChatMessage,
    curMs: Long?,
    zone: ZoneId,
): Boolean {
    if (prev == null) return false
    if (prev.direction == Direction.System || cur.direction == Direction.System) return false
    if (prev.direction != cur.direction) return false
    if (prev.authorKey != cur.authorKey) return false
    if (prevMs == null && curMs == null) return true
    if (prevMs == null || curMs == null) return false
    if (!sameCalendarDay(prevMs, curMs, zone)) return false
    val gap = curMs - prevMs
    return gap in 0..RUN_GAP_MS
}

/** One entry of the rendered timeline. */
public sealed class TimelineItem {
    /** The lazy-list key. */
    public abstract val key: String

    /** A centred day pill. */
    public data class DateSeparator(override val key: String, val label: String) : TimelineItem()

    /** A message, with its position in its run and its corner time. */
    public data class Row(
        val message: ChatMessage,
        /** First of a run: carries the sender name and the avatar in groups. */
        val firstInRun: Boolean,
        /** Last of a run: the row after it gets the wide gap. */
        val lastInRun: Boolean,
        /** "14:05" from the DS row, or `null` when the row carries no usable time. */
        val time: String?,
    ) : TimelineItem() {
        /**
         * Keyed on the row's identity rather than its `messageId`, because an
         * unsent row does not have one yet: two of them would then share a
         * key, and the lazy list would draw one row for two messages. It does
         * not move when the send lands, so the row is not rebuilt mid-send.
         */
        override val key: String get() = "m-${message.rowId}"
    }
}

/**
 * What an outgoing row's time slot says.
 *
 * A message that has not reached the server has no server time to show, so
 * the slot carries its state instead: it is the same place the eye already
 * looks for "when", and it means the row needs no second line. "Not sent" is
 * the word the web client, the media bubbles and iOS all use
 * (`docs/ux/media-messages.md` section 3) - two spellings of the same thing
 * would be one of them being wrong.
 */
public fun statusLabel(ack: Ack, time: String?): String? = when (ack) {
    Ack.PENDING -> "Sending..."
    Ack.FAILED -> "Not sent"
    Ack.SENT -> time
}

/**
 * The two bookkeeping rows the receive loop files as system rows carry protocol
 * names and nothing a reader acts on; the timeline does not draw them (UX spec
 * section 6). The conversation list already skipped them for its preview
 * (`ChatListScreen.kt`); the timeline kept drawing them in a pill, found live
 * 2026-09-26. iOS has the same rule in `ChatTimeline.isHiddenSystemRow`. A call
 * row is a system row too and IS drawn; it is told apart by what it holds.
 */
public fun isHiddenSystemRow(message: ChatMessage): Boolean {
    if (message.direction != Direction.System || message.call != null) return false
    return message.text == "[mls commit]" || message.text == "[mls external join]"
}

/**
 * Fold [allMessages] (already in journal order), less the hidden system rows,
 * into date separators and rows
 * with run boundaries. Rows without a parseable time never open a separator.
 */
public fun buildTimeline(
    allMessages: List<ChatMessage>,
    nowMs: Long,
    zone: ZoneId,
    use24h: Boolean,
    locale: Locale,
): List<TimelineItem> {
    // Dropped before the fold, so a hidden row neither opens a date separator
    // nor breaks a run between two messages that read as one.
    val messages = allMessages.filterNot(::isHiddenSystemRow)
    val out = ArrayList<TimelineItem>(messages.size + 8)
    val times = messages.map { parseMessageTimeMs(it.createdAt) }
    var lastSeparatorDayMs: Long? = null
    for (i in messages.indices) {
        val msg = messages[i]
        val ms = times[i]
        if (ms != null && (lastSeparatorDayMs == null || !sameCalendarDay(lastSeparatorDayMs, ms, zone))) {
            out += TimelineItem.DateSeparator(
                key = "d-${dateOf(ms, zone)}",
                label = dateSeparatorLabel(ms, nowMs, zone, locale),
            )
            lastSeparatorDayMs = ms
        }
        val prev = messages.getOrNull(i - 1)
        val next = messages.getOrNull(i + 1)
        val first = !continuesRun(prev, times.getOrNull(i - 1), msg, ms, zone)
        val last = next == null || !continuesRun(msg, ms, next, times[i + 1], zone)
        out += TimelineItem.Row(
            message = msg,
            firstInRun = first,
            lastInRun = last,
            time = statusLabel(msg.ack, ms?.let { formatClockTime(it, zone, use24h, locale) }),
        )
    }
    return out
}
