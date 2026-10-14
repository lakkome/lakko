// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0074 — two small debts a session carries, each a pure rule over a
 * per-account row so a host JVM test runs it without a server:
 *
 *   * the RECORDED IDENTITY KEY (section 0.1): auth records the account's
 *     identity public key on the first `POST /auth/genesis/attest`. Until
 *     it has, device removal answers `account-key-required`, and whoever
 *     first holds a session could record a key of their own. So the client
 *     calls the mint at signup and on every landing until it succeeded
 *     once for the key it holds, and again after a key reset (new key).
 *
 *   * the OWED LOGOUT (section 2.3): `POST /auth/session/logout` is best
 *     effort; a logout made offline keeps the session pair aside and tells
 *     the server at the next launch, then drops it.
 */

// ---------------------------------------------------------------------
// The recorded identity key
// ---------------------------------------------------------------------

/** `409` from the genesis mint: auth holds ANOTHER key for the account. */
public const val IDENTITY_KEY_MISMATCH: String = "identity-key-mismatch"

/** `403` from device removal (auth and discovery): the account has no
 *  recorded identity key, or the account statement was refused. */
public const val ACCOUNT_KEY_REQUIRED: String = "account-key-required"

/**
 * Whether the genesis mint must be called for [currentPubkeyB64] now:
 * always, until this device saw it succeed for exactly that key. A mark for
 * another key (the keys were reset since) does not count.
 */
public fun genesisMintNeeded(recordedPubkeyB64: String?, currentPubkeyB64: String): Boolean =
    recordedPubkeyB64 != currentPubkeyB64

/** What one genesis mint did. */
public enum class GenesisMintOutcome {
    /** Auth signed the key: it is the recorded one. Marked; not asked again
     *  for this key. */
    Recorded,

    /** `409 identity-key-mismatch`: auth holds another key. Not marked;
     *  asked again at the next landing (a reset whose `finish` this device
     *  has not learned of, or a key another party recorded first). */
    Mismatch,

    /** Anything else: asked again at the next landing. */
    Retry,
}

/** Classify the result of `POST /auth/genesis/attest`; `null` is a 2xx. */
public fun genesisMintOutcome(error: Throwable?): GenesisMintOutcome = when {
    error == null -> GenesisMintOutcome.Recorded
    error is AuthHttpError && error.status == HTTP_CONFLICT_CODE && error.code == IDENTITY_KEY_MISMATCH ->
        GenesisMintOutcome.Mismatch
    else -> GenesisMintOutcome.Retry
}

/** The per-account "recorded" mark: the public key the mint last succeeded
 *  for (a public value; plain storage). */
public class GenesisRecordedStore(private val storage: MarkerStorage) {

    public fun recordedPubkeyB64(accountIdB64: String): String? = try {
        storage.get(KEY_PREFIX + accountIdB64)
    } catch (_: Throwable) {
        null
    }

