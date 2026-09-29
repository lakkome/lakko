// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0057 — the co-published mark must survive the process: a co-publisher
 * that restarts and opens the channel's Manage screen must still not claim.
 */
@RunWith(RobolectricTestRunner::class)
class CoPublishedJournalsStoreTest {

    @Test
    fun `a mark survives a new store instance`() {
        val context = RuntimeEnvironment.getApplication()
        val hex = "ab".repeat(32)
        val other = "cd".repeat(32)
        CoPublishedJournals.onDevice(context).mark(hex.uppercase())
        val reopened = CoPublishedJournals.onDevice(context)
        assertTrue(reopened.contains(hex))
        assertFalse(reopened.contains(other))
        reopened.mark(other)
        assertTrue(CoPublishedJournals.onDevice(context).contains(hex))
        assertTrue(CoPublishedJournals.onDevice(context).contains(other))
    }
}
