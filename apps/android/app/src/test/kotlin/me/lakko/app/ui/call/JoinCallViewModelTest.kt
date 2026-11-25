// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.GuestSession
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.SessionAccount
import me.lakko.app.calls.HostedCallsTest
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.l10n.EnglishForTests
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The call-link landing and a guest account (`docs/ux/call-links.md` section
 * 9): Join mints one guest account and goes into the call, and after
 * `No, thanks` deleted that account on this same screen, Join mints another.
 *
 * The second half is the defect found live on web on 2026-10-02: the
 * double-tap latch set by the guest signup was never released after a
 * successful signup, so Join did nothing once the account was deleted.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class JoinCallViewModelTest {
    init {
        // The guest name field is prefilled from the catalog.
        EnglishForTests.install()
    }

    private val dispatcher = StandardTestDispatcher()

    /** The call host's own scope, on the test scheduler. Not the test's
     *  background scope: `advanceUntilIdle` does not run a background task
     *  nothing in the foreground is waiting for, and the landing awaits the
     *  host from the view model's scope. */
    private val hostScope = CoroutineScope(dispatcher + SupervisorJob())

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        hostScope.cancel()
        Dispatchers.resetMain()
    }

    /** The auth layer as the landing sees it: a guest signup lands a session
     *  at once, marked as a guest; `No, thanks` ends it. */
    private class FakeAuth : JoinCallAuth {
        override val state = MutableStateFlow<AuthState>(AuthState.Anon())
        override val guestAccount = MutableStateFlow<GuestSession?>(null)
        override val identitySecrets = MutableStateFlow<IdentitySecrets?>(null)

        var signups = 0

        /** When set, a signup waits here (the proof of work). */
        var gate: CompletableDeferred<Unit>? = null
        var failure: Throwable? = null

        override suspend fun signupGuest(onProofSolved: () -> Unit) {
            signups += 1
            val n = signups
            gate?.await()
            failure?.let { throw it }
            onProofSolved()
            val accountIdB64 = "acct-$n"
            // The repository's order: the guest flag, the secrets, the state.
            guestAccount.value = GuestSession(accountIdB64 = accountIdB64, markedAtMs = 1L, claimedUsername = null)
            identitySecrets.value = IdentitySecrets(
                accountId = ByteArray(16) { n.toByte() },
                exportKey = ByteArray(32),
                accountIdentitySeed = ByteArray(64),
                session = me.lakko.app.auth.SessionPair("token-$n", Long.MAX_VALUE, "refresh-$n", Long.MAX_VALUE, 1L),
            )
            state.value = AuthState.Session(SessionAccount(accountIdB64 = accountIdB64, username = ""))
        }

        /** `No, thanks`: the account is deleted and the device signed out
         *  (state first, then the guest flag, as the repository does). */
        fun discard() {
            identitySecrets.value = null
            state.value = AuthState.Anon()
            guestAccount.value = null
        }
    }

    private class Harness(
        val model: JoinCallViewModel,
        val auth: FakeAuth,
        val host: LinkCallHost,
        val port: HostedCallsTest.FakeLinkPort,
        val savedNames: MutableList<String>,
    )

    private val call = DecodedCall(
        callId = ByteArray(16) { 0x11 },
        ownerAccountId = ByteArray(16),
        ownerSignatureKey = ByteArray(32),
        ownerFingerprint = "fingerprint",
        mediaVideo = false,
        valid = true,
        isOwner = false,
    )

    /** How many guest names were drawn (each prefill is `Guest <n>`). */
    private var draws = 0

    private fun TestScope.harness(
        decodeLink: suspend (String, me.lakko.app.auth.IdentitySecrets?) -> CallLinkDecode =
            { _, _ -> CallLinkDecode.Opened(call) },
        probeRoom: suspend (ByteArray) -> me.lakko.app.calls.SfuCallState? = { null },
    ): Harness {
        val auth = FakeAuth()
        val port = HostedCallsTest.FakeLinkPort()
        val host = LinkCallHost(
            scope = hostScope,
            otherOccupant = { null },
            onSlotChanged = {},
            warn = { _, _ -> },
        )
        host.attach(port)
        val savedNames = mutableListOf<String>()
        val model = JoinCallViewModel(
            auth = auth,
            slug = "slug#secret",
            host = host,
            endCurrentCallAction = {},
            decodeLink = decodeLink,
            saveGuestName = { _, name -> savedNames += name },
            // The core's rule, as far as these tests need it: trimmed, blank
            // is "no name", a line break is refused.
            normalizeDisplayName = { typed ->
                if ('\n' in typed) throw IllegalArgumentException("line break")
                typed.trim().ifEmpty { null }
            },
            drawGuestName = { "Guest ${++draws}" },
            probeRoom = probeRoom,
        )
        advanceUntilIdle()
        return Harness(model, auth, host, port, savedNames)
    }

    /** What `JoinCallRoot` does when the auth state becomes a session. */
    private fun TestScope.landSession(h: Harness) {
        h.model.onSessionLanded()
        advanceUntilIdle()
    }

    @Test
    fun `a link that could not be fetched is offered again, and Try again opens it`() = runTest(dispatcher) {
        var answers = listOf<CallLinkDecode>(CallLinkDecode.OpenFailed, CallLinkDecode.Opened(call))
        val h = harness(decodeLink = { _, _ -> answers.first().also { answers = answers.drop(1) } })
        val failed = h.model.uiState.value
        assertTrue(failed.openFailed)
        assertFalse(failed.loading)
        assertEquals(null, failed.decoded)

        h.model.retryDecode()
        advanceUntilIdle()
        val opened = h.model.uiState.value
        assertFalse(opened.openFailed)
        assertEquals(call, opened.decoded)
    }

    @Test
    fun `Join with no session mints a guest account and goes into the call`() = runTest(dispatcher) {
        val h = harness()
        assertTrue(h.model.needsGuestName())

        h.model.onStart()
        advanceUntilIdle()
        assertEquals(1, h.auth.signups)
        landSession(h)

        // The call was asked for with the guest's session, and the name the
        // person typed was recorded for the new account.
        assertEquals(1, h.port.handles.size)
        assertEquals(1, h.savedNames.size)
        val ui = h.model.uiState.value
        assertTrue(ui.guest)
        assertFalse(ui.guestDeleted)
        assertFalse(ui.guestWorking)
    }

    @Test
    fun `a second tap while the guest account is being made does not make another`() = runTest(dispatcher) {
        val h = harness()
        h.auth.gate = CompletableDeferred()

        h.model.onStart()
        advanceUntilIdle()
        h.model.onStart()
        advanceUntilIdle()

        assertEquals(1, h.auth.signups)
        h.auth.gate?.complete(Unit)
        advanceUntilIdle()
        assertEquals(1, h.auth.signups)
    }

    @Test
    fun `after No, thanks on the landing Join mints a new guest on the same screen`() = runTest(dispatcher) {
        val h = harness()
        h.model.onStart()
        advanceUntilIdle()
        landSession(h)
        assertEquals(1, h.port.handles.size)

        // The call is over and the person answers `No, thanks`: the
        // Forget-device orchestration frees the call slot, then the account
        // is gone and the device signed out.
        h.host.leave()
        h.auth.discard()
        advanceUntilIdle()

        val afterDelete = h.model.uiState.value
        assertFalse(afterDelete.guest)
        assertTrue(afterDelete.guestDeleted)
        assertEquals(JoinCallPhase.Idle, afterDelete.phase)
        assertTrue(h.model.needsGuestName())

        // Join again, on the same screen and the same view model.
        h.model.onStart()
        advanceUntilIdle()
        assertEquals(2, h.auth.signups)
        landSession(h)
        assertEquals(2, h.port.handles.size)

        val again = h.model.uiState.value
        assertTrue(again.guest)
        // The line about the deleted account goes once there is a guest again.
        assertFalse(again.guestDeleted)
    }

    @Test
    fun `a failed guest signup releases the latch and Join can be pressed again`() = runTest(dispatcher) {
        val h = harness()
        h.auth.failure = java.io.IOException("offline")
        h.model.onStart()
        advanceUntilIdle()
        assertEquals(JoinCallPhase.Error, h.model.uiState.value.phase)

        h.auth.failure = null
        h.model.onTryAgain()
        h.model.onStart()
        advanceUntilIdle()
        assertEquals(2, h.auth.signups)
    }

    @Test
    fun `a cancelled signup that ends late does not release the latch of the next one`() = runTest(dispatcher) {
        val h = harness()
        val first = CompletableDeferred<Unit>()
        h.auth.gate = first
        h.model.onStart()
        advanceUntilIdle()
        // Cancel, then Join again before the first coroutine has unwound.
        h.model.onCancelGuest()
        val second = CompletableDeferred<Unit>()
        h.auth.gate = second
        h.model.onStart()
        advanceUntilIdle()
        assertEquals(2, h.auth.signups)
        // The second signup is still running: a third tap must not start one.
        h.model.onStart()
        advanceUntilIdle()
        assertEquals(2, h.auth.signups)
        second.complete(Unit)
        advanceUntilIdle()
    }

    /** A guest account from an earlier link, still on the device. */
    private fun returningGuest(h: Harness) {
        h.auth.guestAccount.value = GuestSession(accountIdB64 = "acct-0", markedAtMs = 1L, claimedUsername = null)
        h.auth.identitySecrets.value = IdentitySecrets(
            accountId = ByteArray(16),
            exportKey = ByteArray(32),
            accountIdentitySeed = ByteArray(64),
            session = me.lakko.app.auth.SessionPair("token-0", Long.MAX_VALUE, "refresh-0", Long.MAX_VALUE, 1L),
        )
        h.auth.state.value = AuthState.Session(SessionAccount(accountIdB64 = "acct-0", username = ""))
    }

    @Test
    fun `a new guest's typed name is also their name in the call`() = runTest(dispatcher) {
        val h = harness()
        h.model.onGuestNameChange("  Captain  ")
        h.model.onStart()
        advanceUntilIdle()
        landSession(h)
        assertEquals("Captain", h.port.requests.single().initialCallName)
    }

    @Test
    fun `a guest from an earlier link is asked again and the name goes to the call, not the profile`() =
        runTest(dispatcher) {
            val h = harness()
            returningGuest(h)
            advanceUntilIdle()
            assertTrue(h.model.uiState.value.guest)
            assertTrue(h.model.needsGuestName())

            h.model.onGuestNameChange("Captain")
            h.model.onStart()
            advanceUntilIdle()
            assertEquals(0, h.auth.signups)
            assertTrue(h.savedNames.isEmpty())
            assertEquals("Captain", h.port.requests.single().initialCallName)
        }

    @Test
    fun `a returning guest cannot join under a name the rule refuses`() = runTest(dispatcher) {
        val h = harness()
        returningGuest(h)
        advanceUntilIdle()
        h.model.onGuestNameChange("two\nlines")
        h.model.onStart()
        advanceUntilIdle()
        assertTrue(h.port.requests.isEmpty())
    }

    @Test
    fun `a real account is not asked and starts under its profile name`() = runTest(dispatcher) {
        val h = harness()
        h.auth.identitySecrets.value = IdentitySecrets(
            accountId = ByteArray(16),
            exportKey = ByteArray(32),
            accountIdentitySeed = ByteArray(64),
            session = me.lakko.app.auth.SessionPair("token-r", Long.MAX_VALUE, "refresh-r", Long.MAX_VALUE, 1L),
        )
        h.auth.state.value = AuthState.Session(SessionAccount(accountIdB64 = "acct-r", username = "anna"))
        advanceUntilIdle()
        assertFalse(h.model.needsGuestName())
        h.model.onStart()
        advanceUntilIdle()
        assertEquals(null, h.port.requests.single().initialCallName)
    }

    @Test
    fun `the link's owner is never asked for a guest name`() = runTest(dispatcher) {
        val h = harness(decodeLink = { _, _ -> CallLinkDecode.Opened(call.copy(isOwner = true)) })
        assertFalse(h.model.needsGuestName())
    }

    @Test
    fun `every open of the link draws a new guest name, and the same open draws none`() = runTest(dispatcher) {
        val h = harness()
        val first = h.model.uiState.value.guestName
        h.model.onLinkOpened(1L)
        advanceUntilIdle()
        // The model's first open keeps the name it was made with.
        assertEquals(first, h.model.uiState.value.guestName)
        h.model.onGuestNameChange("Typed")
        // The page moving between roots, or a rotation: the same open.
        h.model.onLinkOpened(1L)
        advanceUntilIdle()
        assertEquals("Typed", h.model.uiState.value.guestName)
        // The link opened again: a new name, never the earlier one.
        h.model.onLinkOpened(2L)
        advanceUntilIdle()
        val second = h.model.uiState.value.guestName
        assertTrue(second != first && second != "Typed")
        h.model.onLinkOpened(3L)
        advanceUntilIdle()
        assertTrue(h.model.uiState.value.guestName != second)
    }

    // MARK: A link to a call that has already ended (seen live 2026-11-24)

    /** A signed-in person with a real account (not a guest). */
    private fun signIn(auth: FakeAuth) {
        auth.identitySecrets.value = IdentitySecrets(
            accountId = ByteArray(16) { 7 },
            exportKey = ByteArray(32),
            accountIdentitySeed = ByteArray(64),
            session = me.lakko.app.auth.SessionPair("token", Long.MAX_VALUE, "refresh", Long.MAX_VALUE, 1L),
        )
        auth.state.value = AuthState.Session(SessionAccount(accountIdB64 = "acct", username = "someone"))
    }

    @Test
    fun `a CallEnded answer from the SFU on join leaves the page showing the ended state`() = runTest(dispatcher) {
        val h = harness()
        signIn(h.auth)
        advanceUntilIdle()

        h.model.onStart()
        advanceUntilIdle()
        assertEquals(JoinCallPhase.Waiting, h.model.uiState.value.phase)

        // The SFU answered `CallEnded` in place of the Welcome.
        val alreadyEnded = h.port.events?.onCallAlreadyEnded
        assertTrue("the host listens for the refusal", alreadyEnded != null)
        alreadyEnded!!.invoke()
        advanceUntilIdle()

        val phase = h.model.uiState.value.phase
        assertEquals(JoinCallPhase.Spent, phase)
        assertFalse("the page stays, saying the call has ended", joinCallReturnsToApp(phase, guest = false))
    }

    @Test
    fun `a call that ends after joining still returns a signed-in person to the app`() = runTest(dispatcher) {
        val h = harness()
        signIn(h.auth)
        advanceUntilIdle()

        h.model.onStart()
        advanceUntilIdle()
        h.port.events!!.onJoined()
        advanceUntilIdle()
        assertEquals(JoinCallPhase.Live, h.model.uiState.value.phase)
        h.port.events!!.onEnded()
        advanceUntilIdle()

        assertEquals(JoinCallPhase.Ended, h.model.uiState.value.phase)
        assertTrue(joinCallReturnsToApp(JoinCallPhase.Ended, guest = false))
        assertFalse(joinCallReturnsToApp(JoinCallPhase.Ended, guest = true))
    }

    @Test
    fun `a room probe answering ended shows the ended state before Join`() = runTest(dispatcher) {
        val probed = mutableListOf<ByteArray>()
        val h = harness(probeRoom = { id ->
            probed += id
            me.lakko.app.calls.SfuCallState.ENDED
        })
        assertEquals(1, probed.size)
        assertTrue(probed[0].contentEquals(call.callId))
        assertEquals(JoinCallPhase.Spent, h.model.uiState.value.phase)
        assertTrue("nothing was joined", h.port.requests.isEmpty())
    }

    @Test
    fun `a room probe that is not ended changes nothing`() = runTest(dispatcher) {
        for (answer in listOf(
            null,
            me.lakko.app.calls.SfuCallState.UNKNOWN,
            me.lakko.app.calls.SfuCallState.LIVE,
            me.lakko.app.calls.SfuCallState.CLOSING,
        )) {
            val h = harness(probeRoom = { answer })
            assertEquals("answer $answer", JoinCallPhase.Idle, h.model.uiState.value.phase)
        }
    }
}
