// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences

/**
 * Which list a conversation belongs to — docs/ux/client-redesign.md
 * section 3 and 5.1.
 *
 * Two lists over the same conversation store, split by kind. A conversation
 * with two PEOPLE is a chat (people are counted by account, never by device
 * leaf — [ChatRepository.memberCount] already does that); anything else is a
 * group. A group created through the new-group flow is a group even while it
 * still has two people.
 *
 * The kind is PERSISTED per conversation ([ConversationKindStore]) so a chat
 * never moves between the two lists on its own: the people count is only the
 * fallback at first sight. The one sanctioned move is chat -> group, when a
 * third person is observed (somebody was added); group -> chat never happens.
 */
public enum class ConversationKind { Chat, Group }

/** The outcome of [decideConversationKind]: what to show, and whether to write it down. */
public data class KindDecision(
    val kind: ConversationKind,
    /** `true` when the decision is new information the store should keep. */
    val persist: Boolean,
)

/**
 * The pure rule behind the persisted kind.
 *
 *  - A stored `Group` is final.
 *  - A stored `Chat` flips to `Group` (and is re-persisted) once the people
 *    count is observed above two; otherwise it stands.
 *  - Nothing stored (first sight through a Welcome or a restore): derive from
 *    the people count — two people is a chat, anything else a group — and
 *    persist. An unreadable count (`null`) shows as a chat and persists
 *    nothing, so the next read with a real count decides.
 */
public fun decideConversationKind(
    stored: ConversationKind?,
    peopleCount: Int?,
): KindDecision = when (stored) {
    ConversationKind.Group -> KindDecision(ConversationKind.Group, persist = false)
    ConversationKind.Chat ->
        if (peopleCount != null && peopleCount > 2) {
            KindDecision(ConversationKind.Group, persist = true)
        } else {
            KindDecision(ConversationKind.Chat, persist = false)
        }
    null -> when {
        peopleCount == null -> KindDecision(ConversationKind.Chat, persist = false)
        peopleCount <= 2 -> KindDecision(ConversationKind.Chat, persist = true)
        else -> KindDecision(ConversationKind.Group, persist = true)
    }
}

/**
 * Whether an explicit write of [requested] over [stored] is allowed. The only
 * refused write is `Chat` over a stored `Group` (never group -> chat).
 */
public fun kindWriteAllowed(stored: ConversationKind?, requested: ConversationKind): Boolean =
    !(stored == ConversationKind.Group && requested == ConversationKind.Chat)

/**
 * Persisted conversation kind, one entry per `(account, group id)`, in plain
 * `SharedPreferences` (a list assignment is not sensitive).
 *
 * The kind belongs to the signed-in ACCOUNT's view of its conversation: until
 * 2026-09-30 it was keyed on the group id alone, so a second account on the
 * same phone that shared a conversation inherited the first account's
 * decision and forget device left it behind. Rows are keyed
 * `kind.v2:<accountIdB64>:<groupIdHex>`; the account is the one bound in
 * [me.lakko.app.contacts.AcceptedContactsStore]. With no account bound a read
 * is `null` and nothing is written. The device-wide `kind:<groupIdHex>` rows
 * of earlier builds are never read; [deleteAccount] removes them.
 *
 * Writers: the new-group flow ([set] `Group`), the contact-accept and
 * invite-accept paths that create a 1x1 ([set] `Chat`), the members panel when
 * this user adds a person ([set] `Group`), and [resolve], which derives the
 * kind once for a conversation first seen through a Welcome or a restore.
 */
public object ConversationKindStore {
    private const val PREFS = "lakko-conversation-kind"
    private const val KEY_PREFIX = "kind.v2:"
    private const val RETIRED_PREFIX = "kind:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun bound(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    private fun accountPrefix(accountIdB64: String): String =
        KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64) + ":"

    /** The row key of [groupIdHex] for [accountIdB64]. Visible for tests. */
    internal fun key(accountIdB64: String, groupIdHex: String): String =
        accountPrefix(accountIdB64) + groupIdHex.lowercase()

    /** The stored kind, or `null` when this conversation has none yet or no
     *  account is signed in. */
    public fun stored(context: Context, groupIdHex: String): ConversationKind? =
        bound()?.let { stored(context, it, groupIdHex) }

    /** [stored] for [accountIdB64], whatever is bound. */
    public fun stored(context: Context, accountIdB64: String, groupIdHex: String): ConversationKind? =
        prefs(context).getString(key(accountIdB64, groupIdHex), null)
            ?.let { raw -> ConversationKind.entries.firstOrNull { it.name == raw } }

    /** Explicit write, subject to [kindWriteAllowed]. Returns the kind now
     *  stored (the requested kind, unstored, with no account signed in). */
    public fun set(context: Context, groupIdHex: String, kind: ConversationKind): ConversationKind {
        val account = bound() ?: return kind
        val current = stored(context, account, groupIdHex)
        if (!kindWriteAllowed(current, kind)) return current ?: kind
        if (current != kind) {
            prefs(context).edit().putString(key(account, groupIdHex), kind.name).apply()
        }
        return kind
    }

    /**
     * The kind to show, applying [decideConversationKind] to the stored value
     * and the live people count, and persisting a new decision (not with no
     * account signed in).
     */
    public fun resolve(context: Context, groupIdHex: String, peopleCount: Int?): ConversationKind {
        val account = bound()
        val decision = decideConversationKind(account?.let { stored(context, it, groupIdHex) }, peopleCount)
        if (decision.persist && account != null) {
            prefs(context).edit().putString(key(account, groupIdHex), decision.kind.name).apply()
        }
        return decision.kind
    }

    /** Drop one conversation's entry for the signed-in account (a deleted chat). */
    public fun remove(context: Context, groupIdHex: String) {
        val account = bound() ?: return
        prefs(context).edit().remove(key(account, groupIdHex)).apply()
    }

    /** Forget device: drop every entry of [accountIdB64] and the retired
     *  device-wide rows. Other accounts' entries stay. */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val p = prefs(context)
        val prefix = accountPrefix(accountIdB64)
        val e = p.edit()
        for (k in p.all.keys) if (k.startsWith(prefix) || k.startsWith(RETIRED_PREFIX)) e.remove(k)
        e.apply()
    }
}
