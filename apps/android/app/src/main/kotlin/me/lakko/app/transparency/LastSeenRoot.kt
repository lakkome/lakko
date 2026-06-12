// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Last-seen transparency-log root store (KT-log S3e, no-rollback gate) —
 * the Android mirror of the web `transparency/last-seen-root.ts`.
 *
 * The transparency log is GLOBAL and append-only — its `log_size` only
 * ever grows. We persist the highest `log_size` this client has ever
 * observed (one global value, not per-account) so that a root whose
 * `log_size` is SMALLER than what we already saw is caught as an
 * unambiguous rollback attack: the log appears to have shrunk, which a
 * correct append-only log can never do.
 *
 * This is the gossip/consistency half of the equivocation defence — a
 * server that tries to show an old, smaller tree (to hide a leaf it later
 * appended) trips the rollback check. The store NEVER advances on a
 * shrink; it only advances on `newLogSize >= seen`.
 *
 * ## Storage posture
 *
 * The high-water mark is NOT secret (a public tree size), but persisting
 * it in [EncryptedSharedPreferences] matches the same hardware-backed
 * at-rest posture as [me.lakko.app.contacts.PeerPinStore]: a device-image
 * attacker cannot silently rewrite the marker to disarm the rollback
 * check. `log_size` fits comfortably in a [Long] (`u64` tree sizes are
 * far below `Long.MAX_VALUE` for any realistic log).
 *
 * The decision logic ([checkAndAdvance]) is abstracted behind
 * [LastSeenRootStore] so the rollback semantics are unit-testable with an
 * in-memory implementation that needs no Android `Context`.
 */
public interface LastSeenRootStore {

    /** Read the highest `log_size` ever seen, or `0` when none persisted.
     *  Exposed for tests + diagnostics. */
    public fun loadLastSeenLogSize(): Long

    /**
     * Compare a freshly-fetched root's `log_size` against the persisted
     * high-water mark, advancing it on a non-shrinking value.
     *
     * Semantics:
     *   * `newLogSize >= seen` → persist `newLogSize` (a `==` is an
     *     idempotent re-observation; a `>` is normal growth). Returns
     *     `true` (OK — the append-only invariant holds; proceed with the
     *     inclusion check).
     *   * `newLogSize < seen`  → the log shrank. **Refuse** — leave the
     *     stored value untouched and return `false` (ROLLBACK). NEVER
     *     advance (or regress) on a shrink.
     *
     * @param newLogSize the `log_size` from the just-fetched signed root.
     * @return `true` when OK + persisted; `false` on a rollback (not
     *         advanced).
     */
    public fun checkAndAdvance(newLogSize: Long): Boolean

    /** Wipe the stored marker. For the "forget device" flow + tests. */
    public fun clear()
}

/**
 * Production [LastSeenRootStore] backed by [EncryptedSharedPreferences].
 *
 * Construct with an Android [Context]; the prefs are opened against
 * `context.applicationContext` so the store outlives any Activity.
 */
public class LastSeenRoot(context: Context) : LastSeenRootStore {

    private val appContext: Context = context.applicationContext

    override fun loadLastSeenLogSize(): Long {
        val v = openPrefs(appContext).getLong(KEY, 0L)
        return if (v >= 0L) v else 0L
    }

    override fun checkAndAdvance(newLogSize: Long): Boolean {
        val seen = loadLastSeenLogSize()
        if (newLogSize < seen) {
            return false
        }
        // Persist only when it actually moves forward to avoid a redundant
        // write on every steady-state observation.
        if (newLogSize > seen) {
            openPrefs(appContext).edit().putLong(KEY, newLogSize).apply()
        }
        return true
    }

    override fun clear() {
        openPrefs(appContext).edit().remove(KEY).apply()
    }

    public companion object {
        private const val PREFS_NAME = "lakko-transparency-root"
        private const val KEY = "last-seen-log-size-v1"

        private fun openPrefs(
            context: Context,
        ): android.content.SharedPreferences {
            val master = MasterKey.Builder(context.applicationContext)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            return EncryptedSharedPreferences.create(
                context.applicationContext,
                PREFS_NAME,
                master,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        }
    }
}

/**
 * In-memory [LastSeenRootStore] used by unit tests for the rollback
 * decision logic (no Android `Context`, no Keystore). Same monotonic
 * semantics as [LastSeenRoot]; not used in production.
 */
public class InMemoryLastSeenRoot : LastSeenRootStore {
    private var seen: Long = 0L

    override fun loadLastSeenLogSize(): Long = seen

    override fun checkAndAdvance(newLogSize: Long): Boolean {
        if (newLogSize < seen) return false
        if (newLogSize > seen) seen = newLogSize
        return true
    }

    override fun clear() {
        seen = 0L
    }
}
