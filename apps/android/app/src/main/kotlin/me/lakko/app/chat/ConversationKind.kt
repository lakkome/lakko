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
 * Per-device persisted conversation kind, one entry per group id, in plain
 * `SharedPreferences` (a list assignment is not sensitive).
 *
 * Writers: the new-group flow ([set] `Group`), the contact-accept and
 * invite-accept paths that create a 1x1 ([set] `Chat`), the members panel when
 * this user adds a person ([set] `Group`), and [resolve], which derives the
 * kind once for a conversation first seen through a Welcome or a restore.
 */
public object ConversationKindStore {
    private const val PREFS = "lakko-conversation-kind"
    private const val KEY_PREFIX = "kind:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(groupIdHex: String): String = KEY_PREFIX + groupIdHex.lowercase()

    /** The stored kind, or `null` when this conversation has none yet. */
    public fun stored(context: Context, groupIdHex: String): ConversationKind? =
        prefs(context).getString(key(groupIdHex), null)
            ?.let { raw -> ConversationKind.entries.firstOrNull { it.name == raw } }

    /** Explicit write, subject to [kindWriteAllowed]. Returns the kind now stored. */
    public fun set(context: Context, groupIdHex: String, kind: ConversationKind): ConversationKind {
        val current = stored(context, groupIdHex)
        if (!kindWriteAllowed(current, kind)) return current ?: kind
        if (current != kind) {
            prefs(context).edit().putString(key(groupIdHex), kind.name).apply()
        }
        return kind
    }

    /**
     * The kind to show, applying [decideConversationKind] to the stored value
     * and the live people count, and persisting a new decision.
     */
    public fun resolve(context: Context, groupIdHex: String, peopleCount: Int?): ConversationKind {
        val decision = decideConversationKind(stored(context, groupIdHex), peopleCount)
        if (decision.persist) {
            prefs(context).edit().putString(key(groupIdHex), decision.kind.name).apply()
        }
        return decision.kind
    }

    /** Drop one conversation's entry (a deleted chat). */
    public fun remove(context: Context, groupIdHex: String) {
        prefs(context).edit().remove(key(groupIdHex)).apply()
    }

    /** Forget-device wipe. */
    public fun clear(context: Context) {
        prefs(context).edit().clear().apply()
    }
}
