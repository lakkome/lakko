// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import me.lakko.app.calls.CallSignalingBodyKind
import me.lakko.app.chat.IncomingChatEvent
import uniffi.lakko_ffi_uniffi.BlockVerdict

/*
 * ADR-0098 section 6, UX section 1.2 — what a block removes from a received
 * batch before anything stores, counts, notifies or rings for it. Applied at
 * the two foreground receive paths (the poll and the swept rows) right after
 * the classifier, so every later stage sees the batch as if the dropped rows
 * never came. A group message is NOT removed here: it is stored and drawn as
 * the collapsed row ([receivedMessageVerdict] = COLLAPSE, read at render).
 */

/**
 * The events of [events] that survive the block register.
 *
 * - a one-to-one chat row or op from a blocked person: dropped;
 * - their typing, read receipt or profile update in the one-to-one: dropped;
 * - a contact request from them carried inside a group: dropped unseen;
 * - their call offer in the one-to-one, or a group call they start: dropped,
 *   so nothing rings and no missed-call row is written.
 */
internal fun withoutBlockedEvents(
    events: List<IncomingChatEvent>,
    oneToOne: Boolean,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
    callBodyKind: (ByteArray) -> CallSignalingBodyKind? = { me.lakko.app.calls.callSignalingBodyKind(it) },
): List<IncomingChatEvent> {
    if (events.isEmpty()) return events
    return events.filterNot { event ->
        when (event) {
            is IncomingChatEvent.Application ->
                receivedMessageVerdict(oneToOne, event.senderKey, isOwn = false, isBlocked) == BlockVerdict.DROP
            is IncomingChatEvent.Op ->
                receivedMessageVerdict(oneToOne, event.senderKey, isOwn = false, isBlocked) == BlockVerdict.DROP
            is IncomingChatEvent.Typing -> dropsIncomingSignal(oneToOne, event.senderKey, isBlocked)
            is IncomingChatEvent.ReadReceipt -> dropsIncomingSignal(oneToOne, event.senderKey, isBlocked)
            is IncomingChatEvent.Profile -> dropsIncomingSignal(oneToOne, event.senderKey, isBlocked)
            is IncomingChatEvent.GroupContactRequest -> dropsContactRequest(event.senderKey, isBlocked)
            is IncomingChatEvent.CallSignaling -> {
                val starts = if (oneToOne) {
                    true
                } else {
                    callBodyKind(event.payloadBytes) == CallSignalingBodyKind.GROUP_START
                }
                starts && declinesCallSilently(event.senderPersonKey, isBlocked)
            }
            else -> false
        }
    }
}
