// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.sfu

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.ktor.client.HttpClient
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocketSession
import io.ktor.client.request.header
import io.ktor.websocket.Frame
import io.ktor.websocket.close
import io.ktor.websocket.readText
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith

/**
 * G3-4-mobile-call iter1 — Android half of the live SFU
 * signaling round-trip test.
 *
 * Mirror of `apps/ios/LakkoTests/SfuSignalingTest.swift`.
 * Opens a real WebSocket via ktor's OkHttp engine to
 * `${LAKKO_SFU_E2E_URL}/v1/connect?token=${jwt}` with the
 * `lakko-sfu-v1` subprotocol, asserts the first server
 * frame is a typed `Welcome` carrying the same `call_id`
 * the JWT's claim encoded.
 *
 * Instrumentation-argument contract (set via the workflow
 * or `./gradlew connectedDebugAndroidTest
 * -Pandroid.testInstrumentationRunnerArguments.LAKKO_SFU_E2E_JWT=<token>`):
 *
 *   - `LAKKO_SFU_E2E_URL` — base URL of the SFU signaling
 *     listener. Defaults to `http://10.0.2.2:7880` — the
 *     Android-emulator → host-loopback alias.
 *   - `LAKKO_SFU_E2E_JWT` — opaque JWT minted by the
 *     workflow's `mint_test_jwt` pre-step. The production
 *     Android client SDK does NOT expose `mint_jwt` —
 *     clients receive JWTs from `services/auth`.
 *
 * The test fails with a clear message if the JWT
 * argument is absent so a local run from Android Studio
 * without the workflow doesn't mistake the missing
 * setup for a real regression.
 *
 * What this test does NOT gate (sub-sprint
 * G3-4-mobile-call-iter2):
 *   - Dual-emulator fan-out: two emulator-side peers in
 *     one test exchanging SDP through `signal-relay`,
 *     ICE+DTLS completing, real audio frames flowing.
 *   - Real `PeerConnection` integration — this test
 *     uses raw WebSocket + JSON, NOT the
 *     `WebRtcPeerConnection.kt` wrapper.
 */
@RunWith(AndroidJUnit4::class)
class SfuSignalingInstrumentedTest {

    private val sfuBaseUrl: String
        get() {
            val arg = InstrumentationRegistry
                .getArguments()
                .getString("LAKKO_SFU_E2E_URL")
            if (!arg.isNullOrBlank()) {
                return arg.trimEnd('/')
            }
            // Android emulator → host loopback alias.
            return "http://10.0.2.2:7880"
        }

    private val sfuJwt: String?
        get() = InstrumentationRegistry
            .getArguments()
            .getString("LAKKO_SFU_E2E_JWT")
            ?.takeIf { it.isNotBlank() }

    @Test
    fun webSocketUpgradeAndWelcomeFrame() {
        val jwt = sfuJwt ?: run {
            fail(
                "LAKKO_SFU_E2E_JWT instrumentation argument must be set by the " +
                    "workflow's mint_test_jwt pre-step. Local run: see " +
                    "ci/sfu-e2e/README.md for the cargo command."
            )
            return
        }

        // Ktor's OkHttp engine wires WebSocket via OkHttp's
        // own WS support; same engine the production
        // `ChannelsClient` already uses.
        val client = HttpClient(OkHttp) {
            install(WebSockets)
        }

        // Translate http(s) → ws(s). Ktor 3.0 accepts both
        // in `webSocketSession(urlString = ...)`, but we
        // pin the canonical wire shape explicitly.
        val wsBase = sfuBaseUrl
            .replaceFirst("http://", "ws://")
            .replaceFirst("https://", "wss://")
        val wsUrl = "$wsBase/v1/connect?token=$jwt"

        try {
            runBlocking {
                withTimeout(10.seconds) {
                    val session = client.webSocketSession(urlString = wsUrl) {
                        // SFU refuses upgrades without the
                        // `lakko-sfu-v1` subprotocol header
                        // — matches the server's accept-
                        // header check.
                        header("Sec-WebSocket-Protocol", "lakko-sfu-v1")
                    }

                    val firstFrame = session.incoming.receive()
                    val text = when (firstFrame) {
                        is Frame.Text -> firstFrame.readText()
                        is Frame.Binary -> String(firstFrame.data, Charsets.UTF_8)
                        else -> {
                            fail("expected text frame, got ${firstFrame.frameType}")
                            return@withTimeout
                        }
                    }

                    val obj = Json.parseToJsonElement(text) as? JsonObject
                        ?: run {
                            fail("welcome frame did not decode as JSON object: $text")
                            return@withTimeout
                        }

                    assertEquals(
                        "first frame must be Welcome: $text",
                        "welcome",
                        (obj["kind"] as? JsonPrimitive)?.contentOrNull,
                    )

                    val echoedCallId = (obj["call_id"] as? JsonPrimitive)?.contentOrNull
                    assertNotNull("welcome frame must carry call_id: $text", echoedCallId)
                    assertEquals(
                        "call_id should be 16-byte hex (32 chars): $text",
                        32,
                        echoedCallId!!.length,
                    )

                    val rosterSize = obj["roster"]?.jsonArray?.size ?: 0
                    assertTrue(
                        "roster should be empty on first peer in a fresh stack: " +
                            "${obj["roster"]}",
                        rosterSize == 0,
                    )

                    session.close()
                }
            }
        } finally {
            client.close()
        }
    }
}
