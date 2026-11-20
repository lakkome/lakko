// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Base64
import android.util.Log
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.decodeChannelPost
import uniffi.lakko_ffi_uniffi.parseBodyEnvelope
import uniffi.lakko_ffi_uniffi.verifyAndDecryptChannelPost

/**
 * ADR-0087 sections 6, 7 and 13 — the read side of one channel, for the
 * conversation screen: which seeds this device holds for each epoch (a
 * publisher's current key, the per-subscriber envelopes, a public channel's
 * public seeds under its read key), the posts fetched to the end, each opened
 * under its epoch's seed and classified (the metadata record and a ban list
 * are adopted, never shown), then folded ([foldChannelTimeline]).
 *
 * Holds per channel what it opened, so the comment and reaction keys can be
 * derived for a post's epoch without opening it again.
 */
public class ChannelReader(
    private val context: Context,
    private val state: ChannelsUiState,
    private val client: ChannelsClient,
    private val channelId: String,
) {
    /** The epoch each readable post was sealed in. */
    private val epochOfPost = HashMap<Long, UInt>()

    /** The server time of each post read. */
    private val tsOfPost = HashMap<Long, Long>()

    /** The server time of [cursor]'s post, once read. */
    public fun tsOf(cursor: Long): Long? = tsOfPost[cursor]

    /** Section 13a — the newest ban list read in the channel. */
    public var banList: uniffi.lakko_ffi_uniffi.FfiBanList? = null
        private set

    /** Whether this account reads the channel through a subscription only
     *  (no publisher keys). */
    public fun isPublisher(): Boolean = state.keysByChannel.value.containsKey(channelId)

    /** The seed of [epoch] this device holds, or `null`. */
    public fun seedFor(epoch: UInt): ByteArray? {
        if (isPublisher()) {
            val current = state.epochByChannel.value[channelId]
            val keys = state.keysByChannel.value[channelId]
            if (keys != null && current == epoch) return keys.seed
        }
        return state.subscribedSeeds(channelId)[epoch]
    }

    /** The epoch [cursor]'s post was sealed in, once read. */
    public fun epochOf(cursor: Long): UInt? = epochOfPost[cursor]

    /** Every post read so far, by cursor, and the decoded row of each. */
    private val viewed = LinkedHashMap<Long, PostView>()
    private val rowOf = HashMap<Long, ChannelRow>()

    /** Where the next read resumes (only new posts are fetched). */
    private var nextAfter = 0uL

    /** When a public seed was last asked for an epoch that had none. */
    private val publicSeedMissAt = HashMap<UInt, Long>()

    /** Fetch the posts not yet read, open them (and again any earlier one
     *  that did not open, once a seed arrives) and fold the whole set.
     *  Throws on a network failure of the post read itself. */
    public suspend fun read(): ChannelTimeline {
        var after = nextAfter
        while (true) {
            val page = client.fetchPosts(channelId, after, READ_PAGE)
            for (p in page.posts) viewed[p.cursor.toLong()] = p
            if (page.posts.isEmpty() || page.nextAfter <= after) break
            after = page.nextAfter
            // A short page is the end: no second request to learn that the
            // next one is empty (one read per resource per pass).
            if (page.posts.size < READ_PAGE) break
            if (viewed.size >= MAX_POSTS_READ) break
        }
        nextAfter = after
        if (isPublisher()) {
            try {
                state.setEpoch(channelId, client.fetchEpoch(channelId))
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                // The held epoch stays.
            }
        }
        val rows = ArrayList<ChannelRow>(viewed.size)
        for ((cursor, p) in viewed) {
            val held = rowOf[cursor]
            val row = if (held == null || held.content == ChannelRowContent.Unavailable) decode(p) else held
            rowOf[cursor] = row
            rows += row
        }
        state.setPosts(channelId, viewed.values.toList())
        return foldChannelTimeline(rows)
    }

    /**
     * ADR-0089 step 5 — the timeline from what this device already holds,
     * with no network read: the posts read this session (by this reader, or
     * kept in [ChannelsUiState.postsByChannel]). `null` when nothing is held.
     */
    public suspend fun readHeld(): ChannelTimeline? {
        if (viewed.isEmpty()) {
            for (p in state.postsByChannel.value[channelId].orEmpty()) viewed[p.cursor.toLong()] = p
        }
        if (viewed.isEmpty()) return null
        val rows = ArrayList<ChannelRow>(viewed.size)
        for ((cursor, p) in viewed) {
            val row = rowOf[cursor] ?: decode(p, offline = true)
            rowOf[cursor] = row
            rows += row
        }
        return foldChannelTimeline(rows)
    }

    private suspend fun decode(p: PostView, offline: Boolean = false): ChannelRow {
        val cursor = p.cursor.toLong()
        val ts = p.serverTsMs.toLong()
        tsOfPost[cursor] = ts
        val wire = Base64.decode(p.payloadB64, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
        val post = try {
            decodeChannelPost(wire)
        } catch (_: Throwable) {
            return ChannelRow(cursor, ts, 0u, ChannelRowContent.Hidden)
        }
        try {
            val epoch = post.epochId()
            val seed = seedFor(epoch) ?: (if (offline) null else publicSeed(epoch))
                ?: return ChannelRow(cursor, ts, epoch, ChannelRowContent.Unavailable)
            val plaintext = try {
                verifyAndDecryptChannelPost(seed, ChannelsComposer.channelIdFromHex(channelId), post)
            } catch (_: Throwable) {
                return ChannelRow(cursor, ts, epoch, ChannelRowContent.Unavailable)
            }
            epochOfPost[cursor] = epoch
            return ChannelRow(cursor, ts, epoch, contentOf(plaintext), wire = wire)
        } finally {
            post.close()
        }
    }

    private fun contentOf(plaintext: ByteArray): ChannelRowContent =
        when (val c = classifyChannelPlaintext(plaintext)) {
            is ClassifiedPost.Meta -> {
                state.adoptSharedMeta(channelId, c.meta)
                ChannelRowContent.Hidden
            }
            is ClassifiedPost.BanList -> {
                val held = banList
                if (held == null || uniffi.lakko_ffi_uniffi.banListIsNewer(c.list.updatedAtMs, held.updatedAtMs)) {
                    banList = c.list
                }
                ChannelRowContent.Hidden
            }
            ClassifiedPost.Malformed -> ChannelRowContent.Hidden
            is ClassifiedPost.Text -> bodyOf(c.text.toByteArray(Charsets.UTF_8))
        }

    private fun bodyOf(bytes: ByteArray): ChannelRowContent {
        val body = try {
            parseBodyEnvelope(bytes)
        } catch (_: Throwable) {
            return ChannelRowContent.Hidden
        }
        return when (body.kind) {
            MessageBodyKind.CHAT -> ChannelRowContent.Post(body.text, body.attachments, body.channelInvite)
            MessageBodyKind.EDIT -> body.target?.let { ChannelRowContent.Edit(it, body.text) } ?: ChannelRowContent.Hidden
            MessageBodyKind.DELETE -> body.target?.let { ChannelRowContent.Delete(it) } ?: ChannelRowContent.Hidden
            // Section 13 — no typing, no read marks, no reply and no
            // in-band reactions in a channel; a service envelope is never
            // a post.
            else -> ChannelRowContent.Hidden
        }
    }

    /**
     * Section 6 — a public channel's seed for [epoch], opened under its read
     * key and kept like a subscriber's envelope. `null` when the channel is
     * not public, no read key is held or none is filed for that epoch.
     */
    private suspend fun publicSeed(epoch: UInt): ByteArray? {
        val channel = state.channels.value.firstOrNull { it.id == channelId } ?: return null
        if (!channel.sharedListed && ChannelReadKeys.get(context, state.boundAccount, channelId) == null) return null
        val readKey = readKey() ?: return null
        // An epoch with no seed filed is asked again at most once a minute,
        // not on every read of the open channel.
        val now = System.currentTimeMillis()
        if (now - (publicSeedMissAt[epoch] ?: 0L) < PUBLIC_SEED_RETRY_MS) return null
        val wire = try {
            client.fetchPublicSeed(channelId, epoch)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        } ?: run {
            publicSeedMissAt[epoch] = now
            return null
        }
        return try {
            uniffi.lakko_ffi_uniffi.openPublicSeedEnvelope(
                readKey,
                ChannelsComposer.channelIdFromHex(channelId),
                epoch,
                wire,
            ).also { state.setSubscribedSeed(channelId, epoch, it) }
        } catch (_: Throwable) {
            Log.i(TAG, "a public seed did not open")
            null
        }
    }

    /** The channel's read key: the owner derives it, anyone else holds the
     *  one the record or the link carried. */
    private suspend fun readKey(): ByteArray? {
        ChannelReadKeys.get(context, state.boundAccount, channelId)?.let { return it }
        val pub = state.publisher ?: return null
        if (!isPublisher() || state.joinedGroupIdOf(channelId) != null) return null
        return pub.withIdentity(persist = false) { ChannelDiscovery.readKey(it, channelId) }
    }

    private companion object {
        private const val TAG = "ChannelReader"

        /** A bound on one read (a channel keeps 30 days of posts). Chosen. */
        private const val MAX_POSTS_READ: Int = 2_000
        private const val PUBLIC_SEED_RETRY_MS: Long = 60_000L
    }
}

/**
 * Section 6 — a public channel's read key as the record or a link carried it,
 * per account and channel. Not secret beyond the channel (it is in the public
 * record), so plain preferences.
 */
public object ChannelReadKeys {
    private const val PREFS = "lakko.channels.read-keys"

    private fun key(accountIdB64: String, channelId: String): String = "$accountIdB64|$channelId"

    public fun get(context: Context, accountIdB64: String?, channelId: String): ByteArray? {
        val acct = accountIdB64 ?: return null
        val raw = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(key(acct, channelId), null) ?: return null
        return try {
            Base64.decode(raw, Base64.NO_WRAP).takeIf { it.size == 32 }
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    public fun put(context: Context, accountIdB64: String, channelId: String, readKey: ByteArray) {
        if (readKey.size != 32 || readKey.all { it == 0.toByte() }) return
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(key(accountIdB64, channelId), Base64.encodeToString(readKey, Base64.NO_WRAP))
            .apply()
    }

    /** Forget device / Delete account. */
    public fun forgetAccount(context: Context, accountIdB64: String) {
        val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val editor = prefs.edit()
        prefs.all.keys.filter { it.startsWith("$accountIdB64|") }.forEach(editor::remove)
        editor.apply()
    }
}

/** Posts asked for per page of a read; a page shorter than this is the
 *  newest one (the service's default page is 50). */
private const val READ_PAGE: Int = 50
