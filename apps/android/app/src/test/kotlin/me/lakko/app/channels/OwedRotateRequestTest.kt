// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * ADR-0087 section 15 (2026-11-08) — a co-publisher's rotate request is owed
 * on disk before it is published, survives the app being killed, is retried
 * until it publishes, cleared on success, and goes with the account wipe.
 */
@RunWith(RobolectricTestRunner::class)
class OwedRotateRequestTest {

    private val context: Context = ApplicationProvider.getApplicationContext()

    @Before
    fun clean() {
        PrefsOwedMarks.deleteAccount(context, "A")
        PrefsOwedMarks.deleteAccount(context, "B")
    }

    @Test
    fun a_failed_send_stays_owed_across_a_restart_and_clears_once_it_publishes() = runBlocking {
        val first = PrefsOwedRotateRequests(context) { "A" }
        assertFalse(reissueRecordOwed(first, "chan") { false })
        // The process dies; a new loop reads the same file.
        val afterRestart = PrefsOwedRotateRequests(context) { "A" }
        assertTrue(afterRestart.isOwed("chan"))
        assertTrue(reissueRecordOwed(afterRestart, "chan") { true })
        assertFalse(PrefsOwedRotateRequests(context) { "A" }.isOwed("chan"))
    }

    @Test
    fun the_mark_is_on_disk_before_the_attempt() = runBlocking {
        val marks = PrefsOwedRotateRequests(context) { "A" }
        var seenByAnotherReader = false
        reissueRecordOwed(marks, "chan") {
            seenByAnotherReader = PrefsOwedRotateRequests(context) { "A" }.isOwed("chan")
            true
        }
        assertTrue(seenByAnotherReader)
    }

    @Test
    fun marks_are_per_account_and_per_kind() {
        PrefsOwedRotateRequests(context) { "A" }.mark("chan")
        assertFalse(PrefsOwedRotateRequests(context) { "B" }.isOwed("chan"))
        assertFalse(PrefsOwedRecordMarks(context) { "A" }.isOwed("chan"))
        assertFalse("no account bound records nothing", PrefsOwedRotateRequests(context) { null }.isOwed("chan"))
    }

    @Test
    fun the_account_wipe_deletes_both_kinds_for_that_account_only() {
        PrefsOwedRotateRequests(context) { "A" }.mark("chan")
        PrefsOwedRecordMarks(context) { "A" }.mark("chan")
        PrefsOwedRotateRequests(context) { "B" }.mark("chan")
        PrefsOwedMarks.deleteAccount(context, "A")
        assertFalse(PrefsOwedRotateRequests(context) { "A" }.isOwed("chan"))
        assertFalse(PrefsOwedRecordMarks(context) { "A" }.isOwed("chan"))
        assertTrue(PrefsOwedRotateRequests(context) { "B" }.isOwed("chan"))
    }
}
