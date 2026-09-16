// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.chat.ChatRepository
import me.lakko.app.profile.ProfileDirectory

/**
 * Chats vs Groups — docs/ux/client-redesign.md section 3.
 *
 * Two lists over the same conversation store, split by kind. A conversation
 * with two PEOPLE is a chat (people are counted by account, never by device
 * leaf — [ChatRepository.memberCount] already does that); anything else is a
 * group. A group created through the new-group flow is a group even while it
 * still has two people, which is what [GroupPresentationStore] remembers.
 *
 * Presentation only: nothing here touches the repository or the MLS state.
 */
public enum class ConversationKind { Chat, Group }

/**
 * Local, per-device presentation facts about groups this device created or
 * named: the "created as a group" flag and the optional local group name. Both
 * live in plain `SharedPreferences` (names are local labels, never sent).
 */
public object GroupPresentationStore {
    private const val PREFS = "lakko-group-presentation"
    private const val KEY_CREATED = "created-as-group"
    private const val NAME_PREFIX = "name:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    public fun markCreatedAsGroup(context: Context, groupIdHex: String) {
        val p = prefs(context)
        val set = p.getStringSet(KEY_CREATED, emptySet()).orEmpty().toMutableSet()
        set += groupIdHex.lowercase()
        p.edit().putStringSet(KEY_CREATED, set).apply()
    }

    public fun wasCreatedAsGroup(context: Context, groupIdHex: String): Boolean =
        prefs(context).getStringSet(KEY_CREATED, emptySet()).orEmpty().contains(groupIdHex.lowercase())

    public fun setName(context: Context, groupIdHex: String, name: String?) {
        val e = prefs(context).edit()
        val trimmed = name?.trim().orEmpty()
        if (trimmed.isEmpty()) e.remove(NAME_PREFIX + groupIdHex.lowercase())
        else e.putString(NAME_PREFIX + groupIdHex.lowercase(), trimmed.take(MAX_NAME_CHARS))
        e.apply()
    }

    public fun name(context: Context, groupIdHex: String): String? =
        prefs(context).getString(NAME_PREFIX + groupIdHex.lowercase(), null)?.takeIf { it.isNotBlank() }

    public const val MAX_NAME_CHARS: Int = 64
}

/** Which list a conversation belongs to. */
public fun conversationKind(
    context: Context,
    repository: ChatRepository,
    groupIdHex: String,
): ConversationKind {
    if (GroupPresentationStore.wasCreatedAsGroup(context, groupIdHex)) return ConversationKind.Group
    // A thread whose handle cannot be read yet defaults to a chat: the
    // overwhelmingly common origin of a new conversation is a contact accept.
    val people = repository.memberCount(groupIdHex) ?: 2
    return if (people <= 2) ConversationKind.Chat else ConversationKind.Group
}

/**
 * The title of a conversation, never a hex id: the other person's name for a
 * chat; the local group name, else a name derived from the members
 * ("Anna, Ivan, Maria" and "+2" beyond three), else "Group".
 */
public fun conversationTitle(
    context: Context,
    repository: ChatRepository,
    directory: ProfileDirectory,
    groupIdHex: String,
): String {
    repository.oneToOnePeerKey(groupIdHex)?.let { key ->
        val people = repository.memberCount(groupIdHex) ?: 2
        if (people <= 2 && !GroupPresentationStore.wasCreatedAsGroup(context, groupIdHex)) {
            return directory.resolve(key).primary
        }
    }
    GroupPresentationStore.name(context, groupIdHex)?.let { return it }
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
