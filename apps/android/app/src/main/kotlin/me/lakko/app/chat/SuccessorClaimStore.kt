// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
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
 * Nothing here is shown: the store has no reader in any view (R.0). Mirror
 * of `apps/web/src/chat/group-closed-store.ts`.
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

    public fun save(accountIdB64: String, groupIdHex: String, value: StoredClaims)

    public fun remove(accountIdB64: String, groupIdHex: String)

    public fun loadAll(accountIdB64: String): Map<String, StoredClaims>
}

/** In-memory backend (tests). */
public class InMemorySuccessorClaimBackend : SuccessorClaimBackend {
    private val rows = HashMap<String, StoredClaims>()

    private fun key(account: String, group: String) = "$account|${group.lowercase()}"

    override fun load(accountIdB64: String, groupIdHex: String): StoredClaims? =
        synchronized(rows) { rows[key(accountIdB64, groupIdHex)] }

    override fun save(accountIdB64: String, groupIdHex: String, value: StoredClaims) {
        synchronized(rows) { rows[key(accountIdB64, groupIdHex)] = value }
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        synchronized(rows) { rows.remove(key(accountIdB64, groupIdHex)) }
    }

    override fun loadAll(accountIdB64: String): Map<String, StoredClaims> =
        synchronized(rows) {
            rows.filterKeys { it.startsWith("$accountIdB64|") }.mapKeys { it.key.substringAfter('|') }
        }
}

/**
 * Production backend: plain preferences, one row per `(account, old group)`.
 * Group ids, public account keys, DS message ids and a Welcome (a public MLS
 * artefact sealed to its recipients) - no key material. Written
 * synchronously: the own-claim record must be on disk before the publish.
 */
public class SharedPrefsSuccessorClaimBackend(context: Context) : SuccessorClaimBackend {
    private val appContext = context.applicationContext

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun prefix(accountIdB64: String) = "$KEY_PREFIX$accountIdB64:"

    override fun load(accountIdB64: String, groupIdHex: String): StoredClaims? =
        try {
            decodeStoredClaims(prefs().getString(prefix(accountIdB64) + groupIdHex.lowercase(), null))
        } catch (_: Throwable) {
            null
        }

    override fun save(accountIdB64: String, groupIdHex: String, value: StoredClaims) {
        prefs().edit().putString(prefix(accountIdB64) + groupIdHex.lowercase(), encodeStoredClaims(value)).commit()
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        prefs().edit().remove(prefix(accountIdB64) + groupIdHex.lowercase()).apply()
    }

    override fun loadAll(accountIdB64: String): Map<String, StoredClaims> =
        try {
            val p = prefix(accountIdB64)
            prefs().all.entries
                .filter { it.key.startsWith(p) }
                .mapNotNull { e -> decodeStoredClaims(e.value as? String)?.let { e.key.removePrefix(p) to it } }
                .toMap()
        } catch (_: Throwable) {
            emptyMap()
        }

    public companion object {
        public const val PREFS: String = "lakko-successor-claims"

        /** Version 3 is the claim state; rows of the visible flow lived in
         *  other preference files and are never read. */
        private const val KEY_PREFIX = "claims-v3:"

        /** Forget every claim state of the account (its data is wiped). */
        public fun deleteAccount(context: Context, accountIdB64: String) {
            try {
                val p = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                val prefix = "$KEY_PREFIX$accountIdB64:"
                val e = p.edit()
                for (k in p.all.keys) if (k.startsWith(prefix)) e.remove(k)
                e.apply()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}
