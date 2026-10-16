// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.DiscoveryClient

/*
 * ADR-0078 section 10 - the invite links this device minted.
 *
 * The three ledgers (`/i/`, `/c/`, `/g/`) name no owner and no issuer: each
 * row holds the public half of a revocation key minted for that one link.
 * The seed of that key lives here, beside what the owner's "Invite links"
 * list shows, and is the only thing that revokes the link. There is no
 * server list of an account's links any more: a link minted on another
 * device is listed and revoked there; one whose record is lost lives until
 * it expires or is used up. A Keystore-backed EncryptedSharedPreferences
 * file, this device only, never backed up, never logged.
 */

/** One link this device minted. */
@Serializable
public data class OwnLink(
    /** [KIND_CONTACT], [KIND_CALL] or [KIND_GROUP]. */
    val kind: String,
    /** The ledger id as the URL slug (base64url, 8 characters). */
    val slug: String,
    /** base64url of the 32-byte seed of the link's own revocation key. */
    val revocationSeed: String,
    val label: String = "",
    /** UNIX ms; 0 = never. */
    val expiryMs: Long = 0,
    /** The `max_uses` sentinel (0 single use, `0xFFFFFFFF` unlimited). */
    val maxUses: Long = 0,
    val bootstrapReferral: Boolean = false,
    val mintedAtMs: Long = 0,
) {
    /** The 32-byte revocation seed, or `null` for a malformed record. */
    public fun seed(): ByteArray? =
        runCatching { base64UrlDecodeNoPad(revocationSeed) }.getOrNull()?.takeIf { it.size == 32 }

    /** The 6-byte ledger id, or `null` for a malformed slug. */
    public fun blobId(): ByteArray? = runCatching { base64UrlDecodeNoPad(slug) }.getOrNull()

    public fun isExpired(nowMs: Long): Boolean = expiryMs != 0L && expiryMs <= nowMs

    public companion object {
        public const val KIND_CONTACT: String = "contact"
        public const val KIND_CALL: String = "call"
        public const val KIND_GROUP: String = "group"
    }
}

/** Where this device keeps its [OwnLink] records, per account. */
public interface OwnLinkStore {
    public fun all(accountIdB64: String): List<OwnLink>

    public fun get(accountIdB64: String, kind: String, slug: String): OwnLink? =
        all(accountIdB64).firstOrNull { it.kind == kind && it.slug == slug }

    public fun put(accountIdB64: String, link: OwnLink)

    public fun remove(accountIdB64: String, kind: String, slug: String)

    public fun clearForAccount(accountIdB64: String)
}

/** In-memory store (tests, and a process with no context). */
public class InMemoryOwnLinkStore : OwnLinkStore {
    private val rows = ConcurrentHashMap<String, OwnLink>()

    override fun all(accountIdB64: String): List<OwnLink> {
        val prefix = accountPrefix(accountIdB64)
        return rows.filterKeys { it.startsWith(prefix) }.values.toList()
    }

    override fun put(accountIdB64: String, link: OwnLink) {
        rows[rowKey(accountIdB64, link.kind, link.slug)] = link
    }

    override fun remove(accountIdB64: String, kind: String, slug: String) {
        rows.remove(rowKey(accountIdB64, kind, slug))
    }

    override fun clearForAccount(accountIdB64: String) {
        val prefix = accountPrefix(accountIdB64)
        rows.keys.removeIf { it.startsWith(prefix) }
    }
}

/** The store over one [android.content.SharedPreferences] file. Never
 *  throws; an unreadable row reads as absent. */
public open class SharedPrefsOwnLinkStore(
    private val prefsProvider: () -> android.content.SharedPreferences,
) : OwnLinkStore {
    private val prefs: android.content.SharedPreferences by lazy { prefsProvider() }

    override fun all(accountIdB64: String): List<OwnLink> = try {
        val prefix = accountPrefix(accountIdB64)
        prefs.all
            .filterKeys { it.startsWith(prefix) }
            .values
            .mapNotNull { value -> (value as? String)?.let { runCatching { json.decodeFromString<OwnLink>(it) }.getOrNull() } }
    } catch (_: Throwable) {
        emptyList()
    }

    override fun put(accountIdB64: String, link: OwnLink) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                prefs.edit().putString(rowKey(accountIdB64, link.kind, link.slug), json.encodeToString(OwnLink.serializer(), link)).apply()
            } catch (_: Throwable) {
                // Best effort: a lost record costs revoking this link from here;
                // it still expires.
            }
        }
    }

    override fun remove(accountIdB64: String, kind: String, slug: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                prefs.edit().remove(rowKey(accountIdB64, kind, slug)).apply()
            } catch (_: Throwable) {
                // best effort
            }
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

