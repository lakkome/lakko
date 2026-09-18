// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * The one thing that makes the phone ring, and the one thing that stops it.
 *
 * ## Why an object and not a screen
 *
 * A ring outlives every screen: it starts with the app closed, it may be
 * answered from the lock screen, and it must stop from a notification action
 * fired while no activity exists. So the ring lives in the process, exactly as
 * the call itself does ([AppCallHost]), and the two paths that can raise it —
 * the FCM wake and the app's own poll — both go through [startRinging].
 *
 * ## What ringing consists of
 *
 * 1. A notification on a dedicated **call channel**: maximum importance, the
 *    device ringtone, vibration, and a request to bypass Do Not Disturb. The
 *    channel is separate from the message channel because a channel's sound and
 *    importance are fixed at creation on Android 8+; a call cannot borrow a
 *    channel built for message previews.
 * 2. `CATEGORY_CALL` plus a **full-screen intent**, which is what puts
 *    [me.lakko.app.ui.call.IncomingCallActivity] over the lock screen. On an
 *    unlocked device the platform shows a heads-up instead — deliberately, and
 *    the reason the actions are on the notification as well as on the screen.
 * 3. [ringing], which the ring screen observes so that a ring stopped anywhere
 *    — the caller hung up, another device answered, the window ran out —
 *    closes it without a second mechanism.
 *
 * ## The window
 *
 * A ring stops itself after what is left of [CALL_RING_WINDOW_MS], measured
 * from the OFFER rather than from the moment this device noticed it: a wake
 * that took twenty seconds to arrive has twenty-five seconds of ringing left,
 * not forty-five. When it runs out the host is told
 * ([OneToOneCallHost.dismissRingingOffer]) so that a held offer cannot ring
 * again the next time the app comes to the foreground.
 */
public object IncomingCallRinger : IncomingCallRingPort {

    private const val TAG = "lakko-call-ring"

    /** Channel id. Fixed for the life of the install: a channel's importance
     *  and sound cannot be changed once created, so a change of ring policy
     *  needs a new id. */
    public const val CHANNEL_ID: String = "lakko-calls-v1"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    private val mutableRinging = MutableStateFlow<IncomingCallRing?>(null)

    /** The call this device is ringing for, or `null`. Observed by the ring screen. */
    public val ringing: StateFlow<IncomingCallRing?> = mutableRinging.asStateFlow()

    private var timeoutJob: Job? = null

    /** Set by `MainActivity`, so a ring raised from a background wake can still
     *  post a notification after the activity is gone. */
    @Volatile
    private var appContext: Context? = null

    /** Give the ringer a context it can keep. Idempotent. */
    public fun attach(context: Context) {
        appContext = context.applicationContext
    }

    /**
     * An offer arrived: ring for it.
     *
     * Idempotent per call id — a push wake and a poll can deliver the same
     * offer, and re-posting would restart the ringtone from the top.
     */
    override fun startRinging(ring: IncomingCallRing) {
        val context = appContext ?: return
        if (mutableRinging.value?.callIdHex == ring.callIdHex) return
        val remaining = callRingRemainingMs(ring.offeredAtMs, System.currentTimeMillis())
        if (remaining <= 0L) {
            // The caller gave up before this device heard about it.
            return
        }
        mutableRinging.value = ring
        post(context, ring, remaining)
        timeoutJob?.cancel()
        timeoutJob = scope.launch {
            delay(remaining)
            // The window ran out: stop, and make sure the offer this device is
            // still holding cannot ring again on the next foreground.
            if (mutableRinging.value?.callIdHex == ring.callIdHex) {
                AppCallHost.instance.dismissRingingOffer(ring.callIdHex)
                stopRinging(ring.callIdHex)
            }
        }
    }

    /** Stop ringing for this call id. A call id that is not ringing is a no-op. */
    override fun stopRinging(callIdHex: String) {
        val current = mutableRinging.value ?: return
        if (current.callIdHex != callIdHex) return
        timeoutJob?.cancel()
        timeoutJob = null
        mutableRinging.value = null
        appContext?.let { cancel(it, callIdHex) }
    }

    /** Stop whatever is ringing (logout, forget device). */
    public fun stopAll() {
        mutableRinging.value?.let { stopRinging(it.callIdHex) }
    }

    // -----------------------------------------------------------------------

