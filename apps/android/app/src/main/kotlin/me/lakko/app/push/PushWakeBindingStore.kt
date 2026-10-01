// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.core.app.NotificationManagerCompat

/**
 * The `(device pseudonym, account)` binding the FCM service gates every wake
 * on (ADR-0042, amended 2026-09-30; the rule itself is [wakeAccountFor]).
 *
 * Plain app [SharedPreferences], like [PushRouteStore]: the service reads it
 * on the FCM callback thread before anything else, and neither value is
 * secret — the pseudonym travels in every wake, and the account id is the key
 * of every per-account store on this device. One binding at a time: a device
 * has at most one active account.
 *
 * Writes the service depends on use `commit()` so that a [clear] has taken
 * effect before the caller does anything else.
 */
public class PushWakeBindingStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** Record that [devicePseudonym] is registered for [accountIdB64],
     *  replacing any earlier binding. */
    public fun bind(devicePseudonym: ByteArray, accountIdB64: String) {
        prefs.edit()
            .putString(KEY_DEVICE, PushClient.pseudonymHex(devicePseudonym))
            .putString(KEY_ACCOUNT, accountIdB64)
            .commit()
    }

    /** The current binding, or `null` when none (or a half-written one). */
    public fun load(): PushWakeBinding? {
        val device = prefs.getString(KEY_DEVICE, null) ?: return null
        val account = prefs.getString(KEY_ACCOUNT, null) ?: return null
        if (device.isEmpty() || account.isEmpty()) return null
        return PushWakeBinding(devicePseudonymHex = device, accountIdB64 = account)
    }

    /** Drop the binding, synchronously. */
    public fun clear() {
        prefs.edit().remove(KEY_DEVICE).remove(KEY_ACCOUNT).commit()
    }

    public companion object {
        /** Shared prefs file name — the FCM service opens this by name. */
        public const val PREFS_NAME: String = "me.lakko.push-wake-binding.v1"
        private const val KEY_DEVICE: String = "device"
        private const val KEY_ACCOUNT: String = "account"
    }
}

/**
 * The push half of Log out and Forget device, run FIRST, before anything else
 * either does: drop the wake binding (so a wake that lands mid-sign-out is
 * already refused) and remove this app's delivered notifications (so the
 * departing account's banners do not stay on the lock screen).
 */
public object PushSignOut {
    private const val TAG: String = "lakko-push"

    /** Idempotent; never throws. */
    public fun clearFirst(context: Context) {
        val appContext = context.applicationContext
        try {
            PushWakeBindingStore(appContext).clear()
        } catch (cause: Throwable) {
            Log.w(TAG, "push wake binding clear failed", cause)
        }
        try {
            NotificationManagerCompat.from(appContext).cancelAll()
        } catch (cause: Throwable) {
            Log.w(TAG, "delivered notifications clear failed", cause)
        }
    }
}
