// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * ADR-0058 — the folded synced state is written after a quiet debounce, and
 * Log out keeps it: stopping the writer writes a change still waiting out its
 * debounce, and nothing is written for the account after the stop.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class SyncStatePersistenceTest {

    private val scope = TestScope(StandardTestDispatcher())
    private val versions = MutableStateFlow(0L)
    private val writes = mutableListOf<Pair<String, String>>()
    private val changed = mutableListOf<String>()

    private val persistence = SyncStatePersistence(
        scope = scope,
        versions = versions,
        debounceMs = DEBOUNCE,
        encode = { "v${versions.value}".toByteArray() },
        write = { account, bytes -> writes += account to String(bytes) },
        onChanged = { changed += it },
    )

    /** Let the debounce fire. */
    private fun settle() {
        scope.advanceTimeBy(DEBOUNCE * 2)
        scope.runCurrent()
    }

    @Test
    fun a_change_still_waiting_out_its_debounce_is_written_by_the_stop() {
        persistence.start(ACCOUNT)
        settle()
        assertEquals(listOf(ACCOUNT to "v0"), writes)

        versions.value = 1L // a folder edit, say, a moment before Log out
        scope.runCurrent()
        assertEquals("the debounce has not fired yet", 1, writes.size)

        persistence.stopAndFlush()
        assertEquals(listOf(ACCOUNT to "v0", ACCOUNT to "v1"), writes)
        assertEquals(listOf(ACCOUNT), changed)
        assertFalse(persistence.isBound)
    }

    @Test
    fun nothing_is_written_for_the_account_after_the_stop() {
        persistence.start(ACCOUNT)
        settle()
        versions.value = 1L
        persistence.stopAndFlush()
        val atStop = writes.toList()

        versions.value = 2L
        settle()
        persistence.stopAndFlush()
        assertEquals(atStop, writes)
    }

    @Test
    fun a_stop_with_nothing_new_writes_nothing() {
        persistence.start(ACCOUNT)
        settle()
        persistence.stopAndFlush()
        assertEquals(listOf(ACCOUNT to "v0"), writes)
        assertTrue("writing back what bootstrap read is not a change", changed.isEmpty())
    }

    @Test
    fun a_stop_before_the_first_write_writes_the_state() {
        persistence.start(ACCOUNT)
        versions.value = 1L
        persistence.stopAndFlush()
        assertEquals(listOf(ACCOUNT to "v1"), writes)
        settle()
        assertEquals(1, writes.size)
    }

    @Test
    fun an_account_switch_writes_the_first_account_before_binding_the_second() {
        persistence.start(ACCOUNT)
        settle()
        versions.value = 1L
        persistence.start(OTHER)
        assertEquals(listOf(ACCOUNT to "v0", ACCOUNT to "v1"), writes)

        versions.value = 2L
        settle()
        assertEquals(OTHER to "v2", writes.last())
        assertTrue(writes.dropLast(1).all { it.first == ACCOUNT })
    }

    @Test
    fun a_failed_write_is_retried_by_the_stop() {
        var fail = true
        val failing = SyncStatePersistence(
            scope = scope,
            versions = versions,
            debounceMs = DEBOUNCE,
            encode = { "v${versions.value}".toByteArray() },
            write = { account, bytes ->
                if (fail) error("disk full")
                writes += account to String(bytes)
            },
            onChanged = {},
        )
        failing.start(ACCOUNT)
        versions.value = 1L
        settle()
        assertTrue(writes.isEmpty())
        fail = false
        failing.stopAndFlush()
        assertEquals(listOf(ACCOUNT to "v1"), writes)
    }

    @Test
    fun write_now_writes_without_waiting_for_the_debounce() {
        // An invite link's record must be on disk before the link is handed
        // out (2026-10-22): a process ended within the debounce forgot it.
        persistence.start(ACCOUNT)
        settle()
        writes.clear()
        versions.value = 1L
        scope.runCurrent()
        assertTrue(persistence.writeNow())
        assertEquals(listOf(ACCOUNT to "v1"), writes)
    }

    @Test
    fun write_now_says_when_nothing_was_written() {
        assertFalse(persistence.writeNow()) // nothing bound
        val failing = SyncStatePersistence(
            scope = scope,
            versions = versions,
            debounceMs = DEBOUNCE,
            encode = { "v${versions.value}".toByteArray() },
            write = { _, _ -> error("disk full") },
            onChanged = {},
        )
        failing.start(ACCOUNT)
        assertFalse(failing.writeNow())
        failing.stopAndFlush()
    }

    private companion object {
        const val DEBOUNCE = 500L
        const val ACCOUNT = "account-A"
        const val OTHER = "account-B"
    }
}
