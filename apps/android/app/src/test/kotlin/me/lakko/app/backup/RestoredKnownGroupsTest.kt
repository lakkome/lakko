// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import kotlinx.coroutines.test.runTest
import me.lakko.app.account.AccountLocalWipe
import me.lakko.app.auth.AuthRepository
import me.lakko.app.contacts.AcceptedContactsStore
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
 * A backup always lists the account's conversations ([RestoredKnownGroups],
 * [BackupRestoreStore]): the ids a restored backup named ride every later
 * upload from this device until it speaks for those groups itself, so the
 * first upload after a restore no longer erases the list.
 */
@RunWith(RobolectricTestRunner::class)
class RestoredKnownGroupsTest {

    private lateinit var context: Context

    private val a = "CgoKCgoKCgoKCgoKCgoKCg=="
    private val b = "CwsLCwsLCwsLCwsLCwsLCw=="
    private val g1 = "11".repeat(32)
    private val g2 = "22".repeat(32)
    private val own = "aa".repeat(32)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    // ---- the rule ----------------------------------------------------------

    @Test
    fun `an upload carries the held groups and the restored ones not joined yet`() {
        assertEquals(
            listOf(own, g1, g2),
            RestoredKnownGroups.backupGroupIds(heldIdsHex = listOf(own), restoredIdsHex = listOf(g1, g2)),
        )
        // No duplicates, whatever the case the two halves are written in.
        assertEquals(
            listOf(own, g2),
            RestoredKnownGroups.backupGroupIds(heldIdsHex = listOf(own.uppercase()), restoredIdsHex = listOf(own, g2)),
        )
        assertEquals(emptyList<String>(), RestoredKnownGroups.backupGroupIds(emptyList(), emptyList()))
    }

    @Test
    fun `a restore records only the groups this device does not hold`() {
        assertEquals(
            setOf(g1),
            RestoredKnownGroups.afterRestore(current = emptySet(), namedIdsHex = listOf(g1, own), heldIdsHex = listOf(own)),
        )
        // Additive: a second backup naming less does not drop what the first named.
        assertEquals(
            setOf(g1, g2),
            RestoredKnownGroups.afterRestore(current = setOf(g1), namedIdsHex = listOf(g2), heldIdsHex = emptyList()),
        )
    }

    // ---- the store ---------------------------------------------------------

    @Test
    fun `the restored groups are per account`() {
        BackupRestoreStore.addRestoredGroups(context, a, listOf(g1, g2), heldIdsHex = emptyList())

        assertEquals(setOf(g1, g2), BackupRestoreStore.restoredGroups(context, a))
        assertEquals(emptySet<String>(), BackupRestoreStore.restoredGroups(context, b))
    }

    @Test
    fun `joining a group, deleting its chat or its ending drops it from the restored set`() {
        BackupRestoreStore.addRestoredGroups(context, a, listOf(g1, g2), heldIdsHex = emptyList())

        // The one call all three make (ChatRepository.dropRestoredKnownGroupBestEffort).
        BackupRestoreStore.removeRestoredGroup(context, a, g1)

        assertEquals(setOf(g2), BackupRestoreStore.restoredGroups(context, a))
        // Absent: a no-op.
        BackupRestoreStore.removeRestoredGroup(context, a, g1)
        assertEquals(setOf(g2), BackupRestoreStore.restoredGroups(context, a))
    }

    @Test
    fun `a group joined by a route that did not say so leaves the restored set at the next upload`() {
        BackupRestoreStore.addRestoredGroups(context, a, listOf(g1, g2), heldIdsHex = emptyList())

        assertEquals(listOf(own, g1, g2), BackupRestoreStore.backupGroupIds(context, a, heldIdsHex = listOf(own)))
        assertEquals(setOf(g1, g2), BackupRestoreStore.restoredGroups(context, a))

        // G1 is held now: still listed (once), and no longer "restored".
        assertEquals(listOf(own, g1, g2), BackupRestoreStore.backupGroupIds(context, a, heldIdsHex = listOf(own, g1)))
        assertEquals(setOf(g2), BackupRestoreStore.restoredGroups(context, a))
    }

