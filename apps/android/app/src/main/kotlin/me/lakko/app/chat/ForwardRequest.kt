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
    /**
     * ADR-0053 section 6 / client contract point 10 — the source conversation
     * is a one-to-one. A forward out of one carries NO origin reference: the
     * only other holder of that conversation is its other participant, so the
     * id would tell every recipient something about two people and help
     * nobody jump anywhere.
     */
    val sourceIsOneToOne: Boolean = false,
)

/**
 * What one visit to the "Forward to" picker sends: one request for the single
 * Forward of section 5.1, several — oldest first — for section 8's bulk
 * Forward. All from one source conversation.
 *
 * [fromSelection] is what tells the picker where a failure lands: a single
 * forward reports it in the picker, a selection's forward returns to the
 * source conversation with the unsent messages still selected and says
 * "Forwarded K of N." (section 8). A one-message selection is still a
 * selection, so this is a flag rather than a count.
 */
public data class ForwardBatch(
    val requests: List<ForwardRequest>,
    val fromSelection: Boolean,
) {
    init {
        require(requests.isNotEmpty()) { "a forward batch names at least one message" }
    }

    /** The conversation every request came from. */
    val sourceGroupIdHex: String get() = requests.first().sourceGroupIdHex
}

/**
 * The one-shot slot between a chat and the picker route.
 *
 * A slot rather than a navigation argument: the request carries a whole
 * message and a free-text name, neither of which belongs in a route string. The chat publishes, the picker route takes it once into its own
 * view model (which survives rotation), and an empty slot on arrival — a
 * process restored onto the picker — means the picker simply closes.
 *
 * Pure Kotlin, so the handshake runs on the host JVM.
 */
public object PendingForward {
    private val slot = MutableStateFlow<ForwardBatch?>(null)

    /** The batch waiting for a picker, or `null`. */
    public val pending: StateFlow<ForwardBatch?> = slot.asStateFlow()

    /** Section 5.1 — one message, chosen from its own action card. */
    public fun publish(request: ForwardRequest) {
        slot.value = ForwardBatch(requests = listOf(request), fromSelection = false)
    }

    /** Section 8 — a selection, oldest first. */
    public fun publishSelection(requests: List<ForwardRequest>) {
        if (requests.isEmpty()) return
        slot.value = ForwardBatch(requests = requests, fromSelection = true)
    }

    /** Take the batch and empty the slot. */
    public fun take(): ForwardBatch? = slot.getAndUpdate { null }
}

/**
 * Section 8 — how a selection's forward ended, handed back from the picker
 * to the source conversation: how many landed, how many were asked for, and
 * which messages were not sent (still to be selected). A complete outcome
 * ([done] == [total]) is what tells the source to leave selection mode.
 */
public data class ForwardOutcome(
    val sourceGroupIdHex: String,
    val done: Int,
    val total: Int,
    val unsentMessageIds: Set<Long>,
) {
    public val complete: Boolean get() = done == total
}

/**
 * The slot the picker leaves a [ForwardOutcome] in for the source
 * conversation, keyed on the conversation so an outcome meant for one chat
 * is never spent by another.
 */
public object PendingForwardOutcome {
    private val slot = MutableStateFlow<ForwardOutcome?>(null)

    /** The outcome waiting to be read, or `null`. */
    public val pending: StateFlow<ForwardOutcome?> = slot.asStateFlow()

    public fun publish(outcome: ForwardOutcome) {
        slot.value = outcome
    }

    /** Take the outcome meant for [groupIdHex], if there is one. */
    public fun take(groupIdHex: String): ForwardOutcome? {
        var taken: ForwardOutcome? = null
        slot.update { current ->
            if (current != null && current.sourceGroupIdHex == groupIdHex) {
                taken = current
                null
            } else {
                current
            }
        }
        return taken
    }
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
