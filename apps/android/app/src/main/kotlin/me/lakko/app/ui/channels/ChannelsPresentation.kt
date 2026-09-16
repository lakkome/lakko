// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.PostView
import me.lakko.app.ui.chat.formatClockTime
import me.lakko.app.ui.chat.formatListTime
import me.lakko.app.ui.chat.sameCalendarDay
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/**
 * Pure presentation helpers for the Channels screens
 * (docs/ux/client-redesign.md section 5.4): everything a row or a status
 * line shows is computed here, off the composition, so it can be unit-tested
 * without Compose.
 */

/** Everything one Channels row renders. */
public data class ChannelRowModel(
    val id: String,
    /** The channel's local name; "Channel" when it has none. */
    val title: String,
    /** "@handle", or `null` when the channel has no public handle. */
    val handle: String?,
    /** "12 subscribers" / "1 subscriber", or `null` when the count is not known. */
    val subscriberLine: String?,
    /** The formatted time of the newest post, or `null` when no post is known. */
    val time: String?,
    /** Sort key: the newest post's server time, else the time the channel was added. */
    val sortMs: Long,
)

/**
 * The row for [channel]. Subscriber count and posts are optional inputs — an
 * owner learns the count from its fan-out, a reader never does; posts are
 * known once the channel has been opened.
 */
public fun channelRowModel(
    channel: ChannelHandle,
    posts: List<PostView>?,
    subscriberCount: Int?,
    nowMs: Long,
    zone: ZoneId,
    use24h: Boolean,
    locale: Locale,
): ChannelRowModel {
    val newest = posts?.maxOfOrNull { it.serverTsMs.toLong() }
    return ChannelRowModel(
        id = channel.id,
        title = channelTitle(channel),
        handle = channelHandleLabel(channel.handle),
        subscriberLine = subscriberCountLine(subscriberCount),
        time = newest?.let { formatListTime(it, nowMs, zone, use24h, locale) },
        sortMs = newest ?: channel.addedAtMs.toLong(),
    )
}

/** The local name, or "Channel" for a channel without one. */
public fun channelTitle(channel: ChannelHandle): String =
    channel.localName.trim().ifEmpty { "Channel" }

/** "@handle" for a non-empty handle (with or without the "@" typed), else `null`. */
public fun channelHandleLabel(handle: String): String? {
    val bare = handle.trim().removePrefix("@")
    return if (bare.isEmpty()) null else "@$bare"
}

/** "1 subscriber" / "12 subscribers", `null` when the count is unknown. */
public fun subscriberCountLine(count: Int?): String? = when {
    count == null -> null
    count == 1 -> "1 subscriber"
    else -> "$count subscribers"
}

/** The rows matching [query] by title or handle (case-insensitive), newest first. */
public fun filterChannelRows(rows: List<ChannelRowModel>, query: String): List<ChannelRowModel> {
    val q = query.trim().removePrefix("@")
    val matched = if (q.isEmpty()) {
        rows
    } else {
        rows.filter {
            it.title.contains(q, ignoreCase = true) ||
                (it.handle?.contains(q, ignoreCase = true) == true)
        }
    }
    return matched.sortedByDescending { it.sortMs }
}

/**
 * A plain relative age for a status line ("Keys rotated 2 h ago"): "just
 * now" under a minute, then minutes, hours and days; anything a week or more
 * back is the short date. Never a negative value: a future instant reads as
 * "just now".
 */
public fun relativeTimeLabel(atMs: Long, nowMs: Long, zone: ZoneId, locale: Locale): String {
    val delta = nowMs - atMs
    val minute = 60_000L
    val hour = 60 * minute
    val day = 24 * hour
    return when {
        delta < minute -> "just now"
        delta < hour -> "${delta / minute} min ago"
        delta < day -> "${delta / hour} h ago"
        delta < 7 * day -> "${delta / day} d ago"
        else -> DateTimeFormatter.ofPattern("d MMM yyyy", locale)
            .format(Instant.ofEpochMilli(atMs).atZone(zone))
    }
}

/**
 * The time on a post card: the clock time for a post from today, otherwise
 * the day and the clock time ("16 Sep, 14:05"; the year is added for another
 * year).
 */
public fun formatPostTime(atMs: Long, nowMs: Long, zone: ZoneId, use24h: Boolean, locale: Locale): String {
    val clock = formatClockTime(atMs, zone, use24h, locale)
    if (sameCalendarDay(atMs, nowMs, zone)) return clock
    val at = Instant.ofEpochMilli(atMs).atZone(zone)
    val now = Instant.ofEpochMilli(nowMs).atZone(zone)
    val pattern = if (at.year == now.year) "d MMM" else "d MMM yyyy"
    return DateTimeFormatter.ofPattern(pattern, locale).format(at) + ", " + clock
}

/**
 * The rotation status line on the Manage screen. Android keeps no rotation
 * history across launches, so the line names a time only for a rotation that
 * happened in this session; otherwise it states the standing behaviour.
 */
public fun rotationStatusLine(lastRotatedAtMs: Long?, nowMs: Long, zone: ZoneId, locale: Locale): String =
    if (lastRotatedAtMs == null) {
        "Keys rotate automatically."
    } else {
        "Keys rotated ${relativeTimeLabel(lastRotatedAtMs, nowMs, zone, locale)}."
    }

/** The one-line reason under "Not available" for a post this device cannot read. */
public fun unreadableReasonLine(olderThanKeys: Boolean): String =
    if (olderThanKeys) {
        "This post is older than the keys this device holds."
    } else {
        "This post could not be opened on this device."
    }
