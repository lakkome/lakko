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
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.GuestPhraseUnavailable
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.l10n.UiText
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The one question a guest is asked outside a call (`docs/ux/call-links.md`
 * section 9): `Create account` (a form) or `No, thanks` (the account is
 * deleted, with no confirmation).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class GuestAccountOfferViewModelTest {

    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private val claims = mutableListOf<Pair<String, String>>()
    private var claimed: String? = null

    private fun vm(
        claim: suspend (String, ByteArray) -> Unit = { username, password ->
            claims += username to String(password)
        },
        discard: suspend () -> Unit = {},
    ) = GuestAccountOfferViewModel(
        claim = claim,
        discard = discard,
        claimedUsername = { claimed },
        validateUsername = { typed ->
            val name = typed.trim().removePrefix("@").lowercase()
            require(name.length >= 3) { "too short" }
            name
        },
        log = { _, _ -> },
    )

    private fun GuestAccountOfferViewModel.fill(username: String = "Mira") {
        onChooseCreate()
        onUsernameChange(username)
        onPasswordChange("correct horse")
        onConfirmChange("correct horse")
    }

    @Test
    fun `the offer opens on the question and Create account opens the form`() {
        val model = vm()
        assertEquals(GuestOfferStep.ASK, model.state.value.step)
        model.onChooseCreate()
        assertEquals(GuestOfferStep.FORM, model.state.value.step)
        model.onBackToAsk()
        assertEquals(GuestOfferStep.ASK, model.state.value.step)
    }

    @Test
    fun `Create account keeps the account under the normalised username`() = runTest(dispatcher) {
        val model = vm()
        model.fill("@Mira")
        model.onCreate()
        assertEquals(GuestOfferBusy.CREATING, model.state.value.busy)
        advanceUntilIdle()
        assertEquals(listOf("mira" to "correct horse"), claims)
        val s = model.state.value
        assertEquals(GuestOfferBusy.NONE, s.busy)
        assertNull(s.error)
        // The password does not stay in the form once it was used.
        assertEquals("", s.password)
        assertEquals("", s.confirm)
    }

    @Test
    fun `a username failing the rule never reaches the claim`() = runTest(dispatcher) {
        val model = vm()
        model.fill("ab")
        model.onCreate()
        advanceUntilIdle()
        assertTrue(claims.isEmpty())
        assertEquals(GuestOfferBusy.NONE, model.state.value.busy)
        assertTrue(model.state.value.error != null)
    }

    @Test
    fun `the password rules are the sign-up form's`() = runTest(dispatcher) {
        val model = vm()
        model.fill()
        model.onConfirmChange("something else")
        model.onCreate()
        advanceUntilIdle()
        assertEquals(UiText("auth.password.error.mismatch"), model.state.value.error)
        model.onPasswordChange("short")
        model.onConfirmChange("short")
        model.onCreate()
        advanceUntilIdle()
        assertEquals("auth.password.error.too_short", model.state.value.error?.key)
        assertTrue(claims.isEmpty())
    }

    @Test
    fun `a retry sends the username already taken, whatever the field holds`() = runTest(dispatcher) {
        var fail = true
        val model = vm(claim = { username, password ->
            claims += username to String(password)
            if (fail) {
                // The claim took the name, then the password step failed.
                claimed = username
                throw AuthNetworkError("/password/reset/start", java.io.IOException("offline"))
            }
        })
        model.fill("mira")
        model.onCreate()
        advanceUntilIdle()
        assertEquals(UiText("common.error.offline"), model.state.value.error)
        assertEquals(GuestOfferStep.FORM, model.state.value.step)
        // The form keeps the password for the retry.
        assertEquals("correct horse", model.state.value.password)

        fail = false
        // The field is not drawn any more; even a stale value is not used.
        model.onUsernameChange("someone-else")
        model.onCreate()
        advanceUntilIdle()
        assertEquals(listOf("mira" to "correct horse", "mira" to "correct horse"), claims)
        assertNull(model.state.value.error)
    }

    @Test
    fun `an account whose phrase is gone can only be declined`() = runTest(dispatcher) {
        val model = vm(claim = { _, _ -> throw GuestPhraseUnavailable() })
        model.fill()
        model.onCreate()
        advanceUntilIdle()
        val s = model.state.value
        assertTrue(s.cannotKeep)
        assertEquals(GuestOfferStep.ASK, s.step)
        assertEquals(UiText("guest.claim.error.phrase_missing"), s.error)
        // `Create account` is not offered again.
        model.onChooseCreate()
        assertEquals(GuestOfferStep.ASK, model.state.value.step)
    }

    @Test
    fun `No, thanks deletes at once, with no step in between`() = runTest(dispatcher) {
        var discarded = 0
        val model = vm(discard = { discarded += 1 })
        model.onDecline()
        assertEquals(GuestOfferBusy.DELETING, model.state.value.busy)
        advanceUntilIdle()
        assertEquals(1, discarded)
        assertFalse(model.state.value.deleteFailed)
    }

    @Test
    fun `a failed delete says so and leaves both buttons`() = runTest(dispatcher) {
        val model = vm(discard = { throw java.io.IOException("offline") })
        model.onDecline()
        advanceUntilIdle()
        val s = model.state.value
        assertTrue(s.deleteFailed)
        assertEquals(GuestOfferBusy.NONE, s.busy)
        assertFalse(s.cannotKeep)
        assertEquals(GuestOfferStep.ASK, s.step)
    }

    @Test
    fun `a second press while busy does nothing`() = runTest(dispatcher) {
        var discarded = 0
        val gate = CompletableDeferred<Unit>()
        val model = vm(discard = {
            discarded += 1
            gate.await()
        })
        model.onDecline()
        advanceUntilIdle()
        model.onDecline()
        model.onChooseCreate()
        advanceUntilIdle()
        assertEquals(1, discarded)
        assertEquals(GuestOfferStep.ASK, model.state.value.step)
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(GuestOfferBusy.NONE, model.state.value.busy)
    }

    @Test
    fun `claim errors read as the form's sentences`() {
        val generic = UiText("guest.claim.error.generic")
        val taken = UiText("auth.signup.error.username_taken")
        assertEquals(UiText("guest.claim.error.phrase_missing"), guestClaimErrorText(GuestPhraseUnavailable()))
        // The claim's own sentence for a taken name.
        assertEquals(taken, guestClaimErrorText(AuthClientArgError("taken", taken)))
        assertEquals(generic, guestClaimErrorText(AuthClientArgError("no guest account to keep")))
        assertEquals(taken, guestClaimErrorText(AuthHttpError(409, "/auth/login-handle", "")))
        assertEquals(taken, guestClaimErrorText(DiscoveryHttpError(409, "/discovery/register", "")))
        assertEquals(
            UiText("common.error.unavailable"),
            guestClaimErrorText(AuthHttpError(503, "/password/reset/start", "")),
        )
        assertEquals(
            UiText("common.error.too_many_attempts"),
            guestClaimErrorText(AuthHttpError(429, "/password/reset/start", "", code = "too-many-attempts")),
        )
        assertEquals(
            UiText("common.error.too_many_attempts"),
            guestClaimErrorText(DiscoveryHttpError(429, "/discovery/register", "")),
        )
        val offline = UiText("common.error.offline")
        assertEquals(offline, guestClaimErrorText(AuthNetworkError("/x", java.io.IOException("down"))))
        assertEquals(offline, guestClaimErrorText(DiscoveryNetworkError("/x", java.io.IOException("down"))))
        assertEquals(generic, guestClaimErrorText(AuthHttpError(500, "/password/reset/finish", "")))
        assertEquals(generic, guestClaimErrorText(IllegalStateException("anything else")))
    }
}
