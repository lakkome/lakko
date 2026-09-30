// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ConversationKindStore
import me.lakko.app.profile.ProfileDirectory

/**
 * Chats vs Groups — docs/ux/client-redesign.md section 3.
 *
 * The kind itself (and its persistence) lives in
 * [me.lakko.app.chat.ConversationKind] / [ConversationKindStore]; this alias
 * keeps the UI-side name the screens already use. Presentation only: nothing
 * here touches the repository or the MLS state.
 */
public typealias ConversationKind = me.lakko.app.chat.ConversationKind

/**
 * Local presentation facts about groups the signed-in account created on this
 * device: the "created as a group" flag, in plain `SharedPreferences`, one
 * string set per account (`created-as-group.v2:<accountIdB64>`). Until
 * 2026-09-30 the set was device-wide, so a second account on the same phone
 * inherited it; the device-wide `created-as-group` set of earlier builds is
 * never read, and [deleteAccount] removes it. With no account bound (see
 * [me.lakko.app.contacts.AcceptedContactsStore]) a read is `false` and nothing
 * is written.
 *
 * The group NAME no longer lives here. Since ADR-0051 a name is a fact shared
 * inside the group — sent as an encrypted envelope, accepted from the owner or
 * an admin, ordered by its stamp — and it is stored per `(account, group)` in
 * [me.lakko.app.chat.GroupNameStore] together with that stamp. What remains
 * here is the one presentation fact that is genuinely local.
 */
public object GroupPresentationStore {
    private const val PREFS = "lakko-group-presentation"
    private const val KEY_PREFIX = "created-as-group.v2:"
    private const val RETIRED_KEY = "created-as-group"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String): String =
        KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    private fun bound(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    public fun markCreatedAsGroup(context: Context, groupIdHex: String) {
        val account = bound() ?: return
        val p = prefs(context)
        val set = p.getStringSet(key(account), emptySet()).orEmpty().toMutableSet()
        set += groupIdHex.lowercase()
        p.edit().putStringSet(key(account), set).apply()
        // The persisted kind is what both lists read; the flag above is kept
        // for the title rule and for entries written before the kind existed.
        ConversationKindStore.set(context, groupIdHex, ConversationKind.Group)
    }

    public fun wasCreatedAsGroup(context: Context, groupIdHex: String): Boolean {
        val account = bound() ?: return false
        return prefs(context).getStringSet(key(account), emptySet()).orEmpty().contains(groupIdHex.lowercase())
    }

    /** Forget device: drop [accountIdB64]'s set and the retired device-wide
     *  one. Other accounts' sets stay. */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        prefs(context).edit().remove(key(accountIdB64)).remove(RETIRED_KEY).apply()
    }

    /** The input cap for a group name (ADR-0051 §1: 1 to 64 scalars). */
    public const val MAX_NAME_CHARS: Int = 64
}

/**
 * Which list a conversation belongs to: the persisted kind, derived once from
 * the people count at first sight and flipped chat -> group when a third
 * person is observed ([me.lakko.app.chat.decideConversationKind]).
 */
public fun conversationKind(
    context: Context,
    repository: ChatRepository,
    groupIdHex: String,
): ConversationKind {
    if (GroupPresentationStore.wasCreatedAsGroup(context, groupIdHex)) {
        return ConversationKindStore.set(context, groupIdHex, ConversationKind.Group)
    }
    return ConversationKindStore.resolve(context, groupIdHex, repository.memberCount(groupIdHex))
}

/** `true` when the conversation is presented as a group (persisted kind). */
public fun isGroupConversation(
    context: Context,
    repository: ChatRepository,
    groupIdHex: String,
): Boolean = conversationKind(context, repository, groupIdHex) == ConversationKind.Group

/**
 * The title of a conversation, never a hex id: the other person's name for a
 * chat; the shared group name (ADR-0051), else a name derived from the members
 * ("Anna, Ivan, Maria" and "+2" beyond three), else "Group".
 *
 * Callers that cache the result must key it on
 * [me.lakko.app.chat.GroupNameStore.version] as well as the roster and the
 * profile directory, so a name arriving on a poll re-titles in place.
 */
public fun conversationTitle(
    context: Context,
    repository: ChatRepository,
    directory: ProfileDirectory,
    groupIdHex: String,
): String {
    repository.oneToOnePeerKey(groupIdHex)?.let { key ->
        if (!isGroupConversation(context, repository, groupIdHex)) {
            return directory.resolve(key).primary
        }
    }
    me.lakko.app.chat.GroupNameStore
        .name(context, repository.currentAccountIdB64(), groupIdHex)
        ?.let { return it }
    return memberDerivedTitle(otherMemberNames(repository, directory, groupIdHex))
}

/** The other members' resolved names, in roster order. */
public fun otherMemberNames(
    repository: ChatRepository,
    directory: ProfileDirectory,
    groupIdHex: String,
): List<String> {
    val own = repository.ownMemberIdentity(groupIdHex)
    val members = repository.members(groupIdHex) ?: return emptyList()
    return members
        .filter { me.lakko.app.chat.outboxB64(it.signaturePublicKey) != own }
        .map { directory.resolve(it.signaturePublicKey).primary }
}

/** "Anna, Ivan, Maria" with "+N" beyond three; "Group" when nobody is known. */
public fun memberDerivedTitle(names: List<String>): String {
    if (names.isEmpty()) return "Group"
    val shown = names.take(3).joinToString(", ")
    val rest = names.size - 3
    return if (rest > 0) "$shown +$rest" else shown
}

/** One- or two-letter initials for an avatar. */
public fun initialsOf(name: String): String {
    val words = name.trim().trimStart('@').split(Regex("\\s+")).filter { it.isNotEmpty() }
    if (words.isEmpty()) return "?"
    val first = words[0].first()
    val second = words.getOrNull(1)?.first()
    val s = if (second != null) "$first$second" else first.toString()
    return s.uppercase()
}
