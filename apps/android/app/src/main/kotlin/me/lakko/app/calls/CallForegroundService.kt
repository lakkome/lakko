// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import me.lakko.app.R
import me.lakko.app.l10n.Localization

/**
 * The call keeps running while Lakko is not on the screen — `docs/ux/calls.md`
 * section 4.5.4: "Closing the system window never ends the call. The call goes
 * on with sound; ... the ongoing-call notification (Android) brings it back."
 *
 * ## Why a foreground service at all
 *
 * Without one, the call ended within seconds of the app leaving the screen,
 * seen on an Android 15 emulator: once the app was no longer visible the
 * platform destroyed its sockets (the signalling connection to the SFU went
 * with them), silenced its microphone and took the camera away. A foreground
 * service with an ongoing notification is the platform's way of saying "this
 * app is in a call".
 *
 * ## Which types
 *
 * `microphone` always, `camera` added while this device's camera sends. Not
 * `phoneCall`: that type requires a self-managed `ConnectionService`, which
 * this client does not have. Android 14 and later refuse a type whose
 * permission is not held and refuse to START a while-in-use type from the
 * background, so the service is started (and its camera type added) from the
 * app while it is on the screen, and only types whose permission is granted
 * are asked for ([ongoingCallServiceTypes]).
 *
 * ## Lifetime
 *
 * Started when a call is placed or answered (the shared `call_bar` rule says
 * it can be put away; a call only ringing has none), stopped by the service
 * itself the moment no call holds the slot ([AppCallHost.occupant], a
 * process-wide flow that keeps running while the app is in the background, so
 * a call the other person ended while Lakko was away takes its notification
 * with it).
 */

/** What the ongoing notification says, and whether the camera type is held. */
public data class OngoingCallSpec(
    /** The call's label, as its screen titles it. */
    val label: String,
    /** This device's camera is sending. */
    val cameraSending: Boolean,
)

/** One foreground service type, without the platform constants, so the choice
 *  is testable on the host JVM. */
public enum class OngoingCallServiceType { MICROPHONE, CAMERA }

/**
 * The types the service runs under: the microphone whenever its permission is
 * held, the camera only while it sends and its permission is held. Android 14
 * and later throw on a type whose permission is missing, so a type is never
 * asked for on the strength of the call alone.
 */
public fun ongoingCallServiceTypes(
    microphoneGranted: Boolean,
    cameraGranted: Boolean,
    cameraSending: Boolean,
): Set<OngoingCallServiceType> = buildSet {
    if (microphoneGranted) add(OngoingCallServiceType.MICROPHONE)
    if (cameraSending && cameraGranted) add(OngoingCallServiceType.CAMERA)
}

/** The platform's bit mask for [types]. */
internal fun ongoingCallServiceTypeMask(types: Set<OngoingCallServiceType>): Int {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return 0
    var mask = 0
    if (OngoingCallServiceType.MICROPHONE in types) mask = mask or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
    if (OngoingCallServiceType.CAMERA in types) mask = mask or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
    return mask
}

/**
 * Starting, updating and stopping the service from the app. Main thread only.
 */
public object OngoingCallNotice {
    private const val TAG = "lakko-call-ongoing"

    /** What the service was last asked to show; `null` while none runs. */
    private var current: OngoingCallSpec? = null

    /**
     * Keep the service in step with the call: start it for a call that can be
     * put away, update its text or its camera type, or (with `null`) stop it.
     * Call while the app is on the screen: Android 12 and later refuse to
     * start a foreground service from the background.
     */
    public fun update(context: Context, spec: OngoingCallSpec?) {
        val app = context.applicationContext
        if (spec == current) return
        if (spec == null) {
            current = null
            app.stopService(Intent(app, CallForegroundService::class.java))
            return
        }
        val intent = Intent(app, CallForegroundService::class.java)
            .putExtra(EXTRA_LABEL, spec.label)
            .putExtra(EXTRA_CAMERA, spec.cameraSending)
        try {
            ContextCompat.startForegroundService(app, intent)
            current = spec
        } catch (cause: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException on Android 12+: the
            // app is not in the foreground. The call is untouched; the next
            // change seen on the screen tries again.
            Log.w(TAG, "ongoing call service not started: ${cause.javaClass.simpleName}")
        } catch (cause: SecurityException) {
            Log.w(TAG, "ongoing call service not started: ${cause.javaClass.simpleName}")
        }
    }

