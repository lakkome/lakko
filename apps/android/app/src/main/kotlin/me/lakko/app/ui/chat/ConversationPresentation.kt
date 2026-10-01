// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import me.lakko.app.chat.ChatRepository
import me.lakko.app.profile.ProfileDirectory

/**
 * Chats vs Groups — docs/ux/client-redesign.md section 3.
 *
 * The kind itself lives in [me.lakko.app.chat.ConversationKind] and is read
 * from the group ([ChatRepository.conversationKind], ADR-0060); this alias
 * keeps the UI-side name the screens already use. Presentation only: nothing
 * here touches the MLS state.
 */
public typealias ConversationKind = me.lakko.app.chat.ConversationKind

/**
 * Presentation constants for groups.
 *
 * The group NAME does not live here: since ADR-0051 a name is a fact shared
 * inside the group and is stored per `(account, group)` in
 * [me.lakko.app.chat.GroupNameStore]. The per-device "created as a group"
 * flag that used to live here is gone since ADR-0060 — the kind is read from
 * the group, the same on every member's device.
 */
public object GroupPresentationStore {
    /** The input cap for a group name (ADR-0051 §1: 1 to 64 scalars). */
    public const val MAX_NAME_CHARS: Int = 64
}

/** Which list a conversation belongs to — read from the group (ADR-0060). */
public fun conversationKind(
    repository: ChatRepository,
    groupIdHex: String,
): ConversationKind = repository.conversationKind(groupIdHex)

/** `true` when the conversation is a group (ADR-0060 kind). */
public fun isGroupConversation(
    repository: ChatRepository,
    groupIdHex: String,
): Boolean = conversationKind(repository, groupIdHex) == ConversationKind.Group

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
        if (!isGroupConversation(repository, groupIdHex)) {
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
