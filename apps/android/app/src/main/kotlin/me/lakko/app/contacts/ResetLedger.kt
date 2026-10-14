// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/*
 * ADR-0073 R.2 "The re-pin" - what this device keeps per peer account beside
 * the pin: the generation it last ACCEPTED, the keys an accepted reset
 * retired, and when the latest reset was signed.
 *
 * Web keeps these on the pin record itself (`contacts/local-store.ts`
 * `acceptedKeyGeneration`, `previousKeysB64`, `supersededKeysB64`,
 * `resetSignedAtMs`). Android's pin is a positional `|` record in
 * EncryptedSharedPreferences, so the same four values live in a ledger row
 * keyed on the same account id; the decisions are the same and pure.
 *
 * Nothing here is a STATE of the contact: no row reads as "reset". A retired
 * key is a display alias for stored history only (C-68); it is never accepted
 * for new traffic, a new pin, a role or a send.
 */

/** One peer account's ledger row. Keys are standard base64. */
@Serializable
public data class ResetLedgerRow(
    /** The generation last accepted for the account (the bundle's at the
     *  first pin; raised only by an accepted reset). */
    val acceptedKeyGeneration: Long? = null,
    /** Every key this account was known under before: display aliases. */
    val previousKeysB64: List<String> = emptyList(),
    /** The keys an ACCEPTED RESET retired: a row or a claim from a leaf
     *  bound to one of these is dropped (R.5, R.6). */
    val supersededKeysB64: List<String> = emptyList(),
    /** The latest accepted attestation's `signed_at`, UNIX milliseconds. */
    val resetSignedAtMs: Long? = null,
)

/** What an accepted reset carries into the store. */
public data class AcceptedReset(
    val accountIdB64: String,
    /** The key this device held for the account (its pin, or the verified
     *  binding of a member it never pinned). */
    val heldKeyB64: String,
    val newKeyB64: String,
    val keyGeneration: Long,
    val signedAtMs: Long,
    val nowMs: Long,
)

/** Outcome of [acceptResetPlan]. */
public sealed interface AcceptResetPlan {
    /** Re-pin to the new key at [marker]; write [row]; [retiredKeysB64] are
     *  the keys that stop being the person's. */
    public data class Accepted(
        val marker: Long,
        val row: ResetLedgerRow,
        val retiredKeysB64: List<String>,
    ) : AcceptResetPlan

    /** The new key is already the pin at this generation or above. */
    public data object Already : AcceptResetPlan

    /** The generation is not above the accepted one: nothing written. The
     *  verdict refuses this before it reaches here; the store repeats the
     *  check so a stale caller cannot lower it. */
    public data object Stale : AcceptResetPlan
}

/**
 * ADR-0073 R.2 "The re-pin", as a pure plan. `pin[account] = new key`, marker
 * `max(held marker, now in seconds)`, accepted generation `g`. The held key
 * and its proven predecessors (a reset makes every predecessor a superseded
 * key) are appended to the previous and superseded keys.
 */
public fun acceptResetPlan(
    pin: Pin?,
    row: ResetLedgerRow?,
    /** The proven predecessors of the pinned key (ADR-0074 section 3.5). */
    provenPredecessorsB64: List<String>,
    reset: AcceptedReset,
): AcceptResetPlan {
    val acceptedGeneration = row?.acceptedKeyGeneration ?: 0L
    if (pin != null && pin.pinnedSignaturePubkeyB64 == reset.newKeyB64) {
        if (acceptedGeneration >= reset.keyGeneration) return AcceptResetPlan.Already
    } else if (reset.keyGeneration <= acceptedGeneration) {
        return AcceptResetPlan.Stale
    }
    val marker = resetRepinMarker(pin?.rotationMarker ?: 0L, reset.nowMs)
    val retired = ArrayList<String>()
    fun retire(k: String) {
        if (k != reset.newKeyB64 && k.isNotEmpty() && k !in retired) retired += k
    }
    if (pin != null) {
        provenPredecessorsB64.forEach(::retire)
        retire(pin.pinnedSignaturePubkeyB64)
    }
    retire(reset.heldKeyB64)
    val previous = ArrayList(row?.previousKeysB64 ?: emptyList())
    val superseded = ArrayList(row?.supersededKeysB64 ?: emptyList())
    for (k in retired) {
        if (k !in previous) previous += k
        if (k !in superseded) superseded += k
    }
    return AcceptResetPlan.Accepted(
        marker = marker,
        row = ResetLedgerRow(
            acceptedKeyGeneration = reset.keyGeneration,
            previousKeysB64 = previous,
            supersededKeysB64 = superseded,
            resetSignedAtMs = reset.signedAtMs,
        ),
        retiredKeysB64 = retired,
    )
}

