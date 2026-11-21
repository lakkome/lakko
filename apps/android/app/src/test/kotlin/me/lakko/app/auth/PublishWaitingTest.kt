// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.FakeAnonTokenSource
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.discovery.IdentityUploadRequest
import me.lakko.app.discovery.TEST_INVITE_LINK_LENGTHS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * UX identity-recovery 15.4 "Waiting for the directory": a reset's publish
 * refused by discovery's cool-down shows how long is left, no Try again, and
 * retries by itself; every other refusal keeps the "could not finish" copy
 * and Try again.
 */
class PublishWaitingTest {

    private val t0 = 1_800_000_000_000L

    private fun cooldown(secs: Long?, code: String = DISCOVERY_IDENTITY_CHANGED_TOO_RECENTLY): PublishOwedReason {
        val body = if (secs == null) """{"error":"$code"}""" else """{"error":"$code","retry_after_secs":$secs}"""
        val err = DiscoveryHttpError(429, "/discovery/identity", body)
        val outcome = discoveryResetOutcome(err) as KeyResetStepOutcome.Retry
        return PublishOwedReason.Retryable(outcome.cause, outcome.retryAfterSecs, observedAtMs = t0)
    }

    @Test
    fun `the cool-down shows the minutes left and no Try again`() {
        val model = publishOwedScreenModel(cooldown(2400), wordless = false, nowMs = t0)
        assertEquals(40L, model.waitingMinutes)
        assertEquals("recovery_phrase.fresh.publish_waiting", model.waitingKey)
        assertFalse(model.showTryAgain)
        assertEquals(emptyList<String>(), model.leadKeys)
        assertEquals(t0 + 2_400_000L, model.autoRetryAtMs)
    }

    @Test
    fun `minutes round up, count down, and never read below one`() {
        val reason = cooldown(61)
        assertEquals(2L, publishOwedScreenModel(reason, false, t0).waitingMinutes)
        assertEquals(1L, publishOwedScreenModel(reason, false, t0 + 1_000L).waitingMinutes)
        assertEquals(1L, publishOwedScreenModel(reason, false, t0 + 61_000L).waitingMinutes)
        assertEquals(1L, publishOwedScreenModel(reason, false, t0 + 600_000L).waitingMinutes)
        assertEquals(1L, publishOwedScreenModel(cooldown(1), false, t0).waitingMinutes)
        assertEquals(60L, publishOwedScreenModel(cooldown(3600), false, t0).waitingMinutes)
    }

    @Test
    fun `the wait counts from when the refusal was seen`() {
        val reason = cooldown(600)
        // Drawn five minutes later (the screen opened late): five minutes left.
        assertEquals(5L, publishOwedScreenModel(reason, false, t0 + 300_000L).waitingMinutes)
        assertEquals(t0 + 600_000L, publishOwedScreenModel(reason, false, t0 + 300_000L).autoRetryAtMs)
    }

    @Test
    fun `a cool-down with no readable wait keeps the could-not-finish copy and Try again`() {
        val model = publishOwedScreenModel(cooldown(null), wordless = false, nowMs = t0)
        assertNull(model.waitingMinutes)
        assertTrue(model.showTryAgain)
        assertNull(model.autoRetryAtMs)
        assertEquals(listOf("recovery_phrase.fresh.publish_owed"), model.leadKeys)
        assertNull(publishOwedScreenModel(cooldown(0), false, t0).waitingMinutes)
    }

    @Test
    fun `another 429 is not the waiting state`() {
        val model = publishOwedScreenModel(cooldown(600, code = "rate-limited"), wordless = false, nowMs = t0)
        assertNull(model.waitingMinutes)
        assertTrue(model.showTryAgain)
        assertNull(model.autoRetryAtMs)
    }

