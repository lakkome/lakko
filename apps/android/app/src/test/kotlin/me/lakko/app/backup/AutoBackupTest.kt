// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The rules the automatic backup turns on. Mirror of the web
 * `backup/auto-backup.test.ts` and the iOS `AutoBackupTests.swift`.
 *
 * Pure rules only: the store needs a `Context`, so its round-trip is covered
 * by the instrumented suite rather than faked here.
 */
class AutoBackupTest {

    @Test
    fun `clean while nothing has changed`() {
        val state = AutoBackup.State(lastUploadAtMs = 1000, dirtySinceMs = null)
        assertEquals(AutoBackup.Verdict.CLEAN, AutoBackup.verdict(state, nowMs = 9_000_000_000))
    }

    @Test
    fun `due the first time, however recently the account was created`() {
        val state = AutoBackup.State(lastUploadAtMs = 0, dirtySinceMs = 10)
        assertEquals(AutoBackup.Verdict.DUE, AutoBackup.verdict(state, nowMs = 20))
    }

    @Test
    fun `throttled inside the interval`() {
        val state = AutoBackup.State(lastUploadAtMs = 1_000_000, dirtySinceMs = 1_000_001)
        assertEquals(
            AutoBackup.Verdict.THROTTLED,
            AutoBackup.verdict(state, nowMs = 1_000_000 + AutoBackup.MIN_INTERVAL_MS - 1),
        )
    }

    @Test
    fun `due once the interval has passed`() {
        val state = AutoBackup.State(lastUploadAtMs = 1_000_000, dirtySinceMs = 1_000_001)
        assertEquals(
            AutoBackup.Verdict.DUE,
            AutoBackup.verdict(state, nowMs = 1_000_000 + AutoBackup.MIN_INTERVAL_MS),
        )
    }

    @Test
    fun `a backwards clock is due rather than wedged off forever`() {
        // A `lastUploadAtMs` in the future makes every later `now - last`
        // negative, so a naive comparison would suppress backups permanently.
        val state = AutoBackup.State(lastUploadAtMs = 9_000_000, dirtySinceMs = 1_000)
        assertEquals(AutoBackup.Verdict.DUE, AutoBackup.verdict(state, nowMs = 1_000_000))
    }

    @Test
    fun `markedDirty records when it first became worth backing up`() {
        assertEquals(500L, AutoBackup.markedDirty(AutoBackup.State.CLEAN, nowMs = 500).dirtySinceMs)
    }

    @Test
    fun `markedDirty does not push the clock forward on every later change`() {
        // Otherwise a person editing several things in a row keeps resetting
        // the moment the change happened, and the throttle is never met.
        val first = AutoBackup.markedDirty(AutoBackup.State.CLEAN, nowMs = 500)
        assertEquals(first, AutoBackup.markedDirty(first, nowMs = 900))
    }

    @Test
    fun `markedUploaded restarts the clock and keeps the group key`() {
        val state = AutoBackup.State(lastUploadAtMs = 1, dirtySinceMs = 2, lastGroupsKey = "g1")
        val next = AutoBackup.markedUploaded(state, nowMs = 777)
        assertEquals(777L, next.lastUploadAtMs)
        assertNull(next.dirtySinceMs)
        assertEquals("g1", next.lastGroupsKey)
    }

    @Test
    fun `the group key ignores order, which the repository does not guarantee`() {
        assertEquals(
            AutoBackup.groupsKey(listOf("g1", "g2")),
            AutoBackup.groupsKey(listOf("g2", "g1")),
        )
    }

    @Test
    fun `a join and a departure both change the group key`() {
        val one = AutoBackup.groupsKey(listOf("g1"))
        assertNotEquals(one, AutoBackup.groupsKey(listOf("g1", "g2")))
        assertNotEquals(one, AutoBackup.groupsKey(listOf("g2")))
    }
}
