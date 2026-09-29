// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.util.concurrent.ConcurrentHashMap

/*
 * ADR-0057 — the two pieces of per-(account, group) state journal ownership
 * keeps on a device:
 *
 *   * the REGISTRAR SEED this account was appointed with (a member's; the
 *     owner derives its root and needs none). A signing secret: stored in the
 *     same Keystore-backed EncryptedSharedPreferences as the push routing
 *     secret, never logged, never sent anywhere, dropped on forget-device.
 *   * the SEED ENVELOPES the owner delivered (account key -> rank). Since the
 *     second security pass (finding 2) the registrar set on the DS is
 *     converged from the live roster with `registrars-set`; this record only
 *     says which accounts still need an envelope. Not a secret, kept in the
 *     same file for one lifecycle.
 *   * the OWED ROOT HANDOVERS (group ids) an outgoing owner still has to send
 *     after an ownership transfer, so one resumes after a restart.
 *   * the ROOT HELD markers (finding 4): groups whose journal root this
 *     account is known to hold on the DS. Group ids only.
 *   * the PENDING ROOT SEEDS (finding 4): an old root seed a new owner opened
 *     and has not yet used successfully, so a takeover survives a restart. A
 *     signing secret, under the same Keystore-backed encryption as the
 *     registrar seeds; never logged, never synced, dropped on forget-device.
 *
 * Both are ports, so the repository's decisions run on the host JVM against
 * the in-memory implementations.
 */

/** Where this account's registrar seeds live. */
public interface RegistrarSeedStore {
    public fun get(accountIdB64: String, groupIdHex: String): ByteArray?

    /** A newer envelope replaces the stored seed. */
    public fun put(accountIdB64: String, groupIdHex: String, seed: ByteArray)

    public fun remove(accountIdB64: String, groupIdHex: String)

    public fun clearForAccount(accountIdB64: String)
}

/** Where the owner's established appointments live. */
public interface AppointmentStore {
    public fun get(accountIdB64: String, groupIdHex: String): Map<String, JournalRank>

    public fun put(accountIdB64: String, groupIdHex: String, appointed: Map<String, JournalRank>)

    public fun clearForAccount(accountIdB64: String)
}

/** Finding 4 — groups whose journal root this account holds on the DS. */
public interface RootHeldStore {
    public fun isHeld(accountIdB64: String, groupIdHex: String): Boolean

    public fun mark(accountIdB64: String, groupIdHex: String)

    public fun clear(accountIdB64: String, groupIdHex: String)

    public fun clearForAccount(accountIdB64: String)
}

/** Finding 4 — old root seeds received as the new owner, kept until the
 *  takeover lands or answers 401. [get] returns a copy the caller wipes. */
public interface PendingRootSeedStore {
    public fun groups(accountIdB64: String): Set<String>

    public fun get(accountIdB64: String, groupIdHex: String): ByteArray?

    public fun put(accountIdB64: String, groupIdHex: String, seed: ByteArray)

    public fun remove(accountIdB64: String, groupIdHex: String)

    public fun clearForAccount(accountIdB64: String)
}

/** In-memory root-held markers (tests). */
public class InMemoryRootHeldStore : RootHeldStore {
    private val rows: MutableSet<String> = ConcurrentHashMap.newKeySet()

    override fun isHeld(accountIdB64: String, groupIdHex: String): Boolean = rowKey(accountIdB64, groupIdHex) in rows

    override fun mark(accountIdB64: String, groupIdHex: String) {
        rows.add(rowKey(accountIdB64, groupIdHex))
    }

    override fun clear(accountIdB64: String, groupIdHex: String) {
        rows.remove(rowKey(accountIdB64, groupIdHex))
    }

    override fun clearForAccount(accountIdB64: String) {
        rows.removeIf { it.startsWith(accountIdB64 + ROW_SEPARATOR) }
    }
}

/** In-memory pending root seeds (tests). Mirrors the on-device store's copy
 *  discipline: what goes in and comes out are copies. */
public class InMemoryPendingRootSeedStore : PendingRootSeedStore {
    private val rows = ConcurrentHashMap<String, ByteArray>()

    override fun groups(accountIdB64: String): Set<String> {
        val prefix = accountIdB64 + ROW_SEPARATOR
        return rows.keys.filter { it.startsWith(prefix) }.map { it.substring(prefix.length) }.toSet()
    }

    override fun get(accountIdB64: String, groupIdHex: String): ByteArray? = rows[rowKey(accountIdB64, groupIdHex)]?.copyOf()

    override fun put(accountIdB64: String, groupIdHex: String, seed: ByteArray) {
        rows.put(rowKey(accountIdB64, groupIdHex), seed.copyOf())?.fill(0)
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        rows.remove(rowKey(accountIdB64, groupIdHex))?.fill(0)
    }

