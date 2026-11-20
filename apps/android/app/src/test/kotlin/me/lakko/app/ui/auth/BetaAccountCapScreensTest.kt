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
import me.lakko.app.auth.RegistrationClosed
import me.lakko.app.auth.SignupOutcome
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * `docs/ux/beta-account-cap.md` on the two screens: the sign-up form gives
 * way to the full state, and the guest question to its closed form.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BetaAccountCapScreensTest {

    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun signupVm(
        open: suspend () -> Boolean,
        run: suspend (String, ByteArray, () -> Unit) -> SignupOutcome = { _, _, _ -> SignupOutcome("acct") },
    ) = SignupViewModel(
        runSignup = run,
        createdAccountId = { null },
        validateUsername = { it.trim().lowercase() },
        normalizeDisplayName = { it.trim() },
        saveOwnProfile = { _, _ -> },
        signupOpen = open,
    )

    private fun SignupViewModel.fill() {
        onUsernameChange("alice")
        onPasswordChange("correct horse")
        onConfirmChange("correct horse")
    }

    @Test
    fun `closed on open replaces the form`() = runTest(dispatcher) {
        val model = signupVm(open = { false })
        model.onShown()
        advanceUntilIdle()
        assertTrue(model.state.value.closed)
    }

    @Test
    fun `open on open keeps the form`() = runTest(dispatcher) {
        val model = signupVm(open = { true })
        model.onShown()
        advanceUntilIdle()
        assertFalse(model.state.value.closed)
    }

    @Test
    fun `closed on submit never starts the sign-up`() = runTest(dispatcher) {
        var ran = false
        val model = signupVm(open = { false }, run = { _, _, _ -> ran = true; SignupOutcome("acct") })
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        assertFalse(ran)
        val s = model.state.value
        assertTrue(s.closed)
        assertFalse(s.busy)
        // Nothing typed is lost.
        assertEquals("alice", s.username)
        assertEquals("correct horse", s.password)
    }

    @Test
    fun `refused at the bind shows the full state`() = runTest(dispatcher) {
        val model = signupVm(open = { true }, run = { _, _, _ -> throw RegistrationClosed() })
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        val s = model.state.value
        assertTrue(s.closed)
        assertFalse(s.busy)
        assertNull(s.error)
    }

    @Test
    fun `Try again checks and returns to the form with the typed input`() = runTest(dispatcher) {
        var open = false
        val gate = CompletableDeferred<Unit>()
        val model = signupVm(open = {
            if (open) gate.await()
            open
        })
        model.fill()
        model.onShown()
        advanceUntilIdle()
        assertTrue(model.state.value.closed)
        // Still closed: stays, no extra message.
        model.onRetryOpen()
        advanceUntilIdle()
        assertTrue(model.state.value.closed)
        assertFalse(model.state.value.checking)
        assertNull(model.state.value.error)
        // Now open: `Checking...` while it runs, then the form.
        open = true
        model.onRetryOpen()
        advanceUntilIdle()
        assertTrue(model.state.value.checking)
        gate.complete(Unit)
        advanceUntilIdle()
        val s = model.state.value
        assertFalse(s.closed)
        assertFalse(s.checking)
        assertEquals("alice", s.username)
        assertEquals("correct horse", s.confirm)
    }

    private fun guestVm(
        open: suspend () -> Boolean,
        claim: suspend (String, ByteArray) -> Unit = { _, _ -> },
        discard: suspend () -> Unit = {},
        claimed: String? = null,
    ) = GuestAccountOfferViewModel(
        claim = claim,
        discard = discard,
        expire = { false },
        claimedUsername = { claimed },
        validateUsername = { it.trim().lowercase() },
        log = { _, _ -> },
        signupOpen = open,
    )

    @Test
    fun `guest question reads closed when the beta is full and OK deletes`() = runTest(dispatcher) {
        var discarded = 0
        val model = guestVm(open = { false }, discard = { discarded += 1 })
        model.onShown()
        advanceUntilIdle()
        assertTrue(model.state.value.closed)
        // Create account is not offered.
        model.onChooseCreate()
        assertEquals(GuestOfferStep.ASK, model.state.value.step)
        model.onDecline()
        advanceUntilIdle()
        assertEquals(1, discarded)
    }

    @Test
    fun `guest question stays as before when open`() = runTest(dispatcher) {
        val model = guestVm(open = { true })
        model.onShown()
        advanceUntilIdle()
        assertFalse(model.state.value.closed)
    }

    @Test
    fun `a guest that already took a name is never told the beta is full`() = runTest(dispatcher) {
        val model = guestVm(open = { false }, claimed = "mira")
        model.onShown()
        advanceUntilIdle()
        assertFalse(model.state.value.closed)
    }

    @Test
    fun `Create account refused at the bind shows the closed question and deletes nothing`() =
        runTest(dispatcher) {
            var discarded = 0
            val model = guestVm(
                open = { true },
                claim = { _, _ -> throw RegistrationClosed() },
                discard = { discarded += 1 },
            )
            model.onChooseCreate()
            model.onUsernameChange("mira")
            model.onPasswordChange("correct horse")
            model.onConfirmChange("correct horse")
            model.onCreate()
            advanceUntilIdle()
            val s = model.state.value
            assertTrue(s.closed)
            assertEquals(GuestOfferStep.ASK, s.step)
            assertNull(s.error)
            assertEquals(0, discarded)
        }
}
