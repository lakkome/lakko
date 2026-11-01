// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.ProposalKind
import uniffi.lakko_ffi_uniffi.ProposalRecord

/**
 * ADR-0045 S4a — the pending-membership-request surface, as
 * `docs/ux/proposal-based-membership.md` specifies it.
 *
 * Android mirror of `apps/web/src/chat/pending-requests.ts`.
 *
 * ## Why a plain module and not part of the Compose screen
 *
 * The host unit suite cannot render a composable, so every rule the UX spec
 * states — the quiet window, the visibility gate, the copy selection — lives
 * here where it is executable, and `MembersPanel` renders what this module
 * decides. Same split `SplitPoll.kt` and `CommitOutbox.kt` already use.
 *
 * ## The three UX rules this module exists to enforce
 *
 * 1. **No optimistic update.** A member with a pending removal STAYS in the
 *    roster, annotated. Dropping the row would assert they are gone while they
 *    are still cryptographically in the group — still receiving every message,
 *    still able to decrypt — until the commit lands. That is the membership
 *    equivalent of a green E2EE pill over an unencrypted call, and Lakko's
 *    honesty corollary to invariant #4 forbids it. The same rule in reverse
 *    governs a pending add: a person who has been *requested* is not a member
 *    and must not appear in the members list, so they get their own section.
 * 2. **No `Cancel`.** Once published, the request is in every member's proposal
 *    store and dies only when some commit lands. OpenMLS exposes a *local*
 *    single-proposal eviction, but there is no **wire** withdraw — evicting
 *    locally would leave the request live in every peer's store, which is a
 *    worse lie than showing it. So the confirmation dialog is the last exit,
 *    and this module deliberately exposes no withdraw verb.
 * 3. **No pending affordance until the quiet window elapses.** The owner is
 *    rank 0 and sweeps its own request immediately, so in the common case the
 *    request lands before anyone could look. A spinner that appears and vanishes
 *    in 300 ms is worse than no spinner: it teaches the user that membership is
 *    slow when it is not.
 *
 * ## What is the source of truth, and what this store adds
 *
 * `Group.pendingProposals()` is the truth about *what* is pending — the core's
 * own retained set, populated on every member by the receipt gate and emptied by
 * OpenMLS on every merged commit, so it needs no GC of its own. This module
 * never second-guesses it.
 *
 * What the core cannot supply is *when* a request arrived, because a
 * `ProposalRecord` carries no timestamp. The quiet window needs one, and per the
 * UX spec it must be the **DS row timestamp**, never a local clock: every member
 * reads the identical value for the identical row, whereas a local
 * `System.currentTimeMillis()` would make the window mean something different on
 * each device. So this module keeps a small per-`(account, group)` observation
 * store keyed on a synthetic request key, and joins it to the live pending set.
 *
 * Entries are pruned against the live set on every fold, so a swept or
 * superseded request cannot leave residue behind.
 */

/** How a proposal is classified for the panel. Mirrors `ProposalKind`. */
public enum class PendingRequestKind {
    ADD,
    REMOVE,
    LEAVE,
    UPDATE,
    ROLE_CHANGE,
    UNSUPPORTED,
}

/** One pending request, joined from the live core set and the observation store. */
public data class PendingRequest(
    /** Stable synthetic key — see [proposalKey]. */
    val key: String,
    val kind: PendingRequestKind,
    /** base64 of the prospective member's signature key. [PendingRequestKind.ADD] only. */
    val addSignatureKeyB64: String?,
    /** The leaf this request removes. REMOVE / LEAVE only.
     *
     *  For a REMOVE this is the proposal's own target field; for a LEAVE it is
     *  the SENDER's leaf, because RFC 9420 `SelfRemove` carries no target and the
     *  removed leaf is the sender's own. `null` when the sender could not be
     *  resolved — see [targetLeafOf]. */
    val targetLeaf: UInt?,
    /** The proposing member's leaf, or `null` for an unresolvable sender. */
    val senderLeaf: UInt?,
    /** DS row timestamp in ms, or `null` when this device never observed the row
     *  (it restored the proposal from a snapshot rather than seeing it land). */
    val observedAtMs: Long?,
    /** `true` when this device published it. Drives the UX §10 exception — a
     *  member always sees the status of a request they made themselves. */
    val own: Boolean,
    /**
     * ADR-0045 UX §6.1.10 — base64 of the successor's signature key, on OUR OWN
     * record only, for a `role-change` that is an ownership transfer.
     *
     * It does two jobs at once, and neither is available any other way. It is
     * the only thing that distinguishes *our pending role change is a transfer*
     * from *our pending role change is a promote* — `ProposalKind::RoleChange`
     * carries no payload, so the core cannot tell them apart and [proposalKey]
     * collapses every role change to the single key `role-change` — and it is
     * what names the person in `Waiting to make {name} the owner.`
     *
     * The KEY is stored, never a label: the label is resolved through the LIVE
     * roster at render time, so a successor who has since left degrades to the
     * unnamed variant rather than to a stale name.
     */
    val transferToB64: String? = null,
    /** [RequestState.PENDING] below the quiet window (render nothing),
     *  [RequestState.WAITING] above it. */
    val state: RequestState,
)