    @Test
    fun `the pending flag round-trips per account`() {
        assertFalse(BackupRestoreStore.retryPending(context, a))
        BackupRestoreStore.setRetryPending(context, a, true)
        assertTrue(BackupRestoreStore.retryPending(context, a))
        assertFalse(BackupRestoreStore.retryPending(context, b))
        BackupRestoreStore.setRetryPending(context, a, false)
        assertFalse(BackupRestoreStore.retryPending(context, a))
    }

    // ---- wipe / log out ----------------------------------------------------

    @Test
    fun `the account wipe clears the store for that account only`() {
        for (acct in listOf(a, b)) {
            BackupRestoreStore.addRestoredGroups(context, acct, listOf(g1, g2), heldIdsHex = emptyList())
            BackupRestoreStore.setRetryPending(context, acct, true)
        }

        // The production step list (Forget device and Delete account both run it).
        AccountLocalWipe.runSteps(context, a, AccountLocalWipe.steps.filter { it.name == "backup-restore" })

        assertEquals(1, AccountLocalWipe.steps.count { it.name == "backup-restore" })
        assertEquals(emptySet<String>(), BackupRestoreStore.restoredGroups(context, a))
        assertFalse(BackupRestoreStore.retryPending(context, a))
        assertEquals(setOf(g1, g2), BackupRestoreStore.restoredGroups(context, b))
        assertTrue(BackupRestoreStore.retryPending(context, b))
    }

    @Test
    fun `log out keeps the store for the account's next sign-in`() {
        BackupRestoreStore.addRestoredGroups(context, a, listOf(g1, g2), heldIdsHex = emptyList())
        BackupRestoreStore.setRetryPending(context, a, true)

        AuthRepository(context).logout()

        assertEquals(setOf(g1, g2), BackupRestoreStore.restoredGroups(context, a))
        assertTrue(BackupRestoreStore.retryPending(context, a))
    }

    // ---- the upload this device makes after a restore -----------------------

    /** Records each upload the driver asks for. */
    private class RecordingUploader : AutoBackupDriver.Uploader {
        val requests = mutableListOf<AutoBackupDriver.UploadRequest>()
        override suspend fun upload(request: AutoBackupDriver.UploadRequest): UploadResult {
            requests += request
            return UploadResult.Uploaded
        }
    }

    private fun driver(
        scope: kotlinx.coroutines.CoroutineScope,
        uploader: AutoBackupDriver.Uploader,
        ownPredecessors: (String) -> List<uniffi.lakko_ffi_uniffi.BackupOwnPredecessor> = { emptyList() },
        personal: (String) -> BackupPersonal = { BackupPersonal.NONE },
        held: () -> List<String>?,
    ) = AutoBackupDriver(
        applicationContext = context,
        scope = scope,
        inputs = AutoBackupDriver.Inputs(
            accountId = { ByteArray(16) { 0x0A } },
            accountIdB64 = { a },
            sessionToken = { "token" },
            accountIdentitySeed = { ByteArray(64) },
            knownGroupIds = held,
            syncedState = { byteArrayOf(1) },
            ownPredecessors = ownPredecessors,
            personal = personal,
        ),
        uploader = uploader,
    )

    @Test
    fun `a change of name, photo or setting gets a backup out, carrying them`() = runTest {
        val uploader = RecordingUploader()
        var personal = BackupPersonal(byteArrayOf(1), byteArrayOf(2))
        val d = driver(this, uploader, personal = { personal }) { listOf(own) }
        assertEquals(AutoBackupDriver.Outcome.UPLOADED, d.runOnce(nowMs = 1_000))
        assertEquals(personal, uploader.requests.last().personal)
        // Nothing moved: nothing goes out.
        assertEquals(AutoBackupDriver.Outcome.CLEAN, d.runOnce(nowMs = 1_000 + AutoBackup.MIN_INTERVAL_MS + 1))
        personal = BackupPersonal(byteArrayOf(9), byteArrayOf(2))
        assertEquals(
            AutoBackupDriver.Outcome.UPLOADED,
            d.runOnce(nowMs = 2_000 + AutoBackup.MIN_INTERVAL_MS),
        )
        assertEquals(personal, uploader.requests.last().personal)
    }

