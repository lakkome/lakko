// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import me.lakko.app.MainActivity

/**
 * `FirebaseMessagingService` subclass — the FCM-side counterpart
 * to the SW `push` handler on Web (1.20a) and the
 * `UNUserNotificationCenterDelegate` callbacks on iOS (1.20c).
 *
 * Two callbacks of interest:
 *
 *   * [onNewToken] — the FCM token rotated. Re-register with
 *     `push-gw` so the new token replaces the prior row.
 *     Idempotent server-side via the UPSERT in
 *     `services/push-gw/src/registry.rs`.
 *
 *   * [onMessageReceived] — a wake arrived. The body shape is
 *     defined by
 *     `services/push-gw/src/providers/fcm.rs::WakeBody`:
 *     `{ v: "1", d: <device_hex>, g: <group_hex>?, p: <pointer>? }`
 *     The service deliberately does NOT attempt MLS decrypt
 *     (no canonical Identity / cached group state available
 *     in the FMS process before the foreground UI is bound);
 *     we surface a generic "New message" alert with the wake
 *     hint as `Intent` extras so the user's tap lands them in
 *     the right thread.
 *
 * Registered in `AndroidManifest.xml`:
 *
 *     <service android:name=".push.LakkoMessagingService"
 *              android:exported="false">
 *       <intent-filter>
 *         <action android:name="com.google.firebase.MESSAGING_EVENT" />
 *       </intent-filter>
 *     </service>
 *
 * ## Threading
 *
 * `onNewToken` runs on a Firebase-owned background thread; we
 * dispatch the suspending re-register onto `Dispatchers.IO`
 * via a `SupervisorJob`-rooted scope so a failure in one
 * leg doesn't cascade. The service's lifecycle is short — we
 * fire-and-forget the coroutine and let the SDK keep the
 * process alive for the duration via its own wakelock.
 */
public class LakkoMessagingService : FirebaseMessagingService() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override fun onNewToken(token: String) {
        Log.i(TAG, "FCM token rotated; re-registering with push-gw")
        scope.launch {
            try {
                val pseudonym = DevicePushId.getOrCreate(applicationContext)
                PushClient().use { client ->
                    client.register(
                        devicePseudonym = pseudonym,
                        provider = PushClient.Provider.FCM,
                        tokenBytes = token.toByteArray(Charsets.UTF_8),
                    )
                }
            } catch (cause: Throwable) {
                Log.w(TAG, "onNewToken re-register failed", cause)
            }
        }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val wake = parseWakeBody(message.data)
        showWakeNotification(wake)
    }

    // -----------------------------------------------------------------
    // Notification rendering
    // -----------------------------------------------------------------

    private fun showWakeNotification(wake: WakeBody?) {
        ensureNotificationChannel(applicationContext)

        // Click intent: launch MainActivity with the wake hint
        // in the extras. MainActivity's `handleWakeIntent`
        // reader publishes into the process-scoped `PushIntake`
        // (Phase 1.20d2); the Compose router's
        // `LaunchedEffect(pendingWake, …)` consumes the slot and
        // routes the user to the matching chat thread (falling
        // back to the chat list when the group is unknown
        // locally — a stale wake).
        val clickIntent = Intent(applicationContext, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            if (wake?.groupHex != null) {
                putExtra(EXTRA_WAKE_GROUP, wake.groupHex)
            }
            if (wake?.pointer != null) {
                putExtra(EXTRA_WAKE_POINTER, wake.pointer)
            }
        }
        val pendingIntent = PendingIntent.getActivity(
            applicationContext,
            wake?.groupHex?.hashCode() ?: 0,
            clickIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        val builder = NotificationCompat.Builder(
            applicationContext,
            CHANNEL_ID,
        )
            // 1.20d uses the system default-app drawable —
            // matches the AndroidManifest `android:icon` fallback
            // from 1.15c. When the app ships a real icon pack
            // (post-MVP), swap this to `R.mipmap.ic_launcher`.
            .setSmallIcon(android.R.drawable.sym_def_app_icon)
            .setContentTitle("Lakko")
            .setContentText("New message")
            .setAutoCancel(true)
            // Tag-replace by group so a burst within the same
            // chat collapses into one OS notification — matches
            // the Web + iOS posture.
            .setGroup(wake?.groupHex ?: "lakko-default")
            .setContentIntent(pendingIntent)
            .setPriority(NotificationCompat.PRIORITY_HIGH)

        val manager = NotificationManagerCompat.from(applicationContext)
        // Per-group notification id — same hash trick as the
        // PendingIntent request code. Stable id means a fresh
        // wake on the same thread replaces the prior banner.
        val id = wake?.groupHex?.hashCode() ?: 0
        try {
            manager.notify(id, builder.build())
        } catch (cause: SecurityException) {
            // API 33+ requires the POST_NOTIFICATIONS runtime
            // permission. Users who declined the prompt would
            // see this on every push — best-effort skip.
            Log.w(TAG, "notify failed (missing POST_NOTIFICATIONS?)", cause)
        }
    }

    private fun ensureNotificationChannel(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Messages",
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = "Wake notifications for new Lakko messages"
        }
        val manager = context.getSystemService(NotificationManager::class.java)
        manager?.createNotificationChannel(channel)
    }

    // -----------------------------------------------------------------
    // Wake body parse
    // -----------------------------------------------------------------

    private fun parseWakeBody(data: Map<String, String>): WakeBody? {
        // The FCM provider in `services/push-gw/src/providers/fcm.rs`
        // emits the wake hint inside the `data` payload (not
        // the `notification` payload, which Google's
        // server-side renderer would auto-display and bypass
        // our `onMessageReceived`). Keys are stringified per
        // the FCM data-payload contract: `v`, `d`, `g`, `p`.
        if (data["v"] != "1") return null
        val device = data["d"] ?: return null
        val group = data["g"]
        val pointer = data["p"]?.toLongOrNull()
        return WakeBody(deviceHex = device, groupHex = group, pointer = pointer)
    }

    private data class WakeBody(
        val deviceHex: String,
        val groupHex: String?,
        val pointer: Long?,
    )

    public companion object {
        /** Logcat tag for FCM-side logging. */
        public const val TAG: String = "lakko-fcm"

        /** Notification channel id. Lives across process
         *  restarts; created on first wake via
         *  [ensureNotificationChannel]. */
        public const val CHANNEL_ID: String = "lakko-messages-v1"

        /** Intent extras used to forward the wake hint into
         *  MainActivity. [PushIntake] (1.20d2) picks these up
         *  and routes the user to the matching chat thread on
         *  the Compose side. */
        public const val EXTRA_WAKE_GROUP: String = "me.lakko.push.wake_group"
        public const val EXTRA_WAKE_POINTER: String = "me.lakko.push.wake_pointer"
    }
}
