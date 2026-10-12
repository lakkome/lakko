// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/*
 * ADR-0065 "Phase 2 revised", R.6 - the per-`(account, group)` CLAIM state a
 * received `group_closed` envelope leaves on this device.
 *
 * Since the visible flow was removed, a `group_closed` envelope has one
 * producer: a device founding the successor of a conversation that cannot be
 * repaired in place. The decision that guards it is `SuccessorClaim.kt`
 * ([claimVerdict]): the FIRST eligible claim in the old journal's order wins,
 * for good. This file is what is kept between sessions:
 *
 *   * `accepted` - the winning claim: the conversation's successor;
 *   * `pending`  - claims that arrived before this device had its own verdict
 *     on the reset, kept in journal order and judged again after the identity
 *     check the claim itself triggers;
 *   * `own`      - this device's own claim, from the moment it is posted
 *     until step 3 decided it;
 *   * `prepared` - the successor this device created locally (step 1), with
 *     the Welcome of its founding commit until that was delivered.
 *
 * Nothing here is shown: the store has no reader in any view (R.0). The
 * rows are SEALED under the device's Keystore key between sessions: a
 * prepared row holds the Welcome of a successor nobody has joined yet.
 * Mirror of `apps/web/src/chat/group-closed-store.ts`.
 */

/** This device's own claim, posted and not yet decided. */
@Serializable
public data class OwnClaim(
    /** Hex of the successor this device created locally. */
    val successorGroupIdHex: String,
    /** The DS message id the publish returned; `null` while the publish is
     *  in flight. */
    val messageId: Long? = null,
)

/**
 * The successor THIS device created for the old group (R.6 step 1), kept
 * from the moment it exists until its Welcome was delivered, and after that
 * as the mark "this device founded it" (the founder alone re-sends, R.7 case
 * 1). The Welcome is a public MLS artefact sealed to its recipients; it is
 * kept so a restart between winning the claim and delivering it does not
 * leave a successor nobody can join.
 */
@Serializable
public data class PreparedSuccessor(
    val successorGroupIdHex: String,
    /** base64 of the Welcome of the founding commit; `null` once delivered. */
    val welcomeB64: String? = null,
    /** base64 account keys of the people the Welcome goes to, one each. */
    val targetKeysB64: List<String> = emptyList(),
    val delivered: Boolean = false,
)

@Serializable
internal data class ClaimRow(
    val m: Long,
    val s: String,
    val k: String,
    val a: Boolean,
    val t: List<String> = emptyList(),
)

/** What is stored for one old group. */
public data class StoredClaims(
    val accepted: SuccessorClaim? = null,
    val pending: List<SuccessorClaim> = emptyList(),
    val own: OwnClaim? = null,
    val prepared: PreparedSuccessor? = null,
)

/** The empty state. */
public val NO_CLAIMS: StoredClaims = StoredClaims()

/** At most this many undecided claims are kept per group: the journal's
 *  claims arrive from members, and an unbounded list would be a way to grow
 *  this device's storage. Chosen, not measured. */
public const val MAX_PENDING_CLAIMS: Int = 16

/** [state] with [claim] kept for a later verdict, in journal order, once. */
public fun withPendingClaim(state: StoredClaims, claim: SuccessorClaim): StoredClaims {
    if (state.pending.any { it.messageId == claim.messageId }) return state
    return state.copy(pending = (state.pending + claim).sortedBy { it.messageId }.take(MAX_PENDING_CLAIMS))
}

/** [state] with [claim] as the conversation's successor; the pending list is
 *  spent. */
public fun withAcceptedClaim(state: StoredClaims, claim: SuccessorClaim): StoredClaims =
    state.copy(accepted = claim, pending = emptyList())

@Serializable
private data class StoredClaimsRow(
    val a: ClaimRow? = null,
    val p: List<ClaimRow> = emptyList(),
    val o: OwnClaim? = null,
    val r: PreparedSuccessor? = null,
)

private fun SuccessorClaim.toRow(): ClaimRow =
    ClaimRow(messageId, successorGroupIdB64, senderKeyB64, senderWasAdmin, adminKeysAtReceiptB64)

private fun ClaimRow.toClaim(): SuccessorClaim? =
    if (m < 0 || s.isEmpty() || k.isEmpty()) null else SuccessorClaim(m, s, k, a, t)

