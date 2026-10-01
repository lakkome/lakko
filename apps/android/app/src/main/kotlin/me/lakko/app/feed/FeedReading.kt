// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.serialization.Serializable

/**
 * The feed's per-device reading state (`docs/ux/feed.md` sections 4.1 and 5):
 * the conversations hidden from feeds, what each feed showed the last time the
 * person left it (the "New" divider), and how far each channel has been read
 * (the channel row's count). Plus the twelve feed icons and the "Show N new"
 * count. Pure: no Compose, no storage, no FFI, so the host JVM suite runs it.
 * [FeedReadingStore] seals it per account on this device; nothing here
 * leaves the device.
 */

// ----- Icons (UX spec section 4.1) ---------------------------------------

/**
 * The twelve feed icons, in the index order the rule file carries. Each
 * client maps a meaning to its own symbol set; an index this list does not
 * know draws [FEED], the default.
 */
public enum class FeedIcon(public val label: String) {
    FEED("Feed"),
    STAR("Star"),
    HEART("Heart"),
    LIGHTNING("Lightning"),
    BOOK("Book"),
    CODE("Code"),
    MUSIC("Music"),
    CAMERA("Camera"),
    GLOBE("Globe"),
    SPEECH_BUBBLE("Speech bubble"),
    BELL("Bell"),
    FLAG("Flag"),
    ;

    /** The index the rule carries. */
    public val index: UByte get() = ordinal.toUByte()

    public companion object {
        /** The icon for a rule's index; unknown draws the default. */
        public fun of(index: UByte): FeedIcon = entries.getOrNull(index.toInt()) ?: FEED
    }
}

// ----- State --------------------------------------------------------------

public const val FEED_READING_VERSION: Int = 1

/** Most conversations kept hidden (chosen). */
public const val MAX_HIDDEN_FROM_FEEDS: Int = 1_000

/** Items remembered per feed for the "New" divider (chosen): the top of a
 *  feed, which is all a person compares against. */
public const val LOOKED_KEYS_PER_FEED: Int = 300

/** Feeds whose last look is kept: the presets plus every saved feed. */
public const val MAX_LOOKED_FEEDS: Int = MAX_SAVED_FEEDS + 8

/** Channels whose read marker is kept (chosen). */
public const val MAX_CHANNEL_READ: Int = 1_000

public data class FeedReading(
    /** Group id hex / channel id hex taken out of every feed on this device. */
    val hidden: Set<String> = emptySet(),
    /** Per feed selection: hashes of the items it showed when last left. */
    val looked: Map<String, List<Long>> = emptyMap(),
    /** Per channel id: the cursor of the newest post read on this device. */
    val channelRead: Map<String, Long> = emptyMap(),
)

@Serializable
internal data class FeedReadingWire(
    val version: Int = FEED_READING_VERSION,
    val hidden: List<String> = emptyList(),
    val looked: Map<String, List<Long>> = emptyMap(),
    val channelRead: Map<String, Long> = emptyMap(),
)

private val CONVERSATION_ID_RE = Regex("^(?:[0-9a-f]{2}){1,128}$")
private val CHANNEL_ID_RE = Regex("^[0-9a-f]{32}$")
private val SELECTION_RE = Regex("^[A-Za-z0-9_:-]{1,64}$")

/** Lenient per entry: a malformed id, selection or cursor is dropped alone;
 *  an unknown version reads as empty. */
internal fun decodeFeedReading(wire: FeedReadingWire?): FeedReading {
    if (wire == null || wire.version != FEED_READING_VERSION) return FeedReading()
    val hidden = wire.hidden.filter { CONVERSATION_ID_RE.matches(it) }.take(MAX_HIDDEN_FROM_FEEDS).toSet()
    val looked = wire.looked.entries
        .filter { SELECTION_RE.matches(it.key) }
        .take(MAX_LOOKED_FEEDS)
        .associate { it.key to it.value.take(LOOKED_KEYS_PER_FEED) }
    val channelRead = wire.channelRead.entries
        .filter { CHANNEL_ID_RE.matches(it.key) && it.value >= 0 }
        .take(MAX_CHANNEL_READ)
        .associate { it.key to it.value }
    return FeedReading(hidden, looked, channelRead)
}

internal fun encodeFeedReading(r: FeedReading): FeedReadingWire =
    FeedReadingWire(FEED_READING_VERSION, r.hidden.toList(), r.looked, r.channelRead)

// ----- Hide from feeds (UX spec section 5) --------------------------------

public fun hideFromFeeds(r: FeedReading, idHex: String): FeedReading {
    if (!CONVERSATION_ID_RE.matches(idHex) || idHex in r.hidden) return r
    if (r.hidden.size >= MAX_HIDDEN_FROM_FEEDS) return r
    return r.copy(hidden = r.hidden + idHex)
}

public fun showInFeedsAgain(r: FeedReading, idHex: String): FeedReading = r.copy(hidden = r.hidden - idHex)

