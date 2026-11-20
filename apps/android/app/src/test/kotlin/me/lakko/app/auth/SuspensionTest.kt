// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.File
import java.util.Base64
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0098 section 4 - a suspended account, seen by its owner: the wire
 * parsing, the reason mapping, the appeal sheet's bounds and the suspended
 * mode's state machine (enter, probe, leave, the ten-minute check, the one
 * appeal), plus the token provider's part (a refresh answering the
 * suspension).
 */
class SuspensionTest {

    private val rawAccount = ByteArray(16) { 0xFB.toByte() }
    private val urlAccount = Base64.getUrlEncoder().withoutPadding().encodeToString(rawAccount)
    private val stdAccount = Base64.getEncoder().encodeToString(rawAccount)
    private val dev = "D".repeat(43)

    @After
    fun tearDown() {
        SuspensionGate.set(false)
    }

    private fun creds(n: Int, grant: String? = null) = SuspendedCredentials(
        accountIdB64 = stdAccount,
        restrictedToken = "s1.$urlAccount.1.$dev.1.9.restricted$n",
        restrictedExpiresAtMs = 1_000_000L + n,
        refreshToken = "r2.$urlAccount.1.$dev.1.9.refresh$n",
        refreshExpiresAtMs = 9_000_000L + n,
        keyGeneration = 1L,
        freshGrant = grant,
        freshGrantExpiresAtMs = grant?.let { 2_000_000L },
    )

    private fun body(withGrant: Boolean): String {
        val grant = if (withGrant) ""","fresh_grant":"k1.$urlAccount.1.5.${"g".repeat(43)}","fresh_grant_expires_at_ms":5""" else ""
        return """{"error":"account-suspended","account_id_b64":"$urlAccount",""" +
            """"restricted_token":"s1.$urlAccount.1.$dev.1700000000000.1700000900000.${"t".repeat(43)}",""" +
            """"restricted_expires_at_ms":1700000900000,""" +
            """"refresh_token":"r2.$urlAccount.1.$dev.1700000000000.1702592000000.${"u".repeat(43)}",""" +
            """"refresh_expires_at_ms":1702592000000,"key_generation":1$grant}"""
    }

    // ---- wire --------------------------------------------------------------

    @Test
    fun `a landing's 403 body gives the credentials, the account in the standard spelling`() {
        val text = body(withGrant = true)
        assertTrue("the body is longer than an error copy keeps", text.length > 512)
        val parsed = parseSuspendedBody(text)!!
        assertEquals(stdAccount, parsed.accountIdB64)
        assertTrue(parsed.restrictedToken.startsWith("s1."))
        assertTrue(parsed.refreshToken.startsWith("r2."))
        assertEquals(1700000900000L, parsed.restrictedExpiresAtMs)
        assertEquals(1L, parsed.keyGeneration)
        assertNotNull(parsed.freshGrant)
        assertEquals(5L, parsed.freshGrantExpiresAtMs)
        assertEquals(parsed.restrictedToken, parsed.asSessionPair().accessToken)
        assertEquals(parsed.refreshToken, parsed.asSessionPair().refreshToken)
        // Never printed.
        assertFalse(parsed.toString().contains("s1."))
        assertFalse(parsed.toString().contains("r2."))
    }

    @Test
    fun `a bare refusal, another error or a broken body carries no credentials`() {
        assertNull(parseSuspendedBody("""{"error":"account-suspended"}"""))
        assertNull(parseSuspendedBody(body(false).replace("account-suspended", "fresh-auth-required")))
        assertNull(parseSuspendedBody(body(false).replace("\"s1.", "\"v3.")))
        assertNull(parseSuspendedBody("not json"))
        assertNull(parseSuspendedBody(null))
        assertNotNull(parseSuspendedBody(body(false)))
        assertNull(parseSuspendedBody(body(false))!!.freshGrant)
        assertTrue(isAccountSuspended(403, ACCOUNT_SUSPENDED))
        assertFalse(isAccountSuspended(401, ACCOUNT_SUSPENDED))
        assertFalse(isAccountSuspended(403, "device-revoked"))
    }

