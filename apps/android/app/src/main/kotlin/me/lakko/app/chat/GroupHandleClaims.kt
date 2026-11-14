// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.channels.HandleClaimRows

/**
 * The group ids New group attempts claimed an `@name` with, newest first, per
 * account, until the group exists (ADR-0087 section 3, note of 2026-11-14).
 *
 * A group's `@name` is claimed before anything about the group leaves the
 * device, under a write key derived from the group id. A step after the claim
 * can fail, or the claim's answer can be lost, and every attempt founds a new
 * group with a new random id, so a retry is another key and discovery answers
 * its own name `name-taken`. A retry moves the name from an earlier attempt's
 * key instead ([claimOrTakeBackGroupHandle]).
 *
 * Per account, in `SharedPreferences`: a name and group ids, no secret.
 * Removed by `AccountLocalWipe`.
 */
internal class GroupHandleClaims(private val rows: HandleClaimRows) {

    /** The group ids (hex) earlier attempts claimed [handle] with, newest first. */
    fun groupIds(accountIdB64: String, handle: String): List<String> {
        if (handle.isEmpty()) return emptyList()
        return rows.get(rowKey(accountIdB64, handle))
            ?.split(',')
            ?.filter { it.isNotEmpty() }
            .orEmpty()
    }

    /** Whether a create on this account claimed [handle] and has not
     *  finished: the name is this account's to take back, not "taken". */
    fun isClaimed(accountIdB64: String, handle: String): Boolean = groupIds(accountIdB64, handle).isNotEmpty()

    /** Keep this attempt's group id for [handle], before any request about
     *  the name goes out. Returns the earlier attempts, newest first. */
    fun keep(accountIdB64: String, handle: String, groupIdHex: String): List<String> {
        if (handle.isEmpty()) return emptyList()
        val earlier = groupIds(accountIdB64, handle).filter { it != groupIdHex }
        rows.put(rowKey(accountIdB64, handle), (listOf(groupIdHex) + earlier).take(MAX_ATTEMPTS).joinToString(","))
        return earlier
    }

    /** The group exists, or the name turned out to be someone else's. */
    fun forget(accountIdB64: String, handle: String) {
        if (handle.isNotEmpty()) rows.remove(rowKey(accountIdB64, handle))
    }

    private fun rowKey(accountIdB64: String, handle: String): String =
        "$accountIdB64|${canonicalHandleInput(handle)}"

    companion object {
        /** How many attempts are kept per name. Chosen, not measured. */
        const val MAX_ATTEMPTS: Int = 8

        private const val PREFS = "lakko.groups.handle-claims"

        private fun prefs(context: Context): SharedPreferences =
            context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

        fun on(context: Context): GroupHandleClaims {
            val held = prefs(context)
            return GroupHandleClaims(
                object : HandleClaimRows {
                    override fun get(key: String): String? = held.getString(key, null)

                    override fun put(key: String, value: String) {
                        held.edit().putString(key, value).commit()
                    }

                    override fun remove(key: String) {
                        held.edit().remove(key).apply()
                    }
                },
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