    @Test
    fun `every upload carries the account's own predecessors and zeroes them after`() = runTest {
        AutoBackupStore.markDirty(context, a, 1)
        val row = uniffi.lakko_ffi_uniffi.BackupOwnPredecessor(ByteArray(64) { 5 }, 1_000uL)
        var carried: Byte? = null
        val uploader = AutoBackupDriver.Uploader { request ->
            carried = request.ownPredecessors.single().seed[0]
            UploadResult.Uploaded
        }
        val outcome = driver(this, uploader, ownPredecessors = { listOf(row) }) { listOf(own) }.runOnce(nowMs = 1_000)
        assertEquals(AutoBackupDriver.Outcome.UPLOADED, outcome)
        assertEquals(5.toByte(), carried)
        assertTrue(row.seed.all { it == 0.toByte() })
    }

    @Test
    fun `a predecessor store that cannot be read fails the upload rather than leaving them out`() = runTest {
        AutoBackupStore.markDirty(context, a, 1)
        val uploader = RecordingUploader()
        val driver = driver(this, uploader, ownPredecessors = { throw IllegalStateException("store") }) { listOf(own) }
        assertEquals(AutoBackupDriver.Outcome.FAILED, driver.runOnce(nowMs = 1_000))
        assertTrue(uploader.requests.isEmpty())
        assertTrue("the change is not lost", AutoBackupStore.load(context, a).dirtySinceMs != null)
    }

    @Test
    fun `the upload after a restore carries the restored groups plus the device's own`() = runTest {
        // A restore whose backup named G1 and G2, on a device holding one group.
        BackupRestoreStore.addRestoredGroups(context, a, listOf(g1, g2), heldIdsHex = listOf(own))
        val uploader = RecordingUploader()
        var held = listOf(own)
        val driver = driver(this, uploader) { held }

        assertEquals(AutoBackupDriver.Outcome.UPLOADED, driver.runOnce(nowMs = 1_000))
        assertEquals(listOf(own, g1, g2), uploader.requests.single().knownGroupIdsHex)

        // The device is admitted to G1: same set, so nothing new to upload,
        // and G1 has left the restored set.
        held = listOf(own, g1)
        assertEquals(AutoBackupDriver.Outcome.CLEAN, driver.runOnce(nowMs = 2_000))
        assertEquals(setOf(g2), BackupRestoreStore.restoredGroups(context, a))

        // The person deletes the G1 chat (the chat layer drops it from both
        // lists): the set changed, and the next upload no longer names G1.
        held = listOf(own)
        BackupRestoreStore.removeRestoredGroup(context, a, g1)
        assertEquals(
            AutoBackupDriver.Outcome.UPLOADED,
            driver.runOnce(nowMs = 2_000 + AutoBackup.MIN_INTERVAL_MS),
        )
        assertEquals(listOf(own, g2), uploader.requests.last().knownGroupIdsHex)
    }

    @Test
    fun `no upload before the chat layer has loaded its groups`() = runTest {
        BackupRestoreStore.addRestoredGroups(context, a, listOf(g1), heldIdsHex = emptyList())
        AutoBackupStore.markDirty(context, a, 1)
        val uploader = RecordingUploader()

        // `null` is "not loaded yet", which is not "holds none".
        assertEquals(AutoBackupDriver.Outcome.NOT_READY, driver(this, uploader) { null }.runOnce(nowMs = 1_000))

        assertTrue(uploader.requests.isEmpty())
        assertTrue("the change is not lost", AutoBackupStore.load(context, a).dirtySinceMs != null)
    }