/** Whether a request has been outstanding long enough to be worth showing. */
public enum class RequestState { PENDING, WAITING }

/**
 * How long a request must be outstanding before any persistent, group-visible
 * pending affordance renders.
 *
 * **CHOSEN, not measured** — the UX spec leaves `QUIET_WINDOW` open (§12.1) and
 * this module is not in a position to close it, so the number is stated as a
 * judgement rather than dressed up as a finding. The budget it is sized against:
 * the owner's own sweep is one local stage plus two DS round-trips, so a window
 * shorter than that would flash an annotation on the path the UX spec is explicit
 * must look instantaneous; and a window much longer than a few seconds would
 * leave a genuine wait invisible for no gain. Nothing else in the system depends
 * on the value — it decides only *when* an annotation appears, never whether a
 * request is live — so it can be re-tuned from a measurement without touching any
 * other rule.
 */
internal const val QUIET_WINDOW_MS: Long = 3_000L

/**
 * A stable key for one request, so an observation recorded when the row landed
 * can be re-joined to the core's pending set on a later render.
 *
 * Derived from what the request *is*, never from a leaf index alone where a
 * signature key is available: OpenMLS reuses a vacated leaf for the next Add, so
 * a leaf-keyed record can silently come to name someone else. For a removal only
 * the leaf is available (the core's `ProposalKind.Remove` carries no signature
 * key), which is why the fold below resolves it against the live roster at render
 * time rather than trusting a stored label.
 *
 * `role-change` collapses to a single key on purpose: `classify` discards the
 * `GroupContextExtensions` payload, so a received role-change request is only
 * describable at group level (UX §11.5) and two of them are indistinguishable.
 */
internal fun proposalKey(record: ProposalRecord): String = when (val k = record.kind) {
    is ProposalKind.Add -> "add:" + b64(k.signatureKey)
    is ProposalKind.Remove -> "remove:${k.targetLeaf}"
    ProposalKind.Leave -> "leave:${record.senderLeaf ?: "?"}"
    ProposalKind.RoleChange -> "role-change"
    ProposalKind.Update -> "update:${record.senderLeaf ?: "?"}"
    ProposalKind.Unsupported -> "unsupported:${record.senderLeaf ?: "?"}"
}

/**
 * The quiet-window rule.
 *
 * An UNOBSERVED request (no DS timestamp — restored from a snapshot rather than
 * seen to land) renders as [RequestState.WAITING] immediately. That is the
 * deliberate direction: it has necessarily been pending since before this session
 * started, so it is by definition past any window, and treating it as
 * [RequestState.PENDING] would hide a genuinely old request behind a timer that
 * never fires.
 */
internal fun requestState(
    observedAtMs: Long?,
    nowMs: Long,
    quietWindowMs: Long = QUIET_WINDOW_MS,
): RequestState {
    if (observedAtMs == null) return RequestState.WAITING
    return if (nowMs - observedAtMs >= quietWindowMs) RequestState.WAITING else RequestState.PENDING
}

