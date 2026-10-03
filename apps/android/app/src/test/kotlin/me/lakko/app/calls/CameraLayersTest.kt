// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.RtpParameters

/**
 * The camera sender in three temporal layers (ADR-0067 step 2). Mirrors the
 * web `camera-sender.test.ts` case by case.
 */
class CameraLayersTest {

    private val sender = Any()

    // ---- addCameraSender ------------------------------------------------

    @Test
    fun `sends the camera in temporal layers`() {
        var layered = 0
        var plain = 0
        val got = addCameraSender(
            addLayered = { layered++; sender },
            addPlain = { plain++; Any() },
        )
        assertSame(sender, got)
        assertEquals(1, layered)
        assertEquals(0, plain)
    }

    @Test
    fun `the new transceiver asks for one spatial and three temporal layers`() {
        val encoding = cameraSendEncoding()
        assertEquals("L1T3", CAMERA_SCALABILITY_MODE)
        assertEquals(CAMERA_SCALABILITY_MODE, encoding.scalabilityMode)
        assertTrue(encoding.active)
        assertNull(encoding.rid)
        assertNull(encoding.scaleResolutionDownBy)
    }

    @Test
    fun `falls back to one layer when libwebrtc refuses the mode`() {
        var refused: RuntimeException? = null
        val got = addCameraSender(
            addLayered = { throw IllegalStateException("C++ addTransceiver failed.") },
            addPlain = { sender },
            onRefused = { refused = it },
        )
        assertSame(sender, got)
        assertTrue(refused is IllegalStateException)
    }

    // ---- requestCameraLayers -------------------------------------------

    @Test
    fun `asks for the layers on the first encoding`() {
        val first = RtpParameters.Encoding(null, true, null)
        val second = RtpParameters.Encoding("h", true, 2.0)
        var applied = 0
        val outcome = requestCameraLayers(listOf(first, second)) { applied++; true }
        assertEquals(CameraLayersOutcome.APPLIED, outcome)
        assertEquals(1, applied)
        assertEquals(CAMERA_SCALABILITY_MODE, first.scalabilityMode)
        assertTrue(first.active)
        assertNull(second.scalabilityMode)
    }

    @Test
    fun `does nothing when the mode is already set or there is no encoding`() {
        val set = RtpParameters.Encoding(null, true, null).apply {
            scalabilityMode = CAMERA_SCALABILITY_MODE
        }
        var applied = 0
        assertEquals(
            CameraLayersOutcome.ALREADY_SET,
            requestCameraLayers(listOf(set)) { applied++; true },
        )
        assertEquals(
            CameraLayersOutcome.NO_ENCODING,
            requestCameraLayers(emptyList()) { applied++; true },
        )
        assertEquals(0, applied)
    }

    @Test
    fun `keeps the call going when libwebrtc refuses`() {
        val declined = RtpParameters.Encoding(null, true, null)
        assertEquals(
            CameraLayersOutcome.REFUSED,
            requestCameraLayers(listOf(declined)) { false },
        )
        val thrown = RtpParameters.Encoding(null, true, null)
        assertEquals(
            CameraLayersOutcome.REFUSED,
            requestCameraLayers(listOf(thrown)) { throw IllegalStateException("disposed") },
        )
    }
}
