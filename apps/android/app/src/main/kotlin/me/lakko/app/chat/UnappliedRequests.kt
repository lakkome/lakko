// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences

/**
 * ADR-0045 S7 residual 1 — the request that was not carried. Android mirror of
 * the web reference `apps/web/src/chat/unapplied-requests.ts`.
 *
 * ## The gap
 *
 * A sweep does not necessarily carry everything it finds. `sweep_keeps`
 * enumerates fifteen `CommitBuilder::build` conditions and
 * `narrow_sweep_by_exclusion` drops whatever still refuses to build — and the
 * commonest cause needs no adversary at all: a **KeyPackage that expired**,
 * reachable precisely because ADR-0045 decision 2 gives a request no deadline.
 *
 * Before this module, an excluded request was simply *gone*. OpenMLS empties the
 * whole proposal store on every merged commit, so the pending set that was the
 * request's only representation is cleared whether or not the commit carried it;
 * [prunePendingRequestObservations] then removes the local observation; and the
 * panel falls silent. For an **approved join** it is worse than silent, because a
 * successful `approve_external_join` **consumes** the retained request, so the
 * quarantine entry is gone too and the admin cannot even re-approve.
 *
 * ## What is knowable, and what is not
 *
 * The core reports **no reason**: `CreateCommitError` carries no proposal
 * reference on any variant, so "why" does not exist at this layer and no copy
 * here may claim one.
 *
 * ## Re-requested first, listed only when that stops (ADR-0050 section 2.5a)
 *
 * This list used to be the whole answer for adds, and nothing at all for the
 * other kinds: a dropped add was listed, a dropped remove, leave or role change
 * was simply gone. Since ADR-0050 section 2.5a a plain member may sweep device
 * admissions, and its sweep drops every other pending request by design, so a
 * dropped request became an ordinary event. Every own request kind is now
 * RE-REQUESTED, silently, from the intent recorded when it was published
 * (`MembershipRetry.kt`, [supersededVerdict]); a request lands here only when
 * that re-request STOPS for a reason other than its effect landing or its
 * subject leaving — the author lost the role it needs, the core refuses it for
 * the group's state, or an approved join's KeyPackage is gone. The intent is
 * keyed on a **signature key**, never a leaf, which is what lets a removal or a
 * leave be tested at all (OpenMLS hands a vacated leaf to the next `Add`).
 *
 * ## What the JOINER is told: nothing, and that is a finding rather than a gap
 *
 * A link joiner is a **non-member polling the group journal**. There is no
 * artifact addressed to it, and the journal is unauthenticated — the DS
 * pseudonym IS the `group_id` and there is no auth on it — so any "your request
 * was not carried" envelope would be a **forgeable denial primitive** rather
 * than a signal. Inference does not work either: an epoch advancing with no
 * rendezvous for us is byte-for-byte what *no admin has approved yet* looks
 * like, which is the ordinary state of a request that by design has no deadline.
 * So the joiner gets no notice; it gets the **repair** —
 * [JOIN_REQUEST_AGAIN] on the `/g/` landing screen, user-driven and never
 * automatic, which re-mints a fresh KeyPackage.
 */

/**
 * What kind of own request did not apply. ADR-0050 section 2.5a point 4 widened
 * this from adds only: every own request kind is now re-requested when a sweep
 * drops it, and is listed here only when the re-request STOPPED for a reason
 * other than its effect landing or its subject leaving.
 */
public enum class UnappliedKind(internal val wire: String) {
    ADD("add"),

    /** An add this device published by approving an external join. Knowable
     *  ONLY on the device that approved: on the wire an approved add is
     *  indistinguishable from an admin adding a contact (ADR-0045 S7). */
    JOIN_APPROVE("join"),
    REMOVE("remove"),
    LEAVE("leave"),
    MAKE_ADMIN("make-admin"),
    REMOVE_ADMIN("remove-admin"),
    TRANSFER_OWNERSHIP("transfer"),
    ;

    internal companion object {
        fun fromWire(wire: String): UnappliedKind? = values().firstOrNull { it.wire == wire }
    }
}

/** One own request that did not apply and will not be re-requested. */
public data class UnappliedRequest(
    /** base64 of the subject's 32-byte Ed25519 signature key. */
    val subjectKeyB64: String,
    val kind: UnappliedKind,
) {
    /** The store key and the dismiss handle: one row per kind per subject. */
    val rowKey: String get() = "${kind.wire}:$subjectKeyB64"
}

