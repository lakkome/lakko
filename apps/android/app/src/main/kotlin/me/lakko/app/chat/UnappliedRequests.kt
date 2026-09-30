// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.l10n.UiText

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
 * here may claim one. What exists is the observation:
 *
 * > **A request of mine left the core's pending set without its effect landing.**
 *
 * That runs on **every** device rather than only on the sweeper — which matters,
 * because the device that swept is often not the device that asked — and it
 * needs no core change, no new wire artifact and no new FFI.
 *
 * ## Why only an `add`
 *
 * `remove:` and `leave:` keys name a **leaf index**, and OpenMLS hands a vacated
 * leaf to the next `Add`, so a roster test on a leaf can be wrong in both
 * directions. `role-change` carries no payload at all. And all three are already
 * **visible** when they fail — the member you tried to remove is still in the
 * roster, the group you failed to leave is still in your list. The `add` is the
 * only request whose failure leaves no trace anywhere, and the only one whose
 * effect test is keyed on a **signature key** rather than a leaf.
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

/** One own `add` request that left the pending set without landing. */
public data class UnappliedAdd(
    /** base64 of the prospective member's 32-byte Ed25519 signature key. */
    val addSignatureKeyB64: String,
    /**
     * `true` when this device published the request by approving an external
     * **join** rather than by adding a contact.
     *
     * Knowable ONLY on the device that made the request: on the wire an approved
     * add is indistinguishable from an admin adding a contact (ADR-0045 S7),
     * which is the point rather than a limitation. That device is the one this
     * notice is shown on.
     */
    val viaJoinRequest: Boolean,
)

/**
 * The own `add` requests that are dead: observed as ours, no longer in the
 * core's live pending set, and not in the roster.
 *
 * Pure. `liveKeys` and `rosterKeysB64` must be read in the SAME pass — the
 * pending set is emptied by the merge that also moves the roster.
 *
 * A `null` roster is a torn read, not an empty group, and claims NOTHING — the
 * same fail-closed direction [membershipRotationDecision]'s caller takes with
 * the identical signal.
 */
internal fun unappliedAddRequests(
    observations: Map<String, RequestObservation>,
    liveKeys: Collection<String>,
    rosterKeysB64: Collection<String>?,
): List<UnappliedAdd> {
    if (rosterKeysB64 == null) return emptyList()
    val live = liveKeys.toSet()
    val roster = rosterKeysB64.toSet()
    val out = mutableListOf<UnappliedAdd>()
    for ((key, seen) in observations) {
        // UX §10 — a member is never told about someone else's request.
        if (!seen.own) continue
        if (!key.startsWith("add:")) continue
        val addKeyB64 = key.removePrefix("add:")
        // `proposalKey` writes `add:` with an empty key when none was resolved.
        // That names nobody, so there is nothing honest to say about it.
        if (addKeyB64.isEmpty() || addKeyB64 == "?") continue
        if (live.contains(key)) continue
        if (roster.contains(addKeyB64)) continue
        out.add(UnappliedAdd(addKeyB64, seen.joinRequest))
    }
    return out
}

/**
 * The line under one row.
 *
 * States that it did not apply and what can be done, **never why** — the core
 * has no reason to give and inventing one would be the same class of lie as a
 * green E2EE pill over an unencrypted call. The UX spec §7 draft string
 * (`This request expired before it could be applied.`) asserts a cause this
 * client cannot establish; the divergence is deliberate.
 *
 * A join approval is **terminal on the admin side** — the approve consumed the
 * retained request and only the joiner holds the key material — so it directs
 * the action to them. An ordinary add is re-doable here.
 */
internal fun unappliedLine(item: UnappliedAdd): UiText =
    if (item.viaJoinRequest) {
        UiText("chat.members.unapplied.join_line")
    } else {
        UiText("chat.members.unapplied.add_line")
    }

/** The one explanatory line under the section heading. True on both counts: the
 *  request left the pending set, so no sweep will carry it, and no client
 *  re-requests (UX §7's automatic re-request is designed and not built). */
internal val UNAPPLIED_EXPLANATION: UiText = UiText("chat.members.unapplied.explanation")

/** The section heading. */
internal fun unappliedHeading(count: Int): UiText =
    UiText("chat.members.unapplied.heading", counts = mapOf("count" to count.toULong()))

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
internal val JOIN_WAIT_PRIMARY: UiText = UiText("group.join.wait_primary")

/** The second line — the part that was missing, and the only thing that makes
 *  the [JOIN_REQUEST_AGAIN] control legible. */
internal val JOIN_WAIT_SECONDARY: UiText = UiText("group.join.wait_secondary")

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
internal val JOIN_REQUEST_AGAIN: UiText = UiText("group.join.request_again")

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/** One stored row. `dismissed` is a tombstone: the detection re-runs on every
 *  fold, so without it the notice could not be dismissed at all. */
internal data class UnappliedRow(
    val viaJoinRequest: Boolean,
    val dismissed: Boolean = false,
)

/** Where the `Not applied` rows live: `addSignatureKeyB64 -> row`. */
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
): List<UnappliedAdd> =
    store.load(accountIdB64, groupIdHex)
        .filterValues { !it.dismissed }
        .map { UnappliedAdd(it.key, it.value.viaJoinRequest) }

/**
 * Record detected rows. Idempotent, and a **dismissed row is never
 * resurrected**.
 */
internal fun recordUnapplied(
    store: UnappliedRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    items: List<UnappliedAdd>,
) {
    if (items.isEmpty()) return
    val rows = store.load(accountIdB64, groupIdHex).toMutableMap()
    var changed = false
    for (item in items) {
        if (rows.containsKey(item.addSignatureKeyB64)) continue
        rows[item.addSignatureKeyB64] = UnappliedRow(item.viaJoinRequest)
        changed = true
    }
    if (changed) store.save(accountIdB64, groupIdHex, rows)
}

/** Dismiss one row. It stays dismissed. */
internal fun dismissUnapplied(
    store: UnappliedRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    addSignatureKeyB64: String,
) {
    val rows = store.load(accountIdB64, groupIdHex).toMutableMap()
    val existing = rows[addSignatureKeyB64] ?: return
    rows[addSignatureKeyB64] = existing.copy(dismissed = true)
    store.save(accountIdB64, groupIdHex, rows)
}

/**
 * The production [UnappliedRequestStore] — one plain `SharedPreferences` file.
 *
 * Row key: `"unapplied-requests-v1:" + accountIdB64 + NUL + groupIdHex` (the NUL
 * written as the Kotlin escape `\u0000`; a RAW NUL in the source would make git
 * classify this file as binary — ADR-0042 MEDIUM-3).
 *
 * Row value: `keyB64|viaJoinRequest|dismissed` entries joined by newline.
 * Standard base64 contains neither `|` nor a newline.
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
            rows.entries.joinToString("\n") {
                "${it.key}|${if (it.value.viaJoinRequest) 1 else 0}|" +
                    "${if (it.value.dismissed) 1 else 0}"
            }

        internal fun decodeRow(raw: String?): Map<String, UnappliedRow> {
            if (raw.isNullOrEmpty()) return emptyMap()
            val out = mutableMapOf<String, UnappliedRow>()
            for (line in raw.split('\n')) {
                val parts = line.split('|')
                if (parts.size != 3) continue
                if (parts[0].isEmpty()) continue
                out[parts[0]] = UnappliedRow(
                    viaJoinRequest = parts[1] == "1",
                    dismissed = parts[2] == "1",
                )
            }
            return out
        }
    }
}