    public fun markRecorded(accountIdB64: String, pubkeyB64: String) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(KEY_PREFIX + accountIdB64, pubkeyB64) }
        } catch (_: Throwable) {
            // Then the mint is simply asked again at the next landing.
        }
    }

    public fun clear(accountIdB64: String) {
        try {
            storage.remove(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-genesis-recorded"
        private const val KEY_PREFIX: String = "genesis-recorded-v1:"

        public fun onDevice(context: Context): GenesisRecordedStore =
            GenesisRecordedStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

// ---------------------------------------------------------------------
// The owed logout
// ---------------------------------------------------------------------

/** A logout the server has not been told about yet: the pair of the session
 *  that ended, kept only to make that one call. */
@Serializable
public class LogoutOwed(
    public val accessToken: String,
    public val accessExpiresAtMs: Long,
    public val refreshToken: String,
    public val refreshExpiresAtMs: Long,
) {
    override fun toString(): String = "LogoutOwed(<redacted>)"
}

/** What the launch does about an owed logout. */
public enum class LogoutRetryStep {
    /** The access token is still usable: call logout with it. */
    Logout,

    /** The access token ran out but the refresh credential did not: refresh
     *  once, then call logout with the new token. */
    RefreshThenLogout,

    /** Both ran out: the session is dead at the server by itself; drop. */
    Drop,
}

/** The pure rule of the one retry. */
public fun logoutRetryStep(owed: LogoutOwed, nowMs: Long): LogoutRetryStep = when {
    owed.accessExpiresAtMs > nowMs -> LogoutRetryStep.Logout
    owed.refreshExpiresAtMs > nowMs -> LogoutRetryStep.RefreshThenLogout
    else -> LogoutRetryStep.Drop
}

/**
 * Whether an owed logout is kept for another launch after an attempt
 * failed with [error]. ONE retry only (ADR-0074 section 2.3): the marker is
 * kept after the logout itself failed for want of a connection, and dropped
 * after its retry whatever the answer. An HTTP answer always drops (a `401`
 * means the session is already gone).
 */
public fun keepLogoutOwed(error: Throwable, isRetry: Boolean): Boolean =
    !isRetry && error is AuthNetworkError

/** The owed logouts of this device, by account. The rows hold credentials,
 *  so the on-device storage is encrypted ([EncryptedMarkerStorage]). */
public class LogoutOwedStore(private val storage: MarkerStorage) {

    public fun load(accountIdB64: String): LogoutOwed? {
        val raw = try {
            storage.get(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        return try {
            json.decodeFromString(LogoutOwed.serializer(), raw)
        } catch (_: Throwable) {
            null
        }
    }

    public fun save(accountIdB64: String, owed: LogoutOwed) {
        try {
            // The index row inside the same gate: written after a wipe, it
            // would name the wiped account on this device again.
            me.lakko.app.account.AccountStoreGate.writeUnlessWiped(accountIdB64) {
                storage.set(KEY_PREFIX + accountIdB64, json.encodeToString(LogoutOwed.serializer(), owed))
                val index = accounts().toMutableSet().apply { add(accountIdB64) }
                storage.set(INDEX_KEY, index.joinToString(INDEX_SEPARATOR))
            }
        } catch (_: Throwable) {
            // The server is then not told; the credentials expire by themselves.
        }
    }

    public fun clear(accountIdB64: String) {
        try {
            storage.remove(KEY_PREFIX + accountIdB64)
            val index = accounts().toMutableSet().apply { remove(accountIdB64) }
            if (index.isEmpty()) storage.remove(INDEX_KEY) else storage.set(INDEX_KEY, index.joinToString(INDEX_SEPARATOR))
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    /** The accounts with an owed logout. */
    public fun accounts(): Set<String> = try {
        storage.get(INDEX_KEY)?.split(INDEX_SEPARATOR)?.filter { it.isNotEmpty() }?.toSet().orEmpty()
    } catch (_: Throwable) {
        emptySet()
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-logout-owed"
        private const val KEY_PREFIX: String = "logout-owed-v1:"
        private const val INDEX_KEY: String = "logout-owed-index-v1"

        /** Not a base64 character, so it cannot occur in an account id. */
        private const val INDEX_SEPARATOR: String = "\n"
        private val json = Json { ignoreUnknownKeys = true }

        public fun onDevice(context: Context): LogoutOwedStore =
            LogoutOwedStore(EncryptedMarkerStorage(context, PREFS_NAME))
    }
}

/** [MarkerStorage] over one `EncryptedSharedPreferences` file (the posture
 *  of [SessionTokenStore]: AES-256-GCM values, master key in the Android
 *  Keystore). Opened on first use: the open costs a Keystore round trip. */
public class EncryptedMarkerStorage(context: Context, private val name: String) : MarkerStorage {
    private val app: Context = context.applicationContext
    private val prefs by lazy {
        val master = MasterKey.Builder(app).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build()
        EncryptedSharedPreferences.create(
            app,
            name,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun get(key: String): String? = prefs.getString(key, null)

    override fun set(key: String, value: String) {
        prefs.edit().putString(key, value).commit()
    }

    override fun remove(key: String) {
        prefs.edit().remove(key).commit()
    }
}

private const val HTTP_CONFLICT_CODE: Int = 409
