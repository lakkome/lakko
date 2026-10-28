// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.util.Log
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.tasks.await
import me.lakko.app.BuildConfig

/**
 * FCM subscribe / unsubscribe orchestrator.
 *
 * Mirror of:
 *
 *   * `apps/web/src/push/subscribe.ts` (1.20b)
 *   * `apps/ios/Lakko/Push/PushSubscriber.swift` (1.20c)
 *
 * Joins three layers:
 *
 *   1. **Programmatic Firebase initialisation** — we don't use
 *      the `com.google.gms:google-services` Gradle plugin (would
 *      require checking in a real `google-services.json` at
 *      build time). Instead we pull the four FCM identity
 *      fields from `BuildConfig.FCM_*` and feed them into
 *      [`FirebaseOptions.Builder`], then
 *      `FirebaseApp.initializeApp(context, options)` on first
 *      use, as the DEFAULT app: `FirebaseMessaging.getInstance()`
 *      and the SDK's own message delivery read only the default
 *      app. Idempotent — a later call finds it and skips the init.
 *   2. **`FirebaseMessaging.getInstance().token.await()`** — the
 *      coroutine-friendly accessor for the per-device FCM
 *      registration token. The Firebase SDK caches the token;
 *      re-calling returns the same string until the OS rotates
 *      it (and our [`LakkoMessagingService.onNewToken`] callback
 *      fires).
 *   3. **The sealed subscriptions** (ADR-0078 section 3) — the token is
 *      kept on the device ([PushDeviceTokens]) and sealed into one push-gw
 *      row per group ([PushSubscriptions.resubscribeAll]); push-gw holds no
 *      device pseudonym and no token in the clear.
 *
 * ## Graceful degradation
 *
 * When any of the four `BuildConfig.FCM_*` fields is empty —
 * the default for a contributor who hasn't set the
 * `lakko.fcm.*` Gradle properties — [`PushSubscriber/subscribe`]
 * returns `null` and the rest of the app keeps working. This
 * mirrors the web side's "no VAPID key ⇒ no-op subscribe"
 * posture: the integration code is in place, the actual
 * registration just doesn't run without credentials.
 */
