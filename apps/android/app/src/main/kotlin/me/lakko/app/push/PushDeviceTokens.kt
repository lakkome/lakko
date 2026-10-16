// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import uniffi.lakko_ffi_uniffi.FfiPushProvider
import uniffi.lakko_ffi_uniffi.FfiPushTokenEntry

/**
 * This device's FCM token (ADR-0078 section 3).
 *
 * Before ADR-0078 the device registered its token once at push-gw under a
 * random device pseudonym and subscribed that pseudonym to every group, so
 * push-gw held the devices x groups graph. Now push-gw holds no pseudonym and
 * no token in the clear: every group subscription carries this device's token
 * sealed under that group's unlock key ([GroupPushRouting] via
 * [NativePushRoutingPorts]). So the token is kept here, on the device, and
 * sealed afresh into each subscription; when it changes, every subscription
 * of the signed-in account is sent again ([PushSubscriber.tokensChanged]).
 *
 * Device state, not account state. `EncryptedSharedPreferences` named
 * `"lakko-push"` (the file the retired device pseudonym lived in), value
 * encrypted under a Keystore master key. Cleared at Log out and Forget device
 * after the account's subscriptions were removed.
 */
public class PushDeviceTokens(private val prefsProvider: () -> SharedPreferences) {

    public constructor(context: Context) : this({ openPrefs(context) })

    /** The FCM token, or `null`. */
    public val fcm: String?
        get() = runCatching { prefsProvider().getString(KEY_FCM, null) }.getOrNull()
            ?.takeIf { it.isNotEmpty() && it.toByteArray(Charsets.UTF_8).size <= MAX_TOKEN_BYTES }

    /** Store the FCM token. Returns whether it changed. An empty or over-long
     *  token is not stored (it clears the slot). */
    public fun setFcm(token: String?): Boolean {
        val next = token?.takeIf { it.isNotEmpty() && it.toByteArray(Charsets.UTF_8).size <= MAX_TOKEN_BYTES }
        val before = fcm
        runCatching {
            val edit = prefsProvider().edit()
            if (next != null) edit.putString(KEY_FCM, next) else edit.remove(KEY_FCM)
            edit.commit()
        }
        return before != next
    }

    /** The entries a sealed subscription carries; empty when this device has
     *  no token (push off): then nothing is subscribed. */
    public fun entries(): List<FfiPushTokenEntry> =
        fcm?.let { listOf(FfiPushTokenEntry(FfiPushProvider.FCM, it.toByteArray(Charsets.UTF_8))) }
            ?: emptyList()

    /** Forget the token. */
    public fun clear() {
        runCatching { prefsProvider().edit().remove(KEY_FCM).commit() }
    }

    public companion object {
        private const val PREFS_NAME = "lakko-push"
        private const val KEY_FCM = "fcm-token-v1"

        /** Longest token a subscription carries (mirror of `MAX_PUSH_TOKEN_LEN`). */
        public const val MAX_TOKEN_BYTES: Int = 2048

        private fun openPrefs(context: Context): SharedPreferences {
            val master = MasterKey.Builder(context.applicationContext)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            return EncryptedSharedPreferences.create(
                context.applicationContext,
                PREFS_NAME,
                master,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        }
    }
}

/**
 * Subscription ids a wipe removed from the route rows before the sign-out's
 * unsubscribe could read them (ADR-0078): [PushSubscriber.unsubscribe] drains
 * them. Plain app [SharedPreferences]; an id names a push-gw row and nothing
 * else.
 */
public class PendingPushUnsubscribes(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** Remember [ids] for the next unsubscribe. */
    public fun stash(ids: List<ByteArray>) {
        if (ids.isEmpty()) return
        val existing = prefs.getStringSet(KEY, emptySet()).orEmpty()
        prefs.edit().putStringSet(KEY, existing + ids.map(PushClient::pseudonymHex)).commit()
    }

    /** Take every stashed id. */
    public fun drain(): List<ByteArray> {
        val hexes = prefs.getStringSet(KEY, emptySet()).orEmpty()
        prefs.edit().remove(KEY).commit()
        return hexes.mapNotNull(::normalizeSubscriptionIdHex).map { hex ->
            ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        }
    }

    private companion object {
        const val PREFS_NAME = "me.lakko.push-sub.pending-unsubscribe.v1"
        const val KEY = "ids"
    }
}