    override fun clearForAccount(accountIdB64: String) {
        for (group in groups(accountIdB64)) remove(accountIdB64, group)
    }
}

/** In-memory stores (tests). */
public class InMemoryRegistrarSeedStore : RegistrarSeedStore {
    private val rows = ConcurrentHashMap<String, ByteArray>()

    override fun get(accountIdB64: String, groupIdHex: String): ByteArray? = rows[rowKey(accountIdB64, groupIdHex)]?.copyOf()

    override fun put(accountIdB64: String, groupIdHex: String, seed: ByteArray) {
        rows[rowKey(accountIdB64, groupIdHex)] = seed.copyOf()
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        rows.remove(rowKey(accountIdB64, groupIdHex))
    }

    override fun clearForAccount(accountIdB64: String) {
        rows.keys.removeIf { it.startsWith(accountIdB64 + ROW_SEPARATOR) }
    }
}

/** In-memory appointments (tests). */
public class InMemoryAppointmentStore : AppointmentStore {
    private val rows = ConcurrentHashMap<String, Map<String, JournalRank>>()

    override fun get(accountIdB64: String, groupIdHex: String): Map<String, JournalRank> =
        rows[rowKey(accountIdB64, groupIdHex)].orEmpty()

    override fun put(accountIdB64: String, groupIdHex: String, appointed: Map<String, JournalRank>) {
        rows[rowKey(accountIdB64, groupIdHex)] = appointed.toMap()
    }

    override fun clearForAccount(accountIdB64: String) {
        rows.keys.removeIf { it.startsWith(accountIdB64 + ROW_SEPARATOR) }
    }
}

/** The on-device implementation of both ports ([appointments] is the
 *  second). */
public class EncryptedJournalOwnershipStore(context: Context) : RegistrarSeedStore {
    private val app = context.applicationContext

