// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences

/**
 * Per-account `Set<String>` of known group-id hex strings.
 *
 * Persists across process death so [ChatRepository.bootstrap]
 * can iterate every previously-joined group + call `load_group`
 * (UniFFI) to rebuild each `Group` handle eagerly. Without this
 * list the snapshot restore would lay down OpenMLS provider
 * storage but leave no in-memory `Group` references — peer
 * commits or application messages to those groups would silently
 * miss `pollGroup`'s `groups[groupIdHex]` lookup.
 *
 * **Why a separate per-account `SharedPreferences` file** rather
 * than reusing [me.lakko.app.auth.SessionStore]'s shape: the
 * SessionStore is keyed by a single fixed prefs name and
 * carries one account's `(accountIdB64, username)` row; this
 * store needs a `StringSet` keyed by `accountIdB64` so multiple
 * accounts on the same device don't collide. SharedPreferences
 * `getStringSet` is the right tool — atomic upsert under the
 * hood, fast on warm reads, no DB ceremony.
 *
 * **Why a `Set` and not a `List`** even though the call sites
 * iterate sequentially: `Group::create` / `joinFromWelcome`
 * idempotency is the user-visible property. Re-bootstrap on a
 * restart shouldn't append duplicates; `Set` upserts naturally.
 *
 * Mirror of `apps/web/src/chat/mls-store.ts`'s
 * `knownGroupIds` IDB index.
 *
 * Every id here is a group THIS device holds MLS state for. The
 * snapshot and this list are both device-local and are never
 * restored from the account backup, which carries no MLS state
 * (2026-10-02). The ids a restored backup NAMED - conversations
 * this device has not been admitted to yet - live in
 * `backup.BackupRestoreStore` and must never be added here:
 * `bootstrap` would try to load a group the provider does not
 * hold.
 *
 * Phase 1.15f5 — bootstrap-side eager rebuild. The store is
 * touched on three paths: append on `startGroupWith` /
 * `joinGroupFromWelcome`, read on `bootstrap` (after the local
 * snapshot restore), wipe per-account on
 * `forgetDevice(accountIdB64)`.
 */
public class KnownGroupsStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** Read the set of known group-id hex strings for [accountIdB64].
     *  Empty set when the account has none yet (fresh signup or a
     *  cleanly-forgotten device). */
    public fun load(accountIdB64: String): Set<String> {
        return prefs.getStringSet(keyFor(accountIdB64), emptySet())?.toSet() ?: emptySet()
    }

    /** Atomically add [groupIdHex] to the set for [accountIdB64].
     *  No-op when already present. */
    public fun add(accountIdB64: String, groupIdHex: String) {
        val current = load(accountIdB64).toMutableSet()
        if (!current.add(groupIdHex)) return
        prefs.edit().putStringSet(keyFor(accountIdB64), current).apply()
    }

    /**
     * ADR-0045 UX §6.2.4 — drop ONE group from the set, so a chat the user
     * deleted is not eagerly rebuilt by the next [ChatRepository.bootstrap].
     *
     * No-op when absent. Deliberately narrower than [clear], which is the
     * whole-account `forgetDevice` wipe.
     */
    public fun remove(accountIdB64: String, groupIdHex: String) {
        val current = load(accountIdB64).toMutableSet()
        if (!current.remove(groupIdHex)) return
        prefs.edit().putStringSet(keyFor(accountIdB64), current).apply()
    }

    /** Wipe the entire set for [accountIdB64]. Called from
     *  `forgetDevice` — same posture as the
     *  `IdentitySnapshotStore.deleteSnapshot` wipe (per-account,
     *  best-effort, no error surfaced if the row is already
     *  absent). */
    public fun clear(accountIdB64: String) {
        prefs.edit().remove(keyFor(accountIdB64)).apply()
    }

    private fun keyFor(accountIdB64: String): String = KEY_PREFIX + accountIdB64

    public companion object {
        public const val PREFS_NAME: String = "me.lakko.known-groups.v1"

        /** Per-account key prefix — the suffix is the standard-base64
         *  `accountIdB64` shared by every per-account store. */
        private const val KEY_PREFIX: String = "known_groups."
    }
}
