// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context

/**
 * Security pass F5 (2026-11-15) - the channel ids whose create started on
 * this device and has not finished (marked before the PublisherGroup is
 * founded and the journal claimed, cleared once the channel exists here).
 *
 * New channel reuses the id a failed create claimed its name for
 * ([ChannelHandleClaims]); [newChannelCreateId] reuses it only while the
 * create is recorded here as unfinished. An id whose create finished may by
 * now be a live channel whose group another owner device changed: founding or
 * claiming under it again could reset that channel's journal, so a fresh id
 * is taken instead. Per account, plain preferences; removed by the account
 * wipe.
 */
public object ChannelCreatePending {
    private const val PREFS = "lakko.channels.create-pending"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(account: String, channelId: String) = "pending|$account|${channelId.lowercase()}"

    public fun mark(context: Context, account: String, channelId: String): Boolean =
        prefs(context).edit().putBoolean(key(account, channelId), true).commit()

    public fun isPending(context: Context, account: String, channelId: String): Boolean =
        prefs(context).getBoolean(key(account, channelId), false)

    public fun clear(context: Context, account: String, channelId: String) {
        prefs(context).edit().remove(key(account, channelId)).commit()
    }

    /** The account wipe. */
    public fun forgetAccount(context: Context, account: String) {
        val p = prefs(context)
        val edit = p.edit()
        p.all.keys.filter { it.startsWith("pending|$account|") }.forEach(edit::remove)
        edit.commit()
    }
}

/**
 * The id New channel creates [handle] under: the id a failed create claimed
 * the name for when that create is still unfinished ([pending]), else a fresh
 * one (the kept id is forgotten first). Never the id of a create that
 * finished.
 */
internal fun newChannelCreateId(
    claims: ChannelHandleClaims,
    accountIdB64: String,
    handle: String,
    pending: (channelId: String) -> Boolean,
): String {
    val id = claims.idFor(accountIdB64, handle)
    if (handle.isEmpty() || pending(id)) return id
    // Not recorded as unfinished: the id was minted just now (nothing
    // started under it) or its create finished. Either way a fresh id is
    // safe, and a finished create's id never is.
    claims.forget(accountIdB64, handle)
    return claims.idFor(accountIdB64, handle)
}
