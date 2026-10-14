// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import me.lakko.app.StaleBindingException
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * Ending a session and the writers of its stores exclude each other: the end
 * waits for a write in flight on another thread, and no write starts after
 * it. Real threads, since the defect is a thread race and not a suspension.
 */
class AccountStoreGateTest {

    private val account = "CgoKCgoKCgoKCgoKCgoKCg=="

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    @Test
    fun `an open account is written`() {
        assertEquals("written", AccountStoreGate.write(account) { "written" })
        assertEquals("written", AccountStoreGate.writeOrEnd(account) { "written" })
    }

    @Test
    fun `no write starts after the session ended`() {
        AccountStoreGate.seal(account)
        var wrote = false
        assertNull(AccountStoreGate.write(account) { wrote = true })
        try {
            AccountStoreGate.writeOrEnd(account) { wrote = true }
            fail("a write its caller depends on was reported as made")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertFalse(wrote)
        assertTrue(AccountStoreGate.isSealed(account))
    }

    @Test
    fun `the wipe waits for a write in flight on another thread and none follows it`() {
        // The store: what a writer puts there, and what the wipe removes.
        val store = HashMap<String, String>()
        val writing = CountDownLatch(1)
        val finishWrite = CountDownLatch(1)
        val writer = thread {
            AccountStoreGate.write(account) {
                writing.countDown()
                // Past its "which account?" read, inside the write itself.
                finishWrite.await(5, TimeUnit.SECONDS)
                synchronized(store) { store["snapshot"] = "of the account being forgotten" }
            }
        }
        assertTrue(writing.await(5, TimeUnit.SECONDS))

        val wiped = CountDownLatch(1)
        val wiper = thread {
            AccountStoreGate.seal(account) { synchronized(store) { store.clear() } }
            wiped.countDown()
        }
        // The wipe does not run under the write.
        assertFalse(wiped.await(200, TimeUnit.MILLISECONDS))
        finishWrite.countDown()
        assertTrue(wiped.await(5, TimeUnit.SECONDS))
        writer.join(5_000)
        wiper.join(5_000)

        // The write landed BEFORE the wipe, which removed it; a writer that
        // comes after is refused.
        assertTrue(synchronized(store) { store.isEmpty() })
        val late = thread {
            AccountStoreGate.write(account) { synchronized(store) { store["snapshot"] = "written back" } }
        }
        late.join(5_000)
        assertTrue(synchronized(store) { store.isEmpty() })
    }

    @Test
    fun `a writer that arrives during the wipe is held off and then refused`() {
        val store = HashMap<String, String>()
        val wiping = CountDownLatch(1)
        val finishWipe = CountDownLatch(1)
        val wiper = thread {
            AccountStoreGate.seal(account) {
                wiping.countDown()
                finishWipe.await(5, TimeUnit.SECONDS)
                synchronized(store) { store.clear() }
            }
        }
        assertTrue(wiping.await(5, TimeUnit.SECONDS))
        val wrote = AtomicBoolean(false)
        val returned = CountDownLatch(1)
        val writer = thread {
            AccountStoreGate.write(account) {
                wrote.set(true)
                synchronized(store) { store["row"] = "written back" }
            }
            returned.countDown()
        }
        assertFalse(returned.await(200, TimeUnit.MILLISECONDS))
        finishWipe.countDown()
        assertTrue(returned.await(5, TimeUnit.SECONDS))
        wiper.join(5_000)
        writer.join(5_000)
        assertFalse(wrote.get())
        assertTrue(synchronized(store) { store.isEmpty() })
    }

    @Test
    fun `writers do not wait for each other`() {
        val bothInside = CountDownLatch(2)
        val release = CountDownLatch(1)
        val writers = List(2) {
            thread {
                AccountStoreGate.write(account) {
                    bothInside.countDown()
                    release.await(5, TimeUnit.SECONDS)
                }
            }
        }
        assertTrue(bothInside.await(5, TimeUnit.SECONDS))
        release.countDown()
        writers.forEach { it.join(5_000) }
    }

    @Test
    fun `another account is not held back by a sealed one`() {
        AccountStoreGate.seal(account)
        assertEquals(1, AccountStoreGate.write("CwsLCwsLCwsLCwsLCwsLCw==") { 1 })
    }

    @Test
    fun `the account is written again from its next landing`() {
        AccountStoreGate.seal(account)
        AccountStoreGate.open(account)
        assertFalse(AccountStoreGate.isSealed(account))
        assertEquals(1, AccountStoreGate.write(account) { 1 })
    }

    @Test
    fun `the spellings of one account id are one account`() {
        // Standard base64 as the auth side writes it; URL-safe and unpadded
        // as the chat side does.
        val standard = "+/8AAQIDBAUGBwgJCgsMDQ=="
        val urlSafe = "-_8AAQIDBAUGBwgJCgsMDQ"
        AccountStoreGate.seal(standard)
        assertTrue(AccountStoreGate.isSealed(urlSafe))
        assertNull(AccountStoreGate.write(urlSafe) { 1 })
        AccountStoreGate.open(urlSafe)
        assertFalse(AccountStoreGate.isSealed(standard))
    }

    @Test
    fun `the wipe itself may write through the gate it holds`() {
        // A store that deletes by writing an empty row.
        var wrote = false
        AccountStoreGate.seal(account) {
            AccountStoreGate.write(account) { wrote = true }
        }
        assertTrue(wrote)
        // Any other thread is refused all the same.
        var late = false
        thread { AccountStoreGate.write(account) { late = true } }.join(5_000)
        assertFalse(late)
    }

    @Test
    fun `a writer of an earlier session is refused after the same account signed in again`() {
        // A handle loaded in the first session, holding that session's state.
        val first = AccountStoreGate.session(account)
        assertEquals("written", AccountStoreGate.writeOrEnd(first) { "written" })

        AccountStoreGate.seal(account) // signed out
        AccountStoreGate.open(account) // the same account signed in again

        // The gate is open for the account, and the old writer is not let in.
        assertEquals(1, AccountStoreGate.write(account) { 1 })
        var rolledBack = false
        try {
            AccountStoreGate.writeOrEnd(first) { rolledBack = true }
            fail("the previous session's state was written over the new one's")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertFalse(rolledBack)
        // A writer of the session that is open now is.
        assertEquals("written", AccountStoreGate.writeOrEnd(AccountStoreGate.session(account)) { "written" })
    }

    @Test
    fun `the runtimes sealing one ended session count as one seal`() {
        val before = AccountStoreGate.session(account)
        AccountStoreGate.seal(account)
        AccountStoreGate.seal(account) // the chat runtime, then the auth one
        AccountStoreGate.open(account)
        val after = AccountStoreGate.session(account)
        AccountStoreGate.seal(account)
        AccountStoreGate.open(account)
        // Each of the three sessions is told apart from the others.
        for (stale in listOf(before, after)) {
            try {
                AccountStoreGate.writeOrEnd(stale) { }
                fail("a stale session wrote")
            } catch (_: StaleBindingException) {
                // expected
            }
        }
    }
}