/** One recorded observation of a proposal landing. */
internal data class RequestObservation(
    /** DS row timestamp, ms since epoch. */
    val atMs: Long,
    /** `true` when this device published the request. */
    val own: Boolean,
    /**
     * ADR-0045 UX §6.1.10 — see [PendingRequest.transferToB64].
     *
     * It lives HERE, on the observation record, rather than in a store of its
     * own, and that is a correctness choice rather than tidiness: this store is
     * pruned against the core's live pending set on every fold
     * ([prunePendingRequestObservations]), so a swept or superseded transfer
     * takes its successor key with it. A separate store would have to re-derive
     * that prune, and a stale key surviving it would make an unrelated LATER
     * role change render `Waiting to make {name} the owner.` about somebody
     * nobody proposed.
     */
    val transferToB64: String? = null,
    /**
     * ADR-0045 S7 residual 1 — `true` when OUR OWN `add` request was published
     * by approving an external **join** rather than by adding a contact.
     *
     * It lives here for the same reason [transferToB64] does: this store is
     * pruned against the core's live pending set, so the flag dies with the
     * request it describes. It is knowable only on the device that made the
     * request, because on the wire an approved add is indistinguishable from an
     * admin adding a contact (ADR-0045 S7) — and that device is the one
     * `UnappliedRequests.kt` shows the notice on.
     */
    val joinRequest: Boolean = false,
    /**
     * ADR-0050 section 2.5a point 4 — what THIS device asked for, recorded so a
     * request a sweep dropped can be re-requested from the intent rather than
     * from old bytes. Set only on an entry stored under [intentObservationKey],
     * never on a proposal-keyed entry: a proposal key names a LEAF for a
     * removal, and OpenMLS reuses a vacated leaf, so the intent is keyed on
     * the subject's signature key instead (see [RetryableRequest.key]).
     */
    val intent: RetryableRequest? = null,
)

/**
 * ADR-0050 section 2.5a point 4 — the namespace an own request's INTENT lives
 * under in the same per-`(account, group)` observation row.
 *
 * It shares the row with the proposal observations rather than taking a store
 * of its own, but not their lifetime: [prunePendingRequestObservations] drops a
 * proposal observation the moment the core stops holding the request, and that
 * moment is exactly when the superseded detector needs the intent. So an entry
 * under this prefix survives the prune and is removed only by the detector
 * (effect landed, subject left, membership ended, stopped) or with the group.
 *
 * No proposal key starts with it ([proposalKey] writes `add:` / `remove:` /
 * `leave:` / `role-change` / `update:` / `unsupported:`), so the fold never
 * joins an intent to a live record and the panel never renders one.
 */
internal const val INTENT_KEY_PREFIX: String = "intent:"

/** The row key an own request's intent is stored under. */
internal fun intentObservationKey(intent: RetryableRequest): String =
    INTENT_KEY_PREFIX + intent.key

/** What [foldPendingRequests] produced. */
internal data class FoldedRequests(
    val requests: List<PendingRequest>,
    val liveKeys: List<String>,
)

/**
 * Which leaf a request removes, for the two kinds that remove one.
 *
 * A REMOVE names its target explicitly. A LEAVE does NOT: it is RFC 9420
 * `SelfRemove`, and the core is the authority on what that means —
 * `commit_effects` in `crates/lakko-mls/src/group_proposals.rs` states it,
 * "`SelfRemove` names no target: the removed leaf is the sender's own", and the
 * UniFFI mapping carries no target field on [ProposalKind.Leave] at all. So the
 * removed leaf is resolved here from the SENDER, exactly as the core resolves it.
 *
 * Reading `ProposalKind.Remove.targetLeaf` straight through — which is what this
 * used to do — left every leave with a `null` [PendingRequest.targetLeaf], so
 * [rowAnnotation]'s `targetLeaf == leafIndex` test could never match one and the
 * `Waiting to leave.` copy the UX spec §5.2 requires was unreachable.
 *
 * This is the same leaf [proposalKey] already keys a leave on, so the two now
 * agree rather than carrying two different notions of which leaf a leave
 * concerns.
 *
 * An UNRESOLVABLE sender stays `null` and must not collapse onto a real leaf: the
 * core refuses such a proposal anyway, and defaulting it to `0` would annotate the
 * owner's row for a request nobody made.
 */
private fun targetLeafOf(record: ProposalRecord): UInt? = when (val k = record.kind) {
    is ProposalKind.Remove -> k.targetLeaf
    ProposalKind.Leave -> record.senderLeaf
    else -> null
}

/**
 * Join the core's live pending set to the observation store.
 *
 * A record with `stored == false` is EXCLUDED: the receipt gate refused it, so no
 * sweep will ever commit it and it is not a pending request. That is a refusal
 * reported as data rather than as a throw, because a proposal advances no state —
 * dropping it unstored *is* the refusal.
 *
 * Returns the requests in the core's own order plus the keys that are still live,
 * so the caller can prune the store in the same pass.
 */
