// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.l10n.UiText

/**
 * ADR-0065 phase 2 / `docs/ux/identity-recovery.md` section 16 — a contact
 * whose account keys were reset, as this device records it.
 *
 * ## The trigger (16.1)
 *
 * The fail-closed identity gate ([ContactsClient.verifyBundleAccountKey])
 * refuses a changed key that carries no supersession proof, and the KT-log
 * enrichment ([SupersessionRefused.loggedReset]) says whether the log's latest
 * entry for the new key is an owner-initiated RESET. Only that pair — refused
 * for an ABSENT proof, and confirmed as a reset by the log — becomes a STATE of
 * the contact ([ContactKeyState.RESET_UNVERIFIED]). A changed key the log does
 * not confirm, or cannot, keeps today's error copy and sets nothing: that is a
 * possible compromise, not a reset, and nothing in section 16 applies to it.
 *
 * ## What is kept (16.8)
 *
 * Per contact, keyed on the stable account id so the record survives the key
 * change: the key that was pinned before ([ResetRecord.previousKeyB64]), the
 * published key the log confirmed ([ResetRecord.pendingKeyB64]), when it was
 * noticed, and the directory's key generation when it was served. Confirming
 * (16.2) re-pins the account id to the pending key with a monotone marker, sets
 * the state back to normal and KEEPS the previous key: the eviction
 * bookkeeping in the chat module still needs to recognise the old devices.
 *
 * Verification is per device: it is never synced, and it tells the contact
 * nothing.
 *
 * Every decision here is a pure function so the host JVM suite runs it; the
 * store is a plain per-account `SharedPreferences` row (two public keys, a
 * state and two numbers — no key material).
 */

/** Whether the new keys of a contact have been compared out of band. */
public enum class ContactKeyState {
    /** The pinned key is the published key (or no reset was ever noticed). */
    NORMAL,

    /** The published key is a logged reset the person has not verified yet. */
    RESET_UNVERIFIED,
}

/** One contact's reset record (16.8). Keys are standard base64. */
public data class ResetRecord(
    /** The contact's account id, standard base64 — the record's key. */
    val accountIdB64: String,
    /** The key pinned before the reset was noticed. Kept after verification. */
    val previousKeyB64: String,
    /** The published key the log confirmed as a reset; the pin after confirming. */
    val pendingKeyB64: String,
    /** When this device noticed the reset, Unix milliseconds. */
    val observedAtMs: Long,
    /** `key_generation` the directory served with the pending key, when known. */
    val keyGeneration: Long?,
    val state: ContactKeyState,
) {
    /** 16.1 — the state every blocked action asks about. */
    val unverified: Boolean get() = state == ContactKeyState.RESET_UNVERIFIED
}

/**
 * 16.1 — does a fail-closed refusal become the reset STATE of the contact?
 *
 * Exactly when the proof was absent and the log confirmed the served key as a
 * reset. A present-but-invalid proof, a key mismatch or a backward marker is a
 * changed key the log does not explain in the shape phase 2 handles; so is a
 * `null` or `false` marker.
 */
public fun refusalIsLoggedReset(
    reason: SupersessionRefusedReason,
    loggedReset: Boolean?,
): Boolean = reason == SupersessionRefusedReason.ProofAbsent && loggedReset == true

/**
 * UX 16.1 ("What is blocked") — the refusal line `Verify {name}'s new safety
 * number first.` carries an `Open contact` action beside it, pointing at the
 * one surface that lifts the block. This answers the key the contact screen is
 * opened under: the PINNED (previous) key of a logged-reset refusal, standard
 * base64, which is the key the contact is stored under until re-verification.
 * `null` for any other refusal (a possible compromise has no state to open)
 * and when the refusal carries no pin.
 */
public fun refusalOpenContactKeyB64(
    reason: SupersessionRefusedReason,
    loggedReset: Boolean?,
    pinnedPubkeyB64: String?,
): String? = if (refusalIsLoggedReset(reason, loggedReset)) pinnedPubkeyB64?.takeIf { it.isNotEmpty() } else null