    private fun post(context: Context, ring: IncomingCallRing, remainingMs: Long) {
        ensureChannel(context)
        val content = callRingContent(ring.callerName, ring.video)
        val builder = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.sym_action_call)
            .setContentTitle(content.title)
            .setContentText(content.body)
            // A call is an interruption and is allowed to be one
            // (docs/ux/calls.md section 1).
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            // The ring belongs on a locked screen: it is the whole point.
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            // Not dismissible by a swipe, and not cancelled by tapping it:
            // only answering, declining, the caller hanging up or the window
            // running out stops a ring.
            .setOngoing(true)
            .setAutoCancel(false)
            .setTimeoutAfter(remainingMs)
            .setFullScreenIntent(ringScreenIntent(context, ring, CALL_RING_ACTION_SHOW), true)
            .setContentIntent(ringScreenIntent(context, ring, CALL_RING_ACTION_SHOW))
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                "Decline",
                declineIntent(context, ring),
            )
            .addAction(
                android.R.drawable.sym_action_call,
                "Accept",
                ringScreenIntent(context, ring, CALL_RING_ACTION_ACCEPT),
            )
        try {
            NotificationManagerCompat.from(context)
                .notify(notificationId(ring.callIdHex), builder.build())
        } catch (cause: SecurityException) {
            // API 33+ without POST_NOTIFICATIONS. The in-app presentation
            // (OneToOneCallOverlay) still shows when the app is open.
            Log.w(TAG, "could not post the incoming-call notification", cause)
        }
    }

    private fun cancel(context: Context, callIdHex: String) {
        try {
            NotificationManagerCompat.from(context).cancel(notificationId(callIdHex))
        } catch (cause: Throwable) {
            Log.w(TAG, "could not cancel the incoming-call notification", cause)
        }
    }

    /**
     * The full-screen / tap / Accept intent: the ring screen, carrying the call
     * it is about. Every route to answering goes through that screen so the
     * microphone permission is asked for in one place.
     */
    private fun ringScreenIntent(context: Context, ring: IncomingCallRing, action: String): PendingIntent {
        val intent = Intent(context, me.lakko.app.ui.call.IncomingCallActivity::class.java).apply {
            this.action = action
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            putExtra(CALL_RING_EXTRA_GROUP, ring.groupIdHex)
            putExtra(CALL_RING_EXTRA_CALL_ID, ring.callIdHex)
        }
        return PendingIntent.getActivity(
            context,
            requestCode(ring.callIdHex, action),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /** Decline goes to a receiver, not an activity: refusing a call must not
     *  open the app (docs/ux/calls.md section 2.2). */
    private fun declineIntent(context: Context, ring: IncomingCallRing): PendingIntent {
        val intent = Intent(context, IncomingCallActionReceiver::class.java).apply {
            action = CALL_RING_ACTION_DECLINE
            putExtra(CALL_RING_EXTRA_GROUP, ring.groupIdHex)
            putExtra(CALL_RING_EXTRA_CALL_ID, ring.callIdHex)
        }
        return PendingIntent.getBroadcast(
            context,
            requestCode(ring.callIdHex, CALL_RING_ACTION_DECLINE),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Calls",
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = "Incoming Lakko calls"
            enableVibration(true)
            vibrationPattern = RING_VIBRATION_PATTERN
            lockscreenVisibility = android.app.Notification.VISIBILITY_PUBLIC
            setShowBadge(false)
            val ringtone = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
            if (ringtone != null) {
                setSound(
                    ringtone,
                    AudioAttributes.Builder()
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                        .build(),
                )
            }
            // Only honoured when the user has granted Do Not Disturb access;
            // without the grant the platform ignores it, which is the correct
            // outcome and not something to work around.
            try {
                setBypassDnd(true)
            } catch (cause: SecurityException) {
                Log.w(TAG, "Do Not Disturb override not granted; calls follow the user's DND policy", cause)
            }
        }
        manager.createNotificationChannel(channel)
    }

    /** Stable per call so a re-post replaces rather than stacks. */
    private fun notificationId(callIdHex: String): Int = NOTIFICATION_ID_BASE + callIdHex.hashCode()

    private fun requestCode(callIdHex: String, action: String): Int =
        31 * callIdHex.hashCode() + action.hashCode()

    private const val NOTIFICATION_ID_BASE = 0x4C41 // 'L','A'

    private val RING_VIBRATION_PATTERN = longArrayOf(0, 700, 700, 700, 700)
}
