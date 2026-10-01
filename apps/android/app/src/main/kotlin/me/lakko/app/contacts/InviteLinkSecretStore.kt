// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.util.concurrent.ConcurrentHashMap

/*
 * docs/ux/client-redesign.md section 5.3 — the device that created an `/i/`
 * invite link keeps its link secret for the account.
 *
 * Since links are sealed (2026-09-28) the discovery ledger never holds a
 * link's secret: it rides only in the URL fragment. So the creating device
 * keeps the 16-character fragment text, keyed by the ledger id (the `/i/<id>`
 * slug), with the other per-account secrets (a Keystore-backed
 * EncryptedSharedPreferences file), surviving a restart and a logout. A revoke
 * deletes the row; forget-device deletes every row of the account.
 *
 * The store is a port so the repository's rules run on the host JVM against
 * the in-memory implementation.
 */

/** Where this device keeps the link secrets of the `/i/` links it created. */
public interface InviteLinkSecretStore {
    /** The fragment text (16 base64url characters) of the link [slug], or
     *  `null` when this device does not hold it. */
    public fun get(accountIdB64: String, slug: String): String?

    /** Every held secret of the account, slug -> fragment text. */
    public fun all(accountIdB64: String): Map<String, String>

    public fun put(accountIdB64: String, slug: String, secret: String)

    public fun remove(accountIdB64: String, slug: String)

    public fun clearForAccount(accountIdB64: String)
}

/** In-memory store (tests, and a repository built without a [Context]). */
public class InMemoryInviteLinkSecretStore : InviteLinkSecretStore {
    private val rows = ConcurrentHashMap<String, String>()

    override fun get(accountIdB64: String, slug: String): String? = rows[rowKey(accountIdB64, slug)]

    override fun all(accountIdB64: String): Map<String, String> {
        val prefix = accountPrefix(accountIdB64)
        return rows.filterKeys { it.startsWith(prefix) }.mapKeys { it.key.substring(prefix.length) }
    }

    override fun put(accountIdB64: String, slug: String, secret: String) {
        rows[rowKey(accountIdB64, slug)] = secret
    }

    override fun remove(accountIdB64: String, slug: String) {
        rows.remove(rowKey(accountIdB64, slug))
    }

    override fun clearForAccount(accountIdB64: String) {
        val prefix = accountPrefix(accountIdB64)
        rows.keys.removeIf { it.startsWith(prefix) }
    }
}

/**
 * The store over one [android.content.SharedPreferences] file. Never throws;
 * an unreadable file reads as empty (the row then shows the "created on another
 * device" line, and Revoke still works). Production passes the encrypted file
 * ([EncryptedInviteLinkSecretStore]); a host test passes a plain one, and a
 * second instance over the same file is what a restart looks like.
 */
public open class SharedPrefsInviteLinkSecretStore(
    private val prefsProvider: () -> android.content.SharedPreferences,
) : InviteLinkSecretStore {
    private val prefs: android.content.SharedPreferences by lazy { prefsProvider() }

    override fun get(accountIdB64: String, slug: String): String? = try {
        prefs.getString(rowKey(accountIdB64, slug), null)
    } catch (_: Throwable) {
        null
    }

    override fun all(accountIdB64: String): Map<String, String> = try {
        val prefix = accountPrefix(accountIdB64)
        prefs.all
            .filterKeys { it.startsWith(prefix) }
            .mapNotNull { (key, value) -> (value as? String)?.let { key.substring(prefix.length) to it } }
            .toMap()
    } catch (_: Throwable) {
        emptyMap()
    }

    override fun put(accountIdB64: String, slug: String, secret: String) {
        try {
            prefs.edit().putString(rowKey(accountIdB64, slug), secret).apply()
        } catch (_: Throwable) {
            // Best effort: a lost secret costs re-sharing from this device,
            // never the link itself.
        }
    }

    override fun remove(accountIdB64: String, slug: String) {
        try {
            prefs.edit().remove(rowKey(accountIdB64, slug)).apply()
        } catch (_: Throwable) {
            // best effort
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        try {
            val prefix = accountPrefix(accountIdB64)
            val editor = prefs.edit()
            for (key in prefs.all.keys) if (key.startsWith(prefix)) editor.remove(key)
            editor.apply()
        } catch (_: Throwable) {
            // best effort
        }
    }
}

/** The on-device store: one Keystore-backed EncryptedSharedPreferences file,
 *  like the other per-account secrets. */
public class EncryptedInviteLinkSecretStore(context: Context) : SharedPrefsInviteLinkSecretStore({
    val app = context.applicationContext
    val master = MasterKey.Builder(app)
        .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
        .build()
    EncryptedSharedPreferences.create(
        app,
        ENCRYPTED_PREFS_NAME,
        master,
        EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
        EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
    )
})

private const val ENCRYPTED_PREFS_NAME: String = "lakko-invite-link-secrets"

private const val KEY_PREFIX: String = "invite-link-secret-v1:"

/** `|` is in neither standard base64 nor base64url, so the split is
 *  unambiguous and an account prefix never matches a longer account id. */
private const val ROW_SEPARATOR: Char = '|'

private fun accountPrefix(accountIdB64: String): String = KEY_PREFIX + accountIdB64 + ROW_SEPARATOR

private fun rowKey(accountIdB64: String, slug: String): String = accountPrefix(accountIdB64) + slug