    @Test
    fun `a network failure or another refusal keeps Try again`() {
        val offline = PublishOwedReason.Retryable(
            DiscoveryNetworkError("/discovery/identity", java.io.IOException("no route")),
            observedAtMs = t0,
        )
        val net = publishOwedScreenModel(offline, false, t0)
        assertTrue(net.showTryAgain)
        assertNull(net.waitingMinutes)
        assertEquals(listOf("recovery_phrase.fresh.publish_owed"), net.leadKeys)

        val unavailable = PublishOwedReason.Retryable(
            DiscoveryHttpError(503, "/discovery/identity", """{"error":"key-reset-disabled"}"""),
            observedAtMs = t0,
        )
        assertTrue(publishOwedScreenModel(unavailable, false, t0).showTryAgain)

        val refused = publishOwedScreenModel(PublishOwedReason.Refused(IllegalStateException()), false, t0)
        assertTrue(refused.showTryAgain)
        assertEquals(
            listOf("recovery_phrase.fresh.publish_owed", "recovery_phrase.fresh.publish_refused"),
            refused.leadKeys,
        )
        val signIn = publishOwedScreenModel(PublishOwedReason.SignInRequired, false, t0)
        assertFalse(signIn.showTryAgain)
        assertNull(signIn.autoRetryAtMs)
    }

    @Test
    fun `an account with no phrase waits with its own sentence and the same minutes`() {
        val model = publishOwedScreenModel(cooldown(600), wordless = true, nowMs = t0)
        assertEquals(emptyList<String>(), model.leadKeys)
        assertEquals("account.keys_changed.waiting", model.waitingKey)
        assertEquals(10L, model.waitingMinutes)
        assertFalse(model.showTryAgain)
        assertEquals(t0 + 600_000L, model.autoRetryAtMs)
        assertEquals(1L, publishOwedScreenModel(cooldown(600), wordless = true, nowMs = t0 + 599_000L).waitingMinutes)
        // Outside the cool-down the wordless account keeps its finishing copy.
        val other = publishOwedScreenModel(cooldown(null), wordless = true, nowMs = t0)
        assertEquals(listOf("account.keys_changed.finishing"), other.leadKeys)
        assertNull(other.waitingKey)
        assertTrue(other.showTryAgain)
    }

    @Test
    fun `the wait is read from Retry-After when the body carries none`() = runTest {
        val engine = MockEngine {
            respond(
                content = """{"error":"identity-changed-too-recently"}""",
                status = HttpStatusCode.TooManyRequests,
                headers = headersOf(
                    HttpHeaders.ContentType to listOf("application/json"),
                    HttpHeaders.RetryAfter to listOf("1200"),
                ),
            )
        }
        DiscoveryClient(
            baseUrl = "http://test.invalid",
            engine = engine,
            inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS },
            anonTokens = FakeAnonTokenSource(),
        ).use { client ->
            try {
                client.identityUpload(IdentityUploadRequest(account_id = "A", signature_pubkey = "S", x25519_pubkey = "X", x25519_binding = "B"))
                fail("expected a 429")
            } catch (err: DiscoveryHttpError) {
                assertEquals(429, err.status)
                assertEquals(DISCOVERY_IDENTITY_CHANGED_TOO_RECENTLY, err.code)
                assertEquals(1200L, err.retryAfterSecs)
                assertEquals(
                    20L,
                    publishOwedScreenModel(
                        PublishOwedReason.Retryable(err, err.retryAfterSecs, t0),
                        false,
                        t0,
                    ).waitingMinutes,
                )
            }
        }
    }

    @Test
    fun `the body's wait wins over the header`() = runTest {
        val engine = MockEngine {
            respond(
                content = """{"error":"identity-changed-too-recently","retry_after_secs":90}""",
                status = HttpStatusCode.TooManyRequests,
                headers = headersOf(
                    HttpHeaders.ContentType to listOf("application/json"),
                    HttpHeaders.RetryAfter to listOf("1200"),
                ),
            )
        }
        DiscoveryClient(
            baseUrl = "http://test.invalid",
            engine = engine,
            inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS },
            anonTokens = FakeAnonTokenSource(),
        ).use { client ->
            try {
                client.identityUpload(IdentityUploadRequest(account_id = "A", signature_pubkey = "S", x25519_pubkey = "X", x25519_binding = "B"))
                fail("expected a 429")
            } catch (err: DiscoveryHttpError) {
                assertEquals(90L, err.retryAfterSecs)
            }
        }
    }
}
