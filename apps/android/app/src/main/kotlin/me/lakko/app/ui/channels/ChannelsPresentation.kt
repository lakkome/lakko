// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.PostView
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.joinFacts
import me.lakko.app.ui.chat.formatListTime
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * Pure presentation helpers for the Channels screens
 * (docs/ux/client-redesign.md section 5.4, docs/ux/group-and-channel-names.md
 * section 3.2): everything a row, a header or a status line shows is computed
 * here, off the composition, so it can be unit-tested without Compose. The
 * words come from the catalog (ADR-0061): each helper that says something takes
 * the [Strings] of the language on screen.
 */

/**
 * Whether this account follows a channel: it is in the list and this device
 * holds a reader key for it. Either alone is a subscribe that never finished,
 * or a channel known only by name.
 */
public fun channelIsFollowed(listed: Boolean, readerKeyHeld: Boolean): Boolean = listed && readerKeyHeld

/**
 * Whether a channel's page reads the channel, which registers a reader key and
 * spends a `channel-subscribe` token when there is none, or shows the Subscribe
 * page, which does neither. A person becomes a subscriber only by pressing
 * Subscribe (`docs/ux/group-and-channel-privacy.md` section 5.1).
 */
public fun channelPageReads(followed: Boolean, pressed: Boolean): Boolean = followed || pressed

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
 * known once the channel has been opened. [role] picks the fallback line.
 */
public fun channelRowModel(
    channel: ChannelHandle,
    posts: List<PostView>?,
    subscriberCount: Int?,
    role: ChannelViewerRole,
    nowMs: Long,
    zone: ZoneId,
    use24h: Boolean,
    locale: Locale,
    strings: Strings,
): ChannelRowModel {
    val newest = posts?.maxOfOrNull { it.serverTsMs.toLong() }
    return ChannelRowModel(
        id = channel.id,
        title = channelTitle(channel, strings),
        handle = channelHandleLabel(channel.handle),
        subtitle = channelSubtitle(channel, subscriberCount, role, strings),
        time = newest?.let { formatListTime(it, nowMs, zone, use24h, locale) },
        sortMs = newest ?: channel.addedAtMs.toLong(),
    )
}

/**
 * The title precedence (ADR-0051 §5): the shared name from the newest record,
 * else the local name, else the bare handle, else "Channel".
 */
public fun channelTitle(channel: ChannelHandle, strings: Strings): String {
    channel.sharedName.trim().takeIf { it.isNotEmpty() }?.let { return it }
    channel.localName.trim().takeIf { it.isNotEmpty() }?.let { return it }
    channel.handle.trim().removePrefix("@").takeIf { it.isNotEmpty() }?.let { return it }
    return strings["channels.fallback_name"]
}

/** The description shown under the name: the shared one, else the local one; `null` when blank. */
public fun channelDescription(channel: ChannelHandle): String? {
    channel.sharedDescription.trim().takeIf { it.isNotEmpty() }?.let { return it }
    return channel.localDescription.trim().takeIf { it.isNotEmpty() }
}

/**
 * "@handle · 12 subscribers", "@handle", "12 subscribers", or the fallback
 * "Your channel" / "Publisher" / "Subscribed". The handle is left out when the title is
 * already the handle, so a channel with no name never shows it twice.
 */
public fun channelSubtitle(
    channel: ChannelHandle,
    subscriberCount: Int?,
    role: ChannelViewerRole,
    strings: Strings,
): String {
    val parts = ArrayList<String>(2)
    val handle = channelHandleLabel(channel.handle)
    if (handle != null && !titleIsHandle(channel, strings)) parts += handle
    subscriberCountLine(subscriberCount)?.let { parts += it.renderWith(strings) }
    if (parts.isEmpty()) parts += channelStatusLine(role).renderWith(strings)
    return joinFacts(parts, strings)
}

/** `true` when the title falls back to the handle (no shared or local name). */
public fun titleIsHandle(channel: ChannelHandle, strings: Strings): Boolean {
    val bare = channel.handle.trim().removePrefix("@")
    return bare.isNotEmpty() && channelTitle(channel, strings) == bare
}

/** The viewer's relation to a channel (ADR-0087 section 15). */
public enum class ChannelViewerRole { OWNER, PUBLISHER, SUBSCRIBER }

/**
 * The role from what this device holds: publisher keys mean the account
 * publishes there; [joinedAsCoPublisher] (a PublisherGroup this account
 * joined through a co-publisher link, `ChannelsUiState.joinedGroupIdOf`)
 * means it is a publisher who is not the owner. The owner's channel is the
 * one whose group its own seed derives, so it has no joined group id.
 */
public fun channelViewerRole(holdsPublisherKeys: Boolean, joinedAsCoPublisher: Boolean): ChannelViewerRole = when {
    !holdsPublisherKeys -> ChannelViewerRole.SUBSCRIBER
    joinedAsCoPublisher -> ChannelViewerRole.PUBLISHER
    else -> ChannelViewerRole.OWNER
}

