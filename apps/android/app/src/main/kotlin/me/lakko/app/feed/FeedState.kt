// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.serialization.Serializable

/**
 * The person's own feeds on this device (ADR-0063 section 6): each a rule in
 * the core's canonical encoding. Mirror of `apps/web/src/feed/feed-store.ts`;
 * sealed per account by [FeedSealedStore]. They do not follow the person to
 * other devices.
 *
 * ```text
 *   plaintext  { version: 1, feeds: [{ id, rule: <base64url rule bytes> }] }
 * ```
 *
 * The codec is lenient per entry: a feed whose id or bytes are malformed is
 * dropped alone. Whether the bytes are a valid rule is the core's to say when
 * the feed is opened.
 */
public const val FEED_STATE_VERSION: Int = 1

/** Most feeds kept (chosen): a switcher row, not a library. */
public const val MAX_SAVED_FEEDS: Int = 20

public class SavedFeed(
    /** Local id, random, stable across edits. */
    public val id: String,
    /** The rule's canonical bytes. */
    public val rule: ByteArray,
)

public data class FeedState(val feeds: List<SavedFeed> = emptyList())

@Serializable
internal data class FeedStateWire(
    val version: Int = FEED_STATE_VERSION,
    val feeds: List<SavedFeedWire> = emptyList(),
)

@Serializable
internal data class SavedFeedWire(val id: String = "", val rule: String = "")

private val ID_RE = Regex("^[A-Za-z0-9_-]{8,32}$")
private val urlEncoder = java.util.Base64.getUrlEncoder().withoutPadding()
private val urlDecoder = java.util.Base64.getUrlDecoder()

internal fun decodeFeedState(wire: FeedStateWire?): FeedState {
    if (wire == null || wire.version != FEED_STATE_VERSION) return FeedState()
    val feeds = ArrayList<SavedFeed>()
    for (f in wire.feeds) {
        if (!ID_RE.matches(f.id) || feeds.any { it.id == f.id }) continue
        val bytes = try {
            urlDecoder.decode(f.rule)
        } catch (_: IllegalArgumentException) {
            continue
        }
        feeds += SavedFeed(f.id, bytes)
        if (feeds.size >= MAX_SAVED_FEEDS) break
    }
    return FeedState(feeds)
}

internal fun encodeFeedState(state: FeedState): FeedStateWire =
    FeedStateWire(FEED_STATE_VERSION, state.feeds.map { SavedFeedWire(it.id, urlEncoder.encodeToString(it.rule)) })

/** A fresh local feed id. */
public fun newFeedId(random: java.security.SecureRandom = java.security.SecureRandom()): String {
    val bytes = ByteArray(12)
    random.nextBytes(bytes)
    return urlEncoder.encodeToString(bytes)
}

/** Add or replace a feed; a new one goes to the end. */
public fun upsertFeed(state: FeedState, feed: SavedFeed): FeedState {
    val i = state.feeds.indexOfFirst { it.id == feed.id }
    if (i >= 0) return FeedState(state.feeds.toMutableList().also { it[i] = feed })
    return FeedState((state.feeds + feed).take(MAX_SAVED_FEEDS))
}

public fun removeFeed(state: FeedState, id: String): FeedState = FeedState(state.feeds.filter { it.id != id })