public class PushSubscriber(
    private val context: Context,
    private val client: PushClient = PushClient(),
    /** The client for an unsubscribe that outlives the live session
     *  (logout): it spends a token drawn with that departing session. */
    private val detachedClient: (me.lakko.app.anon.AnonSession) -> PushClient = { session ->
        PushClient(
            anonTokens = me.lakko.app.anon.DetachedAnonTokenSource(
                me.lakko.app.anon.AnonTokens.shared,
                session,
            ),
        )
    },
) {

    /**
     * Request the FCM registration token, store it, and send every sealed
     * subscription of [accountIdB64] carrying it (ADR-0078). Idempotent —
     * the Firebase SDK caches the token; re-running replaces the same rows.
     *
     * Returns `null` (no throw) when:
     *
     *   * The build has no FCM credentials configured (missing
     *     `lakko.fcm.*` Gradle properties).
     *   * Firebase fails to initialise / fetch the token (no
     *     Google Play Services on the device, no network, …).
     *   * The token or the binding cannot be stored.
     *
     * Errors are logged via `android.util.Log.w(TAG, …)`. The caller treats
     * `null` as "push not available, fall back to foreground polling".
     *
     * ADR-0042, amended 2026-09-30 — the account is recorded in
     * [PushWakeBindingStore], the binding the FCM service gates every wake
     * on, before any subscription is sent. [stillSignedIn] is asked right
     * before that write, so a subscribe that outlived a Log out (which clears
     * the binding first) does not re-create it. When the binding named
     * ANOTHER account (it never logged out), that account's subscriptions
     * are removed first.
     */
    public suspend fun subscribe(
        accountIdB64: String,
        stillSignedIn: () -> Boolean = { true },
    ): SubscribeResult? {
        if (!ensureFirebase(context)) return null

        val token = try {
            // `FirebaseMessaging.getInstance(firebaseApp)` was made
            // package-private in Firebase BoM 33+; the public path is the
            // no-arg overload, which resolves to the default app, the one
            // [ensureFirebase] initialises.
            FirebaseMessaging.getInstance().token.await()
        } catch (cause: Throwable) {
            Log.w(TAG, "FCM token fetch failed", cause)
            return null
        }
        if (token.isNullOrEmpty()) {
            Log.w(TAG, "FCM token empty — no Play Services or no network?")
            return null
        }

        if (!stillSignedIn()) {
            Log.i(TAG, "signed out during push subscribe; nothing bound or sent")
            return null
        }
        val bindings = PushWakeBindingStore(context)
        try {
            val previous = bindings.load()?.accountIdB64
            if (previous != null && previous != accountIdB64) {
                retire(PushRouteStore(context).subscriptionIds(previous), client)
            }
            PushDeviceTokens(context).setFcm(token)
            bindings.bind(accountIdB64)
        } catch (cause: Throwable) {
            // Without a binding every wake is dropped: push is unavailable.
            Log.w(TAG, "push token or binding write failed", cause)
            return null
        }
        PushSubscriptions.resubscribeAll(context, accountIdB64)
        return SubscribeResult(fcmToken = token)
    }

    /**
     * Remove [accountIdB64]'s sealed subscriptions at push-gw and forget this
     * device's token. Called on logout + forgetDevice. Idempotent +
     * best-effort: failures are logged.
     *
     * The subscription ids live in the account's route rows; a wipe that ran
     * first hands the ids it removed to [PendingPushUnsubscribes], drained
     * here.
     *
     * ADR-0054: each removal spends a `push-register` token. Logout calls
     * this after the live session is gone, so it passes the departing
     * [session] and the tokens are drawn with it.
     */
    public suspend fun unsubscribe(
        accountIdB64: String?,
        session: me.lakko.app.anon.AnonSession? = null,
    ) {
        val ids = PendingPushUnsubscribes(context).drain().toMutableList()
        if (accountIdB64 != null) ids += PushRouteStore(context).subscriptionIds(accountIdB64)
        val unsubscribeClient = if (session != null) detachedClient(session) else client
        try {
            retire(ids, unsubscribeClient)
        } finally {
            if (unsubscribeClient !== client) unsubscribeClient.close()
        }
        PushDeviceTokens(context).clear()
        // The FCM token stays a dangling no-op on Google's side: no push-gw
        // row holds it any more.
    }

    /** `POST /push/unsubscribe` for each id, one after another, best-effort. */
    private suspend fun retire(ids: List<ByteArray>, via: PushClient) {
        val seen = HashSet<String>()
        for (subId in ids) {
            if (!seen.add(PushClient.pseudonymHex(subId))) continue
            try {
                via.unsubscribe(subId)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                // A row left behind holds the token sealed under a key push-gw
                // only sees inside a wake; it goes when FCM reports the token
                // invalid.
                Log.w(TAG, "push-gw unsubscribe failed", cause)
            }
        }
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    public companion object {
        /** Logcat tag for the push module. */
        public const val TAG: String = "lakko-push"

        /**
         * Initialise the default Firebase app from `BuildConfig.FCM_*`
         * unless it exists. Returns false when FCM is not configured for
         * this build or the init fails; push then stays off.
         *
         * It must be the DEFAULT app: `FirebaseMessaging.getInstance()` and
         * the SDK's delivery to [LakkoMessagingService] read only that one.
         * Initialising a named app instead left the token fetch failing
         * with "Default FirebaseApp is not initialized".
         */
        public fun ensureFirebase(context: Context): Boolean {
            val options = firebaseOptionsOrNull() ?: run {
                Log.i(TAG, "FCM disabled (missing BuildConfig.FCM_* fields)")
                return false
            }
            return try {
                val apps = FirebaseApp.getApps(context.applicationContext)
                if (apps.none { it.name == FirebaseApp.DEFAULT_APP_NAME }) {
                    FirebaseApp.initializeApp(context.applicationContext, options)
                }
                true
            } catch (cause: Throwable) {
                Log.w(TAG, "Firebase init failed", cause)
                false
            }
        }

        private fun firebaseOptionsOrNull(): FirebaseOptions? {
            val projectId = BuildConfig.FCM_PROJECT_ID
            val appId = BuildConfig.FCM_APPLICATION_ID
            val apiKey = BuildConfig.FCM_API_KEY
            val senderId = BuildConfig.FCM_SENDER_ID
            if (projectId.isEmpty() || appId.isEmpty() ||
                apiKey.isEmpty() || senderId.isEmpty()
            ) {
                return null
            }
            return FirebaseOptions.Builder()
                .setProjectId(projectId)
                .setApplicationId(appId)
                .setApiKey(apiKey)
                .setGcmSenderId(senderId)
                .build()
        }
    }
}

/** Result of a successful [PushSubscriber.subscribe]. */
public data class SubscribeResult(
    val fcmToken: String,
)
