// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import me.lakko.app.StaleBindingException
import org.junit.After
import org.junit.Assert.assertEquals
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
 * A suspending call into the chat repository is work of the account that was
 * bound when it was made. Its caller's coroutine is not the repository's to
 * cancel (a screen, a call host, an app-wide scope), so the call itself stands
 * down when the session ends under it: nothing after its suspension runs,
 * nothing is returned for the caller to file, and a failure it meets on the
 * way is reported as the session having ended.
 */
@RunWith(RobolectricTestRunner::class)
class SessionWorkTest {

    private lateinit var repository: ChatRepository

    /** A caller that is NOT cancelled by a sign-out, like a call host. */
    private val caller = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @Before
    fun setUp() {
        val context: Context = RuntimeEnvironment.getApplication()
        repository = ChatRepository(applicationContext = context)
    }

    @After
    fun tearDown() {
        caller.cancel()
        repository.close()
    }

    @Test
    fun `a call that is not overtaken returns what it read`() = runBlocking {
        assertEquals(7, withTimeout(5_000) { repository.callForSession { 7 } })
    }

    @Test
    fun `a call that outlives a sign-out writes nothing after its suspension and returns nothing`() = runBlocking {
        val suspended = CompletableDeferred<Unit>()
        val response = CompletableDeferred<Unit>()
        var wroteAfterTheAwait = false
        val call = caller.async {
            repository.callForSession {
                suspended.complete(Unit)
                response.await()
                // What every entry point does between its network call and
                // its first write.
                repository.ensureSessionForTest()
                wroteAfterTheAwait = true
                "the answer"
            }
        }
        withTimeout(5_000) { suspended.await() }

        repository.clear()
        response.complete(Unit)

        try {
            withTimeout(5_000) { call.await() }
            fail("a call started before the sign-out handed its caller a result")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertFalse(wroteAfterTheAwait)
    }

    @Test
    fun `a call whose body never asks still returns nothing after a sign-out`() = runBlocking {
        val suspended = CompletableDeferred<Unit>()
        val response = CompletableDeferred<Unit>()
        val call = caller.async {
            repository.callForSession {
                suspended.complete(Unit)
                response.await()
                "read for the account that left"
            }
        }
        withTimeout(5_000) { suspended.await() }
        repository.clear()
        response.complete(Unit)

        var returned: String? = null
        try {
            returned = withTimeout(5_000) { call.await() }
        } catch (_: StaleBindingException) {
            // expected
        }
        assertNull(returned)
    }

    @Test
    fun `a failure after the session ended is reported as the session ending`() = runBlocking {
        val suspended = CompletableDeferred<Unit>()
        val response = CompletableDeferred<Unit>()
        val call = caller.async {
            repository.callForSession {
                suspended.complete(Unit)
                response.await()
                // A handle closed by the sign-out throws like this.
                throw IllegalStateException("Group object has already been destroyed")
            }
        }
        withTimeout(5_000) { suspended.await() }
        repository.clear()
        response.complete(Unit)

        try {
            withTimeout(5_000) { call.await() }
            fail("the call returned")
        } catch (_: StaleBindingException) {
            // expected: not the IllegalStateException
        }
    }

    @Test
    fun `a failure while the session stands is the failure`() = runBlocking {
        try {
            withTimeout(5_000) {
                repository.callForSession<Unit> { throw IllegalStateException("refused") }
            }
            fail("the call returned")
        } catch (cause: IllegalStateException) {
            assertEquals("refused", cause.message)
        }
    }

    @Test
    fun `a call made from inside another keeps the outer call's binding`() = runBlocking {
        val suspended = CompletableDeferred<Unit>()
        val response = CompletableDeferred<Unit>()
        var innerRan = false
        val call = caller.async {
            repository.callForSession {
                suspended.complete(Unit)
                response.await()
                // After the sign-out. Captured afresh, this inner call would
                // belong to whoever is bound now and run.
                repository.callForSession { innerRan = true }
            }
        }
        withTimeout(5_000) { suspended.await() }
        repository.clear()
        response.complete(Unit)

        try {
            withTimeout(5_000) { call.await() }
            fail("the call returned")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertFalse(innerRan)
    }

    @Test
    fun `work launched for the session asks its binding even where a catch swallowed the cancellation`() =
        runBlocking {
            val suspended = CompletableDeferred<Unit>()
            val response = CompletableDeferred<Unit>()
            var wroteAfterTheCatch = false
            val job = repository.launchForSession {
                try {
                    suspended.complete(Unit)
                    response.await()
                } catch (_: Throwable) {
                    // A best-effort catch: the cancellation is gone here.
                }
                try {
                    repository.ensureSessionForTest()
                    wroteAfterTheCatch = true
                } catch (_: StaleBindingException) {
                    // expected
                }
            }
            withTimeout(5_000) { suspended.await() }
            repository.clear()
            response.complete(Unit)
            withTimeout(5_000) { job.join() }
            assertFalse(wroteAfterTheCatch)
        }

    @Test
    fun `a call made after the sign-out belongs to the next session`() = runBlocking {
        repository.clear()
        assertTrue(withTimeout(5_000) { repository.callForSession { true } })
    }

    @Test
    fun `an entry point with no session answers as before`() = runBlocking {
        // Not bootstrapped: nothing to poll, and no stale binding either.
        val check = withTimeout(5_000) { repository.pollAllGroupsReporting() }
        assertEquals(0, check.tried)
        assertTrue(withTimeout(5_000) { repository.recordFeedSeen(emptyMap(), { null }, { _, _ -> null }) }.isEmpty())
    }
}
