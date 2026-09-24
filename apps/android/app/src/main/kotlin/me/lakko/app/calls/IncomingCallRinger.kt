// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
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
import uniffi.lakko_ffi_uniffi.RingDecision
import uniffi.lakko_ffi_uniffi.callRingWindowMs
import uniffi.lakko_ffi_uniffi.ringDecision

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
 *    unlocked device the platform shows a heads-up instead, which is why this
 *    whole notification is skipped while the app itself is in the foreground
 *    ([setAppForeground]): the heads-up lands on top of the app's own
 *    full-screen ring, covers it, and folds away after a few seconds, so
 *    answering became a race against a disappearing strip — four of seven
 *    calls were lost that way on the 2026-09-21 walk. When the app is open it
 *    rings itself instead, with the device ringtone and the same vibration
 *    pattern the channel carries, and the notification is posted the moment
 *    the app goes away.
 * 3. [ringing], which the ring screen observes so that a ring stopped anywhere
 *    — the caller hung up, another device answered, the window ran out —
 *    closes it without a second mechanism.
 *
 * ## The window
 *
 * A ring stops itself after what is left of the shared ring window
 * ([uniffi.lakko_ffi_uniffi.callRingWindowMs]), measured from the OFFER rather
 * than from the moment this device noticed it: a wake that took twenty seconds
 * to arrive has twenty-five seconds of ringing left, not forty-five. When it
 * runs out the host is told ([OneToOneCallHost.dismissRingingOffer]) so that a
 * held offer cannot ring again the next time the app comes to the foreground.
 *
 * ## Mute
 *
 * `docs/ux/calls.md` section 2.1 — a muted chat never rings. This is where
 * that is applied, because it is where both paths meet: putting it in the FCM
 * service and again in the poll host would be the same rule twice, and the
 * two copies would drift.
 *
 * The verdict itself is not decided here either. [uniffi.lakko_ffi_uniffi.ringDecision]
 * (`crates/lakko-ui-rules/src/ring.rs`) answers it for all three clients, and
 * the three answers are deliberately distinct: `RING` interrupts, `SILENT`
 * records the call and interrupts nobody, and `TOO_LATE` says the caller has
 * already given up. `SILENT` is NOT `TOO_LATE` and is not a dropped call — a
 * person who muted a conversation asked not to be interrupted by it, not to be
 * kept from finding out who tried to reach them.
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

    /** `true` while the app's own window is resumed, so it is showing the ring
     *  itself and the notification would only cover it. */
    @Volatile
    private var appInForeground: Boolean = false

    private var ringtonePlayer: MediaPlayer? = null

    /**
     * The app came to the front, or went away. Reported by `MainActivity`.
     *
     * Moving either way DURING a ring swaps the two surfaces rather than
     * doubling them: coming to the front cancels the notification and takes
     * over the ringing, going away posts the notification and stops.
     */
    public fun setAppForeground(foreground: Boolean) {
        if (appInForeground == foreground) return
        appInForeground = foreground
        val ring = mutableRinging.value ?: return
        val context = appContext ?: return
        applySurface(context, ring, remainingMs(ring))
    }

    /** What is left of this ring's window, against the shared 45 s number. */
    private fun remainingMs(ring: IncomingCallRing): Long =
        callRingRemainingMs(ring.offeredAtMs, System.currentTimeMillis(), windowMs())

    /** The ring window, from its one home (`crates/lakko-ui-rules/src/ring.rs`). */
    private fun windowMs(): Long = callRingWindowMs().toLong()

    /**
     * Make exactly one surface ring, per [ringSurface], and stop the other.
     *
     * Every moment the answer can change goes through here — the ring starting
     * and each foreground transition during one — so the two can never both
     * run and can never both stop.
     */
    private fun applySurface(context: Context, ring: IncomingCallRing, remainingMs: Long) {
        // Two questions, asked in order, and they are not the same question.
        // The core answers WHETHER this device may interrupt at all — the
        // rule shared with the other two clients. `ringSurface` answers WHICH
        // surface does the interrupting on Android, which no other client has
        // and which the core therefore knows nothing about.
        val decision = ringDecision(
            ring.muted,
            callRingOfferAgeMs(remainingMs, windowMs()).toULong(),
        )
        val surface = when (decision) {
            RingDecision.RING ->
                ringSurface(ringing = true, appInForeground = appInForeground, remainingMs = remainingMs)
            // `docs/ux/calls.md` sections 2.1 and 2.1.1 — a muted chat never
            // rings. No ringtone, no vibration, no full-screen intent, no
            // call-category heads-up over whatever the person is doing.
            //
            // And no quiet notification either. That is a judgement, so here
            // is the reasoning. The call is ALREADY recorded, in the place a
            // muted conversation's content belongs: the chat's own timeline
            // and the call log, written by the host when the window runs out
            // ([OneToOneCallHost.dismissRingingOffer]) exactly as it is for an
            // unmuted call nobody answered. A notification would be a second
            // record of the same event — one the person has to dismiss, for a
            // conversation they asked to stop hearing from. And a call
            // notification is worth having only while it can be answered: a
            // silent, low-priority card is noticed minutes later, when the
            // window is long gone, so it would offer an answer that connects
            // to nobody, which is the very thing `TOO_LATE` exists to refuse.
            //
            // `SILENT` is NOT a dropped call and is NOT `TOO_LATE`:
            // [startRinging] still registers the ring, the window still runs,
            // and the missed-call record still lands. Only the interruption is
            // withheld.
            //
            // The full-screen ring goes too, and it is not this file that
            // withholds it: [me.lakko.app.ui.call.OneToOneCallOverlay] asks
            // the same shared rule through
            // [me.lakko.app.calls.oneToOneCallPresentation]. What replaces it
            // is NOT nothing — section 2.1.1, decided 2026-09-23. The call is
            // offered on two surfaces that interrupt no one: a panel in the
            // conversation's own layout and that chat's row in the conversation
            // list ([me.lakko.app.calls.quietCallOffer]), so it stays
            // answerable for the whole window. Drawing nothing at all was the
            // first reading, and it made a muted contact unanswerable — muting
            // a conversation became half-blocking the person.
            RingDecision.SILENT,
            // The caller has already given up.
            RingDecision.TOO_LATE,
            -> RingSurface.NONE
        }
        when (surface) {
            RingSurface.IN_APP -> {
                cancel(context, ring.callIdHex)
                startInAppRing(context)
            }
            RingSurface.NOTIFICATION -> {
                stopInAppRing()
                post(context, ring, remainingMs)
            }
            RingSurface.NONE -> {
                stopInAppRing()
                cancel(context, ring.callIdHex)
            }
        }
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
        val remaining = remainingMs(ring)
        if (remaining <= 0L) {
            // The caller gave up before this device heard about it — the
            // core's `TooLate`, which is the one answer that stops here. A
            // muted offer does NOT stop here: it is registered, timed and
            // recorded like any other, and only [applySurface] withholds the
            // interruption.
            return
        }
        mutableRinging.value = ring
        applySurface(context, ring, remaining)
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
        stopInAppRing()
        appContext?.let { cancel(it, callIdHex) }
    }

    /** Stop whatever is ringing (logout, forget device). */
    public fun stopAll() {
        mutableRinging.value?.let { stopRinging(it.callIdHex) }
    }

    // -----------------------------------------------------------------------

    /**
     * Ring from inside the app: the device ringtone on a loop plus the
     * channel's vibration pattern.
     *
     * The ringer mode is read rather than assumed, because suppressing the
     * notification also gives up the channel's own silent/vibrate handling and
     * an app that rings aloud on a silenced phone is worse than the defect
     * this replaces. Do Not Disturb is NOT re-implemented here — a phone in
     * DND that would have let the channel through is the one case the app
     * rings quieter than the notification would have, which is the safe
     * direction to be wrong in.
     */
    private fun startInAppRing(context: Context) {
        stopInAppRing()
        val mode = try {
            context.getSystemService(AudioManager::class.java)?.ringerMode
        } catch (cause: Throwable) {
            Log.w(TAG, "could not read the ringer mode", cause)
            null
        } ?: AudioManager.RINGER_MODE_NORMAL
        if (mode == AudioManager.RINGER_MODE_NORMAL) {
            val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
            if (uri != null) {
                try {
                    ringtonePlayer = MediaPlayer().apply {
                        setAudioAttributes(
                            AudioAttributes.Builder()
                                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                                .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                                .build(),
                        )
                        setDataSource(context, uri)
                        isLooping = true
                        prepare()
                        start()
                    }
                } catch (cause: Throwable) {
                    // A ringtone the device will not play is not a reason to
                    // fail the call: the screen is up either way.
                    Log.w(TAG, "could not play the incoming-call ringtone", cause)
                    ringtonePlayer = null
                }
            }
        }
        if (mode != AudioManager.RINGER_MODE_SILENT) {
            try {
                vibrator(context)?.vibrate(
                    VibrationEffect.createWaveform(RING_VIBRATION_PATTERN, 0),
                )
            } catch (cause: Throwable) {
                Log.w(TAG, "could not vibrate for the incoming call", cause)
            }
        }
    }

    private fun stopInAppRing() {
        ringtonePlayer?.let { player ->
            try {
                player.stop()
            } catch (cause: Throwable) {
                Log.w(TAG, "could not stop the incoming-call ringtone", cause)
            }
            player.release()
        }
        ringtonePlayer = null
        val context = appContext ?: return
        try {
            vibrator(context)?.cancel()
        } catch (cause: Throwable) {
            Log.w(TAG, "could not stop the incoming-call vibration", cause)
        }
    }

    private fun vibrator(context: Context): Vibrator? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            context.getSystemService(VibratorManager::class.java)?.defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            context.getSystemService(Vibrator::class.java)
        }

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
            // (OneToOneCallOverlay) asks the same rule and stays away too.
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
