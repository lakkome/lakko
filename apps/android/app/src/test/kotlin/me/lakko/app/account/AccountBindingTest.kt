// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import android.content.Context
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import me.lakko.app.StaleBindingException
import me.lakko.app.auth.AuthRepository
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * Every process-lifetime holder of per-account state ends its binding at the
 * points the account changes, so work started for the account that left
 * stands down: the auth repository at sign-out and forget device, the chat,
 * contacts and device-sync runtimes when they are cleared or handed another
 * account.
 */
@RunWith(RobolectricTestRunner::class)
class AccountBindingTest {

    private lateinit var context: Context
    private val accountA = ByteArray(16) { 0x0A }
    private val accountB = ByteArray(16) { 0x0B }

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `log out ends the auth binding`() {
        val auth = AuthRepository(context)
        val bound = auth.captureAccountBinding()
        assertTrue(bound.isCurrent)

        auth.logout()

        assertFalse(bound.isCurrent)
        try {
            // What a landing does before it writes or signs the account in.
            bound.ensureCurrent()
            fail("a flow that outlived the sign-out went on")
        } catch (_: StaleBindingException) {
            // expected
        }
    }

    @Test
    fun `forget device ends the auth binding`() {
        val auth = AuthRepository(context)
        val bound = auth.captureAccountBinding()
        auth.forgetDevice()
        assertFalse(bound.isCurrent)
    }

    @Test
    fun `a binding captured after the sign-out is the next one`() {
        val auth = AuthRepository(context)
        auth.logout()
        val next = auth.captureAccountBinding()
        assertTrue(next.isCurrent)
        auth.logout()
        assertFalse(next.isCurrent)
    }

    @Test
    fun `clearing the chat repository ends its binding and the work launched for it`() = runBlocking {
        val repository = ChatRepository(applicationContext = context)
        val bound = repository.captureSessionBinding()
        val started = CompletableDeferred<Unit>()
        val response = CompletableDeferred<Unit>()
        var wroteAfterTheAwait: Boolean? = null
        val job = repository.launchForSession {
            started.complete(Unit)
            response.await()
            wroteAfterTheAwait = true
        }
        withTimeout(5_000) { started.await() }

        repository.clear()
        response.complete(Unit)
        withTimeout(5_000) { job.join() }

        assertFalse(bound.isCurrent)
        assertTrue(job.isCancelled)
        assertNull(wroteAfterTheAwait)
        // Work launched now belongs to the next session and runs.
        var ran = false
        withTimeout(5_000) { repository.launchForSession { ran = true }.join() }
        assertTrue(ran)
        repository.close()
    }

    @Test
    fun `the contacts binding ends on clear and on another account, not on the same account`() {
        val contacts = ContactsRepository(authRepo = AuthRepository(context), appContext = null)
        contacts.bindAccount(accountA)
        val forA = contacts.captureSessionBinding()

        // Idempotent re-bind (every operation does it): the same binding.
        contacts.bindAccount(accountA)
        assertTrue(forA.isCurrent)

        // Another account bound over it.
        contacts.bindAccount(accountB)
        assertFalse(forA.isCurrent)
        val forB = contacts.captureSessionBinding()
        assertTrue(forB.isCurrent)

        // Log out.
        contacts.clear()
        assertFalse(forB.isCurrent)

        // The same account signing in again is a new binding.
        contacts.bindAccount(accountB)
        assertFalse(forB.isCurrent)
        assertTrue(contacts.captureSessionBinding().isCurrent)
    }

    @Test
    fun `clearing the device-sync runtime ends its binding`() {
        val sync = DeviceGroupSyncRepository(context)
        sync.bindAccountForTest("AAAAAAAAAAAAAAAAAAAAAA==")
        val bound = sync.captureSessionBinding()
        assertTrue(bound.isCurrent)

        sync.clear()

        assertFalse(bound.isCurrent)
        sync.close()
    }
}
