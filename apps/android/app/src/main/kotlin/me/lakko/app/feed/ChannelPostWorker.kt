// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.content.Context
import android.util.Log
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelsClient
import java.util.concurrent.TimeUnit

/**
 * `docs/ux/channel-notifications.md` section 3 — while Lakko is not in front,
 * the followed channels are read every 15 minutes (the platform's shortest
 * periodic interval, with a network) so a new post can notify. There is no
 * push for channels yet; the system decides when the work runs (Doze and
 * app standby can stretch the interval), so a post can be announced late.
 *
 * The read here is READ-ONLY apart from the "last notified post" cursor: it
 * fetches each followed channel from the post log's stored cursor, opens the
 * posts under the subscriber seeds on disk and hands the read to
 * [ChannelPostNotifier]. It writes neither the channel-post log, nor the read
 * markers, nor the channel list (a metadata record read here is left for the
 * foreground sync, which reads it again from the same cursor and adopts it),
 * so it never races the app's own channel state. A channel this device
 * publishes to is skipped: its posts are the person's own and never notify.
 *
 * Skipped while the app is in front (the foreground sync reads then), while
 * no session is stored, and while anti-censorship is on but not armed in this
 * process: a background read never goes around the tunnel. Fails quietly.
 */
public class ChannelPostWorker(
    appContext: Context,
    params: WorkerParameters,
) : CoroutineWorker(appContext, params) {

    override suspend fun doWork(): Result {
        val ctx = applicationContext
        try {
            me.lakko.app.l10n.Localization.init(ctx)
            if (me.lakko.app.push.isAppForegrounded(ctx)) return Result.success()
            if (!backgroundNetworkAllowed(ctx)) return Result.success()
            val account = me.lakko.app.auth.SessionStore(ctx).load()?.accountIdB64
                ?.takeIf { it.isNotEmpty() } ?: return Result.success()
            if (me.lakko.app.account.AccountStoreGate.isSealed(account) ||
                me.lakko.app.account.AccountStoreGate.isWiped(account)
            ) {
                return Result.success()
            }
            withTimeoutOrNull(BUDGET_MS) {
                withContext(Dispatchers.IO) { readAndNotify(ctx, account) }
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w(TAG, "background channel read failed", e)
        }
        return Result.success()
    }

    private suspend fun readAndNotify(ctx: Context, account: String) {
        val stored = StoredChannels.load(ctx, account)
        if (stored.readable.isEmpty()) return
        val log = FeedSealedStore.loadChannelPostLog(ctx, account)
        val marks = FeedReadingStore.flow(ctx, account).value.channelRead
        val reads = ChannelsClient().use { client ->
            readChannelsOnce(
                channels = stored.readable,
                log = log,
                readMarkers = marks,
                nowMs = System.currentTimeMillis(),
                deps = { channel -> channelSyncDeps(client, channel) { _, _ -> } },
            )
        }
        if (reads.isEmpty()) return
        val notifier = ChannelPostNotifier(
            ChannelNotifiedStore.on(ctx),
            withdraw = { tag, gone -> ChannelNotificationPoster.withdraw(ctx, tag, gone) },
        ) { ChannelNotificationPoster.show(ctx, it) }
        notifier.onReads(account, reads, storedChannelFacts(ctx, account, stored))
    }

    public companion object {
        private const val TAG = "ChannelPostWorker"

        /** The unique periodic work name. */
        public const val WORK_NAME: String = "lakko.channel-posts.v1"

        /** The platform's shortest periodic interval. */
        public const val INTERVAL_MINUTES: Long = 15L

        /** A run's time budget, under the system's ten minutes (chosen). */
        private const val BUDGET_MS: Long = 4L * 60_000L

        /**
         * Schedule the periodic read; idempotent (an existing schedule is
         * kept). Best effort: WorkManager not initialised (a host test) is
         * logged, not thrown.
         */
        public fun schedule(context: Context) {
            try {
                val request = PeriodicWorkRequestBuilder<ChannelPostWorker>(INTERVAL_MINUTES, TimeUnit.MINUTES)
                    .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                    .build()
                WorkManager.getInstance(context.applicationContext)
                    .enqueueUniquePeriodicWork(WORK_NAME, ExistingPeriodicWorkPolicy.KEEP, request)
            } catch (e: Throwable) {
                Log.w(TAG, "channel post work not scheduled", e)
            }
        }
    }
}

/**
 * A background read may use the network only when anti-censorship is off, or
 * on and armed in this process; on but not armed, nothing is sent (a request
 * would go direct, around the tunnel the person chose).
 */
internal fun backgroundNetworkAllowed(context: Context): Boolean {
    val on = try {
        me.lakko.app.transport.antiCensorshipEnabled(context)
    } catch (_: Throwable) {
        true
    }
    return !on || me.lakko.app.transport.AntiCensorshipConfig.current.armed
}

/** What the background read knows of the account's channels, from disk. */
internal class StoredChannels(
    val held: List<ChannelHandle>,
    val publisherIds: Set<String>,
    val readable: List<ChannelReadable>,
) {
    /** The channels whose posts never notify on this device: those it
     *  publishes to, and those the account owns or co-publishes from another
     *  device (docs/ux/second-device.md section 4a). */
    val ownIds: Set<String> = channelsOwnedByAccount(held, publisherIds)

    companion object {
        fun load(context: Context, account: String): StoredChannels {
            val subscriberPrefs = me.lakko.app.channels.ChannelKeyStore.encryptedPrefs(context)
            val logStore = me.lakko.app.channels.SharedPreferencesChannelLogStore(subscriberPrefs)
            val held = logStore.loadChannels(account)
            val seeds = logStore.loadSubscribedSeeds(account)
            val publisherIds = me.lakko.app.channels.ChannelsUiState.publisherChannelIds(
                me.lakko.app.channels.ChannelPublisherKeyStore.encryptedPrefs(context),
                account,
            )
            return StoredChannels(held, publisherIds, subscriberReadables(held, seeds, publisherIds))
        }
    }
}

/**
 * The channels the background read opens: every held channel this device
 * follows with at least one seed, except those it publishes to.
 */
internal fun subscriberReadables(
    held: List<ChannelHandle>,
    seeds: Map<String, Map<UInt, ByteArray>>,
    publisherIds: Set<String>,
): List<ChannelReadable> = held.mapNotNull { c ->
    if (c.id.lowercase() in publisherIds) return@mapNotNull null
    val map = seeds[c.id]
    if (map.isNullOrEmpty()) return@mapNotNull null
    ChannelReadable(c.id, map, null, map.keys.maxOf { it.toLong() })
}

/**
 * Read each of [channels] once from its slot in [log] and return the reads
 * that moved a slot. Nothing is written. A channel the server does not answer
 * is passed over.
 */
internal suspend fun readChannelsOnce(
    channels: List<ChannelReadable>,
    log: ChannelPostLog,
    readMarkers: Map<String, Long>,
    nowMs: Long,
    deps: (ChannelReadable) -> ChannelSyncDeps,
): List<ChannelRead> {
    val reads = ArrayList<ChannelRead>()
    for (channel in channels) {
        val slot = log.channels[channel.idHex]
        try {
            val next = syncChannelSlot(channel.idHex, slot, deps(channel), nowMs)
            if (channelReadMoved(slot, next)) {
                reads += ChannelRead(channel.idHex, slot, next, readMarkers[channel.idHex])
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w("ChannelPostWorker", "background channel read failed for a channel", e)
        }
    }
    return reads
}

/** The read reached a record past [before]: something to judge. */
internal fun channelReadMoved(before: ChannelPostSlot?, after: ChannelPostSlot): Boolean =
    after.cursor > (before?.cursor ?: 0L) || after.posts.size != (before?.posts?.size ?: 0)

/** The facts of [stored]'s channels for the notifier, read from disk. */
internal fun storedChannelFacts(context: Context, account: String, stored: StoredChannels): ChannelPostNotifier.Facts =
    object : ChannelPostNotifier.Facts {
        override val held: Set<String> = stored.held.mapTo(HashSet()) { it.id.lowercase() }

        override fun muted(channelIdHex: String): Boolean =
            me.lakko.app.devices.sync.ConversationMute.isMuted(context, account, channelIdHex)

        override fun openAndVisible(channelIdHex: String): Boolean = VisibleChannel.isShown(channelIdHex)

        override fun own(channelIdHex: String): Boolean = channelIdHex.lowercase() in stored.ownIds

        override fun title(channelIdHex: String): String = channelNotificationTitle(stored.held, channelIdHex)

        override fun body(channelIdHex: String, post: PersistedChannelPost): String = channelNotificationBodyOf(post)
    }

/** The channel's name as its Chats row shows it. */
internal fun channelNotificationTitle(held: List<ChannelHandle>, channelIdHex: String): String {
    val strings = me.lakko.app.l10n.Localization.strings.value
    val channel = held.firstOrNull { it.id.equals(channelIdHex, ignoreCase = true) }
        ?: return strings["channels.fallback_name"]
    return me.lakko.app.ui.channels.channelTitle(channel, strings)
}

/** The Chats row's second line for [post], else the generic "New message". */
internal fun channelNotificationBodyOf(post: PersistedChannelPost): String {
    val strings = me.lakko.app.l10n.Localization.strings.value
    return channelNotificationBody(post, strings["notification.message.new"]) {
        me.lakko.app.ui.chat.channelRowPreviewLine(it, strings)
    }
}

/** The facts of the bound [channels] state for the notifier, in the
 *  foreground sync: mute from the synced flags ([isMuted]), own when this device
 *  publishes to the channel (as the channel's row counts it). */
public fun liveChannelFacts(
    channels: me.lakko.app.channels.ChannelsUiState,
    isMuted: (String) -> Boolean,
): ChannelPostNotifier.Facts = object : ChannelPostNotifier.Facts {
    private val list = channels.channels.value
    // docs/ux/second-device.md section 4a — a reader device of the owner or
    // a publisher announces no post of the channel either.
    private val publishing = channelsOwnedByAccount(list, channels.keysByChannel.value.keys) +
        list.filter { channels.accountRoleOf(it.id) != me.lakko.app.channels.ChannelAccountRole.SUBSCRIBER }
            .map { it.id.lowercase() }

    override val held: Set<String> = list.mapTo(HashSet()) { it.id.lowercase() }

    override fun muted(channelIdHex: String): Boolean = isMuted(channelIdHex)

    override fun openAndVisible(channelIdHex: String): Boolean = VisibleChannel.isShown(channelIdHex)

    override fun own(channelIdHex: String): Boolean = channelIdHex.lowercase() in publishing

    override fun title(channelIdHex: String): String = channelNotificationTitle(list, channelIdHex)

    override fun body(channelIdHex: String, post: PersistedChannelPost): String = channelNotificationBodyOf(post)
}

/**
 * The channels of [held] whose posts never notify on this device: the ones
 * it publishes to ([publisherIds]) and the ones the account owns or
 * co-publishes from another device (their stored role), lowercase. Only the
 * account's own side posts there, and a reader device cannot tell its own
 * account's post from another publisher's (docs/ux/second-device.md
 * section 4a).
 */
internal fun channelsOwnedByAccount(held: List<ChannelHandle>, publisherIds: Set<String>): Set<String> {
    val out = publisherIds.mapTo(HashSet()) { it.lowercase() }
    for (c in held) {
        if (c.role != me.lakko.app.channels.ChannelAccountRole.SUBSCRIBER) out += c.id.lowercase()
    }
    return out
}