internal fun foldPendingRequests(
    records: List<ProposalRecord>,
    observations: Map<String, RequestObservation>,
    nowMs: Long,
    quietWindowMs: Long = QUIET_WINDOW_MS,
): FoldedRequests {
    val requests = mutableListOf<PendingRequest>()
    val liveKeys = mutableListOf<String>()
    for (record in records) {
        if (!record.stored) continue
        val key = proposalKey(record)
        liveKeys.add(key)
        val seen = observations[key]
        requests.add(
            PendingRequest(
                key = key,
                kind = normaliseKind(record.kind),
                addSignatureKeyB64 = (record.kind as? ProposalKind.Add)?.let { b64(it.signatureKey) },
                targetLeaf = targetLeafOf(record),
                senderLeaf = record.senderLeaf,
                observedAtMs = seen?.atMs,
                own = seen?.own ?: false,
                // Only meaningful on our own record, and only for a role
                // change — which is exactly where it is ever written.
                transferToB64 = seen?.transferToB64,
                state = requestState(seen?.atMs, nowMs, quietWindowMs),
            ),
        )
    }
    return FoldedRequests(requests, liveKeys)
}

/**
 * The panel's visibility gate (UX §10).
 *
 * > The pending set is visible to the owner and admins. Every member additionally
 * > sees the status of a request they made themselves. No member is ever told
 * > about a pending change to another member's status unless they can act on it.
 *
 * The pending set is a work queue and only owner/admins have a verb over it.
 * Showing a plain member "Bob is waiting to be removed" tells them something they
 * cannot act on, about someone else's standing, in a window that did not exist
 * before ADR-0045 created it — and in a group where a removal is a safety action,
 * advertising it is a hazard.
 *
 * Note what this is NOT: it is not concealment from the target. The receipt gate
 * stores the proposal on **every** member's device including theirs, so a
 * determined target can see it regardless. The choice is only whether to build a
 * surface that broadcasts it. Not rendering something asserts nothing, so it
 * breaks no honesty rule, and it matches what the panel already does — add /
 * remove / the invite block / join requests are all role-gated today.
 *
 * The own-request exception is non-negotiable: without it a self-remove is an
 * action with no feedback.
 */
internal fun visibleRequests(
    requests: List<PendingRequest>,
    canManageMembers: Boolean,
): List<PendingRequest> =
    if (canManageMembers) requests.toList() else requests.filter { it.own }

/** The §5.1 pending-notice copy, or `null` when nothing is waiting. */
public data class PendingNotice(val count: UiText, val liveness: UiText)

/** How often an open members panel re-reads the group. A read that shows no
 *  change does nothing. Chosen, not measured. */
internal const val MEMBERS_PANEL_REFRESH_MS: Long = 2_000L

internal fun pendingNotice(requests: List<PendingRequest>): PendingNotice? {
    val waiting = requests.count { it.state == RequestState.WAITING }
    if (waiting == 0) return null
    return PendingNotice(
        count = UiText("chat.members.pending.count", counts = mapOf("count" to waiting.toULong())),
        // ONE line, not two. The escalated variant went with the `stalled`
        // state: ADR-0045 decision 2 gives a waiting request no deadline, so
        // there is no moment at which the client learns something new to say.
        // This sentence is true at ten seconds and at ten days, it states
        // INACTION (the only thing the client can observe), it names no person,
        // and it blames nobody.
        liveness = UiText("chat.members.pending.liveness"),
    )
}

/**
 * The row annotation for a member, or `null` when that member has no pending
 * request against them.
 *
 * "Waiting to leave." when the request targets the reader's own leaf — the
 * self-remove that is how leaving works — otherwise "Waiting to be removed." A
 * pending role change deliberately gets no row annotation: it carries no target,
 * so it cannot be attributed to a member (UX §11.5); it contributes to the notice
 * count and shows only at panel level.
 */
internal fun rowAnnotation(
    requests: List<PendingRequest>,
    leafIndex: UInt,
    ownLeaf: UInt?,
): UiText? {
    val hit = requests.firstOrNull {
        it.state == RequestState.WAITING &&
            (it.kind == PendingRequestKind.REMOVE || it.kind == PendingRequestKind.LEAVE) &&
            it.targetLeaf == leafIndex
    } ?: return null
    return UiText(
        if (leafIndex == ownLeaf) "chat.members.pending.waiting_leave" else "chat.members.pending.waiting_removed",
    )
}

