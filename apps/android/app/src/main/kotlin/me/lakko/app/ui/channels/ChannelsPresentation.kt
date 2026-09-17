// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.PostView
import me.lakko.app.ui.chat.formatListTime
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/**
 * Pure presentation helpers for the Channels screens
 * (docs/ux/client-redesign.md section 5.4, docs/ux/group-and-channel-names.md
 * section 3.2): everything a row, a header or a status line shows is computed
 * here, off the composition, so it can be unit-tested without Compose.
 */

/** Everything one Channels row renders. */
public data class ChannelRowModel(
    val id: String,
    /** The shared name (ADR-0051), else the local name, else the handle, else "Channel". */
    val title: String,
    /** "@handle", or `null` when the channel has no public handle. Kept for search. */
    val handle: String?,
    /** "@handle · 12 subscribers", or "Subscribed" / "Your channel" when there
     *  is nothing else to say. Never repeats the handle when the title IS the handle. */
    val subtitle: String,
    /** The formatted time of the newest post, or `null` when no post is known. */
    val time: String?,
    /** Sort key: the newest post's server time, else the time the channel was added. */
    val sortMs: Long,
)

/**
 * The row for [channel]. Subscriber count and posts are optional inputs — an
 * owner learns the count from its fan-out, a reader never does; posts are
 * known once the channel has been opened. [isOwner] picks the fallback line.
 */
public fun channelRowModel(
    channel: ChannelHandle,
    posts: List<PostView>?,
    subscriberCount: Int?,
    isOwner: Boolean,
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
        subtitle = channelSubtitle(channel, subscriberCount, isOwner),
        time = newest?.let { formatListTime(it, nowMs, zone, use24h, locale) },
        sortMs = newest ?: channel.addedAtMs.toLong(),
    )
}

/**
 * The title precedence (ADR-0051 §5): the shared name from the newest record,
 * else the local name, else the bare handle, else "Channel".
 */
public fun channelTitle(channel: ChannelHandle): String {
    channel.sharedName.trim().takeIf { it.isNotEmpty() }?.let { return it }
    channel.localName.trim().takeIf { it.isNotEmpty() }?.let { return it }
    channel.handle.trim().removePrefix("@").takeIf { it.isNotEmpty() }?.let { return it }
    return "Channel"
}

/** The description shown under the name: the shared one, else the local one; `null` when blank. */
public fun channelDescription(channel: ChannelHandle): String? {
    channel.sharedDescription.trim().takeIf { it.isNotEmpty() }?.let { return it }
    return channel.localDescription.trim().takeIf { it.isNotEmpty() }
}

/**
 * "@handle · 12 subscribers", "@handle", "12 subscribers", or the fallback
 * "Your channel" / "Subscribed". The handle is left out when the title is
 * already the handle, so a channel with no name never shows it twice.
 */
public fun channelSubtitle(channel: ChannelHandle, subscriberCount: Int?, isOwner: Boolean): String {
    val parts = ArrayList<String>(2)
    val handle = channelHandleLabel(channel.handle)
    if (handle != null && !titleIsHandle(channel)) parts += handle
    subscriberCountLine(subscriberCount)?.let { parts += it }
    if (parts.isEmpty()) parts += channelStatusLine(isOwner)
    return parts.joinToString(" · ")
}

/** `true` when the title falls back to the handle (no shared or local name). */
public fun titleIsHandle(channel: ChannelHandle): Boolean {
    val bare = channel.handle.trim().removePrefix("@")
    return bare.isNotEmpty() && channelTitle(channel) == bare
}

/** The one-line standing status on a channel: the viewer's relation to it. */
public fun channelStatusLine(isOwner: Boolean): String = if (isOwner) "Your channel" else "Subscribed"

/** What the reader's header card shows. */
public data class ChannelHeaderModel(
    val title: String,
    /** "@handle", or `null` when there is none or it would repeat the title. */
    val handleLine: String?,
    /** The description, or `null` when blank. */
    val description: String?,
    /** "Subscribed" / "Your channel". */
    val statusLine: String,
)

/** The header card for [channel] as [isOwner] sees it. */
public fun channelHeaderModel(channel: ChannelHandle, isOwner: Boolean): ChannelHeaderModel =
    ChannelHeaderModel(
        title = channelTitle(channel),
        handleLine = channelHandleLabel(channel.handle)?.takeIf { !titleIsHandle(channel) },
        description = channelDescription(channel),
        statusLine = channelStatusLine(isOwner),
    )

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
 * The muted time on a post card, relative while it is recent: "Just now"
 * under a minute, "5 min ago", "2 h ago" within a day, then the date ("16 Sep",
 * with the year for another year). A future instant reads as "Just now".
 */
public fun postRelativeTimeLabel(atMs: Long, nowMs: Long, zone: ZoneId, locale: Locale): String {
    val delta = nowMs - atMs
    val minute = 60_000L
    val hour = 60 * minute
    val day = 24 * hour
    if (delta < minute) return "Just now"
    if (delta < hour) return "${delta / minute} min ago"
    if (delta < day) return "${delta / hour} h ago"
    val at = Instant.ofEpochMilli(atMs).atZone(zone)
    val now = Instant.ofEpochMilli(nowMs).atZone(zone)
    val pattern = if (at.year == now.year) "d MMM" else "d MMM yyyy"
    return DateTimeFormatter.ofPattern(pattern, locale).format(at)
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