private val claimsJson = Json { ignoreUnknownKeys = true }

/** The row as JSON. Visible for the codec test. */
internal fun encodeStoredClaims(value: StoredClaims): String =
    claimsJson.encodeToString(
        StoredClaimsRow.serializer(),
        StoredClaimsRow(
            a = value.accepted?.toRow(),
            p = value.pending.map { it.toRow() },
            o = value.own,
            r = value.prepared,
        ),
    )

/** The inverse; `null` for a row that is not this schema, and a malformed
 *  accepted claim drops the whole row rather than being trusted. */
internal fun decodeStoredClaims(raw: String?): StoredClaims? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = claimsJson.decodeFromString(StoredClaimsRow.serializer(), raw)
        val accepted = row.a?.let { it.toClaim() ?: return null }
        StoredClaims(
            accepted = accepted,
            pending = row.p.mapNotNull { it.toClaim() }.sortedBy { it.messageId }.take(MAX_PENDING_CLAIMS),
            own = row.o?.takeIf { it.successorGroupIdHex.isNotEmpty() },
            prepared = row.r?.takeIf { it.successorGroupIdHex.isNotEmpty() },
        )
    } catch (_: Exception) {
        null
    }
}

/** Where the claim states live between sessions. */
public interface SuccessorClaimBackend {
    public fun load(accountIdB64: String, groupIdHex: String): StoredClaims?

    /** `false` when the row could not be written to disk (it is still what
     *  [load] answers for the rest of the session). */
    public fun save(accountIdB64: String, groupIdHex: String, value: StoredClaims): Boolean

    public fun remove(accountIdB64: String, groupIdHex: String)

    public fun loadAll(accountIdB64: String): Map<String, StoredClaims>
}

/** In-memory backend (tests). */
public class InMemorySuccessorClaimBackend : SuccessorClaimBackend {
    private val rows = HashMap<String, StoredClaims>()

    private fun key(account: String, group: String) = "$account|${group.lowercase()}"

    override fun load(accountIdB64: String, groupIdHex: String): StoredClaims? =
        synchronized(rows) { rows[key(accountIdB64, groupIdHex)] }

    override fun save(accountIdB64: String, groupIdHex: String, value: StoredClaims): Boolean {
        synchronized(rows) { rows[key(accountIdB64, groupIdHex)] = value }
        return true
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        synchronized(rows) { rows.remove(key(accountIdB64, groupIdHex)) }
    }

    override fun loadAll(accountIdB64: String): Map<String, StoredClaims> =
        synchronized(rows) {
            rows.filterKeys { it.startsWith("$accountIdB64|") }.mapKeys { it.key.substringAfter('|') }
        }
}

private val CLAIM_FILE = MapSerializer(String.serializer(), String.serializer())

/** Every claim state of one account as one stored text. Visible for the
 *  codec test. */
internal fun encodeClaimFile(rows: Map<String, StoredClaims>): String =
    claimsJson.encodeToString(
        CLAIM_FILE,
        rows.mapKeys { it.key.lowercase() }.mapValues { encodeStoredClaims(it.value) },
    )

/** The inverse; a text that is not this schema is "no claims", and a row
 *  that does not decode is left out rather than trusted. */
internal fun decodeClaimFile(raw: String?): Map<String, StoredClaims> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        claimsJson.decodeFromString(
            CLAIM_FILE,
            raw,
        ).mapNotNull { (group, row) -> decodeStoredClaims(row)?.let { group.lowercase() to it } }.toMap()
    } catch (_: Exception) {
        emptyMap()
    }
}

/** One sealed text per account: what [SealedSuccessorClaimBackend] reads
 *  and writes. Production seals with the device's Keystore key. */
public interface ClaimBlobs {
    public fun read(accountIdB64: String): String?

    /** `false` when nothing was written. */
    public fun write(accountIdB64: String, text: String): Boolean

    public fun delete(accountIdB64: String)
}

/** Unsealed, in memory (tests). */
public class InMemoryClaimBlobs : ClaimBlobs {
    public val texts: MutableMap<String, String> = java.util.concurrent.ConcurrentHashMap()

