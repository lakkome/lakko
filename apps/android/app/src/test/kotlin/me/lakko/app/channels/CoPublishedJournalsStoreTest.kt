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
 * The mark belongs to the account that asked to co-publish (2026-09-30).
 */
@RunWith(RobolectricTestRunner::class)
class CoPublishedJournalsStoreTest {

    private val a = "CgoKCgoKCgoKCgoKCgoKCg=="
    private val b = "CwsLCwsLCwsLCwsLCwsLCw=="

    @Test
    fun `a mark survives a new store instance`() {
        val context = RuntimeEnvironment.getApplication()
        val hex = "ab".repeat(32)
        val other = "cd".repeat(32)
        CoPublishedJournals.onDevice(context) { a }.mark(hex.uppercase())
        val reopened = CoPublishedJournals.onDevice(context) { a }
        assertTrue(reopened.contains(hex))
        assertFalse(reopened.contains(other))
        reopened.mark(other)
        assertTrue(CoPublishedJournals.onDevice(context) { a }.contains(hex))
        assertTrue(CoPublishedJournals.onDevice(context) { a }.contains(other))
    }

    @Test
    fun `a mark belongs to one account and forget drops only that account's`() {
        val context = RuntimeEnvironment.getApplication()
        val hex = "ab".repeat(32)
        CoPublishedJournals.onDevice(context) { a }.mark(hex)
        assertFalse(CoPublishedJournals.onDevice(context) { b }.contains(hex))
        assertFalse(CoPublishedJournals.onDevice(context) { null }.contains(hex))
        CoPublishedJournals.onDevice(context) { null }.mark("ef".repeat(32))
        assertFalse(CoPublishedJournals.onDevice(context) { a }.contains("ef".repeat(32)))

        CoPublishedJournals.onDevice(context) { b }.mark(hex)
        CoPublishedJournals.onDevice(context) { a }.deleteAccount(a)
        assertFalse(CoPublishedJournals.onDevice(context) { a }.contains(hex))
        assertTrue(CoPublishedJournals.onDevice(context) { b }.contains(hex))
    }
}