/**
 * 16.2 — the monotone marker the re-pin carries. The pin store refuses a
 * marker below the current one (ADR-0016 rule #4), so the re-pin never moves
 * it backwards, and a later reset verified later carries a later clock, so a
 * later re-verification is never refused as backwards either. The directory's
 * key generation is recorded on the [ResetRecord] but not used as the marker:
 * the marker space is proof-expiry seconds, and a small integer in it would
 * read as older than any proof ever accepted.
 */
public fun resetRepinMarker(currentMarker: Long, nowSecs: Long): Long =
    maxOf(currentMarker, nowSecs)

/** Build the record a noticed reset writes. */
public fun resetRecordFor(
    accountIdB64: String,
    previousKeyB64: String,
    pendingKeyB64: String,
    observedAtMs: Long,
    keyGeneration: Long?,
): ResetRecord = ResetRecord(
    accountIdB64 = accountIdB64,
    previousKeyB64 = previousKeyB64,
    pendingKeyB64 = pendingKeyB64,
    observedAtMs = observedAtMs,
    keyGeneration = keyGeneration,
    state = ContactKeyState.RESET_UNVERIFIED,
)

/**
 * A reset noticed AGAIN for a contact already flagged: the same pending key
 * is a re-observation and keeps the record (its first stamp); a DIFFERENT
 * pending key is a later reset — the record moves to it, and the previous key
 * stays the one pinned before the first reset, since that is what the
 * eviction bookkeeping still has to recognise.
 */
public fun mergeResetObservation(existing: ResetRecord?, incoming: ResetRecord): ResetRecord {
    if (existing == null) return incoming
    if (existing.pendingKeyB64 == incoming.pendingKeyB64 && existing.unverified) return existing
    return incoming.copy(previousKeyB64 = if (existing.unverified) existing.previousKeyB64 else existing.pendingKeyB64)
}

/** 16.2 — the record after a successful confirmation. */
public fun confirmedRecord(record: ResetRecord): ResetRecord =
    record.copy(state = ContactKeyState.NORMAL)

/** What a reset-aware surface may say for a blocked action (16.1 table). */
public object KeysResetCopy {
    /** The contacts-list secondary line. */
    public val ROW_SUB: UiText = UiText("contacts.keys_reset.row_sub")

    public fun banner(name: String?): UiText =
        if (name != null) UiText("contacts.keys_reset.banner", texts = mapOf("name" to name)) else UiText("contacts.keys_reset.banner_unnamed")

    public val BANNER_ACTION: UiText = UiText("contacts.keys_reset.banner_action")

    /** `Verify {name}'s new safety number first.` */
    public fun verifyFirst(name: String?): UiText =
        if (name != null) UiText("contacts.keys_reset.verify_first", texts = mapOf("name" to name)) else VERIFY_FIRST_SHORT

    public val VERIFY_FIRST_SHORT: UiText = UiText("contacts.keys_reset.verify_first_short")

    public val OPEN_CONTACT: UiText = UiText("contacts.keys_reset.open_contact")

    /** The member picker's disabled row. */
    public val PICKER_SUB: UiText = UiText("contacts.keys_reset.picker_sub")
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/** Per-account persistence of [ResetRecord]s, keyed on the contact's account id. */
public interface ContactKeyStateStore {
    public fun load(ownerAccountIdB64: String, peerAccountIdB64: String): ResetRecord?

    public fun loadAll(ownerAccountIdB64: String): List<ResetRecord>

    public fun save(ownerAccountIdB64: String, record: ResetRecord)

    public fun remove(ownerAccountIdB64: String, peerAccountIdB64: String)

