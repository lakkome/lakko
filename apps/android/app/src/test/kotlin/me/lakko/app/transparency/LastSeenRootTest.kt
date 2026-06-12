// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the KT-log S3e no-rollback gate
 * ([LastSeenRootStore.checkAndAdvance]).
 *
 * Run as `:app:testDebugUnitTest` — no emulator, no native lib. The
 * decision logic lives behind the [LastSeenRootStore] interface, so the
 * in-memory [InMemoryLastSeenRoot] exercises exactly the same monotonic
 * semantics the production [LastSeenRoot] (EncryptedSharedPreferences)
 * implements. Mirror of the web `last-seen-root` test.
 */
class LastSeenRootTest {

    @Test
    fun `loadLastSeenLogSize defaults to zero`() {
        assertEquals(0L, InMemoryLastSeenRoot().loadLastSeenLogSize())
    }

    @Test
    fun `first observation advances from zero and persists`() {
        val store = InMemoryLastSeenRoot()
        assertTrue(store.checkAndAdvance(5L))
        assertEquals(5L, store.loadLastSeenLogSize())
    }

    @Test
    fun `growth advances the high-water mark`() {
        val store = InMemoryLastSeenRoot()
        store.checkAndAdvance(5L)
        assertTrue(store.checkAndAdvance(9L))
        assertEquals(9L, store.loadLastSeenLogSize())
    }

    @Test
    fun `equal log size is an idempotent ok without regressing`() {
        val store = InMemoryLastSeenRoot()
        store.checkAndAdvance(7L)
        assertTrue(store.checkAndAdvance(7L))
        assertEquals(7L, store.loadLastSeenLogSize())
    }

    @Test
    fun `a smaller log size is a rollback and never advances`() {
        val store = InMemoryLastSeenRoot()
        store.checkAndAdvance(10L)
        // The log appears to have shrunk — refuse.
        assertFalse(store.checkAndAdvance(9L))
        // The stored value is untouched (no regress, no advance).
        assertEquals(10L, store.loadLastSeenLogSize())
    }

    @Test
    fun `rollback then a fresh growth still works against the preserved mark`() {
        val store = InMemoryLastSeenRoot()
        store.checkAndAdvance(10L)
        assertFalse(store.checkAndAdvance(8L)) // rollback, mark stays 10
        // A subsequent legitimate growth past the preserved mark is OK.
        assertTrue(store.checkAndAdvance(11L))
        assertEquals(11L, store.loadLastSeenLogSize())
    }

    @Test
    fun `clear resets the mark to zero`() {
        val store = InMemoryLastSeenRoot()
        store.checkAndAdvance(42L)
        store.clear()
        assertEquals(0L, store.loadLastSeenLogSize())
        // After a clear, any positive size is accepted again.
        assertTrue(store.checkAndAdvance(1L))
    }
}
