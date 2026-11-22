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
 *   * [onNewToken] — the FCM token rotated. It is stored
 *     ([PushDeviceTokens]); while an account is signed in, every sealed
 *     subscription of that account is sent again carrying it
 *     (ADR-0078: push-gw holds the token only inside those rows). A
 *     signed-out device only stores it.
 *
 *   * [onMessageReceived] — a wake arrived. The body shape is
 *     defined by
 *     `services/push-gw/src/providers/fcm.rs::WakeData`:
 *     `{ s: <subscription_hex>, g: <group_hex>?, p: <pointer>?, k: "c"? }`
 *     FIRST the wake gate (ADR-0042, amended 2026-09-30, with ADR-0078,
 *     [wakeAccountFor]): the wake is handled only when a
 *     [PushWakeBinding] exists, `s` is well formed and the bound
 *     account's route row for `g` holds that subscription id; anything
 *     else posts NO notification. The account it is then handled as is
 *     the bound one, for the decrypt, the mute flag, the caller name and
 *     the tap deep link alike.
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

    override fun onCreate() {
        // A push can start this service in a process where no sign-in ran,
        // so the default Firebase app the SDK delivers through may not exist.
        PushSubscriber.ensureFirebase(applicationContext)
        super.onCreate()
    }

    override fun onNewToken(token: String) {
        scope.launch {
            try {
                // ADR-0078 — the token is device state, stored here; every
                // subscription of the signed-in account carries it sealed, so
                // a CHANGED token while signed in sends them all again. With
                // no binding (signed out) nothing is sent.
                val changed = PushDeviceTokens(applicationContext).setFcm(token)
                val binding = PushWakeBindingStore(applicationContext).load()
                if (!changed || binding == null) {
                    Log.i(TAG, "FCM token stored; no subscription to re-send")
                    return@launch
                }
                Log.i(TAG, "FCM token rotated; re-sending the sealed subscriptions")
                PushSubscriptions.resubscribeAll(applicationContext, binding.accountIdB64)
            } catch (cause: Throwable) {
                Log.w(TAG, "onNewToken re-send failed", cause)
            }
        }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        // A push can arrive before any Activity has run in this process: load
        // the interface language for the notification words (ADR-0061).
        me.lakko.app.l10n.Localization.init(applicationContext)
        val wake = parseWakeBody(message.data)
        // ADR-0042, amended 2026-09-30, with ADR-0078 — the wake gate. A wake
        // with no binding (no account signed in), no well-formed `s`, or an
        // `s` the bound account does not hold for the wake's `g` belongs to
        // no session this device holds: post nothing.
        val binding = try {
            PushWakeBindingStore(applicationContext).load()
        } catch (cause: Throwable) {
            Log.w(TAG, "push wake binding unreadable; dropping wake", cause)
            null
        }
        val accountIdB64 = wakeAccountFor(wake?.subscriptionHex, binding)
        if (wake == null || accountIdB64 == null) {
            Log.i(TAG, "wake dropped: not addressed to the signed-in account")
            return
        }
        val held = wake.groupHex == null || try {
            PushRouteStore(applicationContext).lookup(accountIdB64, wake.groupHex, wake.subscriptionHex) != null
        } catch (cause: Throwable) {
            Log.w(TAG, "push route lookup failed; dropping wake", cause)
            false
        }
        if (!held) {
            Log.i(TAG, "wake dropped: not a subscription the signed-in account holds")
            return
        }
        // The app-wide receive sweep, when the app is open, runs at once and
        // stays short for a while (`ReceiveCadence`).
        me.lakko.app.chat.ReceiveCadence.noteWake()
        // ADR-0073 R.5 — a wake for the old segment of a moved one-to-one posts
        // nothing at all, in the FOREGROUND too. The background pass below
        // already answers this, but a foregrounded app runs no background
        // pass and would fall through to the generic banner until the push
        // teardown lands. Two plain-preferences reads (the local
        // `push_gid -> group` index, then the continuity facts); no seed,
        // no snapshot, no network.
        val closed = wakeNamesClosedOneToOne(
            wakeGroupHex = wake.groupHex,
            lookupGroupIdHex = { hex -> PushRouteStore(applicationContext).lookup(accountIdB64, hex) },
            isClosedOneToOne = { hex ->
                hex.lowercase() in
                    me.lakko.app.chat.SharedPrefsContinuityBackend(applicationContext).load(accountIdB64).movedOneToOne
            },
        )
        if (closed) {
            Log.i(TAG, "wake dropped: the conversation moved")
            return
        }
        // READ-ONLY THROWAWAY decrypt of the wake's rows, on screen too: it
        // is what tells a message from someone else (a banner) from this
        // device's own message, another device of the same account, or
        // service traffic (nothing). The live app keeps the durable
        // processing; nothing here is written.
        // `isAppForegrounded` fails toward `false` (background), so a probe
        // error never suppresses the generic banner.
        val appOnScreen = isAppForegrounded(applicationContext)
        val outcome: PushWakeOutcome =
            if (wake.groupHex != null && wake.pointer != null) {
                backgroundPreviewBlocking(wake, accountIdB64)
            } else {
                PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_POINTER)
            }
        // ADR-0073 phase 2 / UX 16.4 — a wake for a closed one-to-one posts
        // nothing at all: no banner, no ring.
        if (outcome.suppressed) {
            Log.i(TAG, "wake dropped: the conversation moved")
            return
        }
        if (handledAsCall(outcome)) return
        val shown = outcome.shownPreview(appOnScreen)
        if (shown == null && !outcome.genericBannerWanted(appOnScreen)) {
            Log.i(
                TAG,
                if (outcome.storedPreview != null) {
                    "wake posts nothing: the app on screen already shows the message"
                } else {
                    "wake posts nothing: no message from someone else"
                },
            )
            return
        }
        // Every generic banner says why, in one plain line (no identifier):
        // a "New message" with no reason in the log is a defect report nobody
        // can act on.
        val reason = wakeBannerLogLine(outcome, shown)
        if (reason != null) Log.i(TAG, reason)
        // docs/ux/media-messages.md section 11, ADR-0047 section 6 — the
        // first photo or video's micro thumbnail, carried inside the message,
        // as the notification's picture: decoded in memory, nothing fetched,
        // so a process this wake started shows it too.
        val picture = me.lakko.app.media.decodeInlineThumb(notificationPictureBytes(shown))
        showWakeNotification(wake, accountIdB64, shown, picture)
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
    private fun backgroundPreviewBlocking(wake: WakeBody, accountIdB64: String): PushWakeOutcome {
        return try {
            runBlocking {
                withTimeoutOrNull(BACKGROUND_DECRYPT_BUDGET_MS) {
                    DsClient().use { ds ->
                        val deps = buildBackgroundDecryptDeps(applicationContext, ds)
                        decryptPushWake(
                            PushWake(
                                groupHex = wake.groupHex,
                                pointer = wake.pointer,
                                accountIdB64 = accountIdB64,
                                subscriptionIdHex = wake.subscriptionHex,
                            ),
                            deps,
                            nowMs = System.currentTimeMillis(),
                            // The ring window from its one home
                            // (`crates/lakko-ui-rules/src/ring.rs`): the two
                            // devices in a call have to give up together.
                            ringWindowMs = uniffi.lakko_ffi_uniffi.callRingWindowMs().toLong(),
                        )
                    }
                } ?: PushWakeOutcome.emptyBecause(WakeFallbackReason.TIMEOUT)
            }
        } catch (cause: Throwable) {
            // The class only: a message text could carry an identifier.
            Log.w(TAG, "background decrypt failed (${cause.javaClass.simpleName})")
            PushWakeOutcome.emptyBecause(WakeFallbackReason.READ_FAILED)
                .copy(fallbackDetail = cause.javaClass.simpleName)
        }
    }

    // -----------------------------------------------------------------
    // Notification rendering
    // -----------------------------------------------------------------

    private fun showWakeNotification(
        wake: WakeBody,
        accountIdB64: String,
        preview: PushPreview?,
        picture: android.graphics.Bitmap? = null,
    ) {
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
        val tapGroupHex = wake.groupHex?.let { pushGidHex ->
            resolveTapGroupHex(pushGidHex) { hex ->
                try {
                    PushRouteStore(applicationContext).lookup(accountIdB64, hex)
                } catch (cause: Throwable) {
                    Log.w(TAG, "push route lookup failed; tap keeps the raw push_gid", cause)
                    null
                }
            }
        }

        // ADR-0073 R.5 — a successor group's banner belongs to its
        // conversation: same stack, same id (a new banner replaces the
        // conversation's), one row. Read from the lineage mirror; nothing is
        // written.
        val groupKey = notificationGroupKey(
            rawPushGidHex = wake.groupHex,
            resolvedGroupHex = tapGroupHex,
            lineage = try {
                me.lakko.app.chat.LineageMirror.load(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                emptyList()
            },
        )

        val clickIntent = Intent(applicationContext, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            if (tapGroupHex != null) {
                putExtra(EXTRA_WAKE_GROUP, tapGroupHex)
            }
            if (wake.pointer != null) {
                putExtra(EXTRA_WAKE_POINTER, wake.pointer)
            }
        }
        val pendingIntent = PendingIntent.getActivity(
            applicationContext,
            groupKey?.hashCode() ?: 0,
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
            .setSmallIcon(me.lakko.app.R.drawable.ic_stat_lakko)
            // Real decrypted preview when the background decrypt
            // yielded one; the generic fallback otherwise.
            .setContentTitle(preview?.title ?: "Lakko")
            .setContentText(preview?.body ?: me.lakko.app.l10n.Localization.strings.value["notification.message.new"])
            .setAutoCancel(true)
            // Tag-replace by group so a burst within the same
            // chat collapses into one OS notification — matches
            // the Web + iOS posture.
            .setGroup(groupKey ?: "lakko-default")
            .setContentIntent(pendingIntent)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
        // Section 11 — the picture rides the same notification as the text, so
        // the lock screen shows it exactly when it shows the text.
        if (picture != null) {
            builder.setLargeIcon(picture)
                .setStyle(
                    NotificationCompat.BigPictureStyle()
                        .bigPicture(picture)
                        .bigLargeIcon(null as android.graphics.Bitmap?)
                        .setSummaryText(preview?.body),
                )
        }

        val manager = NotificationManagerCompat.from(applicationContext)
        // Per-group notification id — same hash trick as the
        // PendingIntent request code. Stable id means a fresh
        // wake on the same thread replaces the prior banner.
        val id = groupKey?.hashCode() ?: 0
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
        ensureMessagesNotificationChannel(context)
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
        // the FCM data-payload contract: `s`, `g`, `p`, `k`. (A `v` key was
        // required here before ADR-0078 although the FCM provider never sent
        // one, so no FCM wake parsed; the check is gone.)
        val subscription = data["s"] ?: return null
        val group = data["g"]
        val pointer = data["p"]?.toLongOrNull()
        return WakeBody(subscriptionHex = subscription, groupHex = group, pointer = pointer)
    }

    private data class WakeBody(
        val subscriptionHex: String,
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
 * The one log line a posted wake banner gets, or `null` for a real preview
 * decrypted on this wake. A banner from the app's stored log and every generic
 * "New message" banner say which it is and why, with no identifier.
 */
internal fun wakeBannerLogLine(outcome: PushWakeOutcome, shown: PushPreview?): String? {
    val generic = shown == null || outcome.fallbackReason == WakeFallbackReason.UNNAMED_SENDER
    if (generic) {
        val why = outcome.fallbackReason ?: WakeFallbackReason.NOTHING_FOUND
        val detail = outcome.fallbackDetail?.let { " ($it)" } ?: ""
        return "wake shows the generic banner: ${why.logText}$detail"
    }
    if (outcome.preview == null && shown === outcome.storedPreview) {
        return "wake shows the stored message: the app had already read it"
    }
    return null
}

/**
 * The message notification channel ([LakkoMessagingService.CHANNEL_ID]),
 * created on first use; the channel post notifications
 * (`docs/ux/channel-notifications.md`) post to it too.
 */
public fun ensureMessagesNotificationChannel(context: Context) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val channel = NotificationChannel(
        LakkoMessagingService.CHANNEL_ID,
        me.lakko.app.l10n.Localization.strings.value["notification.channel.messages"],
        NotificationManager.IMPORTANCE_HIGH,
    ).apply {
        description = me.lakko.app.l10n.Localization.strings.value["notification.channel.messages_desc"]
    }
    val manager = context.getSystemService(NotificationManager::class.java)
    manager?.createNotificationChannel(channel)
}

/**
 * ADR-0073 R.5 — does this wake name the old segment of a moved one-to-one?
 *
 * The wake's `g` is the opaque `push_gid` (ADR-0042); it is resolved to the
 * real group through the same local route index the tap deep link uses
 * ([resolveTapGroupHex], including its fall-back to the raw value for a
 * legacy wake) and the closed-state store is asked about the result. Pure:
 * both stores are lambdas. Never throws, and a failed read answers `false`,
 * so a store that will not open shows the banner rather than swallowing a
 * live conversation's wake.
 */
public fun wakeNamesClosedOneToOne(
    wakeGroupHex: String?,
    lookupGroupIdHex: (String) -> String?,
    isClosedOneToOne: (String) -> Boolean,
): Boolean {
    val raw = wakeGroupHex?.trim()?.lowercase()?.takeIf { it.isNotEmpty() } ?: return false
    val groupHex = resolveTapGroupHex(raw, lookupGroupIdHex).lowercase()
    return try {
        isClosedOneToOne(groupHex)
    } catch (_: Throwable) {
        false
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
