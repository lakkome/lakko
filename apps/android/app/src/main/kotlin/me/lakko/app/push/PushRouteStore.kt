// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.content.SharedPreferences

/**
 * Plaintext `(account, push_gid_hex) -> group_id` reverse index
 * (ADR-0042 — opaque per-group push routing id).
 *
 * Mirror of:
 *
 *   * `apps/web/src/push/group-routing-store.ts` (the `routes`
 *     IndexedDB object store)
 *   * `apps/ios/Lakko/Push/PushRouteStore.swift`
 *
 * ## Why plaintext, and why a shared [SharedPreferences]
 *
 * This is the map the ADR-0041 silent-push background decrypt
 * (`me.lakko.app.push.decryptPushWake`) reverse-resolves: it turns an
 * opaque wake `g` (the `push_gid`) back into the real DS `group_id` for
 * the read-only DS fetch. The
 * [com.google.firebase.messaging.FirebaseMessagingService] runs in
 * the background WITHOUT the device key, so the reverse map MUST be
 * readable without decryption — exactly the rationale that keeps
 * [me.lakko.app.chat.KnownGroupsStore] plaintext. And it is safe to
 * be plaintext: `push_gid` is a one-way BLAKE3 derivation (reveals
 * nothing about the secret), and `group_id` is a public DS routing
 * address already shared with the server. The sensitive value — the
 * raw `push_routing_secret` — lives encrypted in
 * [PushRoutingSecretStore]; nothing here is key material or PII.
 *
 * ## Keyed by account (ADR-0042, amended 2026-09-30)
 *
 * Two accounts in one group derive the same `push_gid`, so a row keyed by
 * the id alone was last-writer-wins and a wake could be decrypted as
 * whichever account wrote it last. Every row is now keyed by
 * `(account, push_gid)`; the wake path looks up only the account the wake
 * is bound to ([wakeAccountFor]), and every delete names its account.
 *
 * ## Storage layout
 *
 * A single `SharedPreferences` file `"me.lakko.push-routing.v2"`.
 * Key: `"<accountIdB64> <pushGidHex>"` (standard base64 and lowercase
 * hex, neither containing a SPACE, so the split is unambiguous).
 * Value: the lowercase-hex `group_id`. The v1 file (keyed by `push_gid`
 * alone) is simply no longer read.
 */
public class PushRouteStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * Upsert `(accountIdB64, pushGidHex) -> groupIdHex`. Idempotent:
     * re-filing the same mapping overwrites in place. Called on every
     * secret persist (mint + receive).
     */
    public fun put(accountIdB64: String, pushGidHex: String, groupIdHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs.edit()
                .putString(keyFor(accountIdB64, pushGidHex), groupIdHex.lowercase())
                .apply()
        }
    }

    /**
     * The `group_id` (lowercase hex) [accountIdB64] filed under
     * [pushGidHex], or `null` when this account has no such row. Another
     * account's row for the same id is never returned.
     */
    public fun lookup(accountIdB64: String, pushGidHex: String): String? =
        prefs.getString(keyFor(accountIdB64, pushGidHex), null)?.takeIf { it.isNotEmpty() }

    /**
     * Delete [accountIdB64]'s row for [pushGidHex].
     *
     * ADR-0043: a rotation only ever ADDS a row under the new id, so the
     * superseded one is deleted here. Scoped to the one account: another
     * account's row for the same id (a shared group) stays. A no-op when
     * the row is absent.
     */
    public fun remove(accountIdB64: String, pushGidHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs.edit().remove(keyFor(accountIdB64, pushGidHex)).apply()
        }
    }

    /**
     * Drop every reverse-index row owned by [accountIdB64], and the rows of
     * the retired v1 file that name it (value `"<accountIdB64> <groupIdHex>"`,
     * never read). Called from [me.lakko.app.account.AccountLocalWipe]. A row
     * for another account on the same device is left intact.
     */
    public fun clearForAccount(accountIdB64: String) {
        val prefix = accountIdB64 + SEPARATOR
        val editor = prefs.edit()
        for (key in prefs.all.keys) {
            if (key.startsWith(prefix)) editor.remove(key)
        }
        editor.apply()
        val retired = retiredPrefs
        val retiredEditor = retired.edit()
        for ((key, value) in retired.all) {
            if ((value as? String)?.startsWith(prefix) == true) retiredEditor.remove(key)
        }
        retiredEditor.apply()
    }

    private val retiredPrefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(RETIRED_PREFS_NAME, Context.MODE_PRIVATE)

    public companion object {
        /** Shared prefs file name — the FMS opens this by name. */
        public const val PREFS_NAME: String = "me.lakko.push-routing.v2"

        /** The v1 file (keyed by `push_gid` alone), never read since
         *  2026-09-30. */
        public const val RETIRED_PREFS_NAME: String = "me.lakko.push-routing.v1"

        /** Key field separator. Neither base64 nor hex contains a space. */
        private const val SEPARATOR: String = " "

        private fun keyFor(accountIdB64: String, pushGidHex: String): String =
            accountIdB64 + SEPARATOR + pushGidHex.lowercase()
    }
}
