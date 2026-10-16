// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.core.app.NotificationManagerCompat

/**
 * The account binding the FCM service gates every wake on (ADR-0042, amended
 * 2026-09-30, with ADR-0078; the rule itself is [wakeAccountFor]).
 *
 * Plain app [SharedPreferences], like [PushRouteStore]: the service reads it
 * on the FCM callback thread before anything else, and the value is not
 * secret — the account id is the key of every per-account store on this
 * device. One binding at a time: a device has at most one active account.
 *
 * Writes the service depends on use `commit()` so that a [clear] has taken
 * effect before the caller does anything else.
 */
public class PushWakeBindingStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** Record that push is set up for [accountIdB64], replacing any earlier
     *  binding. An empty account is not recorded. */
    public fun bind(accountIdB64: String) {
        if (accountIdB64.isEmpty()) return
        prefs.edit().putString(KEY_ACCOUNT, accountIdB64).commit()
    }

    /** The current binding, or `null` when none. */
    public fun load(): PushWakeBinding? {
        val account = prefs.getString(KEY_ACCOUNT, null) ?: return null
        if (account.isEmpty()) return null
        return PushWakeBinding(accountIdB64 = account)
    }

    /** Drop the binding, synchronously. */
    public fun clear() {
        prefs.edit().remove(KEY_ACCOUNT).commit()
    }

    public companion object {
        /** Shared prefs file name — the FCM service opens this by name. v2
         *  since ADR-0078 (v1 carried a device pseudonym beside the account). */
        public const val PREFS_NAME: String = "me.lakko.push-wake-binding.v2"
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
