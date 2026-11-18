// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context

/**
 * ADR-0090 section 3.9 — per account and group, the account key of the member
 * whose Welcome admitted this device (the Welcome's sender resolved to its
 * account). An open-items hand-over is kept from that account, or from an owner
 * or admin. Not secret (an account key of a fellow member); kept beside the
 * account's other rows and deleted with them.
 */
public object AdmittingAccounts {
    private const val PREFS = "lakko-admitting-accounts"

    @Volatile
    private var context: Context? = null

    @Volatile
    private var account: String? = null

    /** Bind to the signed-in account (the repository's bootstrap). */
    public fun bind(context: Context, accountIdB64: String?) {
        this.context = context.applicationContext
        this.account = accountIdB64
    }

    public fun record(context: Context, accountIdB64: String, groupIdHex: String, key: ByteArray?) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
        val k = "$accountIdB64/${groupIdHex.lowercase()}"
        if (key == null) prefs.remove(k) else prefs.putString(k, java.util.Base64.getEncoder().encodeToString(key))
        prefs.apply()
    }

    /** The admitting account of [groupIdHex] for the bound account, or `null`. */
    public fun current(groupIdHex: String): ByteArray? {
        val ctx = context ?: return null
        val acct = account ?: return null
        return ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString("$acct/${groupIdHex.lowercase()}", null)
            ?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() }
    }

    /** Forget device / account wipe: drop [accountIdB64]'s rows. */
    public fun deleteAccountRows(context: Context, accountIdB64: String) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val edit = prefs.edit()
        for (k in prefs.all.keys) if (k.startsWith("$accountIdB64/")) edit.remove(k)
        edit.apply()
    }
}
