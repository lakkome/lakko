// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context

/**
 * ADR-0090 decision A (section 2.5) - this device founded the account's
 * DeviceGroup alone (ADR-0081 third amendment point 4: no other device
 * answered its knock), so it founds again the PublisherGroup of every channel
 * the account owns. Recorded when the founding happens, per account, and
 * cleared per channel once that channel was re-founded; the publisher loop
 * runs it. A device that has siblings never records it.
 */
public object ChannelRefounding {
    private const val PREFS = "lakko.channels.refounding"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** The founding-alone happened for [account] at [atMs]. */
    public fun markFoundedAlone(context: Context, account: String, atMs: Long = System.currentTimeMillis()) {
        prefs(context).edit().putLong("owed|$account", atMs).putStringSet("done|$account", emptySet()).apply()
    }

    /** Whether a re-founding is owed for [account]. */
    public fun isOwed(context: Context, account: String): Boolean = prefs(context).contains("owed|$account")

    /** Whether [channelId] was already re-founded since the founding. */
    public fun isDone(context: Context, account: String, channelId: String): Boolean =
        channelId in prefs(context).getStringSet("done|$account", emptySet()).orEmpty()

    /** [channelId] was re-founded. */
    public fun markDone(context: Context, account: String, channelId: String) {
        val done = prefs(context).getStringSet("done|$account", emptySet()).orEmpty() + channelId
        prefs(context).edit().putStringSet("done|$account", done).apply()
    }

    /** Every owned channel was re-founded: the record goes. */
    public fun finish(context: Context, account: String) {
        prefs(context).edit().remove("owed|$account").remove("done|$account").apply()
    }

    /** Forget device. */
    public fun forgetAccount(context: Context, account: String) {
        prefs(context).edit().remove("owed|$account").remove("done|$account").apply()
    }
}
