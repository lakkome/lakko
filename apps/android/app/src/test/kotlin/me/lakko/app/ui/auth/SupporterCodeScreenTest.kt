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
import me.lakko.app.auth.HeldPlace
import me.lakko.app.auth.SignupOutcome
import me.lakko.app.auth.SupporterCodeUsed
import me.lakko.app.auth.SupporterPlaces
import me.lakko.app.auth.SupporterRedeemOutcome
import me.lakko.app.l10n.UiText
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** `docs/ux/supporters.md` section 1 on the sign-up screen. */
@OptIn(ExperimentalCoroutinesApi::class)
class SupporterCodeScreenTest {

    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private class FakePlaces(
        var place: HeldPlace? = null,
        var answer: suspend () -> SupporterRedeemOutcome = { SupporterRedeemOutcome.Failed },
    ) : SupporterPlaces {
        var drops = 0
        val redeemed = mutableListOf<String>()

        override fun held(): HeldPlace? = place

        override fun drop() {
            drops += 1
            place = null
        }

        override suspend fun redeem(text: String): SupporterRedeemOutcome {
            redeemed += text
            val out = answer()
            if (out is SupporterRedeemOutcome.Held) place = out.place
            return out
        }
    }

    private fun vm(
        places: SupporterPlaces,
        open: suspend () -> Boolean = { false },
        run: suspend (String, ByteArray, () -> Unit) -> SignupOutcome = { _, _, _ -> SignupOutcome("acct") },
    ) = SignupViewModel(
        runSignup = run,
        createdAccountId = { null },
        validateUsername = { it.trim().lowercase() },
        normalizeDisplayName = { it.trim() },
        saveOwnProfile = { _, _ -> },
        signupOpen = open,
        supporterPlaces = places,
    )

    private fun SignupViewModel.fill() {
        onUsernameChange("alice")
        onPasswordChange("correct horse")
        onConfirmChange("correct horse")
    }

    private val code = "K7QM-2XRA-9DPF-4WNB"
    private val place = HeldPlace("tok", 3, 1)

    @Test
    fun `the code view opens from the full state and Back returns`() = runTest(dispatcher) {
        val model = vm(FakePlaces())
        model.onShown()
        advanceUntilIdle()
        assertTrue(model.state.value.closed)
        model.onOpenSupporterCode()
        assertTrue(model.state.value.codeEntry)
        model.onSupporterCodeBack()
        assertFalse(model.state.value.codeEntry)
        assertTrue(model.state.value.closed)
    }

    @Test
    fun `continue does nothing until sixteen letters and digits`() = runTest(dispatcher) {
        val places = FakePlaces()
        val model = vm(places)
        model.onShown()
        advanceUntilIdle()
        model.onOpenSupporterCode()
        model.onSupporterCodeChange("K7QM-2XRA")
        model.onSupporterCodeContinue()
        advanceUntilIdle()
        assertTrue(places.redeemed.isEmpty())
    }

    @Test
    fun `a working code shows the form with the held line`() = runTest(dispatcher) {
        val gate = CompletableDeferred<SupporterRedeemOutcome>()
        val places = FakePlaces(answer = { gate.await() })
        val model = vm(places)
        model.onShown()
        advanceUntilIdle()
        model.onOpenSupporterCode()
        model.onSupporterCodeChange(code)
        model.onSupporterCodeContinue()
        advanceUntilIdle()
        assertTrue(model.state.value.codeChecking)
        gate.complete(SupporterRedeemOutcome.Held(place))
        advanceUntilIdle()
        val s = model.state.value
        assertFalse(s.closed)
        assertFalse(s.codeEntry)
        assertFalse(s.codeChecking)
        assertTrue(s.placeHeld)
        assertEquals(listOf(code), places.redeemed)
    }

    @Test
    fun `a failed code keeps what was typed with its line`() = runTest(dispatcher) {
        for ((outcome, key) in listOf(
            SupporterRedeemOutcome.Unknown to "auth.signup.code.unknown",
            SupporterRedeemOutcome.Limited to "auth.signup.code.limited",
            SupporterRedeemOutcome.Failed to "auth.signup.code.failed",
        )) {
            val model = vm(FakePlaces(answer = { outcome }))
            model.onShown()
            advanceUntilIdle()
            model.onOpenSupporterCode()
            model.onSupporterCodeChange(code)
            model.onSupporterCodeContinue()
            advanceUntilIdle()
            val s = model.state.value
            assertTrue(s.codeEntry)
            assertEquals(code, s.code)
            assertEquals(UiText(key), s.codeError)
        }
    }

    @Test
    fun `a place held on the device keeps the form when the beta is full`() = runTest(dispatcher) {
        var ran = false
        val model = vm(FakePlaces(place = place), open = { false }, run = { _, _, _ -> ran = true; SignupOutcome("acct") })
        model.onShown()
        advanceUntilIdle()
        assertFalse(model.state.value.closed)
        assertTrue(model.state.value.placeHeld)
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        assertTrue(ran)
        assertFalse(model.state.value.closed)
    }

    @Test
    fun `a used code shows the full state with its line and drops the place`() = runTest(dispatcher) {
        val places = FakePlaces(place = place)
        val model = vm(places, open = { false }, run = { _, _, _ -> throw SupporterCodeUsed() })
        model.onShown()
        advanceUntilIdle()
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        val s = model.state.value
        assertTrue(s.closed)
        assertTrue(s.codeUsed)
        assertFalse(s.placeHeld)
        assertNull(s.error)
        assertEquals(1, places.drops)
        // Opening the code view again clears the line.
        model.onOpenSupporterCode()
        assertFalse(model.state.value.codeUsed)
    }

    @Test
    fun `a free place leaves the held token unspent on the device`() = runTest(dispatcher) {
        val places = FakePlaces(place = place)
        val model = vm(places, open = { true })
        model.onShown()
        advanceUntilIdle()
        model.fill()
        model.onSubmit()
        advanceUntilIdle()
        assertEquals(0, places.drops)
        assertEquals(place, places.place)
    }
}
