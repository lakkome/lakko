// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.SIGNUP_RATE_LIMITED_TEXT
import me.lakko.app.auth.SignupOutcome
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * ADR-0054 UX section 1 - the signup working state: while the one proof of
 * work runs the row offers Cancel, which stops the work and returns to the
 * filled-in form; once the proof is ready there is no Cancel; a network-wide
 * signup limit reads as its own sentence and turns the button into
 * `Try again`, the form kept.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SignupViewModelTest {

    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun vm(
        run: suspend (String, ByteArray, () -> Unit) -> SignupOutcome,
    ) = SignupViewModel(
        runSignup = run,
        createdAccountId = { null },
        validateUsername = { it.trim().lowercase() },
        normalizeDisplayName = { it.trim() },
        saveOwnProfile = { _, _ -> },
        parkRecoveryPhrase = {},
    )

    private fun SignupViewModel.fill() {
        onUsernameChange("alice")
        onPasswordChange("correct horse")
        onConfirmChange("correct horse")
    }

    @Test
    fun cancelWhileSolvingReturnsToTheFilledInForm() = runTest(dispatcher) {
        var cancelled = false
        val never = CompletableDeferred<SignupOutcome>()
        val model = vm { _, _, _ ->
            try {
                never.await()
            } catch (e: kotlinx.coroutines.CancellationException) {
                cancelled = true
                throw e
            }
        }
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        assertTrue(model.state.value.busy)
        assertTrue(model.state.value.solving)
        model.onCancel()
        advanceUntilIdle()
        assertTrue("the solve was cancelled", cancelled)
        val s = model.state.value
        assertFalse(s.busy)
        assertFalse(s.solving)
        assertNull(s.error)
        assertEquals("alice", s.username)
        assertEquals("correct horse", s.password)
    }

    @Test
    fun cancelStopsTheProofOfWorkChunkLoop() = runTest(dispatcher) {
        val chunks = java.util.concurrent.atomic.AtomicInteger()
        val running = java.util.concurrent.CountDownLatch(2)
        val executor = java.util.concurrent.Executors.newSingleThreadExecutor()
        try {
            val model = vm { _, _, onProofSolved ->
                // The real loop over a chunk solver that never finds a proof.
                me.lakko.app.auth.solvePow("seed", 8, solver = { _, _, _ ->
                    chunks.incrementAndGet()
                    running.countDown()
                    Thread.sleep(5)
                    null
                }, executor = executor)
                onProofSolved()
                error("unreachable: the solver never finds a proof")
            }
            model.fill()
            model.onSubmit()
            advanceUntilIdle()
            assertTrue(running.await(5, java.util.concurrent.TimeUnit.SECONDS))
            model.onCancel()
            advanceUntilIdle()
            // Once a task queued behind the loop on its single thread runs,
            // the loop has returned; no chunk may start after that.
            val drained = java.util.concurrent.CountDownLatch(1)
            executor.execute { drained.countDown() }
            assertTrue(drained.await(5, java.util.concurrent.TimeUnit.SECONDS))
            val after = chunks.get()
            Thread.sleep(50)
            assertEquals("the solve kept burning CPU after Cancel", after, chunks.get())
            assertFalse(model.state.value.busy)
        } finally {
            executor.shutdownNow()
        }
    }

    @Test
    fun noCancelOnceTheProofIsReady() = runTest(dispatcher) {
        val never = CompletableDeferred<SignupOutcome>()
        var cancelled = false
        val model = vm { _, _, onProofSolved ->
            onProofSolved()
            try {
                never.await()
            } catch (e: kotlinx.coroutines.CancellationException) {
                cancelled = true
                throw e
            }
        }
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        assertTrue(model.state.value.busy)
        assertFalse(model.state.value.solving)
        model.onCancel()
        advanceUntilIdle()
        assertFalse("past the proof the account is being created; cancel is a no-op", cancelled)
        assertTrue(model.state.value.busy)
        never.cancel()
    }

    @Test
    fun aSignupLimitShowsItsSentenceAndTryAgainWithTheFormKept() = runTest(dispatcher) {
        val model = vm { _, _, _ ->
            throw AuthHttpError(429, "/signup/start", "", code = "signup-rate-limited", retryAfterSecs = 3600)
        }
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        val s = model.state.value
        assertFalse(s.busy)
        assertEquals(SIGNUP_RATE_LIMITED_TEXT, s.error)
        assertTrue(s.rateLimited)
        assertEquals("alice", s.username)
        assertEquals("correct horse", s.confirm)
    }
}
