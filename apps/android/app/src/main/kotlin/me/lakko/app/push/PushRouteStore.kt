// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.content.SharedPreferences

/**
 * Plaintext `push_gid_hex -> (account, group_id)` reverse index
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
 * (`me.lakko.app.push.decryptPushPreview`, wired in Sprint 4)
 * reverse-resolves: it turns an opaque wake `g` (the `push_gid`)
 * back into the real DS `group_id` for the read-only DS fetch. The
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
 * ## Storage layout
 *
 * A single `SharedPreferences` file `"me.lakko.push-routing.v1"`.
 * Key: the lowercase-hex `push_gid` (64 chars — the FMS lookup key).
 * Value: `"<accountIdB64> <groupIdHex>"` — `accountIdB64` is
 * base64 and `groupIdHex` is lowercase hex, neither containing a
 * SPACE, so the single-space separator is unambiguous.
 *
 * NOTE (Sprint 3 scope): this store is only POPULATED here. Wiring
 * the FMS decrypt path to reverse-resolve it lands in Sprint 4; the
 * [lookup] accessor is provided now as the canonical reader.
 */
public class PushRouteStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** A resolved reverse-index row. */
    public data class RouteEntry(
        val accountIdB64: String,
        val groupIdHex: String,
    )

    /**
     * Upsert `pushGidHex -> (accountIdB64, groupIdHex)`. Idempotent:
     * re-filing the same mapping overwrites in place. Called on every
     * secret persist (mint + receive).
     */
    public fun put(pushGidHex: String, accountIdB64: String, groupIdHex: String) {
        prefs.edit()
            .putString(pushGidHex.lowercase(), accountIdB64 + SEPARATOR + groupIdHex)
            .apply()
    }

    /**
     * Reverse-map a `push_gid` (lowercase hex) to its owning
     * `(accountIdB64, groupIdHex)`, or `null` when unknown. The read
     * the ADR-0041 silent-push decrypt path performs in Sprint 4 to
     * turn an opaque wake `g` back into the real DS `group_id`.
     */
    public fun lookup(pushGidHex: String): RouteEntry? {
        val raw = prefs.getString(pushGidHex.lowercase(), null) ?: return null
        val sep = raw.indexOf(SEPARATOR)
        if (sep <= 0 || sep >= raw.length - 1) return null
        return RouteEntry(
            accountIdB64 = raw.substring(0, sep),
            groupIdHex = raw.substring(sep + 1),
        )
    }

    /**
     * Delete ONE reverse-index row, by its `push_gid` (lowercase hex).
     *
     * ADR-0043. The asymmetry this closes: [PushRoutingSecretStore] is
     * keyed `(account, group)` and is **overwritten in place** by a
     * rotation, but this store is keyed by the `push_gid` itself — so a
     * rotation only ever ADDS a row and the superseded
     * `push_gid -> (account, group_id)` entry survives forever. Left
     * behind, each rotation grows the reverse map by one dead entry whose
     * `push_gid` nothing will ever notify again.
     *
     * A no-op when the row is absent (`SharedPreferences.remove` on an
     * unknown key succeeds), so the caller never has to check first — a
     * rotation on a device that had not yet indexed the old gid is simply
     * a no-op.
     *
     * Scoped to the ONE key: never touches [PushRoutingSecretStore], so it
     * can never strand a group by deleting the material a LIVE `push_gid`
     * is derived from.
     */
    public fun remove(pushGidHex: String) {
        prefs.edit().remove(pushGidHex.lowercase()).apply()
    }

    /**
     * Drop every reverse-index row owned by [accountIdB64]. Called
     * from `ChatRepository.forgetDevice`. Best-effort; a row for
     * another account on the same device is left intact.
     */
    public fun clearForAccount(accountIdB64: String) {
        val editor = prefs.edit()
        for ((key, value) in prefs.all) {
            val str = value as? String ?: continue
            val sep = str.indexOf(SEPARATOR)
            if (sep > 0 && str.substring(0, sep) == accountIdB64) editor.remove(key)
        }
        editor.apply()
    }

    public companion object {
        /** Shared prefs file name — the FMS opens this by name. */
        public const val PREFS_NAME: String = "me.lakko.push-routing.v1"

        /** Value field separator. Neither base64 nor hex contains a
         *  space, so the split is unambiguous. */
        private const val SEPARATOR: String = " "
    }
}
