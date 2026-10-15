// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.StaleBindingException
import me.lakko.app.account.AccountStoreGate
import org.junit.After
import org.junit.Assert.assertArrayEquals
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
 * The key writes of a sign-in route (ADR-0077 decision 4, G-48): refused only
 * when the account was wiped after the route began. A sign-out, or a wipe that
 * came before the route (signing back in to a forgotten account), refuses
 * nothing.
 */
class SignInKeyWritesTest {

    private val account = "CgoKCgoKCgoKCgoKCgoKCg=="

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun forget(accountIdB64: String) {
        AccountStoreGate.seal(accountIdB64) { AccountStoreGate.markWiped(accountIdB64) }
    }

    @Test
    fun `a route that began after a sign-out and an earlier wipe writes every key`() {
        forget(account)
        AccountStoreGate.seal(account)
        val route = AccountStoreGate.wipeMark()
        val store = HashMap<String, String>()

        assertFalse(SignInKeyWrites.isWiped(route, account))
        SignInKeyWrites.required(route, account) { store["device-key"] = "k" }
        assertTrue(SignInKeyWrites.ifNotWiped(route, account) { store["export-key"] = "e" })

        assertEquals(setOf("device-key", "export-key"), store.keys)
    }

    @Test
    fun `a wipe while the route is out refuses its key writes`() {
        val route = AccountStoreGate.wipeMark()
        // Forget device runs while the route's round trip is out.
        forget(account)
        val store = HashMap<String, String>()

        assertTrue(SignInKeyWrites.isWiped(route, account))
        assertFalse(SignInKeyWrites.ifNotWiped(route, account) { store["account-seed"] = "s" })
        try {
            SignInKeyWrites.required(route, account) { store["device-key"] = "k" }
            fail("the route must end, not land an account whose keys a wipe removed")
        } catch (_: StaleBindingException) {
            // expected: a cancellation, no error shown
        }
        assertTrue(store.isEmpty())
    }

    @Test
    fun `a wipe of another account refuses nothing`() {
        val route = AccountStoreGate.wipeMark()
        forget("AQEBAQEBAQEBAQEBAQEBAQ==")
        assertEquals(1, SignInKeyWrites.required(route, account) { 1 })
        assertTrue(SignInKeyWrites.ifNotWiped(route, account) { })
    }

    @Test
    fun `an error the write throws is passed on`() {
        val route = AccountStoreGate.wipeMark()
        try {
            SignInKeyWrites.ifNotWiped(route, account) { throw IllegalStateException("disk") }
            fail("the store's own failure is the caller's to handle")
        } catch (e: IllegalStateException) {
            assertEquals("disk", e.message)
        }
    }

    @Test
    fun `a wipe that starts during a key write waits for it and then removes it`() {
        val route = AccountStoreGate.wipeMark()
        val store = HashMap<String, String>()
        val writing = CountDownLatch(1)
        val release = CountDownLatch(1)
        val writer = thread {
            SignInKeyWrites.required(route, account) {
                writing.countDown()
                release.await(5, TimeUnit.SECONDS)
                synchronized(store) { store["device-key"] = "k" }
            }
        }
        assertTrue(writing.await(5, TimeUnit.SECONDS))
        val wiped = AtomicBoolean(false)
        val wipe = thread {
            AccountStoreGate.seal(account) {
                AccountStoreGate.markWiped(account)
                synchronized(store) { store.clear() }
                wiped.set(true)
            }
        }
        Thread.sleep(100)
        assertFalse(wiped.get())
        release.countDown()
        writer.join(5_000)
        wipe.join(5_000)
        assertTrue(wiped.get())
        assertTrue(store.isEmpty())
        // The route's next key write is refused.
        assertFalse(SignInKeyWrites.ifNotWiped(route, account) { store["export-key"] = "e" })
        assertTrue(store.isEmpty())
    }

    // ------------------------------------------------------- device id source

    @Test
    fun `a device id source mints no seed for an account wiped since the route began`() {
        var asked = 0
        val source = DeviceIdSource { _ ->
            asked += 1
            ByteArray(DEVICE_ID_LEN) { 7 }
        }
        val route = AccountStoreGate.wipeMark()
        assertArrayEquals(ByteArray(DEVICE_ID_LEN) { 7 }, source.deviceIdFor(account, route))
        assertEquals(1, asked)

        forget(account)
        assertNull(source.deviceIdFor(account, route))
        assertEquals(1, asked)
        // A route that began after the wipe is answered.
        assertArrayEquals(ByteArray(DEVICE_ID_LEN) { 7 }, source.deviceIdFor(account, AccountStoreGate.wipeMark()))
    }
}
