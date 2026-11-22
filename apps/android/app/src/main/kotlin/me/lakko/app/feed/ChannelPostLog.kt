// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.serialization.Serializable

/**
 * The channel-post log (ADR-0063 section 3): the posts of subscribed and own
 * channels this device has decrypted, kept so the feed can rank them without
 * opening each channel. Mirror of `apps/web/src/feed/channel-post-log.ts`.
 *
 * Pure: the codec, the merge and the per-channel sync live here with their
 * network and FFI dependencies injected, so the host JVM suite exercises them
 * with no Keystore, no network and no native library. [FeedSealedStore] seals
 * this payload under a Keystore key; [ChannelPostSync] drives the sync while
 * the app is in front.
 *
 * Version 2 keeps posts with attachments and no text, with each
 * attachment's kind, so a channel's row in Chats names a caption-less photo,
 * file or GIF as a group's row does; a version-1 log (which passed those
 * posts over) is dropped and fetched again. As web.
 *
 * Version 3 also keeps each attachment's reference (blob id, content key,
 * media metadata), so the feed draws a channel's photo, GIF, voice or video
 * message in place as it draws a group's (`docs/ux/feed.md` section 3, base
 * scenario 32). The log is sealed under a Keystore key ([FeedSealedStore]),
 * as the group message log that holds the same references is. A version-2
 * log (names only) is dropped and fetched again.
 *
 * Version 4 also keeps a content post's chat envelope (ADR-0092: a location,
 * contact card or article), so the feed draws it and a forward from it
 * carries it. A version-3 log is dropped and fetched again. As web.
 *
 * Bounds, both chosen: posts older than the server's 30-day retention are
 * dropped, and at most [CHANNEL_POST_CAP] posts are kept per channel, newest
 * kept.
 */
public const val CHANNEL_POST_LOG_VERSION: Int = 4

/** Most posts kept per channel (chosen, ADR-0063 open question 1). */
public const val CHANNEL_POST_CAP: Int = 500

/** The server's post retention; older posts are dropped from the log. */
public const val CHANNEL_POST_RETENTION_MS: Long = 30L * 86_400_000L

/** Posts asked for per request. */
public const val CHANNEL_POST_PAGE: Int = 100

/** Pages fetched per channel per sync, so one busy channel cannot hold the
 *  loop; the rest arrives on the next sync. */
public const val CHANNEL_POST_MAX_PAGES: Int = 5

/** Most attachments recorded per post: the row names the first and counts
 *  the rest (chosen, as web). */
public const val CHANNEL_POST_MEDIA_CAP: Int = 16

/** What the log keeps of one attachment: what names its kind on a list row
 *  ("Photo", "GIF", "File"), and the reference the feed downloads and opens
 *  it by (`null` when it was not kept, or for a test row). */
@Serializable
public data class PersistedChannelPostMedia(
    val mime: String? = null,
    val kind: String? = null,
    val ref: me.lakko.app.chat.PersistedAttachment? = null,
)

/** One decrypted post: its text, its attachments, or both, and (a location,
 *  contact card or article) its content. */
@Serializable
public data class PersistedChannelPost(
    /** The server's per-channel cursor, the post's id in the feed. */
    val cursor: Long,
    /** Server receive time, unix ms. */
    val serverTsMs: Long,
    val text: String,
    /** The post's attachments, in order; empty when it has none. */
    val media: List<PersistedChannelPostMedia> = emptyList(),
    /** ADR-0092 — the post's chat envelope, standard base64, when it carries
     *  a location, contact card or article (re-parsed when read). Absent in
     *  a row written before it. */
    val contentEnvelopeB64: String? = null,
    /** ADR-0099 section 2 — the post's wire as served (base64url), what a
     *  report from the channel's info includes when this post is picked. */
    val wireB64: String? = null,
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
    // Always written: the store's JSON leaves default values out, so without
    // this a log of any version was written with no version at all and read
    // back as the current one (a version-1 log survived the bump to 2).
    @OptIn(kotlinx.serialization.ExperimentalSerializationApi::class)
    @kotlinx.serialization.EncodeDefault(kotlinx.serialization.EncodeDefault.Mode.ALWAYS)
    val version: Int = CHANNEL_POST_LOG_VERSION,
    val channels: Map<String, ChannelPostSlot> = emptyMap(),
)

/** The JSON shape of the stored log: unknown keys ignored, nulls left out. */
private val channelPostLogJson = kotlinx.serialization.json.Json {
    ignoreUnknownKeys = true
    explicitNulls = false
}

/** The stored text of [log]; it always names its version. */
public fun encodeChannelPostLog(log: ChannelPostLog): String =
    channelPostLogJson.encodeToString(ChannelPostLog.serializer(), log)

