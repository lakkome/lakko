// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.ClassifiedPost
import me.lakko.app.channels.classifyChannelPlaintext
import me.lakko.app.chat.from

/**
 * Channels join the foreground poll (ADR-0063 section 4): while the app is in
 * front, the posts of every channel this device can read are fetched,
 * decrypted and filed into the channel-post log the feed ranks. Mirror of
 * `apps/web/src/feed/ChannelPostSync.tsx`.
 *
 * Driven from the session shell: a sync runs once the channel state is bound,
 * every [CHANNEL_POST_SYNC_INTERVAL_MS] while the app is started, and again
 * on every return to the foreground. Only one sync runs at a time and it is
 * the only writer of the log, so there is no merge between writers.
 *
 * A post is opened under the seed of its own epoch, never another (ADR-0031,
 * invariant #4). A post whose epoch is newer than every seed this device holds
 * stops the cursor, so it is tried again once the seed arrives; an older one
 * without a seed is past the retained window and is passed over.
 */
public const val CHANNEL_POST_SYNC_INTERVAL_MS: Long = 60_000L

/** Pending "sync now" requests; conflated, so a burst is one extra sync. */
private val syncRequests = Channel<Unit>(Channel.CONFLATED)

/**
 * Sync now rather than at the next tick. Called after this device publishes
 * a post, so the author sees it in the feed at once instead of up to a minute
 * later. Picked up by [awaitChannelPostSyncTick]; while no sync loop runs the
 * request waits for the next one, which syncs at once on start anyway.
 */
public fun requestChannelPostSync() {
    syncRequests.trySend(Unit)
}

/** Wait until the next sync is due: the interval passes or a sync is requested. */
public suspend fun awaitChannelPostSyncTick() {
    withTimeoutOrNull(CHANNEL_POST_SYNC_INTERVAL_MS) { syncRequests.receive() }
}

/** A listener of [subscribeChannelReads]. */
public fun interface ChannelReadListener {
    public suspend fun onReads(accountIdB64: String, reads: List<ChannelRead>)
}

private val readListeners = java.util.concurrent.CopyOnWriteArraySet<ChannelReadListener>()

/**
 * `docs/ux/channel-notifications.md` — hear every channel read a sync makes
 * (the session shell's loop and the feed's pull to refresh alike), with the
 * slot before and after it and the read marker before it, once the log holding
 * it is saved. Returns the unsubscribe. As web `subscribeChannelReads`.
 */
public fun subscribeChannelReads(listener: ChannelReadListener): () -> Unit {
    readListeners += listener
    return { readListeners -= listener }
}

private suspend fun publishChannelReads(accountIdB64: String, reads: List<ChannelRead>) {
    if (reads.isEmpty()) return
    for (l in readListeners) {
        try {
            l.onReads(accountIdB64, reads)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w("ChannelPostSync", "channel read listener failed", e)
        }
    }
}

/** One sync at a time across every [ChannelPostSync] (the session shell's
 *  loop and the feed's pull to refresh), so the log keeps a single writer. */
private val syncRunning = Mutex()

/** A channel this device can open posts of, and how. */
internal class ChannelReadable(
    val idHex: String,
    val seedByEpoch: Map<UInt, ByteArray>?,
    /** The publisher's current content seed (the channel view's posture). */
    val publisherSeed: ByteArray?,
    val newestSeedEpoch: Long,
)

