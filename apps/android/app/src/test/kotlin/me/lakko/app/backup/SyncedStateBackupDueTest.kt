// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.devices.sync.SyncStatePersistence
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * Walk of 2026-11-14, defect 1 — a change of the persisted synced state (a
 * channel created: its subscription register and admission seed; a folder,
 * a pin) makes the automatic backup due, through the one choke point the
 * synced-state writer calls ([markBackupDueForSyncedState]). Before: on web
 * nothing but contacts, membership and the own profile did, so a created
 * channel never reached the backup and a phrase restore did not know it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class SyncedStateBackupDueTest {

    private lateinit var context: Context
    private val a = "DQ0NDQ0NDQ0NDQ0NDQ0NDQ=="
    private val own = "ab".repeat(32)

    private val persistScope = TestScope(StandardTestDispatcher())
    private val versions = MutableStateFlow(0L)
    private var now = 1_000L

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    private fun settle() {
        persistScope.advanceTimeBy(DEBOUNCE * 2)
        persistScope.runCurrent()
    }

    private val uploads = mutableListOf<AutoBackupDriver.UploadRequest>()

    private fun driver(scope: kotlinx.coroutines.CoroutineScope) = AutoBackupDriver(
        applicationContext = context,
        scope = scope,
        inputs = AutoBackupDriver.Inputs(
            accountId = { ByteArray(16) { 0x0D } },
            accountIdB64 = { a },
            sessionToken = { "token" },
            accountIdentitySeed = { ByteArray(64) },
            knownGroupIds = { listOf(own) },
            syncedState = { "v${versions.value}".toByteArray() },
        ),
        uploader = { request ->
            uploads += request
            UploadResult.Uploaded
        },
    )

    /** The writer exactly as the repository wires it, but on a test clock. */
    private fun persistence() = SyncStatePersistence(
        scope = persistScope,
        versions = versions,
        debounceMs = DEBOUNCE,
        encode = { "v${versions.value}".toByteArray() },
        write = { _, _ -> },
        onChanged = { account -> markBackupDueForSyncedState(context, account, now) },
    )

    @Test
    fun `a synced-state change makes the backup due and the upload carries it`() = runTest {
        val d = driver(this)
        // First run: the baseline upload (membership is new on this device).
        assertEquals(AutoBackupDriver.Outcome.UPLOADED, d.runOnce(nowMs = now))
        val writer = persistence()
        writer.start(a)
        settle()
        // Writing back what bootstrap read is not a change.
        now += AutoBackup.MIN_INTERVAL_MS + 1
        assertEquals(AutoBackupDriver.Outcome.CLEAN, d.runOnce(nowMs = now))

        // A channel is created: its registers bump the synced-state version.
        versions.value = 1L
        settle()
        assertEquals(AutoBackupDriver.Outcome.UPLOADED, d.runOnce(nowMs = now + 1))
        assertEquals("v1", String(uploads.last().syncedState ?: ByteArray(0)))
        writer.stopAndFlush()
    }

    @Test
    fun `the minimum interval still bounds a synced-state change`() = runTest {
        val d = driver(this)
        assertEquals(AutoBackupDriver.Outcome.UPLOADED, d.runOnce(nowMs = now))
        val writer = persistence()
        writer.start(a)
        settle()
        versions.value = 1L
        settle()
        assertEquals(AutoBackupDriver.Outcome.THROTTLED, d.runOnce(nowMs = now + 1))
        assertEquals(
            AutoBackupDriver.Outcome.UPLOADED,
            d.runOnce(nowMs = now + AutoBackup.MIN_INTERVAL_MS + 1),
        )
        writer.stopAndFlush()
    }

    private companion object {
        const val DEBOUNCE = 500L
    }
}
