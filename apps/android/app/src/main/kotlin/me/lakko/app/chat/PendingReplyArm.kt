// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update

/**
 * `docs/ux/feed.md` section 5 — Reply chosen on a feed item opens the
 * conversation with the reply strip already set (message-actions.md 3.1).
 * The feed has no composer; it leaves the message here and the chat arms the
 * reply once its timeline holds the row, the way [PendingMessageJump] carries
 * a jump. Keyed on the group so one conversation never spends another's.
 */
public object PendingReplyArm {
    private val slot = MutableStateFlow<Pair<String, Long>?>(null)

    public fun publish(groupIdHex: String, messageId: Long) {
        slot.value = groupIdHex to messageId
    }

    /** Take the reply meant for [groupIdHex], if there is one. */
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

    /** The message id waiting for [groupIdHex], without taking it. */
    public fun peek(groupIdHex: String): Long? = slot.value?.takeIf { it.first == groupIdHex }?.second
}
