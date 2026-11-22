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
 * account, unless this device knows which of the two it held first
 * ([HeldConversationGroup.heldSinceMs], [ConversationHeldSince]): then the
 * newer one-to-one with the person continues the older one whoever founded
 * either (base scenario 9, 2026-11-22): a person who lost their one-to-one and
 * founded a new one, or a one-to-one another device of this account founded
 * while this device held an older one. When the times are equal or unknown,
 * two one-to-ones founded by the same side are left as they are.
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
    /** When this device first held the group, or `null` when unknown. */
    val heldSinceMs: Long? = null,
)

/** Whether [a] was first held strictly after [b] on this device. */
private fun heldAfter(a: HeldConversationGroup, b: HeldConversationGroup): Boolean =
    a.heldSinceMs != null && b.heldSinceMs != null && a.heldSinceMs > b.heldSinceMs

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
        // Already part of a conversation (a successor, or a root others continue).
        if (joined.conversationHex != joined.groupHex) continue
        if (held.any { it.groupHex != joined.groupHex && it.conversationHex == joined.groupHex }) continue
        val root = held
            .filter {
                it.groupHex != joined.groupHex &&
                    it.conversationHex != joined.conversationHex &&
                    peer(it) == p &&
                    // Newer, whoever founded either; else the peer's over this
                    // account's, unless this device knows the latter is newer.
                    (heldAfter(joined, it) ||
                        (joined.founderKey == p && it.founderKey == ownKey && !heldAfter(it, joined)))
            }
            .map { it.conversationHex }
            .sorted()
            .firstOrNull()
        if (root != null) out += DuplicateFold(successorHex = joined.groupHex, rootHex = root)
    }
    return out
}

/**
 * When this device first held each conversation group, per account (base
 * scenario 9): a group joined later is recorded later, which orders two
 * one-to-ones with the same person for [duplicateOneToOneFolds]. A group seen
 * for the first time together with others (the first run after an upgrade, a
 * restore) shares their time and orders nothing. Device-local; never sent.
 */
internal object ConversationHeldSince {
    /** Pure: the next record and the time of each group in [groupHexes]
     *  ([stored] completed with [nowMs] for groups without one; groups no
     *  longer held dropped). */
    fun next(stored: Map<String, Long>, groupHexes: List<String>, nowMs: Long): Map<String, Long> =
        groupHexes.associateWith { stored[it] ?: nowMs }

    private const val PREFS = "lakko_conversation_held_since"

    /** One key per account whichever base64 spelling the caller holds. */
    private fun keyOf(accountB64: String): String =
        accountB64.replace('-', '+').replace('_', '/').trimEnd('=')

    /** The record of [accountB64] on this device, completed and stored. A
     *  failure reads as unknown (`null` per group). */
    fun record(context: android.content.Context, accountB64: String, groupHexes: List<String>, nowMs: Long): Map<String, Long?> {
        return try {
            val prefs = context.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
            val key = keyOf(accountB64)
            val raw = prefs.getString(key, null)
            val stored = raw?.let { decode(it) }.orEmpty()
            val next = next(stored, groupHexes, nowMs)
            if (next != stored) {
                if (!prefs.edit().putString(key, encode(next)).commit()) {
                    return groupHexes.associateWith { stored[it] }
                }
            }
            next
        } catch (_: Throwable) {
            groupHexes.associateWith { null }
        }
    }

    /** Forget device / delete account. */
    fun clear(context: android.content.Context, accountB64: String) {
        try {
            context.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE).edit().remove(keyOf(accountB64)).commit()
        } catch (_: Throwable) {
            // nothing kept
        }
    }

    fun encode(map: Map<String, Long>): String = map.entries.joinToString(";") { "${it.key}=${it.value}" }

    fun decode(raw: String): Map<String, Long> =
        raw.split(';').mapNotNull { entry ->
            val i = entry.indexOf('=')
            if (i <= 0) return@mapNotNull null
            val v = entry.substring(i + 1).toLongOrNull() ?: return@mapNotNull null
            entry.substring(0, i) to v
        }.toMap()
}
