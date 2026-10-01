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
public enum class FeedIcon(public val labelKey: String) {
    FEED("feed.icon.feed"),
    STAR("feed.icon.star"),
    HEART("feed.icon.heart"),
    LIGHTNING("feed.icon.lightning"),
    BOOK("feed.icon.book"),
    CODE("feed.icon.code"),
    MUSIC("feed.icon.music"),
    CAMERA("feed.icon.camera"),
    GLOBE("feed.icon.globe"),
    SPEECH_BUBBLE("feed.icon.speech_bubble"),
    BELL("feed.icon.bell"),
    FLAG("feed.icon.flag"),
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

/** Most conversations kept hidden (chosen): hiding one more drops the
 *  oldest hidden. */
public const val MAX_HIDDEN_FROM_FEEDS: Int = 1_000

/** Items remembered per feed for the "New" divider (chosen, as web's
 *  `MAX_LOOKED_KEYS`): well past a screenful, and an item further down the
 *  list than this is old news anyway. */
public const val LOOKED_KEYS_PER_FEED: Int = 2_000

/** Feeds whose last look is kept: the presets plus every saved feed. */
public const val MAX_LOOKED_FEEDS: Int = MAX_SAVED_FEEDS + 8

/** Channels whose read marker is kept (chosen). */
public const val MAX_CHANNEL_READ: Int = 1_000

public data class FeedReading(
    /** Group id hex / channel id hex taken out of every feed on this device,
     *  in the order they were hidden (oldest first). */
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

/** Hide [idHex]; when [MAX_HIDDEN_FROM_FEEDS] are already hidden, the oldest
 *  hidden is dropped to make room (UX spec section 5, as web). */
public fun hideFromFeeds(r: FeedReading, idHex: String): FeedReading {
    if (!CONVERSATION_ID_RE.matches(idHex) || idHex in r.hidden) return r
    val next = LinkedHashSet<String>(r.hidden)
    next.add(idHex)
    while (next.size > MAX_HIDDEN_FROM_FEEDS) next.remove(next.first())
    return r.copy(hidden = next)
}

public fun showInFeedsAgain(r: FeedReading, idHex: String): FeedReading = r.copy(hidden = r.hidden - idHex)

/**
 * The feed's sources without the hidden ones. Applied before the input is
 * built, so a hidden conversation reaches no preset, no own feed and no
 * preview.
 */
public fun withoutHidden(sources: List<FeedSource>, hidden: Set<String>): List<FeedSource> =
    if (hidden.isEmpty()) sources else sources.filter { it.idHex !in hidden }

/** The hidden conversations this device still holds, the most recently
 *  hidden first (the "Hidden from feeds" list). [hidden] iterates in the
 *  order the conversations were hidden. */
public fun hiddenSources(sources: List<FeedSource>, hidden: Set<String>): List<FeedSource> {
    val byId = sources.associateBy { it.idHex }
    return hidden.toList().asReversed().mapNotNull { byId[it] }
}

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
 * that already has a marker is left alone. [newestHeld] is
 * [channelReadSeed] of the channel's slot.
 */
public fun seedChannelRead(r: FeedReading, channelIdHex: String, newestHeld: Long): FeedReading {
    if (!CHANNEL_ID_RE.matches(channelIdHex) || channelIdHex in r.channelRead) return r
    val next = LinkedHashMap(r.channelRead)
    next[channelIdHex] = newestHeld.coerceAtLeast(0L)
    while (next.size > MAX_CHANNEL_READ) next.remove(next.keys.first())
    return r.copy(channelRead = next)
}

/** Where a channel's marker starts (as web's `seedChannelReads`): its
 *  slot's cursor, never below its newest post, never below zero. The slot
 *  cursor also covers posts passed over unread, so they never count as new. */
public fun channelReadSeed(slotCursor: Long, postCursors: List<Long>): Long =
    maxOf(slotCursor, postCursors.maxOrNull() ?: 0L, 0L)

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

// ----- Refresh failure (UX spec state table, "refresh failed (network)") ---

/** What one check did with one kind of source: how many it tried and how
 *  many answered (a group poll or a channel fetch that did not fail). */
public data class SourceCheck(val tried: Int, val answered: Int) {
    public companion object {
        public val NONE: SourceCheck = SourceCheck(0, 0)
    }
}

/** What one run of one check found; `null` (no outcome) when it tried
 *  nothing, so it says nothing. */
public enum class CheckOutcome { REACHED, NOTHING }

public fun outcomeOf(check: SourceCheck): CheckOutcome? = when {
    check.answered > 0 -> CheckOutcome.REACHED
    check.tried > 0 -> CheckOutcome.NOTHING
    else -> null
}

/** The kinds of source the feed checks. */
public enum class CheckKind { GROUPS, CHANNELS }

/** The latest outcome per kind (as web's `RefreshState`). */
public data class FeedRefreshState(
    val groups: CheckOutcome? = null,
    val channels: CheckOutcome? = null,
)

/** Fold one run in. A run that tried nothing (no channels, every group held
 *  back) leaves the kind as it was, so it neither raises nor clears the
 *  banner. */
public fun recordCheck(state: FeedRefreshState, kind: CheckKind, check: SourceCheck): FeedRefreshState {
    val outcome = outcomeOf(check) ?: return state
    return when (kind) {
        CheckKind.GROUPS -> if (state.groups == outcome) state else state.copy(groups = outcome)
        CheckKind.CHANNELS -> if (state.channels == outcome) state else state.copy(channels = outcome)
    }
}

/**
 * `true` when the "Couldn't check for new messages." banner shows: something
 * was tried and no source of either kind answered in its latest check. The
 * banner goes at the next check that reaches anything.
 */
public fun feedRefreshFailed(state: FeedRefreshState): Boolean {
    if (state.groups == CheckOutcome.REACHED || state.channels == CheckOutcome.REACHED) return false
    return state.groups == CheckOutcome.NOTHING || state.channels == CheckOutcome.NOTHING
}
