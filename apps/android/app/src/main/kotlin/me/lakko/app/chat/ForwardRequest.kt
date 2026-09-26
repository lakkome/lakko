// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.update

/**
 * ADR-0052 decision 2 — a message on its way to the "Forward to" picker.
 *
 * [authorLabel] is what the source timeline called the author when Forward
 * was chosen ("You" for an own message, the resolved name otherwise), decided
 * where that resolution lives and carried here so the picker never re-derives
 * a name. [message] is the row as it stood at that moment, edited text and
 * all.
 */
public data class ForwardRequest(
    val sourceGroupIdHex: String,
    val message: ChatMessage,
    val authorLabel: String?,
)

/**
 * The one-shot slot between a chat and the picker route.
 *
 * A slot rather than a navigation argument, the way
 * [me.lakko.app.contacts.PendingContactLookup] is one: the request carries a
 * whole message and a free-text name, neither of which belongs in a route
 * string. The chat publishes, the picker route takes it once into its own
 * view model (which survives rotation), and an empty slot on arrival — a
 * process restored onto the picker — means the picker simply closes.
 *
 * Pure Kotlin, so the handshake runs on the host JVM.
 */
public object PendingForward {
    private val slot = MutableStateFlow<ForwardRequest?>(null)

    /** The request waiting for a picker, or `null`. */
    public val pending: StateFlow<ForwardRequest?> = slot.asStateFlow()

    public fun publish(request: ForwardRequest) {
        slot.value = request
    }

    /** Take the request and empty the slot. */
    public fun take(): ForwardRequest? = slot.getAndUpdate { null }
}

/**
 * Section 5.1 "Tap to jump" — the message a chat screen should scroll to and
 * light when it opens, keyed on the conversation it lives in.
 *
 * Published by the screen that navigates (a tapped attribution whose origin
 * this device holds), consumed by the destination once its timeline shows the
 * row. Keyed on the group so a jump meant for one conversation is never spent
 * by another that happened to open first.
 */
public object PendingMessageJump {
    private val slot = MutableStateFlow<Pair<String, Long>?>(null)

    public fun publish(groupIdHex: String, messageId: Long) {
        slot.value = groupIdHex to messageId
    }

    /** The message id waiting for [groupIdHex], without taking it. */
    public fun peek(groupIdHex: String): Long? =
        slot.value?.takeIf { it.first == groupIdHex }?.second

    /** Take the jump meant for [groupIdHex], if there is one. */
    public fun take(groupIdHex: String): Long? {
        var taken: Long? = null
        slot.update { current ->
            if (current != null && current.first == groupIdHex) {
                taken = current.second
                null
            } else {
                current
            }
        }
        return taken
    }
}
