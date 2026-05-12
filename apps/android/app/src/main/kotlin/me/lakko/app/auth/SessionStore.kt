// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.content.SharedPreferences

/**
 * Per-device cache of the (accountId, username) marker.
 *
 * Mirror of `apps/web/src/auth/AuthContext.tsx`'s `localStorage`
 * pair — neither field is secret on its own:
 *
 *   * `accountIdB64` — base64 of the 16-byte UUIDv7 the client
 *     picked at signup; the server uses it on every subsequent
 *     login. Knowing it doesn't help an attacker derive any key
 *     (OPAQUE is password-blinded) and doesn't reveal anything
 *     about the user that isn't already on a captured device.
 *   * `username` — human-visible handle. Routinely sent to the
 *     discovery service in plaintext (or rather, OPRF-blinded
 *     when the discovery surface lands in 1.15e).
 *
 * Plain [SharedPreferences] is the right tool here: small,
 * synchronous, no encryption overhead. The Keystore-wrapped
 * material lives in [DeviceKeyStore], not here.
 *
 * Versioned via [SCHEMA_KEY] so a future shape change can detect
 * + drop old rows instead of mis-parsing them.
 */
public class SessionStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * Load the persisted session marker, or `null` when:
     *   * no record exists,
     *   * the schema version doesn't match (the row is dropped on
     *     the next [save]), or
     *   * one of the fields is missing (corrupted prefs).
     */
    public fun load(): PersistedSession? {
        val version = prefs.getInt(SCHEMA_KEY, 0)
        if (version != SCHEMA_VERSION) return null
        val accountIdB64 = prefs.getString(KEY_ACCOUNT_ID, null) ?: return null
        val username = prefs.getString(KEY_USERNAME, null) ?: return null
        return PersistedSession(
            accountIdB64 = accountIdB64,
            username = username,
        )
    }

    /** Replace the cached record. Commits synchronously — the
     *  signup / login flow that writes here needs the marker to
     *  be visible on the next reload, so an in-flight `apply()`
     *  would be a footgun. */
    public fun save(session: PersistedSession) {
        prefs.edit()
            .putInt(SCHEMA_KEY, SCHEMA_VERSION)
            .putString(KEY_ACCOUNT_ID, session.accountIdB64)
            .putString(KEY_USERNAME, session.username)
            .commit()
    }

    /** Drop the cached record. Used by `forgetDevice` (1.15d7).
     *  Note: this does NOT delete the wrapped device key on disk
     *  — [DeviceKeyStore.deleteDeviceKey] must be called
     *  separately. The two layers are intentionally independent
     *  so `logout()` (drop in-memory session, keep persistence)
     *  and `forgetDevice()` (wipe everything) have clean
     *  primitives to build on. */
    public fun clear() {
        prefs.edit().clear().commit()
    }

    public companion object {
        public const val PREFS_NAME: String = "me.lakko.session.v1"
        private const val SCHEMA_KEY = "schema"
        private const val KEY_ACCOUNT_ID = "account_id_b64"
        private const val KEY_USERNAME = "username"
        private const val SCHEMA_VERSION = 1
    }
}

/** Snapshot of the per-device session marker. */
public data class PersistedSession(
    /** Base64-encoded raw 16-byte UUIDv7 account id. Same shape as
     *  the web's `localStorage` `accountIdB64` field. */
    val accountIdB64: String,
    /** The human-visible handle the user picked at signup, e.g.
     *  `alice` (no `@`). Until 1.15e wires discovery in, this is
     *  set from the typed input verbatim — there's no
     *  normalisation step yet. */
    val username: String,
)
