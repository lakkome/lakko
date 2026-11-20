// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import java.io.File
import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.BackupContactRecord
import uniffi.lakko_ffi_uniffi.BackupOwnPredecessor
import uniffi.lakko_ffi_uniffi.ParsedBackupPayload

/**
 * The one backup restore every phrase route runs ([restoreBackupAndApply]) and
 * the attempt a later landing makes at a restore still pending
 * ([retryPendingBackupRestore]).
 *
 * Driven through [BackupRestorePorts] on the host JVM: `AuthRepository.recover`
 * and `AuthRepository.provideRecoveryPhrase` themselves derive the seed through
 * the native core, which this JVM does not load, so the routes are covered by
 * the rule they both call plus the structural checks at the end of this file.
 */
class BackupRestoreTest {

    private val g1 = "11".repeat(32)
    private val g2 = "22".repeat(32)

    private fun contact(byte: Byte, name: String) = BackupContactRecord(
        ed25519Pubkey = ByteArray(32) { byte },
        x25519Pubkey = ByteArray(32),
        displayName = name,
        accountId = ByteArray(16) { byte },
    )

    private fun payload(
        contacts: List<BackupContactRecord> = listOf(contact(1, "Alice"), contact(2, "Bob")),
        groups: List<String> = listOf(g1, g2),
        synced: ByteArray = byteArrayOf(7, 7, 7),
        predecessors: List<BackupOwnPredecessor> = emptyList(),
        ownProfile: ByteArray = ByteArray(0),
        visibility: ByteArray = ByteArray(0),
    ) = ParsedBackupPayload(
        accountId = ByteArray(16) { 9 },
        createdAtMs = 1_700_000_000_000uL,
        ed25519Seed = ByteArray(32),
        acceptedContacts = contacts,
        knownGroupIds = groups.map { hex -> ByteArray(32) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() } },
        syncedState = synced,
        ownPredecessors = predecessors,
        ownProfile = ownProfile,
        visibility = visibility,
    )

    private fun predecessor(tag: Int) = BackupOwnPredecessor(seed = ByteArray(64) { tag.toByte() }, adoptedAtMs = tag.toULong() * 1_000uL)

    /** Records everything the restore did. The fetch answers from a script. */
    private class FakePorts(private val script: MutableList<() -> GatedRestoreOutcome>) : BackupRestorePorts {
        var fetches = 0
        val staleAnswers = mutableListOf<Boolean>()
        val mergedContacts = mutableListOf<List<BackupContactRecord>>()
        val mergedSyncedState = mutableListOf<ByteArray>()
        val rememberedGroups = mutableListOf<List<String>>()
        var dirtyMarks = 0
        var retry = false
        val alerts = mutableListOf<RestoreReason>()
        val logs = mutableListOf<String>()
        var failContactMerge = false

        /** The steps that touch the keys, in order. */
        val steps = mutableListOf<String>()
        val storedSeeds = mutableListOf<ByteArray>()
        var storeAnswer: OwnPredecessorsStored? = OwnPredecessorsStored(conversationKeyChanged = true)
        var rebindFails = false

        /** A stale head: the fetch asks, and refuses when the answer is no. */
        var staleReason: RestoreReason? = null

        override suspend fun fetch(
            confirmStaleOverride: suspend (RestoreReason) -> Boolean,
        ): GatedRestoreOutcome {
            fetches += 1
            staleReason?.let { reason ->
                val accepted = confirmStaleOverride(reason)
                staleAnswers += accepted
                if (!accepted) return GatedRestoreOutcome.RollbackBlocked(hard = false, reason = reason)
            }
            return script.removeAt(0)()
        }

        override fun storeOwnPredecessors(predecessors: List<BackupOwnPredecessor>): OwnPredecessorsStored? {
            steps += "store"
            predecessors.forEach { storedSeeds += it.seed.copyOf() }
            return storeAnswer
        }

        override suspend fun rebindConversationKey() {
            steps += "rebind"
            if (rebindFails) throw IllegalStateException("chat layer busy")
        }

        override fun mergeContacts(contacts: List<BackupContactRecord>, createdAtMs: ULong): Int {
            if (failContactMerge) throw IllegalStateException("store unavailable")
            steps += "contacts"
            mergedContacts += contacts
            return contacts.size
        }

        override fun mergeSyncedState(bytes: ByteArray) {
            mergedSyncedState += bytes
        }

        override fun rememberRestoredGroups(groupIdsHex: List<String>) {
            rememberedGroups += groupIdsHex
        }

        val appliedProfiles = mutableListOf<ByteArray>()
        val appliedVisibilities = mutableListOf<ByteArray>()
        var everyoneUnknownNotes = 0
        val restoredMarks = mutableListOf<Boolean>()
        var failOwnProfile = false

        override fun applyOwnProfile(envelope: ByteArray) {
            if (failOwnProfile) throw IllegalStateException("does not decode")
            appliedProfiles += envelope
        }

        override fun applyVisibility(envelope: ByteArray) {
            appliedVisibilities += envelope
        }

        override fun noteEveryoneUnknown() {
            everyoneUnknownNotes += 1
        }

        override fun markRestored(knewConversations: Boolean) {
            restoredMarks += knewConversations
        }

        override fun markBackupDirty() {
            dirtyMarks += 1
        }

        /** Every write of the flag, in order, with the fetch count at the time. */
        val retryWrites = mutableListOf<Pair<Boolean, Int>>()

        override fun setRetryPending(pending: Boolean) {
            retry = pending
            retryWrites += pending to fetches
        }

        override fun retryPending(): Boolean = retry

        override fun raiseRollbackAlert(reason: RestoreReason) {
            alerts += reason
        }

        override fun log(message: String, cause: Throwable?) {
            logs += message
        }

        val applied: Boolean
            get() = mergedContacts.isNotEmpty() || mergedSyncedState.isNotEmpty() || rememberedGroups.isNotEmpty()
    }

    private fun ports(vararg outcomes: () -> GatedRestoreOutcome) = FakePorts(outcomes.toMutableList())

    private val neverAsked: suspend (RestoreReason) -> Boolean = { error("no stale head in this test") }

    // ---- ADR-0074 section 3.7: the own predecessors -----------------------

    @Test
    fun `a backup's own predecessors are stored first and the key moves last`() = runTest {
        val carried = listOf(predecessor(1), predecessor(2))
        val p = ports({ GatedRestoreOutcome.Restored(payload(predecessors = carried)) })

        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("recover", p, neverAsked))

        assertEquals(listOf("store", "contacts", "rebind"), p.steps)
        assertEquals(listOf<Byte>(1, 2), p.storedSeeds.map { it[0] })
        assertFalse(p.retry)
        // Nothing is left in memory, and no line carries a seed.
        assertTrue(carried.all { row -> row.seed.all { it == 0.toByte() } })
        assertTrue(p.logs.none { line -> "seed" in line.lowercase() })
    }

    // ---- G-56: the parked device's look at the backup ---------------------

    @Test
    fun `a parked device takes only the earlier keys from the backup`() = runTest {
        val carried = listOf(predecessor(1))
        val p = ports({ GatedRestoreOutcome.Restored(payload(predecessors = carried)) })
        assertTrue(fetchOwnPredecessorsOnly(p))
        assertEquals(listOf("store"), p.steps)
        assertFalse(p.applied)
        // The pending-restore flag is the landing's, not this look's.
        assertTrue(p.retryWrites.isEmpty())
        assertTrue(carried.all { row -> row.seed.all { it == 0.toByte() } })
    }

    @Test
    fun `a backup without earlier keys, a failed fetch or a stale head stores nothing`() = runTest {
        assertFalse(fetchOwnPredecessorsOnly(ports({ GatedRestoreOutcome.Restored(payload()) })))
        assertFalse(fetchOwnPredecessorsOnly(ports({ GatedRestoreOutcome.NoBackup })))
        assertFalse(fetchOwnPredecessorsOnly(ports({ throw java.io.IOException("offline") })))
        val stale = ports({ GatedRestoreOutcome.Restored(payload(predecessors = listOf(predecessor(1)))) })
        stale.staleReason = RestoreReason.values().first()
        assertFalse(fetchOwnPredecessorsOnly(stale))
        assertEquals(listOf(false), stale.staleAnswers)
        assertTrue(stale.steps.isEmpty())
    }

    @Test
    fun `an account that never rotated touches neither the store nor the binding`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload()) })
        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("recover", p, neverAsked))
        assertEquals(listOf("contacts"), p.steps)
    }

    @Test
    fun `keys this device already held change no binding`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload(predecessors = listOf(predecessor(1)))) })
        p.storeAnswer = OwnPredecessorsStored(conversationKeyChanged = false)
        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("retry", p, neverAsked))
        assertEquals(listOf("store", "contacts"), p.steps)
    }

    @Test
    fun `predecessors that could not be stored keep the restore pending and uploads held`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload(predecessors = listOf(predecessor(1)))) })
        p.storeAnswer = null

        assertEquals(BackupRestoreOutcome.FETCH_FAILED, restoreBackupAndApply("recover", p, neverAsked))

        // The rest of the backup still lands, and the next landing tries again.
        assertEquals(1, p.mergedContacts.size)
        assertTrue(p.retry)
        assertEquals(0, p.dirtyMarks)
        assertFalse("rebind" in p.steps)
    }

    @Test
    fun `a rebind that fails does not fail the restore`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload(predecessors = listOf(predecessor(1)))) })
        p.rebindFails = true
        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("recover", p, neverAsked))
        assertFalse(p.retry)
    }

    @Test
    fun `a backup carries the account's predecessors oldest first as copies`() {
        val rows = listOf(
            me.lakko.app.auth.OwnPredecessor(ByteArray(32) { 1 }, ByteArray(64) { 1 }, 1_000L),
            me.lakko.app.auth.OwnPredecessor(ByteArray(32) { 2 }, ByteArray(64) { 2 }, 2_000L),
        )
        val carried = ownPredecessorsForBackup(rows)
        assertEquals(listOf(1_000uL, 2_000uL), carried.map { it.adoptedAtMs })
        assertTrue(carried[0].seed.contentEquals(rows[0].phraseSeed))
        rows.forEach { it.zero() }
        assertEquals(1.toByte(), carried[0].seed[0])
    }

    // ---- A / B: what a restore applies ------------------------------------

    @Test
    fun `a restore applies contacts, the synced state and the known groups`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload()) })

        val outcome = restoreBackupAndApply("recover", p, neverAsked)

        assertEquals(BackupRestoreOutcome.APPLIED, outcome)
        assertEquals(listOf("Alice", "Bob"), p.mergedContacts.single().map { it.displayName })
        assertEquals(listOf(7, 7, 7), p.mergedSyncedState.single().map { it.toInt() })
        assertEquals(listOf(listOf(g1, g2)), p.rememberedGroups)
        assertEquals(1, p.dirtyMarks)
        assertFalse(p.retry)
    }

    @Test
    fun `an empty synced state is not merged`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload(synced = ByteArray(0))) })

        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("recover", p, neverAsked))

        assertTrue(p.mergedSyncedState.isEmpty())
        assertEquals(1, p.mergedContacts.size)
    }

    @Test
    fun `a part that fails does not cost the others`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload()) })
        p.failContactMerge = true

        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("recover", p, neverAsked))

        assertTrue(p.mergedContacts.isEmpty())
        assertEquals(1, p.mergedSyncedState.size)
        assertEquals(listOf(listOf(g1, g2)), p.rememberedGroups)
    }

    @Test
    fun `the restore has no way to load MLS state`() {
        // Structural: the ports are everything a restore can touch, and none
        // takes or returns provider state. A snapshot port would show up here.
        // (`substringBefore('-')`: a method taking a `ULong` is name-mangled.)
        val portNames = BackupRestorePorts::class.java.declaredMethods.map { it.name.substringBefore('-') }.toSet()
        assertEquals(
            setOf(
                "fetch", "storeOwnPredecessors", "rebindConversationKey", "mergeContacts", "mergeSyncedState",
                "rememberRestoredGroups", "markBackupDirty", "setRetryPending", "retryPending",
                "raiseRollbackAlert", "log", "applyOwnProfile", "applyVisibility", "noteEveryoneUnknown",
                "markRestored", "applyReceivedProfiles",
            ),
            portNames.filterNot { it.contains('$') }.toSet(),
        )
        // And the parsed payload carries no snapshot to apply.
        val payloadFields = ParsedBackupPayload::class.java.declaredFields.map { it.name }
        assertTrue(payloadFields.none { it.contains("snapshot", ignoreCase = true) })
    }

    // ---- C: the routes never fail the sign-in ------------------------------

    @Test
    fun `no backup on the server is not an error`() = runTest {
        val p = ports({ GatedRestoreOutcome.NoBackup })

        assertEquals(BackupRestoreOutcome.NO_BACKUP, restoreBackupAndApply("provideRecoveryPhrase", p, neverAsked))

        assertFalse(p.applied)
        assertFalse("a 404 is a definitive answer", p.retry)
    }

    @Test
    fun `the restore is marked pending before the fetch starts`() = runTest {
        // So a process killed mid-restore is retried at the next landing: the
        // flag is down before the first byte is asked for, and stays down for
        // as long as the fetch is out.
        val p = ports({ GatedRestoreOutcome.Restored(payload()) })
        var pendingDuringFetch: Boolean? = null
        val observing = object : BackupRestorePorts by p {
            override suspend fun fetch(
                confirmStaleOverride: suspend (RestoreReason) -> Boolean,
            ): GatedRestoreOutcome {
                pendingDuringFetch = p.retry
                return p.fetch(confirmStaleOverride)
            }
        }

        assertEquals(BackupRestoreOutcome.APPLIED, restoreBackupAndApply("recover", observing, neverAsked))

        assertEquals(true, pendingDuringFetch)
        assertEquals("set with no fetch made yet", true to 0, p.retryWrites.first())
        assertFalse(p.retry)
    }

    @Test
    fun `a restore cancelled mid-fetch stays pending`() = runTest {
        val p = ports({ throw kotlinx.coroutines.CancellationException("process going away") })

        var cancelled = false
        try {
            restoreBackupAndApply("provideRecoveryPhrase", p, neverAsked)
        } catch (_: kotlinx.coroutines.CancellationException) {
            cancelled = true
        }

        assertTrue(cancelled)
        assertTrue(p.retry)
    }

    @Test
    fun `a fetch failure is reported, not thrown, and stays pending`() = runTest {
        val p = ports({ GatedRestoreOutcome.TransportFailed(IOException("offline")) })

        assertEquals(BackupRestoreOutcome.FETCH_FAILED, restoreBackupAndApply("provideRecoveryPhrase", p, neverAsked))

        assertFalse(p.applied)
        assertTrue(p.retry)
    }

    @Test
    fun `a fetch that throws is a fetch failure too`() = runTest {
        val p = ports({ throw IOException("connection reset") })

        assertEquals(BackupRestoreOutcome.FETCH_FAILED, restoreBackupAndApply("provideRecoveryPhrase", p, neverAsked))

        assertTrue(p.retry)
    }

    @Test
    fun `a wrong phrase or a tampered envelope applies nothing and is not retried`() = runTest {
        val p = ports({ GatedRestoreOutcome.WrongPhraseOrTampered })

        assertEquals(BackupRestoreOutcome.NOT_APPLIED, restoreBackupAndApply("recover", p, neverAsked))

        assertFalse(p.applied)
        assertFalse(p.retry)
    }

    @Test
    fun `a hard refusal raises the alarm and applies nothing`() = runTest {
        val p = ports({ GatedRestoreOutcome.RollbackBlocked(hard = true, reason = RestoreReason.HASH_MISMATCH) })

        assertEquals(BackupRestoreOutcome.NOT_APPLIED, restoreBackupAndApply("recover", p, neverAsked))

        assertEquals(listOf(RestoreReason.HASH_MISMATCH), p.alerts)
        assertFalse(p.applied)
        assertFalse(p.retry)
    }

    @Test
    fun `a stale head is applied only when the route's screen says yes`() = runTest {
        val declined = ports({ GatedRestoreOutcome.Restored(payload()) })
        declined.staleReason = RestoreReason.BELOW_FLOOR
        assertEquals(
            BackupRestoreOutcome.NOT_APPLIED,
            restoreBackupAndApply("provideRecoveryPhrase", declined) { false },
        )
        assertFalse(declined.applied)
        assertTrue("a declined stale head is no alarm", declined.alerts.isEmpty())

        val accepted = ports({ GatedRestoreOutcome.Restored(payload()) })
        accepted.staleReason = RestoreReason.BELOW_FLOOR
        assertEquals(
            BackupRestoreOutcome.APPLIED,
            restoreBackupAndApply("provideRecoveryPhrase", accepted) { true },
        )
        assertTrue(accepted.applied)
    }

    // ---- C: a pending restore at the next landing -------------------------

    @Test
    fun `a landing with nothing pending fetches nothing`() = runTest {
        val p = ports()

        assertNull(retryPendingBackupRestore(p))

        assertEquals(0, p.fetches)
    }

    @Test
    fun `a failed fetch is tried again at the next landing and applied`() = runTest {
        val p = ports(
            { GatedRestoreOutcome.TransportFailed(IOException("offline")) },
            { GatedRestoreOutcome.Restored(payload()) },
        )
        // The phrase-prompt route: the fetch fails, the sign-in goes on.
        assertEquals(BackupRestoreOutcome.FETCH_FAILED, restoreBackupAndApply("provideRecoveryPhrase", p, neverAsked))
        assertTrue(p.retry)

        // The next landing.
        assertEquals(BackupRestoreOutcome.APPLIED, retryPendingBackupRestore(p))
        assertEquals(2, p.fetches)
        assertEquals(listOf(listOf(g1, g2)), p.rememberedGroups)
        assertEquals(1, p.mergedContacts.size)
        assertFalse(p.retry)

        // And the one after: nothing left to retry.
        assertNull(retryPendingBackupRestore(p))
        assertEquals(2, p.fetches)
    }

    @Test
    fun `a failing retry stays pending and is tried again on the next landing`() = runTest {
        val p = ports(
            { GatedRestoreOutcome.TransportFailed(IOException("offline")) },
            { GatedRestoreOutcome.TransportFailed(IOException("still offline")) },
            { throw IOException("and again") },
            { GatedRestoreOutcome.Restored(payload()) },
        )
        restoreBackupAndApply("recover", p, neverAsked)

        // Two landings whose attempt fails: one fetch each, still pending.
        assertEquals(BackupRestoreOutcome.FETCH_FAILED, retryPendingBackupRestore(p))
        assertTrue("a transport failure is not an answer", p.retry)
        assertEquals(2, p.fetches)
        assertEquals(BackupRestoreOutcome.FETCH_FAILED, retryPendingBackupRestore(p))
        assertTrue(p.retry)
        assertEquals("one attempt per landing", 3, p.fetches)
        assertFalse(p.applied)

        // The landing whose fetch gets through applies it and clears the flag.
        assertEquals(BackupRestoreOutcome.APPLIED, retryPendingBackupRestore(p))
        assertFalse(p.retry)
        assertEquals(listOf(listOf(g1, g2)), p.rememberedGroups)
        assertNull(retryPendingBackupRestore(p))
        assertEquals(4, p.fetches)
    }

    @Test
    fun `a definitive answer clears the pending flag`() = runTest {
        val definitive: List<Pair<String, () -> GatedRestoreOutcome>> = listOf(
            "applied" to { GatedRestoreOutcome.Restored(payload()) },
            "no backup" to { GatedRestoreOutcome.NoBackup },
            "refused by the head gate" to {
                GatedRestoreOutcome.RollbackBlocked(hard = true, reason = RestoreReason.BINDING_INVALID)
            },
            "a stale head that was declined" to {
                GatedRestoreOutcome.RollbackBlocked(hard = false, reason = RestoreReason.FRESH_STALE)
            },
            "a wrong phrase or an unopenable payload" to { GatedRestoreOutcome.WrongPhraseOrTampered },
        )
        for ((name, answer) in definitive) {
            // On a phrase route ...
            val route = ports(answer)
            restoreBackupAndApply("provideRecoveryPhrase", route, neverAsked)
            assertFalse("$name on a phrase route", route.retry)
            // ... and on a later landing's attempt.
            val landing = ports(answer)
            landing.retry = true
            retryPendingBackupRestore(landing)
            assertFalse("$name at a landing", landing.retry)
            assertNull("$name: nothing left for the landing after", retryPendingBackupRestore(landing))
            assertEquals(1, landing.fetches)
        }
    }

    @Test
    fun `a stale head on the retry is declined without a screen`() = runTest {
        val p = ports({ GatedRestoreOutcome.Restored(payload()) })
        p.retry = true
        p.staleReason = RestoreReason.FRESH_STALE

        assertEquals(BackupRestoreOutcome.NOT_APPLIED, retryPendingBackupRestore(p))

        assertEquals(listOf(false), p.staleAnswers)
        assertFalse(p.applied)
        assertFalse(p.retry)
        assertTrue(p.logs.any { "stale head" in it })
    }

    // ---- structural: the sources ------------------------------------------

    private fun source(path: String): String {
        val file = File(path)
        assertTrue("$path not found from ${File(".").absolutePath}", file.exists())
        return file.readText()
    }

    @Test
    fun `nothing on the backup path composes or loads an MLS snapshot`() {
        val backupSources = File("src/main/kotlin/me/lakko/app/backup").listFiles { f -> f.extension == "kt" }
            .orEmpty()
            .map { "src/main/kotlin/me/lakko/app/backup/${it.name}" }
        assertTrue(backupSources.isNotEmpty())
        for (path in backupSources + "src/main/kotlin/me/lakko/app/auth/AuthRepository.kt") {
            val text = source(path)
            for (forbidden in listOf(
                "restoreStorage(", "snapshotStorage(", "wholeStorageSnapshot", "IdentitySnapshotStore.putSnapshot",
            )) {
                assertFalse("$path still uses $forbidden", forbidden in text)
            }
        }
        // The compose call takes exactly these arguments.
        val compose = source("src/main/kotlin/me/lakko/app/backup/BackupClient.kt")
            .substringAfter("val payload = buildBackupPayload(")
            .substringBefore("val envelope")
        val named = Regex("""^\s*(\w+) = """, RegexOption.MULTILINE).findAll(compose).map { it.groupValues[1] }.toList()
        assertEquals(
            listOf(
                "accountId", "createdAtMs", "ed25519Seed", "contacts", "knownGroupIds", "syncedState",
                "ownPredecessors", "ownProfile", "visibility", "receivedProfiles",
            ),
            named,
        )
    }

    @Test
    fun `every compose site signs with the account identity, never the device leaf`() {
        // ADR-0050: the payload's `ed25519Seed` is the phrase-derived
        // AccountIdentity's. The per-device leaf identity names one phone and
        // must never reach a backup (the web auto-backup driver once read the
        // chat layer's device identity here).
        val derive = source("src/main/kotlin/me/lakko/app/backup/BackupClient.kt")
            .substringAfter("public fun ed25519SeedFromIdentitySeed(identitySeed: ByteArray): ByteArray {")
            .substringBefore("\n}")
        // (`SessionSecretWipe.identity` is `Identity.fromSeed` on a seed the
        // sign-out has not wiped; it refuses a wiped one.)
        assertTrue("me.lakko.app.auth.SessionSecretWipe.identity(identitySeed).use { it.ed25519Seed() }" in derive)
        // The automatic backup: the session's phrase seed, through that one helper.
        val wiring = source("src/main/kotlin/me/lakko/app/MainActivity.kt")
            .substringAfter("me.lakko.app.backup.AutoBackupDriver.Inputs(")
            .substringBefore("driver.start()")
        assertTrue(
            Regex("""accountIdentitySeed = \{\s*authRepository\.identitySecrets\.value\?\.accountIdentitySeed\s*\}""")
                .containsMatchIn(wiring),
        )
        val driver = source("src/main/kotlin/me/lakko/app/backup/AutoBackupDriver.kt")
        assertTrue("ed25519SeedFromIdentitySeed(request.accountIdentitySeed)" in driver)
        // The signup / recover baseline: the seed of the phrase in hand.
        val auth = source("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt")
        val baseline = auth.substringAfter("    private suspend fun bootstrapBackupBestEffort(")
            .substringBefore("\n    }\n")
        assertTrue("RecoveryMnemonic.fromPhrase(phrase).use { it.toSeed() }" in baseline)
        assertTrue("me.lakko.app.backup.ed25519SeedFromIdentitySeed(accountIdentitySeed)" in baseline)
        assertTrue("ed25519Seed = seed," in baseline)
        // And nothing on the backup path can reach the device identity at all.
        val backupSources = File("src/main/kotlin/me/lakko/app/backup").listFiles { f -> f.extension == "kt" }.orEmpty()
        for (file in backupSources) {
            for (forbidden in listOf("DeviceIdentityStore", "deviceIdentity(", "SessionIdentities")) {
                assertFalse("${file.name} reaches $forbidden", forbidden in file.readText())
            }
        }
        assertFalse("DeviceIdentityStore" in baseline)
        // Three callers of the upload entry points, and no fourth: the
        // baseline, the automatic backup, and the re-seal a key rotation
        // makes under its new phrase (ADR-0074 section 3.5, "Backup"), which
        // signs with the seed of the phrase this device holds, checked
        // against the session's.
        val mainSources = File("src/main/kotlin").walkTopDown().filter { it.isFile && it.extension == "kt" }.toList()
        val uploadCalls = mainSources.sumOf { f ->
            Regex("""\.(uploadNow|uploadFromCache)\(""").findAll(f.readText()).count()
        }
        assertEquals("AuthRepository baseline + rotation re-seal + CachedPhraseUploader", 3, uploadCalls)
        val rotation = auth.substringAfter("    public suspend fun rotationSealBackup(").substringBefore("\n    }\n")
        assertTrue("me.lakko.app.backup.ed25519SeedFromIdentitySeed(phraseSeed)" in rotation)
        assertFalse("wholeStorageSnapshot" in rotation)
    }

    @Test
    fun `every phrase route runs the one restore, and a recover keeps the local MLS state`() {
        val auth = source("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt")
        fun body(name: String): String {
            val start = auth.indexOf("    public suspend fun $name(")
            assertTrue("$name not found", start >= 0)
            val end = auth.indexOf("\n    }\n", start)
            return auth.substring(start, end)
        }
        for (route in listOf("recover", "provideRecoveryPhrase")) {
            val text = body(route)
            assertEquals("$route runs the shared restore once", 1, Regex("restoreBackupOnPhraseRoute\\(").findAll(text).count())
            assertTrue("$route does not also retry", "retryBackupRestore = false" in text)
            // The restore runs before the session lands.
            assertTrue(text.indexOf("restoreBackupOnPhraseRoute(") < text.indexOf("installIdentitySecrets("))
        }
        // The phrase prompt restores only after the phrase was verified.
        val prompt = body("provideRecoveryPhrase")
        assertTrue(prompt.indexOf("verifySeedAgainstPublishedIdentity(") < prompt.indexOf("restoreBackupOnPhraseRoute("))
        // A recover neither replaces the local snapshot nor drops the
        // pending-commit outbox recorded against it, and never adds a group
        // it holds no state for to the chat layer's own list.
        val recover = body("recover")
        for (forbidden in listOf("CommitOutboxStore", "IdentitySnapshotStore", "KnownGroupsStore", "markRestored")) {
            assertFalse("recover still touches $forbidden", forbidden in recover)
        }
        // One shared function, and it is the only caller of the rule.
        assertEquals(1, Regex("restoreBackupAndApply\\(").findAll(auth).count())
        assertEquals(1, Regex("retryPendingBackupRestore\\(").findAll(auth).count())
    }

    // ---- the own profile and the setting ----------------------------------

    private fun payloadWith(ownProfile: ByteArray, visibility: ByteArray, groups: List<String> = emptyList()) =
        GatedRestoreOutcome.Restored(payload(ownProfile = ownProfile, visibility = visibility, groups = groups))

    @Test
    fun `a restore hands on the own profile and the setting, and marks the list`() = runTest {
        val p = ports({ payloadWith(byteArrayOf(1), byteArrayOf(2), listOf(g1)) })
        val outcome = restoreBackupAndApply("recover", p) { false }
        assertEquals(BackupRestoreOutcome.APPLIED, outcome)
        assertEquals(1, p.appliedProfiles.size)
        assertEquals(1, p.appliedVisibilities.size)
        assertEquals(1, p.everyoneUnknownNotes)
        assertEquals(listOf(true), p.restoredMarks)
    }

    @Test
    fun `an empty field changes nothing`() = runTest {
        val p = ports({ payloadWith(ByteArray(0), ByteArray(0)) })
        restoreBackupAndApply("recover", p) { false }
        assertTrue(p.appliedProfiles.isEmpty())
        assertTrue(p.appliedVisibilities.isEmpty())
        assertEquals(listOf(false), p.restoredMarks)
    }

    @Test
    fun `a profile that does not decode costs nothing else`() = runTest {
        val p = ports({ payloadWith(byteArrayOf(1), byteArrayOf(2)) })
        p.failOwnProfile = true
        val outcome = restoreBackupAndApply("recover", p) { false }
        assertEquals(BackupRestoreOutcome.APPLIED, outcome)
        assertEquals(1, p.appliedVisibilities.size)
    }

    @Test
    fun `no backup still ends the restore, with no conversation to speak of`() = runTest {
        val p = ports({ GatedRestoreOutcome.NoBackup })
        restoreBackupAndApply("recover", p) { false }
        assertEquals(listOf(false), p.restoredMarks)
    }
}