/**
 * The line under one row.
 *
 * States that it did not apply, **never why** — the core reports no reason for
 * an exclusion and a re-request's refusal reason is a developer's sentence, so
 * inventing one would be the same class of lie as a green E2EE pill over an
 * unencrypted call. It does not promise a repair either: a row is here because
 * the re-request stopped, often because this device no longer holds the role
 * the request needs, so "you can do it again" would be untrue as often as not.
 *
 * A join approval is the exception that names an action, because the action is
 * the joiner's and is always available to them: the approve consumed the
 * retained request and only the joiner holds the key material.
 */
internal fun unappliedLine(item: UnappliedRequest): String = when (item.kind) {
    UnappliedKind.ADD -> "Not added."
    UnappliedKind.JOIN_APPROVE -> "Not applied. Ask them to request to join again."
    UnappliedKind.REMOVE -> "Not removed."
    UnappliedKind.LEAVE -> "Not applied. You are still a member."
    UnappliedKind.MAKE_ADMIN -> "Not made an admin."
    UnappliedKind.REMOVE_ADMIN -> "Still an admin."
    UnappliedKind.TRANSFER_OWNERSHIP -> "Ownership was not transferred."
}

/** The one explanatory line under the section heading. True of every row: a
 *  row is written only once the automatic re-request (ADR-0050 section 2.5a
 *  point 4) has stopped, so nothing will send it again on its own. */
internal const val UNAPPLIED_EXPLANATION: String =
    "These changes did not apply and will not be retried on their own."

/** The section heading. */
internal fun unappliedHeading(count: Int): String = "Not applied ($count)"

// ---------------------------------------------------------------------------
// The joiner's half of the same residual
// ---------------------------------------------------------------------------

/**
 * What the `/g/` landing screen says while a request is outstanding.
 *
 * The joiner is told **nothing** about an exclusion, for the reasons in this
 * file's header. What it is told instead is the truth about the wait itself,
 * which the previous copy left out: **both** steps are unbounded.
 */
internal const val JOIN_WAIT_PRIMARY: String =
    "Request sent. An admin approves it, then the change is applied to the group."

/** The second line — the part that was missing, and the only thing that makes
 *  the [JOIN_REQUEST_AGAIN] control legible. */
internal const val JOIN_WAIT_SECONDARY: String =
    "Neither step has a time limit. If nothing happens, you can send the request again."

/**
 * The joiner's one repair, and the ONLY thing available to this side.
 *
 * A fresh request mints a fresh KeyPackage, which is what fixes the commonest
 * cause of an exclusion — but the button never says so, because this side cannot
 * know that was the cause. Deliberately **user-driven and never automatic**:
 * each press is a journal row held to the DS's 30-day TTL, and a timer would
 * spin against a group that is merely quiet.
 *
 * Its reach is honest but partial: an admin still holding an earlier request for
 * the same signature key **keeps the older KeyPackage**
 * (`retain_external_join` de-dups by signature key and skips the push), so a
 * re-request refreshes the material only where the earlier entry is gone —
 * which is exactly the case that matters here, because a successful approve
 * consumed it.
 */
internal const val JOIN_REQUEST_AGAIN: String = "Request again"

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/** One stored row. `dismissed` is a tombstone: a stopped request is recorded
 *  once, and without the tombstone a later stop of the same request would
 *  bring back a row the user already dismissed. */
internal data class UnappliedRow(
    val kind: UnappliedKind,
    val subjectKeyB64: String,
    val dismissed: Boolean = false,
)

/** Where the `Not applied` rows live: [UnappliedRequest.rowKey] -> row. */
internal interface UnappliedRequestStore {
    fun load(accountIdB64: String, groupIdHex: String): Map<String, UnappliedRow>

    fun save(accountIdB64: String, groupIdHex: String, rows: Map<String, UnappliedRow>)

    fun clearForAccount(accountIdB64: String)

    fun clearForGroup(accountIdB64: String, groupIdHex: String)
}