// The "Join requests are not saved" note that used to live here is gone with
// the reason for it: since 2026-09-26 `Group::pending_external_joins` rides the
// MLS storage snapshot (ADR-0027 update note), so a restart shows the same
// list. Rows are still never aged out, which would hide something still live
// for the joiner.

/** The "Waiting to be added" rows — pending adds only, past the window. */
internal fun waitingToBeAdded(requests: List<PendingRequest>): List<PendingRequest> =
    requests.filter { it.kind == PendingRequestKind.ADD && it.state == RequestState.WAITING }

// ---------------------------------------------------------------------------
// The observation store
// ---------------------------------------------------------------------------

/**
 * Where a group's request observations live: `requestKey -> (DS row ms, own)`.
 *
 * Not encrypted: a request key names a proposal already retained in the core's
 * own pending set and a server-assigned timestamp — no key material. Same
 * posture as [BeaconObservationStore]. An own request intent adds a
 * signature key and, for an add, the subject's account id: both are already
 * known to every member of the group, and neither is key material.
 */
internal interface PendingRequestStore {
    fun load(accountIdB64: String, groupIdHex: String): Map<String, RequestObservation>

    fun save(
        accountIdB64: String,
        groupIdHex: String,
        observations: Map<String, RequestObservation>,
    )

    fun clearForAccount(accountIdB64: String)
}

/**
 * Record that a proposal landed, keyed on [proposalKey].
 *
 * The FIRST observation of a key wins: a re-broadcast or a re-request of the same
 * request must not reset its age, or a request that keeps being re-sent would
 * never mature past the quiet window and would stay invisible forever — exactly
 * the wait the notice exists to surface. `own` is upgraded to `true` if this
 * device later publishes the same request, because the own-request visibility
 * exception must not be lost to arrival order.
 *
 * This is the deliberate OPPOSITE of [recordBeaconObservation], where the newest
 * stamp wins: there, recency is the signal.
 *
 * [transferToB64] (ADR-0045 §6.1.10) follows the same first-wins rule for the
 * same reason: the successor belongs to the request that is actually pending,
 * and only the device that published it can supply one. A later observation of
 * the same key without one must not erase it — the receive loop observes its own
 * request landing back off the journal, and that observation carries no
 * successor.
 */
internal fun recordPendingRequestObservation(
    store: PendingRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    key: String,
    atMs: Long,
    own: Boolean,
    transferToB64: String? = null,
    joinRequest: Boolean = false,
) {
    val observations = store.load(accountIdB64, groupIdHex)
    val existing = observations[key]
    store.save(
        accountIdB64,
        groupIdHex,
        observations + (
            key to RequestObservation(
                atMs = existing?.atMs ?: atMs,
                own = (existing?.own ?: false) || own,
                transferToB64 = existing?.transferToB64 ?: transferToB64,
                // Sticky-true, for the same reason `own` is: the receive loop
                // observes our own approval landing back off the journal and
                // that observation knows nothing about where the request came
                // from. Losing the flag to arrival order would silently
                // downgrade the copy to the wrong action.
                joinRequest = (existing?.joinRequest ?: false) || joinRequest,
            )
            ),
    )
}

/**
 * ADR-0045 UX §4 — stamp a proposal ANOTHER member published, with the DS
 * row's own timestamp.
 *
 * This is the receive-side half of [recordPendingRequestObservation], and
 * before it existed there was none: the only production caller stamped OUR
 * OWN requests. A request arriving from a peer therefore carried no
 * observation, [requestState] read `observedAtMs == null`, and by its own
 * documented rule that means `WAITING` **immediately** — so §4's quiet window,
 * which exists precisely so a request that resolves in milliseconds never
 * flashes an annotation, silently never applied to any request the reader did
 * not make themselves. Which is every request an admin ever sees.
 *
 * Two refusals, both returning `false` rather than guessing:
 *
 *   * **`stored == false`.** The receipt gate refused the proposal, so no
 *     sweep will ever commit it and it is not a pending request. Recording it
 *     would leave residue in a store that is otherwise pruned against the live
 *     set.
 *   * **[atMs] is `null`** — the DS timestamp could not be parsed. Falling
 *     back to the local clock is worse than not stamping: an unstamped request
 *     renders `WAITING` immediately, which is merely eager, while a
 *     locally-stamped one silently reintroduces exactly the clock skew the DS
 *     timestamp exists to remove. *"`age` is measured against the DS row
 *     timestamp, never a local clock … every member reads the identical value
 *     for the identical row."*
 *
 * `own` is hard-coded `false`, and the first-wins rule in
 * [recordPendingRequestObservation] is what makes that safe: the receive loop
 * also observes OUR OWN proposal echoing back off the journal, and the `own`
 * flag is upgraded monotonically, so the echo cannot clear §10's
 * non-negotiable own-request exception.
 *
 * Takes millis rather than the ISO string so the rule stays free of
 * `java.time` and reachable from a host unit test; the repository parses.
 *
 * @return whether an observation was written.
 */
