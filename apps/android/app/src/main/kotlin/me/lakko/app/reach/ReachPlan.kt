// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import uniffi.lakko_ffi_uniffi.FfiHeldReach
import uniffi.lakko_ffi_uniffi.FfiReachPurpose
import uniffi.lakko_ffi_uniffi.FfiReachWritePlan

/*
 * G-53 (maintainer decision 2026-11-21) — which held reach grant writes a
 * Welcome or signs a committer delegation is the core's rule
 * (`lakko_contacts::reach_plan`, `planReachWrites` over UniFFI). This file is
 * the client half and decides nothing about the order:
 *
 *  - the grants held for one person are split by the mailbox their binding
 *    opens (verified by the caller against the person's current account key;
 *    a grant that does not verify opens nothing and is left out), one plan
 *    per mailbox, the mailboxes in the order their first grant was listed;
 *  - the grant a search found the person through for this add is passed as
 *    `found` when it is still held;
 *  - what the core names `superseded` is returned so the caller deletes it.
 *
 * Pure apart from the [ReachPlanner] it is handed, so the host JVM tests run it
 * against a stand-in planner.
 */

/** One grant held for the person, as the plan sees it. [grantHex] is the
 *  grant wire (hex), [mailboxHex] the mailbox its binding opens (verified),
 *  [payload] whatever the caller writes with. */
internal class HeldForPlan<G>(
    val kind: ReachKind,
    val scopeHex: String,
    val version: UInt,
    val mailboxHex: String,
    val grantHex: String,
    val payload: G,
)

/** The core's plan for one mailbox: `planReachWrites`. */
internal fun interface ReachPlanner {
    fun plan(
        held: List<FfiHeldReach>,
        purpose: FfiReachPurpose,
        targetScope: ByteArray?,
        sharedScopes: List<ByteArray>,
        found: UInt?,
    ): FfiReachWritePlan
}

/** The production planner: the core's `planReachWrites`. */
internal val NativeReachPlanner: ReachPlanner = ReachPlanner { held, purpose, target, shared, found ->
    uniffi.lakko_ffi_uniffi.planReachWrites(held, purpose, target, shared, found)
}

/** What [planHeldWrites] decided: the grants to try, best first, every
 *  mailbox's in turn, and the ones a higher version replaced. [considered]
 *  counts the grants handed to the core (whose binding verified). */
internal class HeldWritePlan<G>(
    val candidates: List<HeldForPlan<G>>,
    val superseded: List<HeldForPlan<G>>,
    val considered: Int = 0,
)

/**
 * The core's plan over [held] (the grants this device keeps for ONE person),
 * one call per mailbox. [targetScopeHex] is the group the write is for (or
 * `null`), [sharedScopesHex] the groups this account and the person are both
 * current members of, [foundGrantHex] the wire of the grant the search of
 * this add found them through (`null`: none, or not kept). A planner that
 * refuses one mailbox's list plans nothing for it (logged): the write then
 * finds no candidate there, never one the core did not order.
 */
internal fun <G> planHeldWrites(
    held: List<HeldForPlan<G>>,
    purpose: FfiReachPurpose,
    targetScopeHex: String?,
    sharedScopesHex: Collection<String>,
    foundGrantHex: String?,
    planner: ReachPlanner,
): HeldWritePlan<G> {
    val byMailbox = LinkedHashMap<String, MutableList<HeldForPlan<G>>>()
    for (h in held) {
        val list = byMailbox.getOrPut(h.mailboxHex) { ArrayList() }
        // One grant listed twice (filed under the person's current key and
        // under a predecessor) is one grant.
        if (list.none { it.grantHex == h.grantHex }) list += h
    }
    val target = targetScopeHex?.let(::reachUnhex)
    val shared = sharedScopesHex.map { it.lowercase() }.distinct().mapNotNull(::reachUnhex)
    val candidates = ArrayList<HeldForPlan<G>>()
    val superseded = ArrayList<HeldForPlan<G>>()
    var considered = 0
    for (entries in byMailbox.values) {
        val usable = entries.filter { reachUnhex(it.scopeHex) != null }
        if (usable.isEmpty()) continue
        considered += usable.size
        val input = usable.mapIndexed { index, h ->
            FfiHeldReach(h.kind.toFfi(), reachUnhex(h.scopeHex) ?: ByteArray(0), h.version, index.toUInt())
        }
        val found = foundGrantHex?.lowercase()?.let { wanted ->
            usable.indexOfFirst { it.grantHex == wanted }.takeIf { it >= 0 }?.toUInt()
        }
        val plan = try {
            planner.plan(input, purpose, target, shared, found)
        } catch (cause: Throwable) {
            logPlanRefused(cause)
            continue
        }
        plan.candidates.mapNotNullTo(candidates) { usable.getOrNull(it.toInt()) }
        plan.superseded.mapNotNullTo(superseded) { usable.getOrNull(it.toInt()) }
    }
    return HeldWritePlan(candidates, superseded, considered)
}

/**
 * G-53 — whether a write planned by the core may end in the proofless write
 * to the person's mailbox (after every candidate yielded nothing): only when
 * no grant was held for them at all ([considered] `0`), or the plan offered a
 * candidate. Grants held but none usable (a `group` grant with no group shared
 * now) say the mailbox may well be closed, and a proofless write to a closed
 * mailbox is answered as stored and dropped: the write then reports no grant
 * reaches them, so a contact request takes a shared group
 * (`docs/ux/who-can-find-me.md` 6.4) or the "can't message yet" state.
 */
internal fun prooflessAfterPlan(considered: Int, candidates: Int): Boolean = considered == 0 || candidates > 0