/**
 * The per-channel cursors of a stored log [decodeChannelPostLog] refuses
 * (a log of an older version): how far its sync had looked in each channel.
 * Every post at or below such a cursor was already looked at, so the refetch
 * that follows the drop must not count it as new ([readFloorAfterDrop]).
 * Empty when the text is not a log at all, or a channel id or cursor is
 * malformed (that channel alone is left out).
 */
public fun droppedLogCursors(text: String): Map<String, Long> {
    val obj = try {
        channelPostLogJson.parseToJsonElement(text) as? kotlinx.serialization.json.JsonObject
    } catch (_: Throwable) {
        null
    } ?: return emptyMap()
    val channels = obj["channels"] as? kotlinx.serialization.json.JsonObject ?: return emptyMap()
    val out = LinkedHashMap<String, Long>()
    for ((id, slot) in channels) {
        if (!CHANNEL_ID_RE.matches(id)) continue
        val o = slot as? kotlinx.serialization.json.JsonObject ?: continue
        val cursor = (o["cursor"] as? kotlinx.serialization.json.JsonPrimitive)?.content?.toLongOrNull() ?: 0L
        val newestPost = (o["posts"] as? kotlinx.serialization.json.JsonArray).orEmpty().maxOfOrNull { p ->
            ((p as? kotlinx.serialization.json.JsonObject)?.get("cursor") as? kotlinx.serialization.json.JsonPrimitive)
                ?.content?.toLongOrNull() ?: 0L
        } ?: 0L
        val looked = maxOf(cursor, newestPost)
        if (looked > 0) out[id] = looked
    }
    return out
}

/**
 * Read a stored log, `null` when it cannot be used (the caller starts empty
 * and fetches again). A log that names no version was written before the
 * version was always written, i.e. version 1, and is refused like any other
 * version but the current one.
 */
public fun decodeChannelPostLog(text: String): ChannelPostLog? {
    val element = try {
        channelPostLogJson.parseToJsonElement(text)
    } catch (_: Throwable) {
        return null
    }
    val obj = element as? kotlinx.serialization.json.JsonObject ?: return null
    val version = (obj["version"] as? kotlinx.serialization.json.JsonPrimitive)
        ?.let { if (it.isString) null else it.content.toIntOrNull() } ?: 1
    if (version != CHANNEL_POST_LOG_VERSION) return null
    val raw = try {
        channelPostLogJson.decodeFromJsonElement(ChannelPostLog.serializer(), obj)
    } catch (_: Throwable) {
        return null
    }
    return sanitizeChannelPostLog(raw)
}

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
    /** `docs/ux/message-actions.md` section 9 — posts a publisher deleted:
     *  they leave the log, so no list preview, unread count or feed item
     *  ever shows them. */
    deleted: Set<Long> = emptySet(),
): ChannelPostSlot {
    val byCursor = LinkedHashMap<Long, PersistedChannelPost>()
    for (p in slot?.posts.orEmpty()) byCursor[p.cursor] = p
    for (p in fresh) byCursor[p.cursor] = p
    for (c in deleted) byCursor.remove(c)
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
    /** A post the feed shows (text, attachments or both): kept, with, for a
     *  location, contact card or article, its envelope. */
    public data class Text(
        val text: String,
        val media: List<PersistedChannelPostMedia> = emptyList(),
        val contentEnvelopeB64: String? = null,
    ) : PostOutcome

    /** Read and not a feed item (a metadata record, a malformed record, a post
     *  that failed to open under its epoch's seed): passed over. */
    public object Skip : PostOutcome

    /** No seed for the post's epoch and one may still arrive (the epoch is
     *  newer than every seed this device holds): the cursor stops before it
     *  so the next sync tries again. */
    public object Wait : PostOutcome

    /** ADR-0023 — a publisher deleted the post at [target]: it leaves the
     *  log (`docs/ux/message-actions.md` section 9). */
    public data class Delete(val target: Long) : PostOutcome
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
    val deleted = HashSet<Long>()
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
                    fresh += PersistedChannelPost(
                        p.cursor,
                        p.serverTsMs,
                        outcome.text,
                        outcome.media.take(CHANNEL_POST_MEDIA_CAP),
                        outcome.contentEnvelopeB64,
                        wireB64 = p.payloadB64,
                    )
                    cursor = p.cursor
                }
                PostOutcome.Skip -> cursor = p.cursor
                is PostOutcome.Delete -> {
                    deleted += outcome.target
                    fresh.removeAll { it.cursor == outcome.target }
                    cursor = p.cursor
                }
            }
            if (blocked) break
        }
        if (blocked || res.posts.size < CHANNEL_POST_PAGE) break
    }
    return mergeChannelPosts(slot, fresh, cursor, nowMs, deleted)
}