internal fun recordReceivedProposalObservation(
    store: PendingRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    record: ProposalRecord,
    atMs: Long?,
): Boolean {
    if (!record.stored) return false
    if (atMs == null) return false
    recordPendingRequestObservation(
        store = store,
        accountIdB64 = accountIdB64,
        groupIdHex = groupIdHex,
        key = proposalKey(record),
        atMs = atMs,
        own = false,
    )
    return true
}

/**
 * Drop every observation whose request is no longer in the core's pending set —
 * swept, superseded, or the group moved.
 *
 * An own request's INTENT ([INTENT_KEY_PREFIX]) is kept: it is the only record
 * that the request existed once the core has dropped it, and only the
 * superseded detector, which runs on an observed merged commit, may decide
 * whether it landed, must be re-requested, or stopped.
 */
internal fun prunePendingRequestObservations(
    store: PendingRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    liveKeys: List<String>,
) {
    val observations = store.load(accountIdB64, groupIdHex)
    val live = liveKeys.toSet()
    val kept = observations.filterKeys { it in live || it.startsWith(INTENT_KEY_PREFIX) }
    if (kept.size != observations.size) store.save(accountIdB64, groupIdHex, kept)
}

/**
 * ADR-0050 section 2.5a point 4 — record what THIS device asked for, at the
 * moment its proposal is published.
 *
 * The NEWEST intent for a key wins, the opposite of the first-wins proposal
 * observation: an intent is not an age but a description of the request now
 * on the wire, and a later request for the same subject (a re-request, a
 * user's second tap, a promote replaced by a demote in the one role-change
 * slot) is the one a superseded check must re-derive.
 */
internal fun recordOwnRequestIntent(
    store: PendingRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    intent: RetryableRequest,
    atMs: Long,
) {
    val observations = store.load(accountIdB64, groupIdHex)
    store.save(
        accountIdB64,
        groupIdHex,
        observations + (
            intentObservationKey(intent) to RequestObservation(atMs = atMs, own = true, intent = intent)
            ),
    )
}

/** Every own request intent recorded for one group. */
internal fun loadOwnRequestIntents(
    store: PendingRequestStore,
    accountIdB64: String,
    groupIdHex: String,
): List<RetryableRequest> =
    store.load(accountIdB64, groupIdHex)
        .filterKeys { it.startsWith(INTENT_KEY_PREFIX) }
        .values
        .mapNotNull { it.intent }

/** Forget one own request intent — its effect landed, or it stopped. */
internal fun forgetOwnRequestIntent(
    store: PendingRequestStore,
    accountIdB64: String,
    groupIdHex: String,
    intent: RetryableRequest,
) {
    val observations = store.load(accountIdB64, groupIdHex)
    val key = intentObservationKey(intent)
    // Only the exact intent read: a newer request recorded under the same key
    // between the read and this write (a manual tap) must survive.
    if (observations[key]?.intent != intent) return
    store.save(accountIdB64, groupIdHex, observations - key)
}

/**
 * The production [PendingRequestStore] — one plain `SharedPreferences` file.
 *
 * Row key: `"pending-requests-v1:" + accountIdB64 + NUL + groupIdHex` (the NUL
 * written as the Kotlin escape `\u0000`; a RAW NUL in the source would make git
 * classify this file as binary — ADR-0042 MEDIUM-3).
 *
 * Row value: `key|atMs|own|transferToB64|joinRequest|intent` entries joined by
 * newline, the sixth field being [encodeRequestIntent] or empty. A request
 * key is built from base64 / decimal / fixed literals, none of which contain `|`
 * or a newline, and the standard-base64 successor key contains neither either.
 *
 * The fourth field (ADR-0045 §6.1.10) is written as the empty string when
 * absent, which is every request that is not an ownership transfer this device
 * published. The fifth (ADR-0045 S7 residual 1) is `0`/`1`. A row in any earlier
 * shape is simply DROPPED rather than up-converted: there is no deployment to
 * migrate, and the cost of dropping one is bounded to the quiet window and the
 * own-request marker of a request that happened to be in flight across an app
 * upgrade — the request itself lives in the core's pending set and is
 * untouched.
 *
 * Never throws: a corrupt or absent row reads as empty, which degrades the quiet
 * window to "render immediately" rather than losing the request. The request
 * itself lives in the core's pending set, so nothing here is load-bearing for
 * correctness — only for *when* an annotation appears.
 */
