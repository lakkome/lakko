// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context

/**
 * Has this install already raised the system notification prompt?
 *
 * One boolean, written once, read on every session start. It carries no
 * account, no identifier and no key material, so plain `SharedPreferences` —
 * the posture `ReleaseUpdateChecker` already takes for its dismissal state
 * (invariant #1 is not engaged).
 *
 * **It is written before the prompt is raised, not after.** Raising a
 * permission dialog can take the process down with it, and the answer arrives
 * in a callback that a dead process never runs. Writing afterwards would turn
 * that into a prompt on every launch: the flag never lands, the rule keeps
 * saying [NotificationPermissionAsk.Ask], and the platform's two-dismissal
 * budget is spent for the user without them ever choosing anything. Writing
 * first can at worst cost one prompt that was never seen, which system
 * settings can still fix; writing last costs the permission itself.
 *
 * Both calls touch the disk, so callers run them off the main thread.
 *
 * It is deliberately NOT cleared on log out or forget-device. The question it
 * answers is about the phone — "has this person been shown the system prompt
 * from this install" — and the platform's own refusal count does not reset
 * either, so clearing it would only re-ask into a dialog Android no longer
 * shows.
 */
public object NotificationPermissionStore {

    private const val PREFS_NAME = "lakko.notifications"
    private const val KEY_ASKED = "post-notifications-asked-v1"

    /** Whether the system prompt has been raised by this install. */
    public fun askedBefore(context: Context): Boolean =
        prefs(context).getBoolean(KEY_ASKED, false)

    /**
     * Record that the prompt is being raised. Idempotent; committed
     * synchronously so it is on disk before the dialog is.
     */
    @Suppress("ApplySharedPref")
    public fun markAsked(context: Context) {
        prefs(context).edit().putBoolean(KEY_ASKED, true).commit()
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
}