    @Test
    fun `the suspension status is read from its three shapes`() {
        assertEquals(SuspensionStatusAnswer.NotSuspended, parseSuspensionStatus("""{"suspended":false}"""))
        assertEquals(
            SuspensionStatusAnswer.Suspended(SuspensionDetails("child-safety", 1234L, AppealState.SENT)),
            parseSuspensionStatus(
                """{"suspended":true,"reason":5,"reason_name":"child-safety","until_ms":1234,"appeal":"sent"}""",
            ),
        )
        assertEquals(
            SuspensionStatusAnswer.Suspended(SuspensionDetails("spam", null, AppealState.DECLINED)),
            parseSuspensionStatus("""{"suspended":true,"reason":1,"reason_name":"spam","until_ms":null,"appeal":"declined"}"""),
        )
        assertEquals(SuspensionStatusAnswer.Unreachable, parseSuspensionStatus("<html>"))
        assertEquals(SuspensionStatusAnswer.Unreachable, parseSuspensionStatus("""{"other":1}"""))
    }

    // ---- reason mapping ----------------------------------------------------

    @Test
    fun `every reason name maps to the report sheet's catalog key, an unknown one to other`() {
        assertEquals("report.reason.child_safety", suspendedReasonKey("child-safety"))
        assertEquals("report.reason.spam", suspendedReasonKey("spam"))
        assertEquals("report.reason.other", suspendedReasonKey("something-new"))
        assertEquals("report.reason.other", suspendedReasonKey(null))
        assertEquals("report.reason.harassment", suspendedReasonKey(" Harassment "))
        // Each key exists in the compiled source catalog the app ships.
        val catalog = File("src/main/assets/l10n/source.json").readText()
        val keys = (Json.parseToJsonElement(catalog) as JsonObject).let { root ->
            (root["messages"] as? JsonObject ?: root).keys
        }
        for (name in SUSPENSION_REASON_NAMES) {
            val key = suspendedReasonKey(name)
            assertTrue("missing $key", key in keys)
            // The same key the report sheet builds.
            assertEquals(me.lakko.app.moderation.reasonKey(name), key)
        }
    }

    // ---- the appeal sheet --------------------------------------------------

    @Test
    fun `an appeal is 1 to 1000 characters after trimming, the field stops at 1000`() {
        val empty = AppealSheetState()
        assertFalse(empty.canSend)
        assertFalse(empty.withText("   \n ").canSend)
        assertTrue(empty.withText(" x ").canSend)
        assertEquals(1, empty.withText(" x ").length)
        val long = empty.withText("a".repeat(1500))
        assertEquals(APPEAL_MAX_CHARS, long.length)
        assertTrue(long.canSend)
        // Characters, not UTF-16 units: an emoji counts once.
        val emoji = "😀".repeat(1200)
        val clamped = empty.withText(emoji)
        assertEquals(APPEAL_MAX_CHARS, clamped.length)
        assertFalse(empty.withText("x").copy(phase = AppealPhase.SENDING).canSend)
        assertNull(appealPhaseAfter(AppealAnswer.SENT))
        assertEquals(AppealPhase.FAILED, appealPhaseAfter(AppealAnswer.FAILED))
        assertEquals(AppealPhase.UNAVAILABLE, appealPhaseAfter(AppealAnswer.UNAVAILABLE))
    }

    // ---- the screen's second button and the counter --------------------------

    @Test
    fun `Delete account replaces Read my chats exactly when the chats cannot be opened`() {
        for (canRead in listOf(true, false)) {
            for (reading in listOf(true, false)) {
                for (details in listOf(SuspensionDetailsPhase.Loading, SuspensionDetailsPhase.Failed)) {
                    val ui = SuspensionUi(stdAccount, reading = reading, canRead = canRead, details = details)
                    assertEquals(
                        if (canRead) SuspendedSecondaryAction.READ_CHATS else SuspendedSecondaryAction.DELETE_ACCOUNT,
                        suspendedSecondaryAction(ui),
                    )
                }
            }
        }
        // Through the controller: a fresh device's entry offers Delete account.
        val fresh = SuspensionController(FakePorts(), FakeEvents())
        fresh.enter(creds(1), canRead = false)
        assertEquals(SuspendedSecondaryAction.DELETE_ACCOUNT, suspendedSecondaryAction(fresh.ui.value!!))
        val withChats = SuspensionController(FakePorts(), FakeEvents())
        withChats.enter(creds(1), canRead = true)
        assertEquals(SuspendedSecondaryAction.READ_CHATS, suspendedSecondaryAction(withChats.ui.value!!))
    }

