// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Walk 7 (2026-11-20): a group joined while the app-wide sweep paused for the
 * long idle interval was read late. A new group now ends the pause and opens
 * the wake window.
 */
class ReceiveCadenceNewGroupTest {

    @Test
    fun only_a_group_the_sweep_did_not_hold_is_new() {
        assertTrue(ReceiveCadence.groupsAppeared(setOf("a"), listOf("a", "joined")))
        assertFalse(ReceiveCadence.groupsAppeared(setOf("a", "b"), listOf("b", "a")))
        assertFalse(ReceiveCadence.groupsAppeared(setOf("a", "b"), listOf("a")))
    }

    @Test
    fun a_new_group_ends_the_pause_and_opens_the_wake_window(): Unit = runBlocking {
        val groups = MutableStateFlow(listOf("a"))
        val now = 5_000_000L
        val wakeBefore = ReceiveCadence.lastWakeAtMs.value
        val waiter = launch {
            ReceiveCadence.awaitWakeOrNewGroup(
                wakeBefore = wakeBefore,
                swept = setOf("a"),
                groups = groups,
                nowMs = { now },
            )
        }
        yield()
        assertFalse("nothing new yet", waiter.isCompleted)
        groups.value = listOf("a", "joined")
        withTimeout(5_000) { waiter.join() }
        assertTrue(ReceiveCadence.wakeWindowOpen(now + 1))
    }
}