/** What the proposer of an add sends its committers ([committerEnvelopeKind]). */
internal enum class CommitterEnvelopeKind {
    /** The binding of a person found in Everyone and no delegation. */
    OPEN,

    /** Delegations signed with a held grant. */
    SIGNED,

    /** Nothing: another path must carry the Welcome. */
    NONE,
}

/**
 * G-53 — the envelope a proposer sends beside an Add: when the search that
 * produced this add found the person in Everyone ([foundOpen], the binding
 * recorded for the add), the open one, whatever grant is held (a held grant
 * may be dead, and a committer can always write into the open mailbox);
 * else delegations when a held grant can sign them; else none.
 */
internal fun committerEnvelopeKind(foundOpen: Boolean, signingGrantHeld: Boolean): CommitterEnvelopeKind =
    when {
        foundOpen -> CommitterEnvelopeKind.OPEN
        signingGrantHeld -> CommitterEnvelopeKind.SIGNED
        else -> CommitterEnvelopeKind.NONE
    }

/** [held] with each grant of [gone] (class name, grant wire hex) held for the
 *  person filed under [personKeys] taken out ([withoutHeldGrant]); the list
 *  itself when nothing matched. */
internal fun withoutSupersededGrants(
    held: List<StoredHeld>,
    personKeys: List<String>,
    gone: List<Pair<String, String>>,
): List<StoredHeld> {
    var next = held
    for ((kind, grantHex) in gone) next = withoutHeldGrant(next, personKeys, kind, grantHex)
    return next
}

/**
 * G-53 — the groups this account and one person are both current members of:
 * out of [groups] (group id hex, and the person keys of its roster, `null`
 * when this account is no longer an active member there), those whose
 * roster holds a person whose key [resolve]s (proven predecessor -> current)
 * to [personCurrentKey]. Each key is resolved once.
 */
internal fun sharedScopesWith(
    groups: List<Pair<String, List<ByteArray>?>>,
    personCurrentKey: ByteArray,
    resolve: (ByteArray) -> ByteArray,
): List<String> {
    val target = reachHex(personCurrentKey)
    val resolved = HashMap<String, String>()
    val out = ArrayList<String>()
    for ((groupIdHex, people) in groups) {
        if (people == null) continue
        val shared = people.any { key ->
            val hex = reachHex(key)
            resolved.getOrPut(hex) { reachHex(resolve(key)) } == target
        }
        if (shared) out += groupIdHex.lowercase()
    }
    return out
}

/** The core's open-fallback rule: `reachOpenMailboxWriteAllowed`. */
internal fun interface OpenWriteRule {
    fun allowed(recordOpen: Boolean, candidates: Int, refused: Int): Boolean
}

/** The production rule: the core's `reachOpenMailboxWriteAllowed`. */
internal val NativeOpenWriteRule: OpenWriteRule = OpenWriteRule { open, candidates, refused ->
    uniffi.lakko_ffi_uniffi.reachOpenMailboxWriteAllowed(
        open,
        candidates.coerceAtLeast(0).toUInt(),
        refused.coerceAtLeast(0).toUInt(),
    )
}

/** Chosen, not measured: far past the adds one device has in flight; the
 *  oldest record goes first. A dropped record only loses the found grant's
 *  lead (the grant itself stays held). */
internal const val MAX_FOUND_GRANTS: Int = 256

/** [records] with [grantHex] recorded as the grant a search found
 *  [addedAccountKeyHex] through for the add in [groupHex], in place of an
 *  earlier one for the same pair; bounded to [MAX_FOUND_GRANTS]. */
internal fun withFoundGrant(
    records: List<StoredFoundGrant>,
    groupHex: String,
    addedAccountKeyHex: String,
    grantHex: String,
): List<StoredFoundGrant> {
    val group = groupHex.lowercase()
    val added = addedAccountKeyHex.lowercase()
    return (
        records.filterNot { it.groupHex == group && it.addedAccountKeyHex == added } +
            StoredFoundGrant(group, added, grantHex.lowercase())
        ).takeLast(MAX_FOUND_GRANTS)
}

/** [records] without the found grant of ([groupHex], [addedAccountKeyHex]). */
internal fun withoutFoundGrant(
    records: List<StoredFoundGrant>,
    groupHex: String,
    addedAccountKeyHex: String,
): List<StoredFoundGrant> {
    val group = groupHex.lowercase()
    val added = addedAccountKeyHex.lowercase()
    return records.filterNot { it.groupHex == group && it.addedAccountKeyHex == added }
}

/** [records] without the open binding of ([groupHex], [addedAccountKeyHex]). */
internal fun withoutOpenBinding(
    records: List<StoredOpenBinding>,
    groupHex: String,
    addedAccountKeyHex: String,
): List<StoredOpenBinding> {
    val group = groupHex.lowercase()
    val added = addedAccountKeyHex.lowercase()
    return records.filterNot { it.groupHex == group && it.addedAccountKeyHex == added }
}

/** The grant wire (hex) a search found the person filed under [personKeys]
 *  (current key first) through for the add in [groupHex], or `null`. */
internal fun foundGrantFor(records: List<StoredFoundGrant>, groupHex: String, personKeys: List<String>): String? {
    val group = groupHex.lowercase()
    for (key in personKeys) {
        records.lastOrNull { it.groupHex == group && it.addedAccountKeyHex == key.lowercase() }?.let { return it.grantHex }
    }
    return null
}

private fun logPlanRefused(cause: Throwable) {
    try {
        android.util.Log.w("ReachPlan", "G-53: the reach plan refused a held list: ${cause.javaClass.simpleName}")
    } catch (_: Throwable) {
        // Host JVM tests have no android.util.Log.
    }
}
