// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.serialization.Serializable

/**
 * The channel-post log (ADR-0058 section 3): the posts of subscribed and own
 * channels this device has decrypted, kept so the feed can rank them without
 * opening each channel. Mirror of `apps/web/src/feed/channel-post-log.ts`.
 *
 * Pure: the codec, the merge and the per-channel sync live here with their
 * network and FFI dependencies injected, so the host JVM suite exercises them
 * with no Keystore, no network and no native library. [FeedSealedStore] seals
 * this payload under a Keystore key; [ChannelPostSync] drives the sync while
 * the app is in front.
 *
 * Bounds, both chosen: posts older than the server's 30-day retention are
 * dropped, and at most [CHANNEL_POST_CAP] posts are kept per channel, newest
 * kept.
 */
public const val CHANNEL_POST_LOG_VERSION: Int = 1

/** Most posts kept per channel (chosen, ADR-0058 open question 1). */
public const val CHANNEL_POST_CAP: Int = 500

/** The server's post retention; older posts are dropped from the log. */
public const val CHANNEL_POST_RETENTION_MS: Long = 30L * 86_400_000L

/** Posts asked for per request. */
public const val CHANNEL_POST_PAGE: Int = 100

/** Pages fetched per channel per sync, so one busy channel cannot hold the
 *  loop; the rest arrives on the next sync. */
public const val CHANNEL_POST_MAX_PAGES: Int = 5

/** One decrypted text post. */
@Serializable
public data class PersistedChannelPost(
    /** The server's per-channel cursor, the post's id in the feed. */
    val cursor: Long,
    /** Server receive time, unix ms. */
    val serverTsMs: Long,
    val text: String,
)

/** One channel's slice of the log. */
@Serializable
public data class ChannelPostSlot(
    /** Every post at or below this cursor has been looked at. */
    val cursor: Long = 0L,
    /** Ascending by cursor. */
    val posts: List<PersistedChannelPost> = emptyList(),
)

@Serializable
public data class ChannelPostLog(
    val version: Int = CHANNEL_POST_LOG_VERSION,
    val channels: Map<String, ChannelPostSlot> = emptyMap(),
)

private val CHANNEL_ID_RE = Regex("^[0-9a-f]{32}$")

/**
 * Normalise a decoded payload: an unknown version is `null` (the caller starts
 * empty); a malformed channel id, a negative cursor or a post with cursor 0 is
 * dropped alone; posts end up ascending.
 */
public fun sanitizeChannelPostLog(raw: ChannelPostLog?): ChannelPostLog? {
    if (raw == null || raw.version != CHANNEL_POST_LOG_VERSION) return null
    val channels = LinkedHashMap<String, ChannelPostSlot>()
    for ((id, slot) in raw.channels) {
        if (!CHANNEL_ID_RE.matches(id) || slot.cursor < 0) continue
        val posts = slot.posts.filter { it.cursor > 0 }.sortedBy { it.cursor }
        channels[id] = ChannelPostSlot(cursor = slot.cursor, posts = posts)
    }
    return ChannelPostLog(CHANNEL_POST_LOG_VERSION, channels)
}

/**
 * Fold newly decrypted posts into a slot: de-duplicated by cursor (a later
 * copy wins), ascending, trimmed to the retention window and the cap. The
 * cursor never moves backwards.
 */
public fun mergeChannelPosts(
    slot: ChannelPostSlot?,
    fresh: List<PersistedChannelPost>,
    cursor: Long,
    nowMs: Long,
): ChannelPostSlot {
    val byCursor = LinkedHashMap<Long, PersistedChannelPost>()
    for (p in slot?.posts.orEmpty()) byCursor[p.cursor] = p
    for (p in fresh) byCursor[p.cursor] = p
    val oldest = nowMs - CHANNEL_POST_RETENTION_MS
    val posts = byCursor.values
        .filter { it.serverTsMs >= oldest }
        .sortedBy { it.cursor }
        .takeLast(CHANNEL_POST_CAP)
    return ChannelPostSlot(cursor = maxOf(slot?.cursor ?: 0L, cursor), posts = posts)
}

/** Drop the slots of channels this device no longer holds. */
public fun retainChannels(log: ChannelPostLog, keep: Set<String>): ChannelPostLog =
    log.copy(channels = log.channels.filterKeys { it in keep })

// ----- Sync -------------------------------------------------------------

/** What one post decoded to, as far as the log cares. */
public sealed interface PostOutcome {
    /** A text post: kept. */
    public data class Text(val text: String) : PostOutcome

    /** Read and not a feed item (a metadata record, a malformed record, a post
     *  that failed to open under its epoch's seed): passed over. */
    public object Skip : PostOutcome

    /** No seed for the post's epoch and one may still arrive (the epoch is
     *  newer than every seed this device holds): the cursor stops before it
     *  so the next sync tries again. */
    public object Wait : PostOutcome
}

/** One fetched post, before decoding. */
public data class ChannelPostWire(val cursor: Long, val serverTsMs: Long, val payloadB64: String)

public data class ChannelPostPage(val posts: List<ChannelPostWire>)

/** The network and decrypt halves of a sync, injected. */
public interface ChannelSyncDeps {
    public suspend fun fetchPage(channelIdHex: String, after: Long, limit: Int): ChannelPostPage
    public suspend fun decode(channelIdHex: String, payloadB64: String): PostOutcome
}

/**
 * Bring one channel's slot up to date: fetch after its cursor, decode each
 * post, keep the text ones. Stops at the first post that must wait for a seed,
 * at an empty page, or after [CHANNEL_POST_MAX_PAGES] pages. A fetch failure
 * throws; the slot the caller holds is unchanged.
 */
public suspend fun syncChannelSlot(
    channelIdHex: String,
    slot: ChannelPostSlot?,
    deps: ChannelSyncDeps,
    nowMs: Long,
): ChannelPostSlot {
    var cursor = slot?.cursor ?: 0L
    val fresh = ArrayList<PersistedChannelPost>()
    for (page in 0 until CHANNEL_POST_MAX_PAGES) {
        val res = deps.fetchPage(channelIdHex, cursor, CHANNEL_POST_PAGE)
        if (res.posts.isEmpty()) break
        var blocked = false
        for (p in res.posts.sortedBy { it.cursor }) {
            if (p.cursor <= cursor) continue
            when (val outcome = deps.decode(channelIdHex, p.payloadB64)) {
                PostOutcome.Wait -> {
                    blocked = true
                }
                is PostOutcome.Text -> {
                    fresh += PersistedChannelPost(p.cursor, p.serverTsMs, outcome.text)
                    cursor = p.cursor
                }
                PostOutcome.Skip -> cursor = p.cursor
            }
            if (blocked) break
        }
        if (blocked || res.posts.size < CHANNEL_POST_PAGE) break
    }
    return mergeChannelPosts(slot, fresh, cursor, nowMs)
}