public class ChannelPostSync(
    private val context: Context,
    private val channels: ChannelsUiState,
    private val client: ChannelsClient,
) {
    private val running = syncRunning

    private fun readableChannels(): List<ChannelReadable> {
        val out = ArrayList<ChannelReadable>()
        val own = channels.keysByChannel.value
        val seeds = channels.subscribedSeedsByChannel.value
        val epochs = channels.epochByChannel.value
        for (c in channels.channels.value) {
            val keys = own[c.id]
            if (keys != null) {
                // ADR-0087 section 15 — the composer's seed opens only the
                // epoch it serves; the epochs before it from the seeds kept.
                val served = keys.seedEpoch ?: epochs[c.id]
                if (served == null) {
                    out += ChannelReadable(c.id, null, keys.seed, Long.MAX_VALUE)
                } else {
                    val map = (seeds[c.id] ?: emptyMap()) + (served to keys.seed)
                    out += ChannelReadable(c.id, map, null, map.keys.maxOf { it.toLong() })
                }
                continue
            }
            val map = seeds[c.id]
            if (map.isNullOrEmpty()) continue
            out += ChannelReadable(c.id, map, null, map.keys.maxOf { it.toLong() })
        }
        return out
    }

    /** One sync pass. Skipped (not queued) when one is already running: the
     *  timer or the next resume brings the rest. Reports how many channels it
     *  tried and how many answered (`docs/ux/feed.md`, "refresh failed
     *  (network)"); a skipped pass reports [SourceCheck.NONE], so it says
     *  nothing, and a pass that fails as a whole reports every channel tried
     *  and none answered. */
    public suspend fun syncOnce(): SourceCheck {
        val account = channels.boundAccount ?: return SourceCheck.NONE
        if (!running.tryLock()) return SourceCheck.NONE
        return try {
            withContext(Dispatchers.IO) { syncAll(account) }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w(TAG, "channel post sync error", e)
            SourceCheck(readableChannels().size, 0)
        } finally {
            running.unlock()
        }
    }

    /**
     * A sync for the feed's pull to refresh (`docs/ux/feed.md` section 5):
     * waits for a sync already running, then runs one, and reports how many
     * channels it tried and how many answered. A pass that fails as a whole
     * reports every channel tried and none answered.
     */
    public suspend fun syncAndWait(): SourceCheck {
        val account = channels.boundAccount ?: return SourceCheck.NONE
        return running.withLock {
            try {
                withContext(Dispatchers.IO) { syncAll(account) }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                Log.w(TAG, "channel post sync error", e)
                val tried = readableChannels().size
                SourceCheck(tried, 0)
            }
        }
    }

    private suspend fun syncAll(account: String): SourceCheck {
        val readable = readableChannels()
        var answered = 0
        val (before, dropped) = FeedSealedStore.loadChannelPostLogForSync(context, account)
        var log = retainChannels(before, channels.channels.value.map { it.id }.toSet())
        // A stored log of an older version is fetched again from the start;
        // what it had already looked at is not new (as web, markers survive,
        // and none falls below that log's cursor).
        if (dropped != null) FeedReadingStore.update(context, account) { readFloorAfterDrop(it, dropped) }
        var changed = dropped != null || log.channels.size != before.channels.size
        // docs/ux/channel-notifications.md — the read markers as they stand
        // before this read (after the drop floor above, before the seeding
        // below): the rule tells a first read after subscribing (no marker
        // yet) from a refetch.
        val marksBefore = FeedReadingStore.flow(context, account).value.channelRead
        val reads = ArrayList<ChannelRead>()

        for (channel in readable) {
            val deps = channelSyncDeps(client, channel) { id, meta -> channels.adoptSharedMeta(id, meta) }
            val slot = log.channels[channel.idHex]
            try {
                val next = syncChannelSlot(channel.idHex, slot, deps, System.currentTimeMillis())
                if (next.cursor != slot?.cursor || next.posts.size != slot.posts.size) {
                    log = log.copy(channels = log.channels + (channel.idHex to next))
                    changed = true
                    if (channelReadMoved(slot, next)) {
                        reads += ChannelRead(channel.idHex, slot, next, marksBefore[channel.idHex])
                    }
                }
                answered += 1
                // docs/ux/feed.md section 5 — a channel's read marker starts
                // at the newest post held when it is first synced: the slot's
                // cursor, never below its newest post (as web and iOS).
                val newest = channelReadSeed(next.cursor, next.posts.map { it.cursor })
                FeedReadingStore.update(context, account) { seedChannelRead(it, channel.idHex, newest) }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                // A channel the server does not answer for this time; the next
                // sync tries again from the same cursor.
                Log.w(TAG, "channel post sync failed", e)
            }
        }

        if (changed) FeedSealedStore.saveChannelPostLog(context, account, log)
        publishChannelReads(account, reads)
        return SourceCheck(readable.size, answered)
    }

    private companion object {
        const val TAG = "ChannelPostSync"
    }
}

/** The network and decode half of one channel's read: the page fetch over
 *  [client], and the post decode under [channel]'s seeds; a metadata record
 *  goes to [onMeta]. */