/** The rows still worth showing. */
internal fun loadUnapplied(
    store: UnappliedRequestStore,
    accountIdB64: String,
    groupIdHex: String,
): List<UnappliedRequest> =
    store.load(accountIdB64, groupIdHex)
        .values
        .filter { !it.dismissed }
        .map { UnappliedRequest(it.subjectKeyB64, it.kind) }

/**
 * Record stopped requests. Idempotent, and a **dismissed row is never
 * resurrected**.
 */
internal fun recordUnapplied(
    store: UnappliedRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    items: List<UnappliedRequest>,
) {
    if (items.isEmpty()) return
    val rows = store.load(accountIdB64, groupIdHex).toMutableMap()
    var changed = false
    for (item in items) {
        if (rows.containsKey(item.rowKey)) continue
        rows[item.rowKey] = UnappliedRow(item.kind, item.subjectKeyB64)
        changed = true
    }
    if (changed) store.save(accountIdB64, groupIdHex, rows)
}

/** Dismiss one row, by [UnappliedRequest.rowKey]. It stays dismissed. */
internal fun dismissUnapplied(
    store: UnappliedRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    rowKey: String,
) {
    val rows = store.load(accountIdB64, groupIdHex).toMutableMap()
    val existing = rows[rowKey] ?: return
    rows[rowKey] = existing.copy(dismissed = true)
    store.save(accountIdB64, groupIdHex, rows)
}

/**
 * The production [UnappliedRequestStore] — one plain `SharedPreferences` file.
 *
 * Row key: `"unapplied-requests-v1:" + accountIdB64 + NUL + groupIdHex` (the NUL
 * written as the Kotlin escape `\u0000`; a RAW NUL in the source would make git
 * classify this file as binary — ADR-0042 MEDIUM-3).
 *
 * Row value: `kind|subjectKeyB64|dismissed` entries joined by newline; the map
 * key is rebuilt from the first two. Standard base64 contains neither `|` nor
 * a newline. A row in an earlier shape is dropped: there is no deployment to
 * migrate, and a lost row loses a notice, never a request.
 *
 * Never throws: a corrupt or absent row reads as empty, which loses a notice
 * rather than a request — nothing here is load-bearing for correctness.
 */
internal class SharedPrefsUnappliedRequestStore(context: Context) : UnappliedRequestStore {

    private val appContext: Context = context.applicationContext

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String, groupIdHex: String): Map<String, UnappliedRow> =
        try {
            decodeRow(prefs().getString(rowKey(accountIdB64, groupIdHex), null))
        } catch (_: Throwable) {
            emptyMap()
        }

    override fun save(
        accountIdB64: String,
        groupIdHex: String,
        rows: Map<String, UnappliedRow>,
    ) {
        try {
            val key = rowKey(accountIdB64, groupIdHex)
            val editor = prefs().edit()
            if (rows.isEmpty()) editor.remove(key) else editor.putString(key, encodeRow(rows))
            editor.apply()
        } catch (_: Throwable) {
            // best-effort
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

    override fun clearForGroup(accountIdB64: String, groupIdHex: String) {
        try {
            prefs().edit().remove(rowKey(accountIdB64, groupIdHex)).apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    internal companion object {
        private const val PREFS_NAME = "lakko-unapplied-requests"
        private const val KEY_PREFIX = "unapplied-requests-v1:"
        private const val ROW_SEPARATOR = "\u0000"

        private fun rowKey(accountIdB64: String, groupIdHex: String): String =
            KEY_PREFIX + accountIdB64 + ROW_SEPARATOR + groupIdHex

        internal fun encodeRow(rows: Map<String, UnappliedRow>): String =
            rows.values.joinToString("\n") {
                "${it.kind.wire}|${it.subjectKeyB64}|${if (it.dismissed) 1 else 0}"
            }

        internal fun decodeRow(raw: String?): Map<String, UnappliedRow> {
            if (raw.isNullOrEmpty()) return emptyMap()
            val out = mutableMapOf<String, UnappliedRow>()
            for (line in raw.split('\n')) {
                val parts = line.split('|')
                if (parts.size != 3) continue
                val kind = UnappliedKind.fromWire(parts[0]) ?: continue
                if (parts[1].isEmpty()) continue
                val row = UnappliedRow(kind, parts[1], dismissed = parts[2] == "1")
                out[UnappliedRequest(row.subjectKeyB64, row.kind).rowKey] = row
            }
            return out
        }
    }
}
