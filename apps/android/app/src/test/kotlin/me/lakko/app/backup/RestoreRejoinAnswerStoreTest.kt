// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `docs/ux/identity-recovery.md` 7.2: the rejoin's answer is kept per account
 * on the device, so a restart shows the same state, and goes with the
 * restored marker and with the account's local data.
 */
@RunWith(RobolectricTestRunner::class)
class RestoreRejoinAnswerStoreTest {

    private lateinit var context: Context

    @Before
    fun setUp() {
        context = ApplicationProvider.getApplicationContext()
        BackupRestoreStore.clear(context, "acct")
        BackupRestoreStore.clear(context, "other")
    }

    @Test
    fun `nothing is recorded until the rejoin decides`() {
        assertNull(BackupRestoreStore.rejoinAnswer(context, "acct"))
    }

    @Test
    fun `the answer is kept per account and the latest decision wins`() {
        BackupRestoreStore.recordRejoinAnswer(context, "acct", RestoreRejoinAnswer.ANSWERED)
        assertEquals(RestoreRejoinAnswer.ANSWERED, BackupRestoreStore.rejoinAnswer(context, "acct"))
        assertNull(BackupRestoreStore.rejoinAnswer(context, "other"))
        BackupRestoreStore.recordRejoinAnswer(context, "acct", RestoreRejoinAnswer.FOUNDED)
        assertEquals(RestoreRejoinAnswer.FOUNDED, BackupRestoreStore.rejoinAnswer(context, "acct"))
    }

    @Test
    fun `the first conversation clears it with the restored marker`() {
        BackupRestoreStore.markRestoredChats(context, "acct", knewConversations = true)
        BackupRestoreStore.recordRejoinAnswer(context, "acct", RestoreRejoinAnswer.FOUNDED)
        BackupRestoreStore.clearRestoredChats(context, "acct")
        assertNull(BackupRestoreStore.restoredChats(context, "acct"))
        assertNull(BackupRestoreStore.rejoinAnswer(context, "acct"))
    }

    @Test
    fun `wiping the account clears it`() {
        BackupRestoreStore.recordRejoinAnswer(context, "acct", RestoreRejoinAnswer.ANSWERED)
        BackupRestoreStore.recordRejoinAnswer(context, "other", RestoreRejoinAnswer.FOUNDED)
        BackupRestoreStore.clear(context, "acct")
        assertNull(BackupRestoreStore.rejoinAnswer(context, "acct"))
        assertEquals(RestoreRejoinAnswer.FOUNDED, BackupRestoreStore.rejoinAnswer(context, "other"))
    }

    @Test
    fun `the flow starts from what is recorded`() = runTest {
        BackupRestoreStore.recordRejoinAnswer(context, "acct", RestoreRejoinAnswer.ANSWERED)
        assertEquals(RestoreRejoinAnswer.ANSWERED, BackupRestoreStore.rejoinAnswerFlow(context, "acct").first())
    }

    @Test
    fun `a value this build did not write reads as undecided`() {
        assertNull(RestoreRejoinAnswer.fromStored("maybe"))
        assertNull(RestoreRejoinAnswer.fromStored(null))
    }
}
