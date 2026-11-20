// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import java.io.File
import me.lakko.app.account.AccountLocalWipe
import me.lakko.app.account.AccountStoreGate
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * Forget device and Delete account remove the person's own background
 * photos with the rest of the account (ADR-0089 section 5): the
 * `call-background-photos` step of [AccountLocalWipe], run on the device's
 * real no-backup directory, takes the account's photos and leaves another
 * account's.
 */
@RunWith(RobolectricTestRunner::class)
class BackgroundPhotoWipeTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val other = "EBESExQVFhcYGRobHB0eHw=="

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AccountStoreGate.resetForTest()
    }

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun photoFile(acct: String, id: String): File =
        File(
            File(File(context.noBackupFilesDir, BackgroundPhotoStore.DIR), BackgroundPhotoStore.accountDirName(acct)),
            "$id.bin",
        ).apply {
            parentFile?.mkdirs()
            writeBytes(byteArrayOf(1, 2, 3))
        }

    @Test
    fun `the wipe step is part of the full and the staged wipe`() {
        assertTrue(AccountLocalWipe.steps.any { it.name == "call-background-photos" })
        assertTrue(AccountLocalWipe.stagedSteps.any { it.name == "call-background-photos" })
    }

    @Test
    fun `the wipe removes the account's photos and keeps the other account's`() {
        val mine = photoFile(account, "t1_aa")
        val theirs = photoFile(other, "t2_bb")
        val failed = AccountLocalWipe.runSteps(
            context,
            account,
            AccountLocalWipe.steps.filter { it.name == "call-background-photos" },
        )
        assertEquals(emptyList<String>(), failed)
        assertFalse(mine.exists())
        assertFalse(mine.parentFile!!.exists())
        assertTrue(theirs.exists())
    }
}