    @Test
    fun `the appeal counter is written in the interface language's digits`() {
        val region = java.util.Locale.US
        assertEquals("12 / 1,000", appealCounterText(12, java.util.Locale.ENGLISH, region))
        for (tag in listOf("fa", "ar", "hi")) {
            val words = java.util.Locale.forLanguageTag(tag)
            val zero = java.text.DecimalFormatSymbols.getInstance(words).zeroDigit
            val text = appealCounterText(12, words, region)
            val expectedTwelve = String(charArrayOf(zero + 1, zero + 2))
            assertTrue("$tag: $text", text.startsWith(expectedTwelve + " / "))
            assertTrue("$tag: $text", text.contains(zero))
        }
        // At least one of those languages writes non-Latin digits on this JVM,
        // or the test above proves nothing about digits.
        assertTrue(
            listOf("fa", "ar", "hi").any {
                java.text.DecimalFormatSymbols.getInstance(java.util.Locale.forLanguageTag(it)).zeroDigit != '0'
            },
        )
    }

    // ---- the controller ----------------------------------------------------

    private class FakePorts : SuspensionPorts {
        val refreshAnswers = ArrayDeque<SuspensionRefresh>()
        var status: SuspensionStatusAnswer =
            SuspensionStatusAnswer.Suspended(SuspensionDetails("spam", null, AppealState.NONE))
        var appealAnswer = AppealAnswer.SENT
        var refreshCalls = 0
        val refreshedWith = mutableListOf<String>()
        var appealCalls = 0
        val saved = mutableListOf<SuspendedCredentials>()
        val cleared = mutableListOf<String>()

        override suspend fun refresh(refreshToken: String): SuspensionRefresh {
            refreshCalls += 1
            refreshedWith += refreshToken
            return refreshAnswers.removeFirstOrNull() ?: SuspensionRefresh.Unreachable
        }

        override suspend fun status(restrictedToken: String): SuspensionStatusAnswer = status

        override suspend fun appeal(restrictedToken: String, text: String): AppealAnswer {
            appealCalls += 1
            return appealAnswer
        }

        override fun save(credentials: SuspendedCredentials) {
            saved += credentials
        }

        override fun clear(accountIdB64: String) {
            cleared += accountIdB64
        }
    }

    private class FakeEvents : SuspensionEvents {
        val credentials = mutableListOf<SuspendedCredentials>()
        val resumed = mutableListOf<SessionPair>()
        val refused = mutableListOf<SessionRefusal>()
        override fun onCredentials(credentials: SuspendedCredentials) {
            this.credentials += credentials
        }
        override fun onResumed(accountIdB64: String, pair: SessionPair) {
            resumed += pair
        }
        override fun onRefused(accountIdB64: String, refusal: SessionRefusal) {
            refused += refusal
        }
    }

    private val ports = FakePorts()
    private val events = FakeEvents()
    private val controller = SuspensionController(ports, events)

    private fun normalPair() = SessionPair(
        accessToken = "v3.$urlAccount.1.$dev.1.9.access",
        accessExpiresAtMs = 5L,
        refreshToken = "r2.$urlAccount.1.$dev.1.9.next",
        refreshExpiresAtMs = 6L,
        keyGeneration = 1L,
    )

    @Test
    fun `a landing's refusal enters suspended mode on the screen, persisted and served`() {
        controller.enter(creds(1, grant = "k1.x"), canRead = true)
        val ui = controller.ui.value!!
        assertEquals(stdAccount, ui.accountIdB64)
        assertFalse(ui.reading)
        assertTrue(ui.canRead)
        assertEquals(SuspensionDetailsPhase.Loading, ui.details)
        assertEquals(listOf(creds(1, grant = "k1.x")), ports.saved)
        assertEquals(listOf(creds(1, grant = "k1.x")), events.credentials)
        assertTrue(controller.active)
    }

    @Test
    fun `a fresh device shows the screen alone - Read my chats does nothing there`() {
        controller.enter(creds(1), canRead = false)
        controller.readChats()
        assertFalse(controller.ui.value!!.reading)
    }

    @Test
    fun `Read my chats and back, and new credentials keep the person reading`() {
        controller.enter(creds(1), canRead = true)
        controller.readChats()
        assertTrue(controller.ui.value!!.reading)
        controller.enter(creds(2), canRead = true)
        assertTrue(controller.ui.value!!.reading)
        controller.showScreen()
        assertFalse(controller.ui.value!!.reading)
    }

    @Test
    fun `a probe still suspended takes the new credentials and keeps the landing's grant`() = runTest {
        controller.enter(creds(1, grant = "k1.g"), canRead = true)
        ports.refreshAnswers += SuspensionRefresh.Suspended(creds(2))
        assertTrue(controller.probe())
        assertEquals(listOf(creds(1).refreshToken), ports.refreshedWith)
        val held = controller.credentials()!!
        assertEquals(creds(2).restrictedToken, held.restrictedToken)
        assertEquals("k1.g", held.freshGrant)
        assertEquals(held, ports.saved.last())
        assertEquals(held, events.credentials.last())
    }

