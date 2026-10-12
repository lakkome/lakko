// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.WeakHashMap

/**
 * One serial path of parameter writes per sender: the twin of the web
 * `sender-parameters.ts`.
 *
 * A write to a sender's parameters is a read (`RtpSender.getParameters`), a
 * change and a `setParameters`. Two of them on one sender must not overlap:
 * the second would put back what the first had just changed, because it read
 * before the first landed.
 *
 * Several parts of the group call write a camera sender's parameters: its
 * frame-rate preference, its temporal layers and a copy's size
 * (`CameraSenders.kt`, `WebRtcPeerConnection.kt`), whether its encoding is
 * active (`SenderActivity.kt`) and its bitrate ceiling (`PublishBudget.kt`).
 * Each hands its whole read-change-set to [serialParameterWrite].
 *
 * The web client queues promises because a browser's `setParameters` is
 * asynchronous. libwebrtc's Java `setParameters` answers before it returns,
 * so here the whole read-change-set is one synchronous block and the serial
 * path is one lock per sender: a block runs to its end before the next one on
 * that sender starts, in the order the lock is taken. The rule every caller
 * follows inside the block is the other half: read the sender's parameters
 * there, change only the fields the write is about, and set what was read.
 * Parameters read outside the block are never set.
 *
 * No libwebrtc type is needed: the sender is only the key.
 */
internal object SenderParameterWrites {
    private val locks = WeakHashMap<Any, Any>()

    private fun lockOf(sender: Any): Any = synchronized(locks) { locks.getOrPut(sender) { Any() } }

    /**
     * Run [write] (one whole read-change-set of [sender]'s parameters) once
     * every write of that sender started before it has ended, and return or
     * throw as [write] does. Another sender's writes are not held back.
     * Re-entrant: a write may contain another on the same sender.
     */
    fun <T> serial(sender: Any, write: () -> T): T = synchronized(lockOf(sender)) { write() }
}

/** [SenderParameterWrites.serial]. */
internal fun <T> serialParameterWrite(sender: Any, write: () -> T): T =
    SenderParameterWrites.serial(sender, write)
