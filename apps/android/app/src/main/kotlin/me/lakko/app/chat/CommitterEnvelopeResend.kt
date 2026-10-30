// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import java.util.concurrent.ConcurrentHashMap
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import me.lakko.app.reach.ReachManager

/*
 * ADR-0060 section 6 — a proposer re-sends its committer envelope while its
 * Add waits.
 *
 * A delegation lives 23 hours and an ADR-0045 proposal has no deadline, so an
 * Add that waits longer than a delegation would otherwise lose its way to the
 * person. WHEN an envelope is due is the core's one rule
 * (`committerEnvelopeDue`), passed in here; what this file owns is what the
 * rule reads (what the last send covered, durably), which Adds it is asked
 * about (this device's own, still pending), and when it is not worth asking
 * again.
 *
 * Runs at the tail of a poll of the group, the same place as the sweep and the
 * owed-Welcome retry; every rule lives here so the host suite runs it without
 * an FFI group or a network.
 */

/**
 * What this device did about the committer envelope of one person it proposed
 * adding to one group.
 *
 * The row exists from the moment the Add is on the wire, before any envelope
 * is: a first send that failed must stay due, and the person's account key is
 * nowhere else in this device's records of the Add (the own-request intent
 * names a device key and an account id).
 *
 * @property addedKeyB64 the added person's account key, as the KeyPackage's
 *   binding named it.
 * @property deviceKeysB64 the device keys of the Adds this device proposed for
 *   the person: what ties the row to its own-request intents.
 * @property sentAtMs when the last envelope was published, `null` before the
 *   first one.
 * @property signed whether it carried delegations. An envelope without any
 *   names a binding, which does not expire.
 * @property coveredB64 the committer candidates it covered.
 */
@Serializable
internal data class SentCommitterEnvelope(
    val addedKeyB64: String,
    val deviceKeysB64: List<String>,
    val sentAtMs: Long? = null,
    val signed: Boolean = false,
    val coveredB64: List<String> = emptyList(),
)

/**
 * Where the rows live, per group: added person key -> row.
 *
 * Not encrypted, the posture of [PendingRequestStore], which already holds the
 * same request's subject: account and signature keys and a timestamp, no key
 * material.
 */
internal interface SentCommitterEnvelopeStore {
    fun load(accountIdB64: String, groupIdHex: String): Map<String, SentCommitterEnvelope>

    fun save(accountIdB64: String, groupIdHex: String, rows: Map<String, SentCommitterEnvelope>)

    fun clearForAccount(accountIdB64: String)
}

/** One atomic read-modify-write of a group's rows. */
private inline fun SentCommitterEnvelopeStore.update(
    accountIdB64: String,
    groupIdHex: String,
    change: (Map<String, SentCommitterEnvelope>) -> Map<String, SentCommitterEnvelope>,
) {
    synchronized(this) {
        val before = load(accountIdB64, groupIdHex)
        val after = change(before)
        if (after != before) save(accountIdB64, groupIdHex, after)
    }
}

/**
 * This device's Add for [deviceKeyB64] of the person [addedKeyB64] is on the
 * wire. Called before the first send, so a send that fails leaves a row that
 * is due. What an earlier send recorded stays: a second device of the person,
 * or a re-request, is not a send.
 */
internal fun noteCommitterEnvelopeOwed(
    store: SentCommitterEnvelopeStore,
    accountIdB64: String,
    groupIdHex: String,
    addedKeyB64: String,
    deviceKeyB64: String,
) {
    store.update(accountIdB64, groupIdHex) { rows ->
        val row = rows[addedKeyB64] ?: SentCommitterEnvelope(addedKeyB64, emptyList())
        val devices = (row.deviceKeysB64 + deviceKeyB64).distinct()
        rows + (addedKeyB64 to row.copy(deviceKeysB64 = devices))
    }
}

/** An envelope for [addedKeyB64] was published. Only ever after the publish. */
internal fun recordCommitterEnvelopeSent(
    store: SentCommitterEnvelopeStore,
    accountIdB64: String,
    groupIdHex: String,
    addedKeyB64: String,
    signed: Boolean,
    coveredB64: List<String>,
    sentAtMs: Long,
) {
    store.update(accountIdB64, groupIdHex) { rows ->
        // Forgotten while the send was out: the Add landed, nothing is owed.
        val row = rows[addedKeyB64] ?: return@update rows
        rows + (addedKeyB64 to row.copy(sentAtMs = sentAtMs, signed = signed, coveredB64 = coveredB64))
    }
}

private fun forgetCommitterEnvelope(
    store: SentCommitterEnvelopeStore,
    accountIdB64: String,
    groupIdHex: String,
    addedKeyB64: String,
) {
    store.update(accountIdB64, groupIdHex) { rows -> rows - addedKeyB64 }
}