/** The on-device store: one Keystore-backed EncryptedSharedPreferences file. */
public class EncryptedOwnLinkStore(context: Context) : SharedPrefsOwnLinkStore({
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

/** The process-wide store every minting and revoking path uses. */
public object OwnLinkStores {
    @Volatile
    public var current: OwnLinkStore = InMemoryOwnLinkStore()
        private set

    /** Install the on-device store (`AuthRepository` at start-up). */
    public fun install(context: Context) {
        current = EncryptedOwnLinkStore(context)
    }

    /** Tests. */
    public fun installForTest(store: OwnLinkStore) {
        current = store
    }
}

/** Minting and revoking with a link's own key (ADR-0078 section 10). */
public object OwnLinks {
    private val random = SecureRandom()

    /** A fresh revocation key for one link: the seed this device keeps and
     *  the public key the upload carries. */
    public fun freshRevocationKey(): Pair<ByteArray, ByteArray> {
        val seed = ByteArray(32)
        random.nextBytes(seed)
        return seed to uniffi.lakko_ffi_uniffi.linkRevocationPublicKey(seed)
    }

    /** A record for a link just uploaded. */
    public fun record(
        kind: String,
        blobId: ByteArray,
        seed: ByteArray,
        label: String = "",
        expiryMs: Long = 0,
        maxUses: Long = 0,
        bootstrapReferral: Boolean = false,
        nowMs: Long = System.currentTimeMillis(),
    ): OwnLink = OwnLink(
        kind = kind,
        slug = base64UrlEncodeNoPad(blobId),
        revocationSeed = base64UrlEncodeNoPad(seed),
        label = label,
        expiryMs = expiryMs,
        maxUses = maxUses,
        bootstrapReferral = bootstrapReferral,
        mintedAtMs = nowMs,
    )

    /**
     * Revoke every live link this device keeps a record of, each with its own
     * revocation seed, and drop the records. Account deletion runs this first,
     * while the session still works: the directory cannot delete an account's
     * links any more. Best effort; returns how many could not be reached
     * (their records are kept).
     */
    public suspend fun revokeAll(
        accountIdB64: String,
        discovery: DiscoveryClient,
        store: OwnLinkStore = OwnLinkStores.current,
        nowMs: Long = System.currentTimeMillis(),
    ): Int {
        var unreachable = 0
        for (link in store.all(accountIdB64)) {
            val blobId = link.blobId()
            val seed = link.seed()
            if (link.isExpired(nowMs) || blobId == null || seed == null) {
                store.remove(accountIdB64, link.kind, link.slug)
                continue
            }
            try {
                when (link.kind) {
                    OwnLink.KIND_CALL -> discovery.revokeCallInvite(blobId, seed)
                    OwnLink.KIND_GROUP -> discovery.revokeGroupInvite(blobId, seed)
                    else -> discovery.revokeInvite(blobId, seed)
                }
                store.remove(accountIdB64, link.kind, link.slug)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                unreachable += 1
            } finally {
                seed.fill(0)
            }
        }
        return unreachable
    }
}

private val json = Json { ignoreUnknownKeys = true }

private const val ENCRYPTED_PREFS_NAME: String = "lakko-own-links"

private const val KEY_PREFIX: String = "own-link-v1:"

/** `|` is in neither standard base64 nor base64url. */
private const val ROW_SEPARATOR: Char = '|'

private fun accountPrefix(accountIdB64: String): String = KEY_PREFIX + accountIdB64 + ROW_SEPARATOR

private fun rowKey(accountIdB64: String, kind: String, slug: String): String =
    accountPrefix(accountIdB64) + kind + ROW_SEPARATOR + slug
