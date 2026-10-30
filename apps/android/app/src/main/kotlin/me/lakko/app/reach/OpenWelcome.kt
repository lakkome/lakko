// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

/*
 * ADR-0060 section 6 (amended 2026-10-29) — an add of a person found by
 * search, the pure half:
 *
 *  - which held grants a committer delegation may be signed with (a grant the
 *    search found the person through is kept as a held grant first, so it
 *    is one of these);
 *  - which bindings let a sweep write the Welcome to a person found in
 *    Everyone into their open mailbox WITHOUT a proof: only one this device
 *    recorded for the group when it proposed the add, or one carried by a
 *    kept committer-delegation envelope that carries NO delegation (the
 *    proposer had no grant, the person is in Everyone). An envelope carrying
 *    delegations never opens the proofless path: its proposer held a grant,
 *    so the person's mailbox may well be closed.
 *
 * No Android, no native calls: the host JVM tests run all of it.
 */

/**
 * The classes of held grant a committer delegation is signed with, in the
 * order tried: the contact key, then a `group`, `channel` or
 * contacts-of-contacts key (each of these is a key the person handed out to
 * let someone write to them, and a search finds people through the last
 * three). Never `member` (a co-member the person hides from holds it, UX
 * section 6.2), `reply` or `link` (each opens the mailbox for one flow).
 */
public val DELEGATION_GRANT_KINDS: List<ReachKind> =
    listOf(ReachKind.CONTACT, ReachKind.GROUP, ReachKind.CHANNEL, ReachKind.FOF)

/** [held] in the order a delegation is signed from them ([DELEGATION_GRANT_KINDS],
 *  then by scope), each once; classes outside that list are left out. */
public fun delegationGrantOrder(held: Collection<HeldGrantRef>): List<HeldGrantRef> {
    val out = LinkedHashSet<HeldGrantRef>()
    for (kind in DELEGATION_GRANT_KINDS) out.addAll(held.filter { it.kind == kind }.sortedBy { it.scopeHex })
    return out.toList()
}

/** A kept committer-delegation envelope as the open-Welcome rule reads it. */
public class KeptDelegationView(
    /** The added person's account key the envelope names, hex. */
    public val addedAccountKeyHex: String,
    /** The added person's mailbox binding the envelope carries. */
    public val binding: ByteArray,
    /** How many delegations it carries (0: the proposer held no grant). */
    public val delegationCount: Int,
)

/**
 * The bindings a sweep's Welcome to the person filed under [personKeysHex]
 * (current key first) may be written proofless to, in the order tried: this
 * device's own records for [groupHex] ([ownRecords]) first, then the kept
 * envelopes ([kept], already limited to [groupHex]) that carry no
 * delegation. Each must still verify under the person's current account key
 * (the caller's); none at all means the Welcome stays owed.
 */
internal fun openWelcomeBindings(
    ownRecords: List<StoredOpenBinding>,
    kept: List<KeptDelegationView>,
    groupHex: String,
    personKeysHex: List<String>,
): List<ByteArray> {
    val group = groupHex.lowercase()
    val keys = personKeysHex.map { it.lowercase() }
    val out = ArrayList<ByteArray>()
    for (key in keys) {
        for (r in ownRecords) {
            if (r.groupHex != group || r.addedAccountKeyHex != key) continue
            reachUnhex(r.bindingHex)?.let(out::add)
        }
    }
    for (key in keys) {
        for (d in kept) {
            if (d.delegationCount != 0 || d.addedAccountKeyHex.lowercase() != key) continue
            out += d.binding
        }
    }
    return out.distinctBy { reachHex(it) }
}

/** [records] with a binding for ([groupHex], [addedAccountKeyHex]) put in
 *  place of any earlier one for the same pair. */
internal fun withOpenBinding(
    records: List<StoredOpenBinding>,
    groupHex: String,
    addedAccountKeyHex: String,
    bindingHex: String,
): List<StoredOpenBinding> {
    val group = groupHex.lowercase()
    val added = addedAccountKeyHex.lowercase()
    return records.filterNot { it.groupHex == group && it.addedAccountKeyHex == added } +
        StoredOpenBinding(group, added, bindingHex.lowercase())
}