    private val prefs: android.content.SharedPreferences by lazy {
        val master = MasterKey.Builder(app)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            app,
            PREFS_NAME,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun get(accountIdB64: String, groupIdHex: String): ByteArray? =
        prefs.getString(SEED_PREFIX + rowKey(accountIdB64, groupIdHex), null)?.let(::decodeSeed)

    override fun put(accountIdB64: String, groupIdHex: String, seed: ByteArray) {
        prefs.edit()
            .putString(SEED_PREFIX + rowKey(accountIdB64, groupIdHex), java.util.Base64.getEncoder().encodeToString(seed))
            .apply()
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        prefs.edit().remove(SEED_PREFIX + rowKey(accountIdB64, groupIdHex)).apply()
    }

    override fun clearForAccount(accountIdB64: String) {
        val editor = prefs.edit()
        for (key in prefs.all.keys) {
            if (key.startsWith(SEED_PREFIX + accountIdB64 + ROW_SEPARATOR) ||
                key.startsWith(APPOINTED_PREFIX + accountIdB64 + ROW_SEPARATOR) ||
                key.startsWith(OWED_HANDOVER_PREFIX + accountIdB64 + ROW_SEPARATOR) ||
                key.startsWith(ROOT_HELD_PREFIX + accountIdB64 + ROW_SEPARATOR) ||
                key.startsWith(PENDING_ROOT_SEED_PREFIX + accountIdB64 + ROW_SEPARATOR)
            ) {
                editor.remove(key)
            }
        }
        editor.apply()
    }

    // AppointmentStore — `get` / `put` overloads differ by return type, so the
    // appointment half is exposed through [appointments].

    /** The appointment half of this store. */
    public val appointments: AppointmentStore = object : AppointmentStore {
        override fun get(accountIdB64: String, groupIdHex: String): Map<String, JournalRank> =
            decodeAppointments(prefs.getString(APPOINTED_PREFIX + rowKey(accountIdB64, groupIdHex), null))

        override fun put(accountIdB64: String, groupIdHex: String, appointed: Map<String, JournalRank>) {
            prefs.edit()
                .putString(APPOINTED_PREFIX + rowKey(accountIdB64, groupIdHex), encodeAppointments(appointed))
                .apply()
        }

        override fun clearForAccount(accountIdB64: String) {
            this@EncryptedJournalOwnershipStore.clearForAccount(accountIdB64)
        }
    }

    /** The owed root handovers (ADR-0057 S2 finding 1): group ids only, kept
     *  in the same file so forget-device wipes them with the rest. */
    public val owedHandovers: OwedHandoverStore = object : OwedHandoverStore {
        override fun groups(accountIdB64: String): Set<String> {
            val prefix = OWED_HANDOVER_PREFIX + accountIdB64 + ROW_SEPARATOR
            return prefs.all.keys.filter { it.startsWith(prefix) }.map { it.substring(prefix.length) }.toSet()
        }

        override fun add(accountIdB64: String, groupIdHex: String) {
            prefs.edit().putString(OWED_HANDOVER_PREFIX + rowKey(accountIdB64, groupIdHex), "1").apply()
        }

        override fun remove(accountIdB64: String, groupIdHex: String) {
            prefs.edit().remove(OWED_HANDOVER_PREFIX + rowKey(accountIdB64, groupIdHex)).apply()
        }

        override fun clearForAccount(accountIdB64: String) {
            this@EncryptedJournalOwnershipStore.clearForAccount(accountIdB64)
        }
    }

    /** The root-held markers, in the same file. */
    public val rootHeld: RootHeldStore = object : RootHeldStore {
        override fun isHeld(accountIdB64: String, groupIdHex: String): Boolean =
            prefs.contains(ROOT_HELD_PREFIX + rowKey(accountIdB64, groupIdHex))

        override fun mark(accountIdB64: String, groupIdHex: String) {
            prefs.edit().putString(ROOT_HELD_PREFIX + rowKey(accountIdB64, groupIdHex), "1").apply()
        }

        override fun clear(accountIdB64: String, groupIdHex: String) {
            prefs.edit().remove(ROOT_HELD_PREFIX + rowKey(accountIdB64, groupIdHex)).apply()
        }

        override fun clearForAccount(accountIdB64: String) {
            this@EncryptedJournalOwnershipStore.clearForAccount(accountIdB64)
        }
    }

    /** The pending old root seeds, encrypted like the registrar seeds. Written
     *  with `commit()` so a seed is on disk before the takeover is attempted. */
    public val pendingRootSeeds: PendingRootSeedStore = object : PendingRootSeedStore {
        override fun groups(accountIdB64: String): Set<String> {
            val prefix = PENDING_ROOT_SEED_PREFIX + accountIdB64 + ROW_SEPARATOR
            return prefs.all.keys.filter { it.startsWith(prefix) }.map { it.substring(prefix.length) }.toSet()
        }

        override fun get(accountIdB64: String, groupIdHex: String): ByteArray? =
            prefs.getString(PENDING_ROOT_SEED_PREFIX + rowKey(accountIdB64, groupIdHex), null)?.let(::decodeSeed)

        override fun put(accountIdB64: String, groupIdHex: String, seed: ByteArray) {
            prefs.edit()
                .putString(
                    PENDING_ROOT_SEED_PREFIX + rowKey(accountIdB64, groupIdHex),
                    java.util.Base64.getEncoder().encodeToString(seed),
                )
                .commit()
        }

        override fun remove(accountIdB64: String, groupIdHex: String) {
            prefs.edit().remove(PENDING_ROOT_SEED_PREFIX + rowKey(accountIdB64, groupIdHex)).commit()
        }

        override fun clearForAccount(accountIdB64: String) {
            this@EncryptedJournalOwnershipStore.clearForAccount(accountIdB64)
        }
    }

    private companion object {
        const val PREFS_NAME = "lakko-journal-ownership"
        const val ROOT_HELD_PREFIX = "root-held-v1:"
        const val PENDING_ROOT_SEED_PREFIX = "pending-root-seed-v1:"
        const val SEED_PREFIX = "registrar-seed-v1:"
        const val APPOINTED_PREFIX = "appointed-v1:"
        const val OWED_HANDOVER_PREFIX = "owed-root-handover-v1:"

        fun decodeSeed(text: String): ByteArray? =
            try {
                java.util.Base64.getDecoder().decode(text).takeIf { it.size == REGISTRAR_SEED_LEN }
            } catch (_: IllegalArgumentException) {
                null
            }
    }
}

/** Encode appointments as `hex=A,hex=M` (hex has no `=` or `,`). */
public fun encodeAppointments(appointed: Map<String, JournalRank>): String =
    appointed.entries.joinToString(",") { (hex, rank) ->
        hex + "=" + when (rank) {
            JournalRank.ADMIN -> "A"
            JournalRank.MEMBER -> "M"
        }
    }

/** Reverse of [encodeAppointments]; a malformed entry is skipped. */
public fun decodeAppointments(text: String?): Map<String, JournalRank> {
    if (text.isNullOrEmpty()) return emptyMap()
    val out = LinkedHashMap<String, JournalRank>()
    for (part in text.split(',')) {
        val idx = part.indexOf('=')
        if (idx <= 0) continue
        val rank = when (part.substring(idx + 1)) {
            "A" -> JournalRank.ADMIN
            "M" -> JournalRank.MEMBER
            else -> continue
        }
        out[part.substring(0, idx)] = rank
    }
    return out
}

/** A registrar seed's width (the core's 32-byte Ed25519 seed). */
public const val REGISTRAR_SEED_LEN: Int = 32

private const val ROW_SEPARATOR: Char = '|'

private fun rowKey(accountIdB64: String, groupIdHex: String): String =
    accountIdB64 + ROW_SEPARATOR + groupIdHex.lowercase()
