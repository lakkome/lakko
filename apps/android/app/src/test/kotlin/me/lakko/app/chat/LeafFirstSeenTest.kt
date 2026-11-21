// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import me.lakko.app.account.AccountStoreGate
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0091 section 7 (security review 2026-11-21) — the re-admitter's floor:
 * when this device first saw a leaf in a group's roster. Only the first
 * observation is kept; it survives a restart; the group's wipe and Forget
 * device remove it; a sealed account takes no write.
 */
@RunWith(RobolectricTestRunner::class)
class LeafFirstSeenTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AccountStoreGate.resetForTest()
    }

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    @Test
    fun `only the first observation is kept`() {
        val seen = withFirstSeen(emptyMap(), "AB", listOf("01", "02"), 100L)!!
        assertEquals(mapOf("ab:01" to 100L, "ab:02" to 100L), seen)
        assertNull("nothing new, nothing to write", withFirstSeen(seen, "ab", listOf("01"), 900L))
        val more = withFirstSeen(seen, "ab", listOf("01", "03"), 900L)!!
        assertEquals(100L, more["ab:01"])
        assertEquals(900L, more["ab:03"])
    }

    @Test
    fun `the store keeps the first time across instances and per group`() {
        SharedPrefsLeafFirstSeenStore(context).noteSeen(account, "ab", listOf("01"), 100L)
        SharedPrefsLeafFirstSeenStore(context).noteSeen(account, "ab", listOf("01", "02"), 500L)
        SharedPrefsLeafFirstSeenStore(context).noteSeen(account, "cd", listOf("01"), 700L)
        val store = SharedPrefsLeafFirstSeenStore(context)
        assertEquals(100L, store.firstSeen(account, "AB", "01"))
        assertEquals(500L, store.firstSeen(account, "ab", "02"))
        assertEquals(700L, store.firstSeen(account, "cd", "01"))
        assertNull(store.firstSeen(account, "ab", "03"))
        store.removeGroup(account, "ab")
        assertNull(SharedPrefsLeafFirstSeenStore(context).firstSeen(account, "ab", "01"))
        assertEquals(700L, SharedPrefsLeafFirstSeenStore(context).firstSeen(account, "cd", "01"))
        store.clearForAccount(account)
        assertNull(SharedPrefsLeafFirstSeenStore(context).firstSeen(account, "cd", "01"))
    }

    @Test
    fun `a sealed account takes no write`() {
        AccountStoreGate.seal(account)
        val store = SharedPrefsLeafFirstSeenStore(context)
        store.noteSeen(account, "ab", listOf("01"), 100L)
        assertNull(store.firstSeen(account, "ab", "01"))
    }

    @Test
    fun `the re-admitter clamps the rows by this record and its last read from the leaf`() {
        val ledger = ReentryAdmissionLedger { 0L }
        ledger.noteMessage("ab", "01", epoch = 6uL, atMs = 400L)
        assertEquals("any epoch", 400L, ledger.lastMessageAtAnyEpoch("AB", "01"))
        assertNull("only at the current epoch", ledger.lastMessageAt("ab", "01", 7uL))
        val repo = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        val send = repo.substring(repo.indexOf("private suspend fun sendOwedGapBundlesBestEffort("))
        val start = send.indexOf("gapHistoryStart(")
        assertEquals(true, start > 0)
        assertEquals(true, send.indexOf("leafFirstSeen.firstSeen(") > start)
        assertEquals(true, send.indexOf("reentryLedger.lastMessageAtAnyEpoch(") > start)
        assertEquals("nothing is sent with no evidence", true, send.indexOf("if (start == null)") > start)
    }
}