internal class SharedPrefsPendingRequestStore(context: Context) : PendingRequestStore {

    private val appContext: Context = context.applicationContext

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String, groupIdHex: String): Map<String, RequestObservation> =
        try {
            decodeRow(prefs().getString(rowKey(accountIdB64, groupIdHex), null))
        } catch (_: Throwable) {
            emptyMap()
        }

    override fun save(
        accountIdB64: String,
        groupIdHex: String,
        observations: Map<String, RequestObservation>,
    ) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                val key = rowKey(accountIdB64, groupIdHex)
                val editor = prefs().edit()
                if (observations.isEmpty()) {
                    editor.remove(key)
                } else {
                    editor.putString(key, encodeRow(observations))
                }
                editor.apply()
            } catch (_: Throwable) {
                // the quiet window is best-effort
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

    internal companion object {
        private const val PREFS_NAME = "lakko-pending-requests"
        private const val KEY_PREFIX = "pending-requests-v1:"
        private const val ROW_SEPARATOR = "\u0000"

        private fun rowKey(accountIdB64: String, groupIdHex: String): String =
            KEY_PREFIX + accountIdB64 + ROW_SEPARATOR + groupIdHex

        internal fun encodeRow(observations: Map<String, RequestObservation>): String =
            observations.entries.joinToString("\n") {
                "${it.key}|${it.value.atMs}|${if (it.value.own) 1 else 0}|" +
                    (it.value.transferToB64 ?: "") +
                    "|${if (it.value.joinRequest) 1 else 0}" +
                    "|" + (it.value.intent?.let(::encodeRequestIntent) ?: "")
            }

        internal fun decodeRow(raw: String?): Map<String, RequestObservation> {
            if (raw.isNullOrEmpty()) return emptyMap()
            val out = mutableMapOf<String, RequestObservation>()
            for (line in raw.split('\n')) {
                val parts = line.split('|')
                if (parts.size != 6) continue
                val at = parts[1].toLongOrNull() ?: continue
                // An intent field that does not decode drops the whole entry
                // rather than keeping it without one: an intent-keyed entry
                // with no intent would be a request nobody can re-derive.
                val intent = if (parts[5].isEmpty()) {
                    null
                } else {
                    decodeRequestIntent(parts[5]) ?: continue
                }
                out[parts[0]] = RequestObservation(
                    atMs = at,
                    own = parts[2] == "1",
                    // The empty string is the ABSENT successor, not an empty
                    // one: a zero-length base64 key names nobody, and letting it
                    // through would make `ownTransfer` true for a promote.
                    transferToB64 = parts[3].takeIf { it.isNotEmpty() },
                    // ADR-0045 S7 residual 1. Absent reads as an ordinary add,
                    // which is the copy that offers the weaker action rather
                    // than the stronger claim.
                    joinRequest = parts[4] == "1",
                    intent = intent,
                )
            }
            return out
        }
    }
}

// ---------------------------------------------------------------------------

private fun normaliseKind(kind: ProposalKind): PendingRequestKind = when (kind) {
    is ProposalKind.Add -> PendingRequestKind.ADD
    is ProposalKind.Remove -> PendingRequestKind.REMOVE
    ProposalKind.Leave -> PendingRequestKind.LEAVE
    ProposalKind.Update -> PendingRequestKind.UPDATE
    ProposalKind.RoleChange -> PendingRequestKind.ROLE_CHANGE
    ProposalKind.Unsupported -> PendingRequestKind.UNSUPPORTED
}

/** Standard base64 of raw bytes — [outboxB64], reused so a request key means the
 *  same string here and in the durable outbox, and so this module stays host
 *  testable (`android.util.Base64` is a stub on the JVM). */
private fun b64(bytes: ByteArray): String = outboxB64(bytes)
