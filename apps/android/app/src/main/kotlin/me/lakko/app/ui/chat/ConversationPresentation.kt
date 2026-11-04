// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import me.lakko.app.chat.ChatRepository
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.joinList
import me.lakko.app.profile.ProfileDirectory

/**
 * Chats vs Groups — docs/ux/client-redesign.md section 3.
 *
 * The kind itself lives in [me.lakko.app.chat.ConversationKind] and is read
 * from the group ([ChatRepository.conversationKind], ADR-0062); this alias
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
 * flag that used to live here is gone since ADR-0062 — the kind is read from
 * the group, the same on every member's device.
 */
public object GroupPresentationStore {
    /** The input cap for a group name (ADR-0051 §1: 1 to 64 scalars). */
    public const val MAX_NAME_CHARS: Int = 64
}

/** Which list a conversation belongs to — read from the group (ADR-0062). */
public fun conversationKind(
    repository: ChatRepository,
    groupIdHex: String,
): ConversationKind = repository.conversationKind(groupIdHex)

/** `true` when the conversation is a group (ADR-0062 kind). */
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
    strings: Strings = Localization.strings.value,
): String {
    repository.oneToOnePeerKey(groupIdHex)?.let { key ->
        if (!isGroupConversation(repository, groupIdHex)) {
            return directory.resolve(key).primary
        }
    }
    me.lakko.app.chat.GroupNameStore
        .name(context, repository.currentAccountIdB64(), groupIdHex)
        ?.let { return it }
    return memberDerivedTitle(otherMemberNames(repository, directory, groupIdHex), strings)
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
public fun memberDerivedTitle(names: List<String>, strings: Strings = Localization.strings.value): String {
    if (names.isEmpty()) return strings["chat.group.fallback_name"]
    val shown = joinList(names.take(3), strings)
    val rest = names.size - 3
    return if (rest > 0) {
        strings.format("chat.group.title_more", mapOf("names" to shown), mapOf("count" to rest.toULong()))
    } else {
        shown
    }
}

/**
 * The shared initials rule (design-system.md, Avatar; `lakko_ui_rules`), the
 * one every client draws: Android keeps no initials logic of its own. [rule]
 * is a seam so host JVM tests, which cannot load the native core, can stand
 * a function in.
 */
public object AvatarInitials {
    @Volatile
    public var rule: (String) -> String = { uniffi.lakko_ffi_uniffi.avatarInitials(it) }
}

/** The letters an avatar without a photo draws for [name]; the placeholder
 *  "?" when the rule finds none (or the core cannot be reached). */
public fun initialsOf(name: String): String {
    val letters = try {
        AvatarInitials.rule(name)
    } catch (_: Throwable) {
        ""
    }
    return letters.ifEmpty { "?" }
}