/** The row after a first observation of a bundle's generation: its baseline.
 *  Never lowers or replaces a held one (only an accepted reset raises it). */
public fun withGenerationBaseline(row: ResetLedgerRow?, bundleGeneration: Long): ResetLedgerRow? {
    if (row?.acceptedKeyGeneration != null) return null
    if (bundleGeneration < 0) return null
    return (row ?: ResetLedgerRow()).copy(acceptedKeyGeneration = bundleGeneration)
}

/** ADR-0073 R.5 / R.6: every key an accepted reset retired, across all
 *  accounts, mapped to the account id it belonged to. */
public fun supersededKeysOf(rows: Map<String, ResetLedgerRow>): Map<String, String> {
    val out = LinkedHashMap<String, String>()
    for ((accountIdB64, row) in rows) for (k in row.supersededKeysB64) out[k] = accountIdB64
    return out
}

/** The account and generation behind a key an ACCEPTED RESET re-pinned this
 *  device to (R.4): what a Welcome to the new identity derives the
 *  continuity reach key from. */
public data class AcceptedResetOfKey(val accountIdB64: String, val keyGeneration: Long, val signedAtMs: Long)

/**
 * `null` for a key no accepted reset produced (a first pin, a proven
 * rotation): such a person is reached with the grants they issued.
 */
public fun acceptedResetOfKey(
    currentKeyB64: String,
    rows: Map<String, ResetLedgerRow>,
    pinnedKeyOf: (accountIdB64: String) -> String?,
): AcceptedResetOfKey? {
    for ((accountIdB64, row) in rows) {
        if (pinnedKeyOf(accountIdB64) != currentKeyB64) continue
        val signedAt = row.resetSignedAtMs ?: return null
        val generation = row.acceptedKeyGeneration ?: return null
        if (row.supersededKeysB64.isEmpty()) return null
        return AcceptedResetOfKey(accountIdB64, generation, signedAt)
    }
    return null
}

/** Retired key -> the key the person is pinned under now: the display
 *  aliases of stored history. */
public fun retiredKeyAliases(
    rows: Map<String, ResetLedgerRow>,
    pinnedKeyOf: (accountIdB64: String) -> String?,
): Map<String, String> {
    val out = LinkedHashMap<String, String>()
    for ((accountIdB64, row) in rows) {
        val current = pinnedKeyOf(accountIdB64) ?: continue
        for (old in row.previousKeysB64) if (old != current && old !in out) out[old] = current
    }
    return out
}

/** Where ledger rows live, per signed-in account. */
public interface ResetLedgerStore {
    public fun loadAll(ownerAccountIdB64: String): Map<String, ResetLedgerRow>

    public fun save(ownerAccountIdB64: String, accountIdB64: String, row: ResetLedgerRow)

    public fun clearForOwner(ownerAccountIdB64: String)
}

public fun ResetLedgerStore.load(ownerAccountIdB64: String, accountIdB64: String): ResetLedgerRow? =
    loadAll(ownerAccountIdB64)[accountIdB64]

/** In-memory store (tests, context-less configurations). */
public class InMemoryResetLedgerStore : ResetLedgerStore {
    private val rows = HashMap<String, MutableMap<String, ResetLedgerRow>>()