    /** The service stopped itself (the call ended while the app was away). */
    internal fun onServiceStopped() {
        current = null
    }

    internal const val EXTRA_LABEL: String = "label"
    internal const val EXTRA_CAMERA: String = "camera"
}

/**
 * The foreground service. Holds nothing of the call: the call lives in
 * [AppCallHost]; this only keeps the process allowed to run it.
 */
public class CallForegroundService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var watch: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Every start must reach startForeground in time, or the platform
        // ends the app. A restart with no intent has nothing to show: there
        // is no call to keep (START_NOT_STICKY makes that rare).
        val label = intent?.getStringExtra(OngoingCallNotice.EXTRA_LABEL)
        val cameraSending = intent?.getBooleanExtra(OngoingCallNotice.EXTRA_CAMERA, false) == true
        val types = ongoingCallServiceTypes(
            microphoneGranted = granted(Manifest.permission.RECORD_AUDIO),
            cameraGranted = granted(Manifest.permission.CAMERA),
            cameraSending = cameraSending,
        )
        val notification = notification(label ?: "")
        try {
            ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, ongoingCallServiceTypeMask(types))
        } catch (cause: RuntimeException) {
            // Android 14+: a type not allowed right now (a missing permission,
            // or the camera type asked for from the background). Keep what can
            // be kept: the microphone alone, else nothing.
            Log.w(TAG, "foreground types refused: ${cause.javaClass.simpleName}")
            val fallback = types - OngoingCallServiceType.CAMERA
            val kept = fallback.isNotEmpty() && try {
                ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, ongoingCallServiceTypeMask(fallback))
                true
            } catch (again: RuntimeException) {
                Log.w(TAG, "ongoing call service could not run: ${again.javaClass.simpleName}")
                false
            }
            if (!kept) {
                stopSelf()
                return START_NOT_STICKY
            }
        }
        if (label == null) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (watch == null) {
            // Stop the moment no call holds the slot, wherever the app is.
            watch = scope.launch {
                callEnded()
                stopSelf()
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        OngoingCallNotice.onServiceStopped()
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }

    private fun granted(permission: String): Boolean =
        ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED

    private fun notification(label: String): Notification {
        val strings = Localization.strings.value
        ensureChannel(strings["call.ongoing.channel_name"])
        // Bring the app back exactly as it was left: the launcher's own
        // intent resumes the existing task, full screen or collapsed.
        val open = Intent(this, me.lakko.app.MainActivity::class.java)
            .setAction(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_LAUNCHER)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
        val tap = PendingIntent.getActivity(
            this,
            REQUEST_OPEN,
            open,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val returnText = strings["call.ongoing.return"]
        // On a locked screen the person's name stays hidden unless the phone
        // is set to show private content.
        val public = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_lakko)
            .setContentTitle(strings["call.ongoing.channel_name"])
            .setContentText(returnText)
            .build()
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_lakko)
            .setContentTitle(label)
            .setContentText(returnText)
            .setContentIntent(tap)
            .setOngoing(true)
            .setSilent(true)
            .setOnlyAlertOnce(true)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(public)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    private fun ensureChannel(name: String) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = getSystemService(NotificationManager::class.java) ?: return
        val existing = manager.getNotificationChannel(CHANNEL_ID)
        if (existing != null) {
            // Only the name can change after a channel exists; it follows
            // the interface language.
            if (existing.name != name) {
                existing.name = name
                manager.createNotificationChannel(existing)
            }
            return
        }
        val channel = NotificationChannel(CHANNEL_ID, name, NotificationManager.IMPORTANCE_LOW).apply {
            setSound(null, null)
            enableVibration(false)
            setShowBadge(false)
        }
        manager.createNotificationChannel(channel)
    }

    internal companion object {
        /**
         * Suspends until no call holds the slot. A seam for the host JVM
         * tests, where the process-wide call host cannot be built (it loads
         * the native core).
         */
        @Volatile
        internal var callEnded: suspend () -> Unit = { AppCallHost.occupant.first { it == null } }

        private const val TAG = "lakko-call-ongoing"
        private const val CHANNEL_ID = "lakko-call-ongoing-v1"
        private const val NOTIFICATION_ID = 4601
        private const val REQUEST_OPEN = 4602
    }
}
