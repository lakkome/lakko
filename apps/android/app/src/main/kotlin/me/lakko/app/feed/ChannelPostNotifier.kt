// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Channel post notifications (`docs/ux/channel-notifications.md`): hears the
 * channel reads of the foreground sync ([ChannelPostSync]) and of the
 * background work ([ChannelPostWorker]), asks the shared rule which post
 * notifies ([decideChannelRead]), shows it, and keeps the per-channel "last
 * notified post" cursor on this device ([ChannelNotifiedStore]). Mirror of
 * `apps/web/src/feed/ChannelPostNotifier.tsx`.
 *
 * Title: the channel's name as its Chats row shows it; body: the row's second
 * line for that post; tag: the channel, so a newer post replaces the older
 * notification; a tap opens the channel.
 */
public class ChannelPostNotifier(
    private val store: ChannelNotifiedStore,
    private val rule: ChannelNotifyRule = coreChannelNotifyRule,
    private val clock: () -> Long = System::currentTimeMillis,
    private val show: (ChannelNotice) -> Unit,
) {
    /** What the caller knows of the channels when the reads are judged. */
    public interface Facts {
        /** The channels the account holds now; others leave the store. */
        public val held: Set<String>

        public fun muted(channelIdHex: String): Boolean

        public fun openAndVisible(channelIdHex: String): Boolean

        public fun own(channelIdHex: String): Boolean

        /** The notification's title for the channel. */
        public fun title(channelIdHex: String): String

        /** The notification's body for [post] in the channel. */
        public fun body(channelIdHex: String, post: PersistedChannelPost): String
    }

    /**
     * Judge [reads] of [accountIdB64] and show what notifies. The whole
     * read-decide-store runs under one process-wide lock, so the foreground
     * sync and the background work never both announce one post.
     */
    public suspend fun onReads(accountIdB64: String, reads: List<ChannelRead>, facts: Facts) {
        if (reads.isEmpty()) return
        decisionLock.withLock {
            val stored = store.read(accountIdB64)
            for (read in reads) {
                val id = read.channelIdHex.lowercase()
                val decision = try {
                    decideChannelRead(
                        rule,
                        read,
                        ChannelNotifyFacts(
                            muted = facts.muted(id),
                            openAndVisible = facts.openAndVisible(id),
                            own = facts.own(id),
                            lastNotified = stored[id] ?: 0L,
                            nowMs = clock(),
                        ),
                    )
                } catch (e: Throwable) {
                    Log.w(TAG, "channel notification rule failed", e)
                    continue
                }
                store.write(accountIdB64, id, decision.lastNotified, facts.held + id)
                val post = decision.post ?: continue
                try {
                    show(
                        ChannelNotice(
                            channelIdHex = id,
                            tag = channelNotificationTag(id),
                            title = facts.title(id),
                            body = facts.body(id, post),
                        ),
                    )
                } catch (e: Throwable) {
                    Log.w(TAG, "channel notification not shown", e)
                }
            }
        }
    }

    public companion object {
        private const val TAG = "ChannelPostNotifier"

        /** One decision at a time in this process. */
        private val decisionLock = Mutex()
    }
}

/** One notification to show. */
public data class ChannelNotice(
    val channelIdHex: String,
    val tag: String,
    val title: String,
    val body: String,
)

/**
 * The channel screen open in front of the person, or `null`. Set by the
 * channel screen while it is resumed, so a post of the channel being read
 * does not notify.
 */
public object VisibleChannel {
    @Volatile
    private var current: String? = null

    public val id: String?
        get() = current

    public fun shown(channelIdHex: String) {
        current = channelIdHex.lowercase()
    }

    public fun hidden(channelIdHex: String) {
        if (current == channelIdHex.lowercase()) current = null
    }

    public fun isShown(channelIdHex: String): Boolean = current == channelIdHex.lowercase()
}

/**
 * A tapped channel notification: the channel to open. The session shell
 * navigates there and consumes the slot.
 */
public object ChannelNotificationIntake {
    /** The intent extra a channel notification carries. */
    public const val EXTRA_OPEN_CHANNEL: String = "me.lakko.channels.open_channel"

    private val pending = MutableStateFlow<String?>(null)

    public val openRequested: StateFlow<String?> = pending.asStateFlow()

    public fun request(channelIdHex: String) {
        pending.value = channelIdHex.lowercase()
    }

    public fun consume(channelIdHex: String) {
        pending.compareAndSet(channelIdHex, null)
    }
}

/**
 * The system notification for a channel post, posted to the app's message
 * notification channel with one tag per channel, so a newer post replaces the
 * older one. Nothing is posted while notifications are off for Lakko or
 * `POST_NOTIFICATIONS` is not granted (API 33+).
 */
public object ChannelNotificationPoster {
    private const val TAG = "ChannelPostNotifier"

    /** One id under every channel tag; the tag tells channels apart. */
    private const val NOTIFICATION_ID: Int = 0x6c6b6368

    public fun show(context: Context, notice: ChannelNotice) {
        val app = context.applicationContext
        val manager = NotificationManagerCompat.from(app)
        if (!manager.areNotificationsEnabled()) return
        me.lakko.app.push.ensureMessagesNotificationChannel(app)
        val intent = Intent(app, me.lakko.app.MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            putExtra(ChannelNotificationIntake.EXTRA_OPEN_CHANNEL, notice.channelIdHex)
        }
        val pending = PendingIntent.getActivity(
            app,
            notice.tag.hashCode(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(app, me.lakko.app.push.LakkoMessagingService.CHANNEL_ID)
            .setSmallIcon(me.lakko.app.R.drawable.ic_stat_lakko)
            .setContentTitle(notice.title)
            .setContentText(notice.body)
            .setAutoCancel(true)
            .setGroup(notice.tag)
            .setContentIntent(pending)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .build()
        try {
            manager.notify(notice.tag, NOTIFICATION_ID, notification)
        } catch (cause: SecurityException) {
            Log.w(TAG, "notify failed (missing POST_NOTIFICATIONS?)", cause)
        }
    }
}
