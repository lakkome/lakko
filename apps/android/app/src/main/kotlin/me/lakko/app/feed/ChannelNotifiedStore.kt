// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONObject

/**
 * The per-channel "last notified post" cursor of
 * `docs/ux/channel-notifications.md` section 5: kept on this device only,
 * never synced, so a post is never notified twice and a refetch of a
 * channel's history announces nothing. Mirror of
 * `apps/web/src/feed/channel-notified-store.ts`.
 *
 * One preferences row per account, `{ channelIdHex: cursor }`. Writes go
 * through the account store gate ([me.lakko.app.account.AccountStoreGate]),
 * so a late write after Forget device is refused, and the wipe removes the
 * row ([me.lakko.app.account.AccountLocalWipe], step `channel-notified`).
 */
public class ChannelNotifiedStore(private val prefs: SharedPreferences) {

    /** Every channel's stored cursor; empty when there is none or it is
     *  unreadable. */
    public fun read(accountIdB64: String): Map<String, Long> {
        val raw = try {
            prefs.getString(rowKey(accountIdB64), null)
        } catch (_: Throwable) {
            null
        } ?: return emptyMap()
        return try {
            val obj = JSONObject(raw)
            val out = LinkedHashMap<String, Long>()
            for (key in obj.keys()) {
                if (!CHANNEL_RE.matches(key)) continue
                val v = obj.optLong(key, -1L)
                if (v >= 0L) out[key] = v
            }
            out
        } catch (_: Throwable) {
            emptyMap()
        }
    }

    /**
     * Store one channel's cursor; it never moves back. Channels not in [keep]
     * (no longer held) are dropped from the row. Refused when the account's
     * stores are sealed (Forget device): nothing is kept, so the next read
     * decides from the read marker alone.
     */
    public fun write(accountIdB64: String, channelIdHex: String, cursor: Long, keep: Set<String>) {
        val id = channelIdHex.lowercase()
        if (!CHANNEL_RE.matches(id) || cursor < 0L) return
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val current = read(accountIdB64)
            val keepLower = keep.mapTo(HashSet()) { it.lowercase() }
            val next = JSONObject()
            for ((k, v) in current) if (k in keepLower) next.put(k, v)
            next.put(id, maxOf(current[id] ?: 0L, cursor))
            try {
                prefs.edit().putString(rowKey(accountIdB64), next.toString()).commit()
            } catch (_: Throwable) {
                // Best-effort, as web.
            }
        }
    }

    /** Remove the account's row. Called by the Forget-device wipe. */
    public fun forget(accountIdB64: String) {
        try {
            prefs.edit().remove(rowKey(accountIdB64)).commit()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    public companion object {
        /** The preferences file. */
        public const val PREFS: String = "lakko.channels.notified.v1"

        private val CHANNEL_RE = Regex("^[0-9a-f]{32}$")

        /** The row's key for an account. */
        public fun rowKey(accountIdB64: String): String =
            me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

        /** The store on this device. */
        public fun on(context: Context): ChannelNotifiedStore =
            ChannelNotifiedStore(context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE))

        /** Forget device / Delete account. */
        public fun forgetAccount(context: Context, accountIdB64: String) {
            on(context).forget(accountIdB64)
        }
    }
}
