// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** `docs/ux/feed.md` state table, "refresh failed (network)": the app-wide,
 *  per-account holder every check reports into. */
class FeedRefreshStatusTest {
    private val alice = "alice-account"
    private val bob = "bob-account"
    private val nothing = SourceCheck(tried = 3, answered = 0)
    private val some = SourceCheck(tried = 3, answered = 1)

    private fun bound(account: String = alice) = FeedRefreshStatus().also { it.bind(account) }

    @Test
    fun startsClearAndFollowsTheFold() {
        val s = bound()
        assertEquals(FeedRefreshState(), s.state.value)
        assertFalse(s.failed)
        s.record(alice, CheckKind.GROUPS, nothing)
        assertTrue(s.failed)
        // A check that tried nothing leaves it as it was.
        s.record(alice, CheckKind.CHANNELS, SourceCheck.NONE)
        assertTrue(s.failed)
        // The next check that reaches anything clears it.
        s.record(alice, CheckKind.CHANNELS, some)
        assertFalse(s.failed)
        assertEquals(FeedRefreshState(CheckOutcome.NOTHING, CheckOutcome.REACHED), s.state.value)
    }

    @Test
    fun aReportForAnotherAccountOrNoAccountIsDropped() {
        val s = bound()
        s.record(bob, CheckKind.GROUPS, nothing)
        s.record(null, CheckKind.GROUPS, nothing)
        assertEquals(FeedRefreshState(), s.state.value)
        // Unbound: nothing is kept.
        val fresh = FeedRefreshStatus()
        fresh.record(alice, CheckKind.GROUPS, nothing)
        assertFalse(fresh.failed)
    }

    @Test
    fun anAccountSwitchAndAResetStartFromNothing() {
        val s = bound()
        s.record(alice, CheckKind.GROUPS, nothing)
        s.bind(alice)
        assertTrue("re-binding the same account keeps the state", s.failed)
        s.bind(bob)
        assertFalse(s.failed)
        s.record(bob, CheckKind.GROUPS, nothing)
        assertTrue(s.failed)
        s.reset()
        assertFalse(s.failed)
        // After sign-out a late report from the old session is dropped.
        s.record(bob, CheckKind.GROUPS, nothing)
        assertFalse(s.failed)
    }

    @Test
    fun aBackgroundSweepReportChangesTheState() = runTest {
        val s = bound()
        val returned = s.reporting(alice, CheckKind.GROUPS) { nothing }
        assertEquals(nothing, returned)
        assertTrue(s.failed)
        s.reporting(alice, CheckKind.CHANNELS) { some }
        assertFalse(s.failed)
    }

    @Test
    fun aBackgroundCheckThatThrowsCountsAsNotAnsweredAndRethrows() = runTest {
        val s = bound()
        try {
            s.reporting(alice, CheckKind.GROUPS) { throw IllegalStateException("network down") }
            fail("expected the failure to be rethrown")
        } catch (_: IllegalStateException) {
        }
        assertEquals(CheckOutcome.NOTHING, s.state.value.groups)
        assertTrue(s.failed)
    }

    @Test
    fun aCancelledCheckRecordsNothing() = runTest {
        val s = bound()
        try {
            s.reporting(alice, CheckKind.GROUPS) { throw CancellationException("stopped") }
            fail("expected the cancellation to be rethrown")
        } catch (_: CancellationException) {
        }
        assertEquals(FeedRefreshState(), s.state.value)
    }
}
