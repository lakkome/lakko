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
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import me.lakko.app.MainActivity
import me.lakko.app.calls.IncomingCallRinger
import me.lakko.app.ds.DsClient

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
 *     When the app is NOT foregrounded and the wake is a
 *     GroupMessage (`g` + `p` present), the service does a
 *     READ-ONLY THROWAWAY MLS decrypt
 *     ([me.lakko.app.push.decryptPushPreview]) to show the REAL
 *     newest-message text as the notification body — mirror of
 *     the web `sw-push-decrypt.js`. When the app IS foregrounded
 *     the live poll loop handles it, so we defer to a generic
 *     alert. ANY failure (no local account / seed / snapshot,
 *     non-member group, DS / decrypt failure, a Wakeup /
 *     MailboxMessage kind) falls back to the generic "New
 *     message" — invariant #4: a frame that will not authenticate
 *     is dropped, never shown as a guess. Either way the wake hint
 *     rides `Intent` extras so the user's tap lands in the right
 *     thread — post-ADR-0042 the `g` in those extras is the
 *     reverse-resolved real `group_id` (see [resolveTapGroupHex]),
 *     not the opaque `push_gid` the wire carries.
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
        // Silent-push background decrypt (mirror of web sw.js):
        //   * Foreground → defer. The live poll loop does the durable
        //     processing + its own preview; a background decrypt here
        //     would be redundant (and could double-notify).
        //   * Backgrounded + a GroupMessage wake → READ-ONLY THROWAWAY
        //     decrypt for the real newest-message text.
        // `isAppForegrounded` fails toward `false` (background), so a
        // probe error never suppresses the real preview.
        val outcome: PushWakeOutcome? =
            if (wake?.groupHex != null &&
                wake.pointer != null &&
                !isAppForegrounded(applicationContext)
            ) {
                backgroundPreviewBlocking(wake)
            } else {
                null
            }
        if (outcome != null && handledAsCall(outcome)) return
        showWakeNotification(wake, outcome?.preview)
    }

    /**
     * `docs/ux/calls.md` section 2.1 — a call wake rings the phone instead of
     * showing a message banner.
     *
     * Ends are applied before the offer so that a batch carrying both halves of
     * a finished call stops rather than starts; the fold
     * ([me.lakko.app.calls.foldCallRingSignals]) has already removed an offer
     * this batch also ended, so what is left is either a live call or nothing.
     *
     * A MUTED conversation still comes through here, and still returns `true`
     * so no message banner is stacked on it. Whether the phone actually
     * interrupts is decided once for all three clients
     * ([uniffi.lakko_ffi_uniffi.ringDecision]) and applied in
     * [IncomingCallRinger], which withholds the ringtone, the vibration and
     * the full-screen intent while still registering the call so it lands in
     * the history. Deciding it here as well would be the same rule twice.
     *
     * @return `true` when this wake was a call and no message notification
     *   should be shown for it.
     */
    private fun handledAsCall(outcome: PushWakeOutcome): Boolean {
        if (outcome.ring == null && outcome.cancelledCallIds.isEmpty()) return false
        IncomingCallRinger.attach(applicationContext)
        outcome.cancelledCallIds.forEach { IncomingCallRinger.stopRinging(it) }
        val ring = outcome.ring ?: return false
        IncomingCallRinger.startRinging(ring)
        // A ringing phone is the whole notification. A message that arrived in
        // the same batch waits for the app rather than stacking a second banner
        // under a call the user is being asked to answer right now; a batch that
        // only ENDED a call is not a call at all any more, so its preview still
        // shows.
        return true
    }

    /**
     * Run the READ-ONLY throwaway decrypt synchronously within the FMS
     * budget. [onMessageReceived] is dispatched on a Firebase-owned
     * background thread (never the main thread), so `runBlocking` here
     * keeps the process + wakelock alive until we post the
     * notification without risking an ANR. A [withTimeoutOrNull] guard
     * keeps us well inside the FMS window; a timeout or ANY failure
     * degrades to the generic notification (returns `null`).
     */
    private fun backgroundPreviewBlocking(wake: WakeBody): PushWakeOutcome? {
        return try {
            runBlocking {
                withTimeoutOrNull(BACKGROUND_DECRYPT_BUDGET_MS) {
                    DsClient().use { ds ->
                        val deps = buildBackgroundDecryptDeps(applicationContext, ds)
                        decryptPushWake(
                            PushWake(groupHex = wake.groupHex, pointer = wake.pointer),
                            deps,
                            nowMs = System.currentTimeMillis(),
                            // The ring window from its one home
                            // (`crates/lakko-ui-rules/src/ring.rs`): the two
                            // devices in a call have to give up together.
                            ringWindowMs = uniffi.lakko_ffi_uniffi.callRingWindowMs().toLong(),
                        )
                    }
                }
            }
        } catch (cause: Throwable) {
            Log.w(TAG, "background decrypt failed; showing generic notification", cause)
            null
        }
    }

    // -----------------------------------------------------------------
    // Notification rendering
    // -----------------------------------------------------------------

    private fun showWakeNotification(wake: WakeBody?, preview: PushPreview?) {
        ensureNotificationChannel(applicationContext)

        // Click intent: launch MainActivity with the wake hint
        // in the extras. MainActivity's `handleWakeIntent`
        // reader publishes into the process-scoped `PushIntake`
        // (Phase 1.20d2); the Compose router's
        // `LaunchedEffect(pendingWake, …)` consumes the slot and
        // routes the user to the matching chat thread (falling
        // back to the chat list when the group is unknown
        // locally — a stale wake).
        // ADR-0042 — the wake's `g` is the OPAQUE `push_gid`, which the
        // router cannot match against a local thread. Reverse-resolve it
        // to the real `group_id` hex for the TAP extra only; a miss keeps
        // the raw `g` (today's behavior). The notification GROUP key, the
        // notification id, and the PendingIntent request code below all
        // deliberately stay on the raw `push_gid` — it is stable per group,
        // so the existing grouping/replace semantics are already correct.
        val tapGroupHex = wake?.groupHex?.let { pushGidHex ->
            resolveTapGroupHex(pushGidHex) { hex ->
                try {
                    PushRouteStore(applicationContext).lookup(hex)?.groupIdHex
                } catch (cause: Throwable) {
                    Log.w(TAG, "push route lookup failed; tap keeps the raw push_gid", cause)
                    null
                }
            }
        }

        val clickIntent = Intent(applicationContext, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            if (tapGroupHex != null) {
                putExtra(EXTRA_WAKE_GROUP, tapGroupHex)
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
            // Real decrypted preview when the background decrypt
            // yielded one; the generic fallback otherwise.
            .setContentTitle(preview?.title ?: "Lakko")
            .setContentText(preview?.body ?: "New message")
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

        /** Wall-clock budget for the synchronous background decrypt.
         *  `onMessageReceived` must return well within the FCM window
         *  (~10 s), so we cap the DS-fetch + MLS-decrypt work and fall
         *  back to the generic notification on a timeout. */
        private const val BACKGROUND_DECRYPT_BUDGET_MS: Long = 8_000

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

/**
 * ADR-0042 — reverse-map a wake's opaque `push_gid` to the REAL DS
 * `group_id` (lowercase hex) for the notification-TAP deep link.
 *
 * Post-ADR-0042 the wake's `g` is the opaque `push_gid`, but the tap
 * consumer chain — [me.lakko.app.MainActivity.handleWakeIntent] ->
 * [PushIntake] -> the Compose router — matches the extra against
 * `ChatRepository.threads`' real `group_id` hex. Handing it the raw `g`
 * therefore never matches, so every tap fell back to the chat list. This
 * resolves it through the SAME plaintext reverse index the silent-push
 * decrypt path uses ([PushRouteStore.lookup]) — a read, never a write.
 *
 * Pure + injectable so it is unit-testable on the plain JVM without a
 * [android.content.Context]: [lookupGroupIdHex] is the only
 * side-effecting dependency, and this function deliberately does NOT
 * touch [android.util.Log] (a throwing stub off-device — it would defeat
 * the never-throws contract below in exactly the tests meant to prove
 * it). Diagnostics are the caller's job, inside the lambda it supplies.
 *
 * Fallback contract — on a MISS (a legacy pre-ADR-0042 raw-group_id
 * wake, a group whose route was never provisioned, a malformed stored
 * row, or a lookup THROW) this returns [wakeGroupHex] unchanged, i.e.
 * exactly today's behavior, which the router degrades to its existing
 * unknown-group chat-list fallback. It never throws: this runs on the
 * FCM callback thread on the way to posting a notification.
 */
public fun resolveTapGroupHex(
    wakeGroupHex: String,
    lookupGroupIdHex: (String) -> String?,
): String {
    val resolved = try {
        lookupGroupIdHex(wakeGroupHex)
    } catch (cause: Throwable) {
        // Defence in depth — the caller already logs + swallows. Anything
        // reaching here still degrades to the raw wake hex, never escapes.
        null
    }
    return resolved?.trim()?.takeIf { it.isNotEmpty() } ?: wakeGroupHex
}
