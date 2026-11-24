// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import me.lakko.app.l10n.Localization

/**
 * `docs/ux/group-and-channel-privacy.md` section 8.1 — the system
 * notification for one request to join, on the app's message channel, the way
 * the channel post notification goes ([me.lakko.app.feed.ChannelNotificationPoster]):
 * nothing is posted while notifications are off for Lakko or
 * `POST_NOTIFICATIONS` is not granted. One per request (tag per group and
 * joiner); never withdrawn when the request is answered elsewhere.
 */
public object JoinRequestNotificationPoster {
    private const val TAG = "JoinRequestNotice"

    /** One id under every request tag; the tag tells requests apart. */
    private const val NOTIFICATION_ID: Int = 0x6c6b6a72

    public fun show(context: Context, groupIdHex: String, joinerKeyB64: String, notice: JoinRequestNoticeText) {
        try {
            val app = context.applicationContext
            val manager = NotificationManagerCompat.from(app)
            if (!manager.areNotificationsEnabled()) return
            me.lakko.app.push.ensureMessagesNotificationChannel(app)
            val strings = Localization.strings.value
            val tag = joinRequestNotificationTag(groupIdHex, joinerKeyB64)
            val intent = Intent(app, me.lakko.app.MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
                putExtra(JoinRequestsIntake.EXTRA_OPEN_JOIN_REQUESTS, groupIdHex.lowercase())
            }
            val pending = PendingIntent.getActivity(
                app,
                tag.hashCode(),
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            val notification = NotificationCompat.Builder(app, me.lakko.app.push.LakkoMessagingService.CHANNEL_ID)
                .setSmallIcon(me.lakko.app.R.drawable.ic_stat_lakko)
                .setContentTitle(strings[JOIN_REQUEST_NOTICE_TITLE_KEY])
                .setContentText(strings.format(notice.bodyKey, notice.texts))
                .setAutoCancel(true)
                .setContentIntent(pending)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .build()
            manager.notify(tag, NOTIFICATION_ID, notification)
        } catch (cause: SecurityException) {
            Log.w(TAG, "notify failed (missing POST_NOTIFICATIONS?)")
        } catch (cause: Throwable) {
            Log.w(TAG, "notify failed (${cause.javaClass.simpleName})")
        }
    }
}

/**
 * Opening a group's Group info at `Join requests`, from a tapped notification
 * or the bar's `Review`. The session shell navigates to the conversation
 * ([navigateRequested]); the conversation screen of that group takes
 * [takeInfoRequest] and opens its info scrolled to the section.
 */
public object JoinRequestsIntake {
    /** The intent extra a join-request notification carries: the group id hex. */
    public const val EXTRA_OPEN_JOIN_REQUESTS: String = "me.lakko.chat.open_join_requests"

    private val navigate = MutableStateFlow<String?>(null)
    private val info = MutableStateFlow<String?>(null)

    /** A group whose conversation the shell should open. */
    public val navigateRequested: StateFlow<String?> = navigate.asStateFlow()

    /** A group whose info should open at `Join requests`. */
    public val infoRequested: StateFlow<String?> = info.asStateFlow()

    public fun request(groupIdHex: String) {
        val id = groupIdHex.lowercase()
        info.value = id
        navigate.value = id
    }

    public fun consumeNavigate(groupIdHex: String) {
        navigate.compareAndSet(groupIdHex.lowercase(), null)
    }

    /** Whether an info request for [groupIdHex] was waiting; it is consumed. */
    public fun takeInfoRequest(groupIdHex: String): Boolean = info.compareAndSet(groupIdHex.lowercase(), null)
}