/**
 * The people with nothing to send, and the candidates that was found for, so
 * work that cannot succeed is not repeated at every poll.
 *
 * What could change the answer is a new grant or binding for the person, which
 * clears this ([forgetAll]), or a new set of candidates, which no longer
 * matches. In memory: after a restart it is asked once more.
 */
internal class CommitterEnvelopeNothingToSend {
    private val found = ConcurrentHashMap<String, Set<String>>()

    private fun key(accountIdB64: String, groupIdHex: String, addedKeyB64: String) =
        "$accountIdB64/$groupIdHex/$addedKeyB64"

    fun known(accountIdB64: String, groupIdHex: String, addedKeyB64: String, candidatesB64: List<String>): Boolean =
        found[key(accountIdB64, groupIdHex, addedKeyB64)] == candidatesB64.toSet()

    fun note(accountIdB64: String, groupIdHex: String, addedKeyB64: String, candidatesB64: List<String>) {
        found[key(accountIdB64, groupIdHex, addedKeyB64)] = candidatesB64.toSet()
    }

    fun forget(accountIdB64: String, groupIdHex: String, addedKeyB64: String) {
        found.remove(key(accountIdB64, groupIdHex, addedKeyB64))
    }

    fun forgetAll() {
        found.clear()
    }
}

/** What one attempt to send an envelope did. */
internal enum class CommitterEnvelopeSend {
    SENT,

    /** Neither a grant nor a binding is held for the person, or the group
     *  takes no message. */
    NOTHING_TO_SEND,

    /** Another send for the same person is out. */
    ALREADY_OUT,
}

/**
 * Compose, publish and record one envelope: the ONE send, for the first send
 * after the Add and for every re-send, so they cannot come to differ.
 *
 * [compose] signs fresh delegations, or names the recorded open binding, and
 * answers `null` when neither is held; [publish] answers `false` when the group
 * takes no message and throws what a publish throws, which leaves the row as it
 * was (still due, tried again at the next poll).
 */
internal suspend fun sendCommitterEnvelopeOnce(
    store: SentCommitterEnvelopeStore,
    accountIdB64: String,
    groupIdHex: String,
    addedKeyB64: String,
    candidatesB64: List<String>,
    inFlight: MutableSet<String>,
    nothingToSend: CommitterEnvelopeNothingToSend,
    compose: () -> ReachManager.ComposedDelegations?,
    publish: suspend (ByteArray) -> Boolean,
    nowMs: () -> Long,
): CommitterEnvelopeSend {
    val claim = "$accountIdB64/$groupIdHex/$addedKeyB64"
    if (!inFlight.add(claim)) return CommitterEnvelopeSend.ALREADY_OUT
    try {
        if (nothingToSend.known(accountIdB64, groupIdHex, addedKeyB64, candidatesB64)) {
            return CommitterEnvelopeSend.NOTHING_TO_SEND
        }
        val composed = runCatching(compose).getOrNull()
        if (composed == null || !publish(composed.envelope)) {
            nothingToSend.note(accountIdB64, groupIdHex, addedKeyB64, candidatesB64)
            return CommitterEnvelopeSend.NOTHING_TO_SEND
        }
        nothingToSend.forget(accountIdB64, groupIdHex, addedKeyB64)
        recordCommitterEnvelopeSent(
            store = store,
            accountIdB64 = accountIdB64,
            groupIdHex = groupIdHex,
            addedKeyB64 = addedKeyB64,
            signed = composed.signed,
            coveredB64 = candidatesB64,
            sentAtMs = nowMs(),
        )
        return CommitterEnvelopeSend.SENT
    } finally {
        inFlight.remove(claim)
    }
}

/** Where this device's Add of one person stands. */
internal enum class CommitterEnvelopeAddState {
    /** Still in the pending set: an envelope may be due. */
    WAITING,

    /** Landed, or the request is over: nothing more is owed. */
    FINISHED,

    /** A torn read, or a request a sweep dropped, which the superseded pass
     *  decides (and a re-request sends its own envelope). */
    UNDECIDED,
}

/**
 * Where the Add behind [row] stands, read with the superseded check's own
 * verdict, so "landed" and "still waiting" mean what they mean there.
 *
 * Only this device's own intents count: another device's Add has no intent
 * here, and neither has an Add whose request has ended.
 */
internal fun committerEnvelopeAddState(
    row: SentCommitterEnvelope,
    ownIntents: List<RetryableRequest>,
    snapshot: RetryGroupSnapshot,
): CommitterEnvelopeAddState {
    if (snapshot.membershipEnded) return CommitterEnvelopeAddState.FINISHED
    if (snapshot.roster == null) return CommitterEnvelopeAddState.UNDECIDED
    val adds = ownIntents.filterIsInstance<RetryableRequest.Add>()
        .filter { it.subjectKeyB64 in row.deviceKeysB64 }
    if (adds.isEmpty()) return CommitterEnvelopeAddState.FINISHED
    val verdicts = adds.map { supersededVerdict(it, snapshot) }
    return when {
        verdicts.any { it == SupersededVerdict.Keep } -> CommitterEnvelopeAddState.WAITING
        verdicts.all { it is SupersededVerdict.Clear } -> CommitterEnvelopeAddState.FINISHED
        else -> CommitterEnvelopeAddState.UNDECIDED
    }
}