    public fun clearForOwner(ownerAccountIdB64: String)
}

@Serializable
private data class StoredResetRecord(
    val accountIdB64: String,
    val previousKeyB64: String,
    val pendingKeyB64: String,
    val observedAtMs: Long,
    val keyGeneration: Long? = null,
    val state: String,
)

private val resetJson = Json { ignoreUnknownKeys = true }

/** The row as JSON. Visible for the codec test. */
internal fun encodeResetRecord(record: ResetRecord): String =
    resetJson.encodeToString(
        StoredResetRecord.serializer(),
        StoredResetRecord(
            accountIdB64 = record.accountIdB64,
            previousKeyB64 = record.previousKeyB64,
            pendingKeyB64 = record.pendingKeyB64,
            observedAtMs = record.observedAtMs,
            keyGeneration = record.keyGeneration,
            state = record.state.name,
        ),
    )

/** The inverse of [encodeResetRecord]; `null` for anything malformed. */
internal fun decodeResetRecord(raw: String?): ResetRecord? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = resetJson.decodeFromString(StoredResetRecord.serializer(), raw)
        val state = ContactKeyState.entries.firstOrNull { it.name == row.state } ?: return null
        if (row.accountIdB64.isEmpty() || row.previousKeyB64.isEmpty() || row.pendingKeyB64.isEmpty()) return null
        ResetRecord(
            accountIdB64 = row.accountIdB64,
            previousKeyB64 = row.previousKeyB64,
            pendingKeyB64 = row.pendingKeyB64,
            observedAtMs = row.observedAtMs,
            keyGeneration = row.keyGeneration,
            state = state,
        )
    } catch (_: Exception) {
        null
    }
}

/** Production store: plain `SharedPreferences`, one row per `(owner, peer)`. */
public class SharedPrefsContactKeyStateStore(context: Context) : ContactKeyStateStore {
    private val appContext = context.applicationContext

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(ownerAccountIdB64: String, peerAccountIdB64: String): ResetRecord? =
        try {
            decodeResetRecord(prefs().getString(rowKey(ownerAccountIdB64, peerAccountIdB64), null))
        } catch (_: Throwable) {
            null
        }

    override fun loadAll(ownerAccountIdB64: String): List<ResetRecord> =
        try {
            val prefix = ownerPrefix(ownerAccountIdB64)
            prefs().all.entries
                .filter { it.key.startsWith(prefix) }
                .mapNotNull { decodeResetRecord(it.value as? String) }
        } catch (_: Throwable) {
            emptyList()
        }

    override fun save(ownerAccountIdB64: String, record: ResetRecord) {
        prefs().edit().putString(rowKey(ownerAccountIdB64, record.accountIdB64), encodeResetRecord(record)).apply()
    }

    override fun remove(ownerAccountIdB64: String, peerAccountIdB64: String) {
        prefs().edit().remove(rowKey(ownerAccountIdB64, peerAccountIdB64)).apply()
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        try {
            val p = prefs()
            val prefix = ownerPrefix(ownerAccountIdB64)
            val doomed = p.all.keys.filter { it.startsWith(prefix) }
            if (doomed.isEmpty()) return
            val e = p.edit()
            for (k in doomed) e.remove(k)
            e.apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-contact-key-state"
        private const val KEY_PREFIX = "reset-v1:"

        private fun ownerPrefix(ownerAccountIdB64: String): String =
            KEY_PREFIX + AcceptedContactsStore.canonicalAccountB64(ownerAccountIdB64) + ":"

        internal fun rowKey(ownerAccountIdB64: String, peerAccountIdB64: String): String =
            ownerPrefix(ownerAccountIdB64) + AcceptedContactsStore.canonicalAccountB64(peerAccountIdB64)
    }
}

/** In-memory store for the host suite. */
public class InMemoryContactKeyStateStore : ContactKeyStateStore {
    private val rows = HashMap<String, ResetRecord>()

    private fun key(owner: String, peer: String) = "$owner|$peer"

    override fun load(ownerAccountIdB64: String, peerAccountIdB64: String): ResetRecord? =
        rows[key(ownerAccountIdB64, peerAccountIdB64)]

    override fun loadAll(ownerAccountIdB64: String): List<ResetRecord> =
        rows.filterKeys { it.startsWith("$ownerAccountIdB64|") }.values.toList()

    override fun save(ownerAccountIdB64: String, record: ResetRecord) {
        rows[key(ownerAccountIdB64, record.accountIdB64)] = record
    }

    override fun remove(ownerAccountIdB64: String, peerAccountIdB64: String) {
        rows.remove(key(ownerAccountIdB64, peerAccountIdB64))
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        rows.keys.filter { it.startsWith("$ownerAccountIdB64|") }.forEach { rows.remove(it) }
    }
}