    /** While `true` every write fails, as a full disk or a locked Keystore
     *  would. */
    @Volatile
    public var failing: Boolean = false

    override fun read(accountIdB64: String): String? = texts[accountIdB64]

    override fun write(accountIdB64: String, text: String): Boolean {
        if (failing) return false
        texts[accountIdB64] = text
        return true
    }

    override fun delete(accountIdB64: String) {
        texts.remove(accountIdB64)
    }
}

/**
 * Production backend: ONE file per account, sealed (AES-GCM) under the
 * device's Keystore key, under `noBackupFilesDir`
 * ([me.lakko.app.feed.FeedSealedStore], the same root and key as the other
 * sealed local stores). It holds group ids, public account keys, DS message
 * ids and, in a prepared row, the Welcome of the successor's founding commit.
 * The Welcome is sealed to its recipients, but it is also what lets them in,
 * and until the claim is confirmed nobody is meant to hold it: so the file
 * is not left readable to anything that can read this app's storage.
 *
 * Written synchronously and whole on every change; the file is read once per
 * account and kept in memory. A failed write is answered `false` and the row
 * stays in memory, so the session goes on; the caller that needs the row on
 * disk before it publishes (the own claim, a prepared successor) checks.
 */
public class SealedSuccessorClaimBackend(private val blobs: ClaimBlobs) : SuccessorClaimBackend {
    private val lock = Any()
    private val cache = HashMap<String, MutableMap<String, StoredClaims>>()
    private var seenWipes = wipes.get()

    private fun rows(accountIdB64: String): MutableMap<String, StoredClaims> {
        val now = wipes.get()
        if (now != seenWipes) {
            // An account's file was deleted behind this instance.
            cache.clear()
            seenWipes = now
        }
        return cache.getOrPut(accountIdB64) {
            val read = try {
                blobs.read(accountIdB64)
            } catch (_: Throwable) {
                null
            }
            HashMap(decodeClaimFile(read))
        }
    }

    private fun persist(accountIdB64: String, rows: Map<String, StoredClaims>): Boolean =
        try {
            if (rows.isEmpty()) {
                blobs.delete(accountIdB64)
                true
            } else {
                blobs.write(accountIdB64, encodeClaimFile(rows))
            }
        } catch (_: Throwable) {
            false
        }

    override fun load(accountIdB64: String, groupIdHex: String): StoredClaims? =
        synchronized(lock) { rows(accountIdB64)[groupIdHex.lowercase()] }

    override fun save(accountIdB64: String, groupIdHex: String, value: StoredClaims): Boolean =
        synchronized(lock) {
            val rows = rows(accountIdB64)
            rows[groupIdHex.lowercase()] = value
            persist(accountIdB64, rows)
        }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        synchronized(lock) {
            val rows = rows(accountIdB64)
            if (rows.remove(groupIdHex.lowercase()) != null) persist(accountIdB64, rows)
        }
    }

    override fun loadAll(accountIdB64: String): Map<String, StoredClaims> =
        synchronized(lock) { HashMap(rows(accountIdB64)) }

    public companion object {
        /** The directory under the sealed-store root
         *  (`noBackupFilesDir/lakko-feed/`). */
        public const val DIR: String = "successor-claims"

        private val wipes = java.util.concurrent.atomic.AtomicLong(0)

        /** The backend over this device's sealed storage. */
        public fun onDevice(context: Context): SealedSuccessorClaimBackend {
            val app = context.applicationContext
            return SealedSuccessorClaimBackend(
                object : ClaimBlobs {
                    override fun read(accountIdB64: String): String? =
                        me.lakko.app.feed.FeedSealedStore.read(app, DIR, accountIdB64)

                    override fun write(accountIdB64: String, text: String): Boolean =
                        me.lakko.app.feed.FeedSealedStore.write(app, DIR, accountIdB64, text)

                    override fun delete(accountIdB64: String) {
                        me.lakko.app.feed.FeedSealedStore.delete(app, DIR, accountIdB64)
                    }
                },
            )
        }

        /** Forget every claim state of the account (its data is wiped). */
        public fun deleteAccount(context: Context, accountIdB64: String) {
            wipes.incrementAndGet()
            try {
                me.lakko.app.feed.FeedSealedStore.delete(context.applicationContext, DIR, accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}
