// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.CancellationException

/**
 * Remove every push-gw subscription a deleted account held on this device
 * (ADR-0078 section 3: a row names no account, so only the device that made
 * it can name it to push-gw). One `POST /push/unsubscribe` per id through
 * the `unsubscribe` a [run] is handed; each [run] tries only the ids not yet
 * removed, and throws while any is left, so a caller that retries goes on
 * where it stopped.
 */
public class PushSubscriptionsRemoval(subscriptionIds: List<ByteArray>) {
    private val remaining: MutableList<ByteArray> =
        subscriptionIds.distinctBy { PushClient.pseudonymHex(it) }.toMutableList()

    /** How many ids are still to remove. */
    public val left: Int get() = synchronized(remaining) { remaining.size }

    public suspend fun run(unsubscribe: suspend (ByteArray) -> Unit) {
        val batch = synchronized(remaining) { remaining.toList() }
        var failed = 0
        for (subId in batch) {
            try {
                unsubscribe(subId)
                synchronized(remaining) { remaining.remove(subId) }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                failed += 1
            }
        }
        if (failed > 0) throw PushSubscriptionsLeft(failed)
    }
}

/** [count] subscriptions could not be removed in one pass. */
public class PushSubscriptionsLeft(public val count: Int) :
    RuntimeException("$count push subscriptions left")
