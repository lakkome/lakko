// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.content.SharedPreferences

/**
 * The channel id a handle was claimed for, per account, from the moment of the
 * claim until the channel exists.
 *
 * Discovery binds a handle to a channel id for good, and the create that
 * follows can fail, so a retry that minted a new id would be told its own
 * handle is taken. A retry takes the id from here; discovery answers a repeated
 * claim of the same name with the same id (the same handle write key) as the
 * first claim, and the owner's journal
 * claim and the channel create are idempotent on it too.
 */
internal interface HandleClaimRows {
    fun get(key: String): String?
    fun put(key: String, value: String)
    fun remove(key: String)
}

internal class ChannelHandleClaims(private val rows: HandleClaimRows, private val fresh: () -> String) {

    /** The id to create under for [handle] (empty: no handle, a new id). */
    fun idFor(accountIdB64: String, handle: String): String {
        if (handle.isEmpty()) return fresh()
        val key = rowKey(accountIdB64, handle)
        rows.get(key)?.let { return it }
        // Kept before the claim goes out: an answer lost on the way back must
        // not cost the name.
        return fresh().also { rows.put(key, it) }
    }

    /** Whether a create on this account claimed [handle] and has not
     *  finished: the name is this account's to take back, not "taken". */
    fun isClaimed(accountIdB64: String, handle: String): Boolean =
        handle.isNotEmpty() && rows.get(rowKey(accountIdB64, handle)) != null

    /** The name is someone else's, or the channel exists: nothing to reuse. */
    fun forget(accountIdB64: String, handle: String) {
        if (handle.isNotEmpty()) rows.remove(rowKey(accountIdB64, handle))
    }

    private fun rowKey(accountIdB64: String, handle: String): String =
        "$accountIdB64|${handle.lowercase()}"

    companion object {
        private const val PREFS = "lakko.channels.handle-claims"

        private fun prefs(context: Context): SharedPreferences =
            context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

        fun on(context: Context): ChannelHandleClaims {
            val held = prefs(context)
            return ChannelHandleClaims(
                object : HandleClaimRows {
                    override fun get(key: String): String? = held.getString(key, null)

                    override fun put(key: String, value: String) {
                        held.edit().putString(key, value).commit()
                    }

                    override fun remove(key: String) {
                        held.edit().remove(key).apply()
                    }
                },
                ChannelsComposer::freshChannelIdHex,
            )
        }

        /** Forget device / Delete account. */
        fun forgetAccount(context: Context, accountIdB64: String) {
            val held = prefs(context)
            val prefix = "$accountIdB64|"
            val editor = held.edit()
            held.all.keys.filter { it.startsWith(prefix) }.forEach(editor::remove)
            editor.apply()
        }
    }
}