/**
 * The feed's sources without the hidden ones. Applied before the input is
 * built, so a hidden conversation reaches no preset, no own feed and no
 * preview.
 */
public fun withoutHidden(sources: List<FeedSource>, hidden: Set<String>): List<FeedSource> =
    if (hidden.isEmpty()) sources else sources.filter { it.idHex !in hidden }

/** The hidden conversations this device still holds, in the order the
 *  sources list them (the "Hidden from feeds" list). */
public fun hiddenSources(sources: List<FeedSource>, hidden: Set<String>): List<FeedSource> =
    sources.filter { it.idHex in hidden }

// ----- "New" divider (UX spec section 5) ----------------------------------

/** A stable 64-bit hash of an item key, so a feed's last look is small. */
public fun itemKeyHash(key: String): Long {
    val d = java.security.MessageDigest.getInstance("SHA-256").digest(key.toByteArray(Charsets.UTF_8))
    var v = 0L
    for (i in 0 until 8) v = (v shl 8) or (d[i].toLong() and 0xff)
    return v
}

/** What a feed showed, as it is remembered: the first [LOOKED_KEYS_PER_FEED]
 *  item keys, hashed. */
public fun lookedSnapshot(keys: List<String>): List<Long> = keys.take(LOOKED_KEYS_PER_FEED).map(::itemKeyHash)

/** Remember what [selection] showed as the person left it. */
public fun withLooked(r: FeedReading, selection: String, keys: List<String>): FeedReading {
    if (!SELECTION_RE.matches(selection)) return r
    val next = LinkedHashMap(r.looked)
    next.remove(selection)
    next[selection] = lookedSnapshot(keys)
    while (next.size > MAX_LOOKED_FEEDS) next.remove(next.keys.first())
    return r.copy(looked = next)
}

/** Forget the last look of feeds that no longer exist. */
public fun pruneLooked(r: FeedReading, keep: Set<String>): FeedReading =
    if (r.looked.keys.all { it in keep }) r else r.copy(looked = r.looked.filterKeys { it in keep })

/**
 * Where the "New" divider goes: the index of the first item (in drawn order)
 * that was not in this feed when the person last left it. `null` on the first
 * visit ([baseline] `null`) and when nothing is new.
 */
public fun newDividerIndex(keys: List<String>, baseline: Set<Long>?): Int? {
    if (baseline == null || keys.isEmpty()) return null
    val i = keys.indexOfFirst { itemKeyHash(it) !in baseline }
    return i.takeIf { it >= 0 }
}

// ----- "Show N new" (UX spec section 5) -----------------------------------

/** N on the pill: items waiting behind it that the list does not show yet. */
public fun newItemCount(shownKeys: List<String>?, pendingKeys: List<String>?): Int {
    if (shownKeys == null || pendingKeys == null) return 0
    val shown = shownKeys.toHashSet()
    return pendingKeys.count { it !in shown }
}

// ----- Channel read state (UX spec section 5, "Channel posts") ------------

/** Move a channel's read marker to [cursor]; never backwards. */
public fun advanceChannelRead(r: FeedReading, channelIdHex: String, cursor: Long): FeedReading {
    if (!CHANNEL_ID_RE.matches(channelIdHex) || cursor <= 0) return r
    val current = r.channelRead[channelIdHex]
    if (current != null && current >= cursor) return r
    val next = LinkedHashMap(r.channelRead)
    next[channelIdHex] = cursor
    while (next.size > MAX_CHANNEL_READ) next.remove(next.keys.first())
    return r.copy(channelRead = next)
}

/**
 * A channel with no marker yet (a fresh subscription, or one held before
 * markers existed) starts at the newest post this device holds at that
 * moment, so it opens with no count; only posts after it are new. A channel
 * that already has a marker is left alone.
 */
public fun seedChannelRead(r: FeedReading, channelIdHex: String, newestHeld: Long): FeedReading {
    if (!CHANNEL_ID_RE.matches(channelIdHex) || channelIdHex in r.channelRead) return r
    val next = LinkedHashMap(r.channelRead)
    next[channelIdHex] = newestHeld.coerceAtLeast(0L)
    while (next.size > MAX_CHANNEL_READ) next.remove(next.keys.first())
    return r.copy(channelRead = next)
}

/** `true` when the post at [cursor] is still new for the channel. */
public fun isChannelPostUnread(r: FeedReading, channelIdHex: String, cursor: Long, own: Boolean): Boolean =
    !own && cursor > (r.channelRead[channelIdHex] ?: 0L)

/**
 * The channel row's count: posts newer than the newest one read. Reading the
 * newest makes it zero whatever was skipped; a channel this device publishes
 * to counts none, since its posts are the person's own. A channel with no
 * marker yet counts none either: it is about to be seeded ([seedChannelRead]).
 */
public fun channelUnreadCount(postCursors: List<Long>, readThrough: Long?, own: Boolean): Int {
    if (own || readThrough == null) return 0
    return postCursors.count { it > readThrough }
}