    @Test
    fun `a normal pair leaves suspended mode silently`() = runTest {
        controller.enter(creds(1), canRead = true)
        controller.readChats()
        ports.refreshAnswers += SuspensionRefresh.Resumed(normalPair())
        assertFalse(controller.probe())
        assertNull(controller.ui.value)
        assertFalse(controller.active)
        assertEquals(listOf(stdAccount), ports.cleared)
        assertEquals(listOf(normalPair()), events.resumed)
    }

    @Test
    fun `a refused refresh credential ends the mode and is reported`() = runTest {
        controller.enter(creds(1), canRead = true)
        ports.refreshAnswers += SuspensionRefresh.Refused(SessionRefusal.RefreshInvalid)
        assertFalse(controller.probe())
        assertNull(controller.ui.value)
        assertEquals(listOf(SessionRefusal.RefreshInvalid), events.refused)
    }

    @Test
    fun `offline, the mode stays as it is`() = runTest {
        controller.enter(creds(1), canRead = true)
        assertTrue(controller.probe())
        assertEquals(creds(1), controller.credentials())
    }

    @Test
    fun `the check runs now and every ten minutes until the suspension ends`() = runTest {
        controller.enter(creds(1), canRead = true)
        ports.refreshAnswers += SuspensionRefresh.Suspended(creds(2))
        ports.refreshAnswers += SuspensionRefresh.Suspended(creds(3))
        ports.refreshAnswers += SuspensionRefresh.Resumed(normalPair())
        val waits = mutableListOf<Long>()
        controller.keepChecking(wait = { waits += it })
        assertEquals(listOf(SUSPENSION_PROBE_INTERVAL_MS, SUSPENSION_PROBE_INTERVAL_MS), waits)
        assertEquals(10L * 60L * 1000L, SUSPENSION_PROBE_INTERVAL_MS)
        assertEquals(3, ports.refreshCalls)
        assertNull(controller.ui.value)
    }

    @Test
    fun `the details load after a probe, and a failure says so until a retry`() = runTest {
        controller.enter(creds(1), canRead = true)
        ports.status = SuspensionStatusAnswer.Unreachable
        controller.loadDetails()
        assertEquals(SuspensionDetailsPhase.Failed, controller.ui.value!!.details)
        ports.status = SuspensionStatusAnswer.Suspended(SuspensionDetails("hate", 77L, AppealState.NONE))
        controller.loadDetails()
        assertEquals(
            SuspensionDetailsPhase.Loaded(SuspensionDetails("hate", 77L, AppealState.NONE)),
            controller.ui.value!!.details,
        )
    }

    @Test
    fun `an appeal is sent once - after it the screen reads sent and nothing goes out again`() = runTest {
        controller.enter(creds(1), canRead = true)
        controller.loadDetails()
        assertEquals(AppealAnswer.SENT, controller.sendAppeal("  I did not do this  "))
        val details = (controller.ui.value!!.details as SuspensionDetailsPhase.Loaded).details
        assertEquals(AppealState.SENT, details.appeal)
        assertEquals(AppealAnswer.SENT, controller.sendAppeal("again"))
        assertEquals(1, ports.appealCalls)
    }

    @Test
    fun `an appeal that failed can be sent again, an empty one is never sent`() = runTest {
        controller.enter(creds(1), canRead = true)
        controller.loadDetails()
        assertEquals(AppealAnswer.FAILED, controller.sendAppeal("   "))
        assertEquals(0, ports.appealCalls)
        ports.appealAnswer = AppealAnswer.FAILED
        assertEquals(AppealAnswer.FAILED, controller.sendAppeal("text"))
        ports.appealAnswer = AppealAnswer.SENT
        assertEquals(AppealAnswer.SENT, controller.sendAppeal("text"))
        assertEquals(2, ports.appealCalls)
    }

    @Test
    fun `sign-out clears the mode and the record`() {
        controller.enter(creds(1), canRead = false)
        controller.clear()
        assertNull(controller.ui.value)
        assertNull(controller.credentials())
        assertEquals(listOf(stdAccount), ports.cleared)
    }

    // ---- the token provider -------------------------------------------------