/** The one-line standing status on a channel: the viewer's relation to it. */
public fun channelStatusLine(role: ChannelViewerRole): UiText = UiText(
    when (role) {
        ChannelViewerRole.OWNER -> "channels.row.own"
        ChannelViewerRole.PUBLISHER -> "channels.publishers.role_publisher"
        ChannelViewerRole.SUBSCRIBER -> "channels.subscribe.status.subscribed"
    },
)

/** What the reader's header card shows. */
public data class ChannelHeaderModel(
    val title: String,
    /** "@handle", or `null` when there is none or it would repeat the title. */
    val handleLine: String?,
    /** The description, or `null` when blank. */
    val description: String?,
    /** "Subscribed" / "Publisher" / "Your channel". */
    val statusLine: UiText,
)

/** The header card for [channel] as a viewer of [role] sees it. */
public fun channelHeaderModel(channel: ChannelHandle, role: ChannelViewerRole, strings: Strings): ChannelHeaderModel =
    ChannelHeaderModel(
        title = channelTitle(channel, strings),
        handleLine = channelHandleLabel(channel.handle)?.takeIf { !titleIsHandle(channel, strings) },
        description = channelDescription(channel),
        statusLine = channelStatusLine(role),
    )

/** "@handle" for a non-empty handle (with or without the "@" typed), else `null`. */
public fun channelHandleLabel(handle: String): String? {
    val bare = handle.trim().removePrefix("@")
    return if (bare.isEmpty()) null else "@$bare"
}

/** "1 subscriber" / "12 subscribers", `null` when the count is unknown. */
public fun subscriberCountLine(count: Int?): UiText? =
    count?.let { UiText("channels.info.subscribers", counts = mapOf("count" to it.coerceAtLeast(0).toULong())) }

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
public fun relativeTimeLabel(atMs: Long, nowMs: Long, zone: ZoneId, locale: Locale, strings: Strings): String {
    val delta = nowMs - atMs
    val minute = 60_000L
    val hour = 60 * minute
    val day = 24 * hour
    return when {
        delta < minute -> strings["common.time.just_now"]
        delta < hour -> ago("common.time.minutes_ago", delta / minute, strings)
        delta < day -> ago("common.time.hours_ago", delta / hour, strings)
        delta < 7 * day -> ago("common.time.days_ago", delta / day, strings)
        else -> me.lakko.app.l10n.DeviceFormats.dayMonthYear(
            Instant.ofEpochMilli(atMs).atZone(zone),
            locale,
            me.lakko.app.l10n.DeviceFormats.region(),
        )
    }
}

private fun ago(key: String, count: Long, strings: Strings): String =
    strings.format(key, counts = mapOf("count" to count.toULong()))

/**
 * The muted time on a post card, relative while it is recent: "Just now"
 * under a minute, "5 min ago", "2 h ago" within a day, then the date ("16 Sep",
 * with the year for another year). A future instant reads as "Just now".
 */
public fun postRelativeTimeLabel(atMs: Long, nowMs: Long, zone: ZoneId, locale: Locale, strings: Strings): String {
    val delta = nowMs - atMs
    val minute = 60_000L
    val hour = 60 * minute
    val day = 24 * hour
    if (delta < minute) return strings["common.time.just_now"]
    if (delta < hour) return ago("common.time.minutes_ago", delta / minute, strings)
    if (delta < day) return ago("common.time.hours_ago", delta / hour, strings)
    val at = Instant.ofEpochMilli(atMs).atZone(zone)
    val now = Instant.ofEpochMilli(nowMs).atZone(zone)
    val region = me.lakko.app.l10n.DeviceFormats.region()
    return if (at.year == now.year) {
        me.lakko.app.l10n.DeviceFormats.dayMonth(at, locale, region)
    } else {
        me.lakko.app.l10n.DeviceFormats.dayMonthYear(at, locale, region)
    }
}

/**
 * The rotation status line on the Manage screen. Android keeps no rotation
 * history across launches, so the line names a time only for a rotation that
 * happened in this session; otherwise it states the standing behaviour.
 */
public fun rotationStatusLine(
    lastRotatedAtMs: Long?,
    nowMs: Long,
    zone: ZoneId,
    locale: Locale,
    strings: Strings,
): String =
    if (lastRotatedAtMs == null) {
        strings["channels.rotation.never"]
    } else {
        strings.format(
            "channels.rotation.refreshed",
            mapOf("when" to relativeTimeLabel(lastRotatedAtMs, nowMs, zone, locale, strings)),
        )
    }

/** The one-line reason under "Not available" for a post this device cannot read. */
public fun unreadableReasonLine(olderThanKeys: Boolean): UiText =
    UiText(if (olderThanKeys) "channels.post.older_key" else "channels.post.unreadable")
