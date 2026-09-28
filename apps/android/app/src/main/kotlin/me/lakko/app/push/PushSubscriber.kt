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
 *      use. Idempotent — subsequent calls find the named app
 *      already created and skip the init.
 *   2. **`FirebaseMessaging.getInstance().token.await()`** — the
 *      coroutine-friendly accessor for the per-device FCM
 *      registration token. The Firebase SDK caches the token;
 *      re-calling returns the same string until the OS rotates
 *      it (and our [`LakkoMessagingService.onNewToken`] callback
 *      fires).
 *   3. **`PushClient.register`** — POST the token to
 *      `lakko-svc-push-gw` so it knows where to fan out wakes.
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
    /** The client for an unregister that outlives the live session
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
     * Request the FCM registration token, persist the device
     * pseudonym, and tell `push-gw` about it. Idempotent —
     * the Firebase SDK caches the token; re-running returns
     * the same bytes and just re-UPSERTs the server row.
     *
     * Returns `null` (no throw) when:
     *
     *   * The build has no FCM credentials configured (missing
     *     `lakko.fcm.*` Gradle properties).
     *   * Firebase fails to initialise / fetch the token (no
     *     Google Play Services on the device, no network, …).
     *   * `DevicePushId.getOrCreate` fails (Keystore
     *     unavailable).
     *   * `PushClient.register` fails.
     *
     * Errors are logged via `android.util.Log.w(TAG, …)`. The
     * caller treats `null` as "push not available, fall back
     * to foreground polling".
     */
    public suspend fun subscribe(): SubscribeResult? {
        val options = buildFirebaseOptionsOrNull() ?: run {
            Log.i(TAG, "FCM disabled (missing BuildConfig.FCM_* fields)")
            return null
        }
        val firebaseApp = try {
            ensureFirebaseInitialised(options)
        } catch (cause: Throwable) {
            Log.w(TAG, "Firebase init failed", cause)
            return null
        }

        val token = try {
            // `FirebaseMessaging.getInstance(firebaseApp)` was
            // made package-private in Firebase BoM 33+; the
            // public path is the no-arg overload, which resolves
            // to the default app — which is the only one Lakko
            // ever initialises. `firebaseApp` above is what
            // guarantees the default app is ready; we don't
            // need to pass it through here.
            // `firebaseApp` is referenced above as the init
            // gate; the messaging instance picks up the default
            // app implicitly.
            FirebaseMessaging.getInstance().token.await()
        } catch (cause: Throwable) {
            Log.w(TAG, "FCM token fetch failed", cause)
            return null
        }
        if (token.isNullOrEmpty()) {
            Log.w(TAG, "FCM token empty — no Play Services or no network?")
            return null
        }

        val pseudonym = try {
            DevicePushId.getOrCreate(context)
        } catch (cause: Throwable) {
            Log.w(TAG, "device pseudonym unavailable", cause)
            return null
        }

        try {
            client.register(
                devicePseudonym = pseudonym,
                provider = PushClient.Provider.FCM,
                tokenBytes = token.toByteArray(Charsets.UTF_8),
            )
        } catch (cause: Throwable) {
            Log.w(TAG, "push-gw register failed", cause)
            return null
        }

        return SubscribeResult(devicePseudonym = pseudonym, fcmToken = token)
    }

    /**
     * Drop the push registration locally + tell `push-gw` to
     * forget the token. Called on logout + forgetDevice.
     * Idempotent + best-effort: failures are logged.
     *
     * ADR-0054: the unregister spends a `push-register` token. Logout calls
     * this after the live session is gone, so it passes the departing
     * [session] and the token is drawn with it.
     */
    public suspend fun unsubscribe(session: me.lakko.app.anon.AnonSession? = null) {
        val pseudonym = DevicePushId.peek(context)
        if (pseudonym != null) {
            val unregisterClient = if (session != null) detachedClient(session) else client
            try {
                unregisterClient.unregister(
                    devicePseudonym = pseudonym,
                    provider = PushClient.Provider.FCM,
                )
            } catch (cause: Throwable) {
                Log.w(TAG, "push-gw unregister failed", cause)
            } finally {
                if (unregisterClient !== client) unregisterClient.close()
            }
        }
        // Firebase doesn't expose a `deleteToken()` in the
        // coroutine surface as cleanly as iOS's "stop calling
        // register" pattern; for 1.20d we drop the local
        // pseudonym so the server row stops mapping to us, and
        // leave the FCM token as a dangling no-op on Google's
        // side. A future polish item can wire
        // `FirebaseMessaging.deleteToken()` if the dangling
        // token shows up as a leak risk.
        DevicePushId.clear(context)
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private fun buildFirebaseOptionsOrNull(): FirebaseOptions? {
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

    private fun ensureFirebaseInitialised(
        options: FirebaseOptions,
    ): FirebaseApp {
        // Look for an already-initialised app with our name; if
        // present, reuse it (subsequent calls within the same
        // process). Otherwise init now.
        return try {
            FirebaseApp.getInstance(APP_NAME)
        } catch (_: IllegalStateException) {
            FirebaseApp.initializeApp(context.applicationContext, options, APP_NAME)
        }
    }

    public companion object {
        /** Logcat tag for the push module. */
        public const val TAG: String = "lakko-push"

        /** Name we hand to `FirebaseApp.initializeApp` so we
         *  don't collide with the default Firebase app (which
         *  some host SDK might attempt to create). Using a
         *  named app also lets us cleanly query its existence
         *  via `FirebaseApp.getInstance(APP_NAME)`. */
        public const val APP_NAME: String = "lakko"
    }
}

/** Result of a successful [PushSubscriber.subscribe]. */
public data class SubscribeResult(
    val devicePseudonym: ByteArray,
    val fcmToken: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is SubscribeResult) return false
        return devicePseudonym.contentEquals(other.devicePseudonym) &&
            fcmToken == other.fcmToken
    }
    override fun hashCode(): Int =
        31 * devicePseudonym.contentHashCode() + fcmToken.hashCode()
}