    @Test
    fun `a refresh answering the suspension reports it, serves the s1 and refreshes no more`() = runTest {
        val v3 = SessionPair("v3.$urlAccount.1.$dev.1.9.a", 0L, "r2.$urlAccount.1.$dev.1.9.r", 9L, 1L)
        var calls = 0
        val provider = SessionTokenProvider(
            refresher = object : SessionTokenProvider.SessionRefresher {
                override suspend fun refresh(refreshToken: String): SessionPair {
                    calls += 1
                    throw AuthHttpError(403, "/auth/session/refresh", "{}", code = ACCOUNT_SUSPENDED, suspended = creds(1))
                }
            },
            clock = { 100L },
        )
        val suspended = mutableListOf<SuspendedCredentials>()
        val refusals = mutableListOf<SessionRefusal>()
        provider.install(v3, persist = {}, onRefusal = { refusals += it }, onSuspended = { suspended += it })
        // The access token is expired: current() refreshes, learns the suspension.
        val token = provider.current()
        assertEquals(creds(1).restrictedToken, token)
        assertEquals(listOf(creds(1)), suspended)
        assertTrue(provider.isSuspended())
        assertEquals(emptyList<SessionRefusal>(), refusals)
        // From now on nothing refreshes by itself, and a 401 is not retried.
        assertEquals(creds(1).restrictedToken, provider.current())
        assertNull(provider.afterUnauthorized(creds(1).restrictedToken))
        assertEquals(1, calls)
        // The old v3 of the account is still recognised as the account's.
        assertTrue(provider.manages(v3.accessToken))
    }

    @Test
    fun `a bearer route's bare refusal refreshes once to get the credentials`() = runTest {
        val v3 = SessionPair("v3.$urlAccount.1.$dev.1.9.a", Long.MAX_VALUE, "r2.$urlAccount.1.$dev.1.9.r", 9L, 1L)
        var calls = 0
        val provider = SessionTokenProvider(
            refresher = object : SessionTokenProvider.SessionRefresher {
                override suspend fun refresh(refreshToken: String): SessionPair {
                    calls += 1
                    throw AuthHttpError(403, "/auth/session/refresh", "{}", code = ACCOUNT_SUSPENDED, suspended = creds(1))
                }
            },
        )
        val suspended = mutableListOf<SuspendedCredentials>()
        provider.install(v3, persist = {}, onRefusal = {}, onSuspended = { suspended += it })
        provider.suspendedFromBearerRoute()
        provider.suspendedFromBearerRoute()
        assertEquals(1, calls)
        assertEquals(listOf(creds(1)), suspended)
    }

    @Test
    fun `a later refresh with a normal pair reports the end of the suspension`() = runTest {
        var answer: () -> SessionPair = { throw AuthHttpError(403, "/x", "{}", suspended = creds(1)) }
        val provider = SessionTokenProvider(
            refresher = object : SessionTokenProvider.SessionRefresher {
                override suspend fun refresh(refreshToken: String): SessionPair = answer()
            },
        )
        val resumed = mutableListOf<SessionPair>()
        val v3 = SessionPair("v3.$urlAccount.1.$dev.1.9.a", Long.MAX_VALUE, "r2.$urlAccount.1.$dev.1.9.r", 9L, 1L)
        provider.install(v3, persist = {}, onRefusal = {}, onResumed = { resumed += it })
        provider.refreshNow()
        assertTrue(provider.isSuspended())
        answer = { normalPair() }
        assertEquals(normalPair(), provider.refreshNow())
        assertFalse(provider.isSuspended())
        assertEquals(listOf(normalPair()), resumed)
    }

    // ---- the gate -------------------------------------------------------------

    @Test
    fun `while suspended no write reaches the DS or channels, reads still do`() {
        val bases = listOf("https://x.test/native/ds", "https://x.test/native/channels")
        assertFalse(SuspensionGate.refuses("POST", "https://x.test/native/ds/group/aa/messages", bases))
        SuspensionGate.set(true)
        assertTrue(SuspensionGate.refuses("POST", "https://x.test/native/ds/group/aa/messages", bases))
        assertTrue(SuspensionGate.refuses("PUT", "https://x.test/native/ds/group/aa/claim", bases))
        assertTrue(SuspensionGate.refuses("POST", "https://x.test/native/channels/v1/channels/x/posts", bases))
        assertFalse(SuspensionGate.refuses("GET", "https://x.test/native/ds/group/aa/messages", bases))
        assertFalse(SuspensionGate.refuses("POST", "https://x.test/native/auth/account/suspension/appeal", bases))
        assertFalse(SuspensionGate.refuses("POST", "https://x.test/native/discovery/x", bases))
    }
}