internal fun channelSyncDeps(
    client: ChannelsClient,
    channel: ChannelReadable,
    onMeta: (String, me.lakko.app.channels.SharedChannelMeta) -> Unit,
): ChannelSyncDeps = object : ChannelSyncDeps {
    override suspend fun fetchPage(channelIdHex: String, after: Long, limit: Int): ChannelPostPage {
        val res = client.fetchPosts(channelIdHex, after.coerceAtLeast(0L).toULong(), limit)
        return ChannelPostPage(
            res.posts.map { ChannelPostWire(it.cursor.toLong(), it.serverTsMs.toLong(), it.payloadB64) },
        )
    }

    override suspend fun decode(channelIdHex: String, payloadB64: String): PostOutcome =
        decodeChannelPost(channel, payloadB64, onMeta)
}

/** One post of [channel], opened and classified for the log. */
internal fun decodeChannelPost(
    channel: ChannelReadable,
    payloadB64: String,
    onMeta: (String, me.lakko.app.channels.SharedChannelMeta) -> Unit,
): PostOutcome {
    val plaintext: ByteArray = if (channel.publisherSeed != null) {
        // A publisher holds the current content seed only, as the channel
        // view does; a post it cannot open under it is passed over.
        try {
            ChannelsComposer.verifyAndDecrypt(channel.publisherSeed, channel.idHex, payloadB64)
        } catch (_: Throwable) {
            return PostOutcome.Skip
        }
    } else {
        val result = try {
            ChannelsComposer.verifyAndDecryptForEpochs(channel.seedByEpoch.orEmpty(), channel.idHex, payloadB64)
        } catch (_: Throwable) {
            return PostOutcome.Skip
        }
        when (result) {
            is ChannelsComposer.SubscriberPostResult.Decoded -> result.plaintext
            is ChannelsComposer.SubscriberPostResult.MissingEpochSeed ->
                return if (result.epochId.toLong() > channel.newestSeedEpoch) PostOutcome.Wait else PostOutcome.Skip
            ChannelsComposer.SubscriberPostResult.Unreadable -> return PostOutcome.Skip
        }
    }
    return when (val c = classifyChannelPlaintext(plaintext)) {
        // ADR-0087 section 13 — a post's plaintext is the chat envelope:
        // the feed files its text; an edit, a delete or anything else
        // that is no post is skipped.
        is ClassifiedPost.Text -> {
            val body = try {
                uniffi.lakko_ffi_uniffi.parseBodyEnvelope(c.text.toByteArray(Charsets.UTF_8))
            } catch (_: Throwable) {
                null
            }
            if (body == null) {
                // Without the core (a host test) the text is the post.
                PostOutcome.Text(c.text)
            } else {
                // The attachments go with the text: their kinds, so a
                // photo, file or GIF posted without a caption still names
                // itself on the channel's row (as a group's row does), and
                // their references, so the feed draws them in place.
                feedPostOutcome(
                    kind = body.kind,
                    text = body.text,
                    media = body.attachments.map {
                        PersistedChannelPostMedia(
                            mime = it.mime,
                            kind = it.kind,
                            ref = me.lakko.app.chat.PersistedAttachment.from(it),
                        )
                    },
                    envelope = c.text.toByteArray(Charsets.UTF_8),
                )
            }
        }
        is ClassifiedPost.Meta -> {
            // A metadata record read here names the channel in the feed
            // and the list without opening it (ADR-0051).
            onMeta(channel.idHex, c.meta)
            PostOutcome.Skip
        }
        ClassifiedPost.Malformed, is ClassifiedPost.BanList -> PostOutcome.Skip
    }
}

/**
 * What the feed keeps of a decoded channel post (ADR-0063 / ADR-0092): a chat
 * post with words or attachments; a location, contact card or article with
 * its envelope (and an article's images, which are its attachments). A poll
 * or a checklist, an edit, a delete or anything else is passed over: votes
 * and ticks belong to the channel view.
 */
internal fun feedPostOutcome(
    kind: uniffi.lakko_ffi_uniffi.MessageBodyKind,
    text: String,
    media: List<PersistedChannelPostMedia>,
    envelope: ByteArray,
): PostOutcome = when (kind) {
    uniffi.lakko_ffi_uniffi.MessageBodyKind.CHAT ->
        if (text.isNotEmpty() || media.isNotEmpty()) PostOutcome.Text(text, media) else PostOutcome.Skip
    uniffi.lakko_ffi_uniffi.MessageBodyKind.LOCATION,
    uniffi.lakko_ffi_uniffi.MessageBodyKind.CONTACT_CARD,
    uniffi.lakko_ffi_uniffi.MessageBodyKind.ARTICLE,
    -> PostOutcome.Text(text, media, java.util.Base64.getEncoder().encodeToString(envelope))
    else -> PostOutcome.Skip
}
