// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import java.util.Collections
import java.util.concurrent.Executors
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.runBlocking
import me.lakko.app.contacts.AcceptedContactsStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0091 walk 3, W3-R4 — the automatic backup does its assembly off the
 * caller's thread. Seen on a phone: the driver runs in a Compose effect's
 * scope, so `BackupPersonal.of` (received profiles decoded through the FFI)
 * ran on the main thread. A run now does every input read, the assembly and
 * the upload on the dispatcher the driver is given.
 */
@RunWith(RobolectricTestRunner::class)
class AutoBackupDriverThreadTest {

    private lateinit var context: Context
    private val account = "Dg4ODg4ODg4ODg4ODg4ODg=="

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `a run assembles and uploads on the dispatcher it is given`() {
        val threadName = "backup-work-test"
        val executor = Executors.newSingleThreadExecutor { r -> Thread(r, threadName) }
        val dispatcher = executor.asCoroutineDispatcher()
        val seen = Collections.synchronizedList(mutableListOf<Pair<String, String>>())
        fun note(what: String) {
            seen += what to Thread.currentThread().name
        }
        try {
            val driver = AutoBackupDriver(
                applicationContext = context,
                scope = kotlinx.coroutines.CoroutineScope(dispatcher),
                inputs = AutoBackupDriver.Inputs(
                    accountId = { ByteArray(16) { 0x0E } },
                    accountIdB64 = { note("accountIdB64"); account },
                    sessionToken = { "token" },
                    accountIdentitySeed = { ByteArray(64) },
                    knownGroupIds = { note("knownGroupIds"); listOf("cd".repeat(32)) },
                    syncedState = { note("syncedState"); null },
                    ownPredecessors = { note("ownPredecessors"); emptyList() },
                    personal = { note("personal"); BackupPersonal.NONE },
                ),
                uploader = {
                    note("upload")
                    UploadResult.Uploaded
                },
                workDispatcher = dispatcher,
            )
            val caller = Thread.currentThread().name
            val outcome = runBlocking { driver.runOnce(nowMs = 5_000L) }
            assertEquals(AutoBackupDriver.Outcome.UPLOADED, outcome)
            val names = seen.map { it.first }.toSet()
            for (step in listOf("accountIdB64", "knownGroupIds", "personal", "syncedState", "ownPredecessors", "upload")) {
                assertTrue("$step was not read", step in names)
            }
            // (Coroutine debug mode appends " @coroutine#n" to the name.)
            for ((step, thread) in seen) {
                assertTrue("$step ran on $thread, not the work dispatcher", thread.startsWith(threadName))
            }
            assertTrue(!caller.startsWith(threadName))
        } finally {
            dispatcher.close()
        }
    }

    @Test
    fun `the public constructor does not run on the caller's thread`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/backup/AutoBackupDriver.kt").readText()
        assertTrue(
            "the production constructor passes a background dispatcher",
            ") : this(applicationContext, scope, inputs, CachedPhraseUploader, Dispatchers.Default)" in source,
        )
        assertTrue("withContext(workDispatcher) { runOnceHere(nowMs) }" in source)
    }
}
