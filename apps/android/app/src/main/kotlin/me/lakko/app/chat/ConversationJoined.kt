// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences

/**
 * `docs/ux/group-and-channel-privacy.md` section 10 — "Creation counts as
 * activity, so it sits at the top."
 *
 * The time the chat list sorts a chat or a group by is the time of its last
 * message when it has one, else the moment this device created or joined it.
 * The last message wins even when the join is newer: a device added to an
 * account joins all of the account's chats at once, and the history bundle
 * gives those chats their real last message, so taking the newer of the two
 * would stack every chat at the join moment and lose the order. A person
 * removed and added back has no messages left (the end wipes them), so they
 * still land at the top.
 */
internal fun conversationActivityMs(lastMessageMs: Long?, joinedAtMs: Long?): Long? =
    lastMessageMs ?: joinedAtMs

/**
 * The moment this device created or joined each conversation, per
 * `(account, group)`, in plain `SharedPreferences`: a time, about a group this
 * device already lists. Written on creating a group and on joining one from a
 * Welcome, never on loading one, so a restart keeps the moment.
 */
public object ConversationJoinedStore {
    internal const val PREFS: String = "lakko-conversation-joined"
    private const val KEY_PREFIX = "lakko.conversation-joined.v1:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String, groupIdHex: String): String =
        KEY_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase()

    /** The stored moment, or `null` when this device has none for the group. */
    public fun load(context: Context, accountIdB64: String, groupIdHex: String): Long? {
        val stored = prefs(context).getLong(key(accountIdB64, groupIdHex), NONE)
        return stored.takeIf { it != NONE }
    }

    /** A join: a rejoin moves the moment. */
    public fun save(context: Context, accountIdB64: String, groupIdHex: String, atMs: Long) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putLong(key(accountIdB64, groupIdHex), atMs).commit()
        }
    }

    /**
     * A creation. Registering a founded group also runs after a restart, for a
     * group the snapshot brings back, and that is not a new moment.
     */
    public fun saveIfAbsent(context: Context, accountIdB64: String, groupIdHex: String, atMs: Long) {
        if (load(context, accountIdB64, groupIdHex) == null) save(context, accountIdB64, groupIdHex, atMs)
    }

    /** Forget one group (ADR-0087 section 9). */
    public fun delete(context: Context, accountIdB64: String, groupIdHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().remove(key(accountIdB64, groupIdHex)).commit()
        }
    }

    /** Forget every group of [accountIdB64] (forget device). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val p = prefs(context)
        val prefix = KEY_PREFIX + accountIdB64 + ":"
        val e = p.edit()
        for (k in p.all.keys) if (k.startsWith(prefix)) e.remove(k)
        e.commit()
    }

    private const val NONE = Long.MIN_VALUE
}
