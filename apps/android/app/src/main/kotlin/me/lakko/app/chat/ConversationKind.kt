// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.GroupKind

/**
 * Which list a conversation belongs to — docs/ux/client-redesign.md
 * section 3 and 5.1.
 *
 * Two lists over the same conversation store, split by kind. Since ADR-0062
 * the kind is NOT derived on this device: the creator fixes it in the group's
 * GroupContext (`0xFF03`) and every member reads it from there
 * ([ChatRepository.groupKind]), so a two-person group is a group on every
 * member's device and nothing ever moves between the two lists. The people
 * count stays for display only.
 */
public enum class ConversationKind { Chat, Group }

/**
 * The list kind for a group whose GroupContext says [groupKind]:
 * `ONE_TO_ONE` is a chat, anything else a group.
 *
 * `null` counts as a GROUP, the same choice as web: every conversation now
 * carries a kind (every `Group.create` passes one and `joinFromWelcome`
 * refuses a conversation without one), so `null` only covers objects that are
 * not conversations (`PublisherGroup`, `DeviceGroup`) or a read that failed.
 * Reading those as a group never names one member as "the person", never
 * offers the one-to-one pin standing, and never counts people.
 */
public fun conversationKindOf(groupKind: GroupKind?): ConversationKind = when (groupKind) {
    GroupKind.ONE_TO_ONE -> ConversationKind.Chat
    GroupKind.GROUP, null -> ConversationKind.Group
}

/**
 * ADR-0062 section 7 — whether a group-name envelope (ADR-0051) received in a
 * conversation of [groupKind] may be applied. A one-to-one has no group name,
 * and applying one would retitle the person, so it is dropped there. Any other
 * kind (a group, or a group this device could not classify) keeps the
 * ADR-0051 rules unchanged.
 */
public fun groupNameAppliesTo(groupKind: GroupKind?): Boolean = groupKind != GroupKind.ONE_TO_ONE
