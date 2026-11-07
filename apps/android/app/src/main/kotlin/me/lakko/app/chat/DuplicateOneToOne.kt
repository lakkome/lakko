// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * One person, one one-to-one row. Android twin of
 * `apps/web/src/contacts/duplicate-one-to-one.ts`.
 *
 * A device can hold two one-to-ones with the same person: it founded one (A)
 * whose Welcome the other person's device never opened, and later that person
 * founded another (B) in answer to a request this account sent (a link
 * answer, an Add). Both showed as rows in Chats (seen live 2026-11-07,
 * desktop and Pixel).
 *
 * The other person founding B settles it: a device founds a one-to-one in
 * answer to a request only when it holds none live with the requester
 * (ADR-0079 section 5, the restore step), so A is dead on their side and B is
 * where they read. B is therefore recorded as A's successor in the
 * conversation lineage (ADR-0073 R.5): one row, whose head is B, with A's
 * history above it, and A's journal no longer polled.
 *
 * Only a one-to-one founded by the PEER continues one founded by THIS
 * account. Two one-to-ones founded by the same side, or by each side about the
 * same time, are left as they are: neither side's founding then says which one
 * the other reads.
 *
 * Pure: the caller reads the rosters, founders and lineage.
 */

/** A conversation group this device holds, as the rule reads it. */
internal data class HeldConversationGroup(
    /** Lower-case hex of the group id. */
    val groupHex: String,
    /** The conversation the lineage puts it in (its root's hex). */
    val conversationHex: String,
    /** Kind one-to-one (ADR-0062). */
    val isOneToOne: Boolean,
    /** The distinct people of the roster, base64 account keys. */
    val people: List<String>,
    /** The founder's account key (base64), or `null` when unreadable. */
    val founderKey: String?,
)

/** One lineage register to write: [successorHex] continues [rootHex]. */
internal data class DuplicateFold(val successorHex: String, val rootHex: String)

/** The other person of a one-to-one roster, or `null`. */
private fun peerOf(people: List<String>, own: String): String? {
    if (people.size != 2) return null
    val peers = people.filter { it != own }
    return peers.singleOrNull()
}

/**
 * The registers that fold each peer-founded one-to-one into the conversation
 * of an older one-to-one with the same person that this account founded. Only
 * a group that is its own conversation (no lineage yet) is folded; when
 * several own-founded one-to-ones with the person exist, the one whose
 * conversation id sorts first is the root, so every device of the account
 * writes the same register.
 */
internal fun duplicateOneToOneFolds(held: List<HeldConversationGroup>, ownKey: String?): List<DuplicateFold> {
    if (ownKey == null) return emptyList()
    fun peer(g: HeldConversationGroup): String? = if (g.isOneToOne) peerOf(g.people, ownKey) else null
    val out = ArrayList<DuplicateFold>()
    for (joined in held) {
        val p = peer(joined) ?: continue
        if (joined.founderKey != p) continue
        // Already part of a conversation (a successor, or a root others continue).
        if (joined.conversationHex != joined.groupHex) continue
        if (held.any { it.groupHex != joined.groupHex && it.conversationHex == joined.groupHex }) continue
        val root = held
            .filter {
                it.groupHex != joined.groupHex &&
                    it.conversationHex != joined.conversationHex &&
                    it.founderKey == ownKey &&
                    peer(it) == p
            }
            .map { it.conversationHex }
            .sorted()
            .firstOrNull()
        if (root != null) out += DuplicateFold(successorHex = joined.groupHex, rootHex = root)
    }
    return out
}
