// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.sfu

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.HttpURLConnection
import java.net.URL

/**
 * G3-4-mobile — Android half of the dockerised SFU end-to-end
 * gate.
 *
 * Lives under `app/src/androidTest/` so AGP packages it into a
 * separate `app-debug-androidTest.apk` and the
 * `AndroidJUnitRunner` runs it inside the booted device /
 * emulator process. The matching iOS half is
 * `apps/ios/LakkoTests/SfuHealthTest.swift`.
 *
 * What this test proves:
 *   1. The androidTest source set assembles + executes against
 *      the `AndroidJUnitRunner` instrumentation runner.
 *   2. The emulator can reach `lakko-svc-sfu` over the standard
 *      Android-emulator → host loopback (`10.0.2.2` resolves
 *      to the host's `127.0.0.1` per Android's emulator
 *      networking spec).
 *   3. The dockerised SFU HTTP plane responds with `ok` —
 *      the same baseline the iOS test pins above.
 *
 * SFU URL resolution priority:
 *   1. `LAKKO_SFU_E2E_URL` instrumentation argument
 *      (set via `./gradlew connectedDebugAndroidTest
 *      -Pandroid.testInstrumentationRunnerArguments.LAKKO_SFU_E2E_URL=http://...`,
 *      or the CI workflow's `instrumentation-arguments`).
 *   2. `http://10.0.2.2:7880` — the canonical Android-emulator
 *      host loopback. The CI workflow exposes the dockerised
 *      SFU's signaling port on the host's `7880`, so the
 *      emulator-side fallback resolves there.
 *
 * **Out of scope (separate sub-sprints):** dual-emulator WS
 * welcome round-trip, JWT minting from inside the emulator,
 * ICE / DTLS over a synthetic peer connection.
 */
@RunWith(AndroidJUnit4::class)
class SfuHealthInstrumentedTest {

    private val sfuBaseUrl: String
        get() {
            val argFromRunner = InstrumentationRegistry
                .getArguments()
                .getString("LAKKO_SFU_E2E_URL")
            if (!argFromRunner.isNullOrBlank()) {
                return argFromRunner.trimEnd('/')
            }
            // Android emulator → host loopback: `10.0.2.2`
            // maps to the host's `127.0.0.1`. The compose
            // stack at `ci/sfu-e2e/docker-compose.yml`
            // publishes the SFU's signaling port on the
            // host's `7880`.
            return "http://10.0.2.2:7880"
        }

    @Test
    fun healthEndpointReturnsOk() {
        val url = URL("$sfuBaseUrl/health")
        val connection = (url.openConnection() as HttpURLConnection).apply {
            connectTimeout = 5_000
            readTimeout = 5_000
            requestMethod = "GET"
        }

        try {
            val code = connection.responseCode
            assertEquals(
                "SFU /health returned unexpected status code",
                200,
                code,
            )

            // The SFU emits a plain-text `ok` for /health (no
            // trailing newline). We accept a trim() so any
            // future framing tweak does not break the gate.
            val body = BufferedReader(
                InputStreamReader(connection.inputStream, Charsets.UTF_8),
            ).use { it.readText() }

            assertTrue(
                "SFU /health body did not contain 'ok': $body",
                body.trim().contains("ok"),
            )
        } finally {
            connection.disconnect()
        }
    }
}