    override fun loadAll(ownerAccountIdB64: String): Map<String, ResetLedgerRow> =
        synchronized(rows) { rows[ownerAccountIdB64]?.toMap() ?: emptyMap() }

    override fun save(ownerAccountIdB64: String, accountIdB64: String, row: ResetLedgerRow) {
        synchronized(rows) { rows.getOrPut(ownerAccountIdB64) { HashMap() }[accountIdB64] = row }
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        synchronized(rows) { rows.remove(ownerAccountIdB64) }
    }
}

private val ledgerJson = Json { ignoreUnknownKeys = true }
private val ledgerSerializer = MapSerializer(String.serializer(), ResetLedgerRow.serializer())

internal fun encodeResetLedger(all: Map<String, ResetLedgerRow>): String =
    ledgerJson.encodeToString(ledgerSerializer, all)

/** A junk row reads as empty: no generation baseline, nothing superseded. */
internal fun decodeResetLedger(raw: String?): Map<String, ResetLedgerRow> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        ledgerJson.decodeFromString(ledgerSerializer, raw)
    } catch (_: Exception) {
        emptyMap()
    }
}

/** Production store: plain preferences (public keys, account ids, two
 *  numbers), one row per signed-in account. Written synchronously: the
 *  ledger row is what makes a re-pin an ACCEPTED reset to every later read. */
public class SharedPrefsResetLedgerStore(context: Context) : ResetLedgerStore {
    private val appContext = context.applicationContext
    private val lock = Any()

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun loadAll(ownerAccountIdB64: String): Map<String, ResetLedgerRow> =
        try {
            decodeResetLedger(prefs().getString(KEY_PREFIX + ownerAccountIdB64, null))
        } catch (_: Throwable) {
            emptyMap()
        }

    override fun save(ownerAccountIdB64: String, accountIdB64: String, row: ResetLedgerRow) {
        synchronized(lock) {
            val all = loadAll(ownerAccountIdB64) + (accountIdB64 to row)
            me.lakko.app.account.AccountStoreGate.write(ownerAccountIdB64) { prefs().edit().putString(KEY_PREFIX + ownerAccountIdB64, encodeResetLedger(all)).commit() }
        }
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        try {
            prefs().edit().remove(KEY_PREFIX + ownerAccountIdB64).apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-reset-ledger"
        private const val KEY_PREFIX = "ledger-v1:"
    }
}

// ---------------------------------------------------------------------------
// The contact row follows the person
// ---------------------------------------------------------------------------

/** The slice of a contact row the re-key reads. */
public data class ContactRowKeys(
    /** The row's account id, standard base64, or `null` when it carries none. */
    val accountIdB64: String?,
    /** The row's signature key, standard base64. */
    val keyB64: String,
)

/** Which row moves to the new key and which rows fold into it. */
public data class ContactRekeyPlan(val rowIndex: Int, val foldedIndices: Set<Int>)

/**
 * R.2 "The re-pin": the contact ROW is re-keyed to the new key keeping its
 * rename, account id and chat link. The row is the one that names the account
 * id, else one held under a retired key. A second row already under the new
 * key (a contact request from the new identity accepted before the reset was
 * noticed) folds into it: one person, one row.
 *
 * `null` when no row is the person's, or the only such row already holds the
 * new key and nothing folds.
 */
public fun rekeyContactRows(
    rows: List<ContactRowKeys>,
    accountIdB64: String,
    retiredKeysB64: List<String>,
    newKeyB64: String,
): ContactRekeyPlan? {
    val rowIndex = rows.indexOfFirst { it.accountIdB64 == accountIdB64 && it.keyB64 != newKeyB64 }
        .takeIf { it >= 0 }
        ?: rows.indexOfFirst { it.keyB64 in retiredKeysB64 && it.keyB64 != newKeyB64 }.takeIf { it >= 0 }
        ?: return null
    val folded = rows.indices.filter { it != rowIndex && rows[it].keyB64 == newKeyB64 }.toSet()
    return ContactRekeyPlan(rowIndex, folded)
}