    /** The production flag behind the restore's ports; the fetch is scripted. */
    private fun storePorts(answers: MutableList<GatedRestoreOutcome>) = object : BackupRestorePorts {
        override suspend fun fetch(
            confirmStaleOverride: suspend (RestoreReason) -> Boolean,
        ): GatedRestoreOutcome = answers.removeAt(0)
        override fun storeOwnPredecessors(
            predecessors: List<uniffi.lakko_ffi_uniffi.BackupOwnPredecessor>,
        ): OwnPredecessorsStored? = OwnPredecessorsStored(conversationKeyChanged = false)
        override suspend fun rebindConversationKey() = Unit
        override fun mergeContacts(
            contacts: List<uniffi.lakko_ffi_uniffi.BackupContactRecord>,
            createdAtMs: ULong,
        ): Int = 0
        override fun mergeSyncedState(bytes: ByteArray) = Unit
        override fun rememberRestoredGroups(groupIdsHex: List<String>) =
            BackupRestoreStore.addRestoredGroups(context, a, groupIdsHex, heldIdsHex = listOf(own))
        override fun applyOwnProfile(envelope: ByteArray) = Unit
        override fun applyVisibility(envelope: ByteArray) = Unit
        override fun noteEveryoneUnknown() = Unit
        override fun markRestored(knewConversations: Boolean) = Unit
        override fun markBackupDirty() = AutoBackupStore.markDirty(context, a, 1)
        override fun setRetryPending(pending: Boolean) = BackupRestoreStore.setRetryPending(context, a, pending)
        override fun retryPending(): Boolean = BackupRestoreStore.retryPending(context, a)
        override fun raiseRollbackAlert(reason: RestoreReason) = Unit
        override fun log(message: String, cause: Throwable?) = Unit
    }

    @Test
    fun `uploads stay held while the restore is pending, across failing landings`() = runTest {
        val offline = GatedRestoreOutcome.TransportFailed(java.io.IOException("offline"))
        val answers = mutableListOf<GatedRestoreOutcome>(
            offline,
            offline,
            GatedRestoreOutcome.Restored(
                uniffi.lakko_ffi_uniffi.ParsedBackupPayload(
                    accountId = ByteArray(16),
                    createdAtMs = 1uL,
                    ed25519Seed = ByteArray(32),
                    acceptedContacts = emptyList(),
                    knownGroupIds = listOf(ByteArray(32) { 0x11 }, ByteArray(32) { 0x22 }),
                    syncedState = ByteArray(0),
                    ownPredecessors = emptyList(),
                    ownProfile = ByteArray(0),
                    visibility = ByteArray(0),
                ),
            ),
        )
        val ports = storePorts(answers)
        val uploader = RecordingUploader()
        val driver = driver(this, uploader) { listOf(own) }

        // The phrase route: the fetch fails, the sign-in goes on.
        assertEquals(BackupRestoreOutcome.FETCH_FAILED, restoreBackupAndApply("recover", ports) { false })
        // An upload now would replace the backup the restore still has to read.
        assertEquals(AutoBackupDriver.Outcome.NOT_READY, driver.runOnce(nowMs = 1_000))

        // The next landing fails too: still pending, still held.
        assertEquals(BackupRestoreOutcome.FETCH_FAILED, retryPendingBackupRestore(ports))
        assertTrue(BackupRestoreStore.retryPending(context, a))
        assertEquals(AutoBackupDriver.Outcome.NOT_READY, driver.runOnce(nowMs = 2_000))
        assertTrue(uploader.requests.isEmpty())
        assertTrue("the change is not lost", AutoBackupStore.load(context, a).dirtySinceMs != null)

        // The landing after gets an answer: uploads resume, and the first one
        // lists what the restored backup named.
        assertEquals(BackupRestoreOutcome.APPLIED, retryPendingBackupRestore(ports))
        assertFalse(BackupRestoreStore.retryPending(context, a))
        assertEquals(AutoBackupDriver.Outcome.UPLOADED, driver.runOnce(nowMs = 3_000))
        assertEquals(listOf(own, g1, g2), uploader.requests.single().knownGroupIdsHex)
    }
}