/** What one pass did, for the caller's log. */
internal data class CommitterEnvelopeResendResult(
    val sent: Int,
    val forgotten: Int,
)

/**
 * One pass over the group's rows, at the tail of a poll: forget what is over,
 * ask the core's rule ([due]) about what still waits, and [send] what is due.
 *
 * [candidatesB64] are the group's committer candidates now, read with the same
 * function the first send uses. A failed send is the caller's to log: it leaves
 * its row due and does not stop the next person's.
 */
internal suspend fun runCommitterEnvelopeResend(
    store: SentCommitterEnvelopeStore,
    accountIdB64: String,
    groupIdHex: String,
    ownIntents: List<RetryableRequest>,
    snapshot: RetryGroupSnapshot,
    candidatesB64: List<String>,
    nowMs: Long,
    due: (row: SentCommitterEnvelope, candidatesB64: List<String>, nowMs: Long) -> Boolean,
    send: suspend (addedKeyB64: String) -> CommitterEnvelopeSend,
    onFailure: (Throwable) -> Unit = {},
): CommitterEnvelopeResendResult {
    var sent = 0
    var forgotten = 0
    for (row in store.load(accountIdB64, groupIdHex).values) {
        when (committerEnvelopeAddState(row, ownIntents, snapshot)) {
            CommitterEnvelopeAddState.FINISHED -> {
                forgetCommitterEnvelope(store, accountIdB64, groupIdHex, row.addedKeyB64)
                forgotten += 1
            }
            CommitterEnvelopeAddState.UNDECIDED -> Unit
            CommitterEnvelopeAddState.WAITING -> {
                if (candidatesB64.isEmpty() || !due(row, candidatesB64, nowMs)) continue
                try {
                    if (send(row.addedKeyB64) == CommitterEnvelopeSend.SENT) sent += 1
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    onFailure(cause)
                }
            }
        }
    }
    return CommitterEnvelopeResendResult(sent, forgotten)
}

/**
 * The production [SentCommitterEnvelopeStore] — one plain `SharedPreferences`
 * file, one JSON row per `(account, group)`. Row key:
 * `"sent-committer-envelopes-v1:" + accountIdB64 + NUL + groupIdHex`, the NUL
 * written as the Kotlin escape (ADR-0042 MEDIUM-3).
 *
 * Never throws: a corrupt or absent row reads as empty, which costs the re-send
 * of an Add already on the wire and nothing else.
 */
internal class SharedPrefsSentCommitterEnvelopeStore(context: Context) : SentCommitterEnvelopeStore {

    private val appContext: Context = context.applicationContext

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String, groupIdHex: String): Map<String, SentCommitterEnvelope> =
        try {
            val raw = prefs().getString(rowKey(accountIdB64, groupIdHex), null)
            if (raw == null) {
                emptyMap()
            } else {
                JSON.decodeFromString(ROW_SERIALIZER, raw).associateBy { it.addedKeyB64 }
            }
        } catch (_: Throwable) {
            emptyMap()
        }

    override fun save(accountIdB64: String, groupIdHex: String, rows: Map<String, SentCommitterEnvelope>) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                val key = rowKey(accountIdB64, groupIdHex)
                val editor = prefs().edit()
                if (rows.isEmpty()) {
                    editor.remove(key)
                } else {
                    editor.putString(key, JSON.encodeToString(ROW_SERIALIZER, rows.values.toList()))
                }
                editor.apply()
            } catch (_: Throwable) {
                // best-effort, see the class comment
            }
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        try {
            val prefix = KEY_PREFIX + accountIdB64 + ROW_SEPARATOR
            val p = prefs()
            val victims = p.all.keys.filter { it.startsWith(prefix) }
            if (victims.isEmpty()) return
            val editor = p.edit()
            for (k in victims) editor.remove(k)
            editor.apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    private companion object {
        const val PREFS_NAME = "lakko-sent-committer-envelopes"
        const val KEY_PREFIX = "sent-committer-envelopes-v1:"
        const val ROW_SEPARATOR = "\u0000"
        val ROW_SERIALIZER = ListSerializer(SentCommitterEnvelope.serializer())
        val JSON = Json { ignoreUnknownKeys = true }

        fun rowKey(accountIdB64: String, groupIdHex: String): String =
            KEY_PREFIX + accountIdB64 + ROW_SEPARATOR + groupIdHex
    }
}
