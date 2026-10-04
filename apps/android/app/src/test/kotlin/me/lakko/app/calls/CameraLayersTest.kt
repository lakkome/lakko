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
 * The group call's camera sender: three temporal layers (ADR-0067 step 2) and
 * two simulcast layers (step 5). Mirrors the web `camera-sender.test.ts`.
 */
class CameraLayersTest {

    private val sender = Any()

    // ---- the encodings --------------------------------------------------

    @Test
    fun `the simulcast encodings are the light layer then the full one`() {
        val encodings = cameraSimulcastEncodings()
        assertEquals(2, encodings.size)
        val (low, full) = encodings

        assertEquals("q", CAMERA_RID_LOW)
        assertEquals(CAMERA_RID_LOW, low.rid)
        assertEquals(4.0, low.scaleResolutionDownBy!!, 0.0)
        assertEquals(150_000, low.maxBitrateBps)
        assertEquals(CAMERA_SCALABILITY_MODE, low.scalabilityMode)
        assertTrue(low.active)

        assertEquals("f", CAMERA_RID_FULL)
        assertEquals(CAMERA_RID_FULL, full.rid)
        assertNull(full.scaleResolutionDownBy)
        assertNull(full.maxBitrateBps)
        assertEquals(CAMERA_SCALABILITY_MODE, full.scalabilityMode)
        assertTrue(full.active)
    }

    @Test
    fun `the simulcast encodings can leave the temporal structure to the platform`() {
        val (low, full) = cameraSimulcastEncodings(layered = false)
        assertEquals(CAMERA_RID_LOW, low.rid)
        assertEquals(CAMERA_RID_FULL, full.rid)
        assertNull(low.scalabilityMode)
        assertNull(full.scalabilityMode)
        assertEquals(150_000, low.maxBitrateBps)
    }

    @Test
    fun `the single encoding asks for one spatial and three temporal layers`() {
        val encoding = cameraSendEncoding()
        assertEquals("L1T3", CAMERA_SCALABILITY_MODE)
        assertEquals(CAMERA_SCALABILITY_MODE, encoding.scalabilityMode)
        assertTrue(encoding.active)
        assertNull(encoding.rid)
        assertNull(encoding.scaleResolutionDownBy)
    }

    @Test
    fun `the attempts go from two layered simulcast layers down to one encoding`() {
        assertTrue(CAMERA_SIMULCAST_ENABLED)
        val attempts = cameraSenderAttempts()
        assertEquals(
            listOf("simulcast with temporal layers", "simulcast", "temporal layers"),
            attempts.map { it.what },
        )
        assertEquals(listOf("q", "f"), attempts[0].encodings.map { it.rid })
        assertEquals(listOf("L1T3", "L1T3"), attempts[0].encodings.map { it.scalabilityMode })
        assertEquals(listOf("q", "f"), attempts[1].encodings.map { it.rid })
        assertEquals(listOf(null, null), attempts[1].encodings.map { it.scalabilityMode })
        assertEquals(listOf<String?>(null), attempts[2].encodings.map { it.rid })

        assertEquals(listOf("temporal layers"), cameraSenderAttempts(simulcast = false).map { it.what })
    }

    // ---- addCameraSender ------------------------------------------------
    // The one function both publish paths (the initial publish and the
    // mid-call camera-on) add the camera through.

    @Test
    fun `sends the camera as two simulcast layers`() {
        val asked = mutableListOf<List<RtpParameters.Encoding>>()
        var plain = 0
        val got = addCameraSender(
            addWithEncodings = { asked += it; sender },
            addPlain = { plain++; Any() },
        )
        assertSame(sender, got.sender)
        assertEquals("simulcast with temporal layers", got.accepted)
        assertEquals(1, asked.size)
        assertEquals(listOf("q", "f"), asked[0].map { it.rid })
        assertEquals(0, plain)
    }

    @Test
    fun `falls back to one encoding when libwebrtc refuses simulcast`() {
        val refused = mutableListOf<String>()
        val asked = mutableListOf<List<RtpParameters.Encoding>>()
        val got = addCameraSender(
            addWithEncodings = { encodings ->
                asked += encodings
                if (encodings.size > 1) throw IllegalStateException("C++ addTransceiver failed.")
                sender
            },
            addPlain = { Any() },
            onRefused = { what, _ -> refused += what },
        )
        assertSame(sender, got.sender)
        assertEquals("temporal layers", got.accepted)
        assertEquals(listOf("simulcast with temporal layers", "simulcast"), refused)
        assertEquals(3, asked.size)
        assertEquals(1, asked[2].size)
        assertNull(asked[2][0].rid)
        assertEquals(CAMERA_SCALABILITY_MODE, asked[2][0].scalabilityMode)
    }

    @Test
    fun `keeps simulcast without temporal layers when only the mode is refused`() {
        val got = addCameraSender(
            addWithEncodings = { encodings ->
                if (encodings.any { it.scalabilityMode != null }) {
                    throw IllegalStateException("C++ addTransceiver failed.")
                }
                sender
            },
            addPlain = { Any() },
        )
        assertSame(sender, got.sender)
        assertEquals("simulcast", got.accepted)
    }

    @Test
    fun `adds the track plainly when every encoding is refused`() {
        val refused = mutableListOf<String>()
        var cause: RuntimeException? = null
        val got = addCameraSender(
            addWithEncodings = { throw IllegalStateException("C++ addTransceiver failed.") },
            addPlain = { sender },
            onRefused = { what, c -> refused += what; cause = c },
        )
        assertSame(sender, got.sender)
        assertNull(got.accepted)
        assertEquals(
            listOf("simulcast with temporal layers", "simulcast", "temporal layers"),
            refused,
        )
        assertTrue(cause is IllegalStateException)
    }

    @Test
    fun `with simulcast off the camera is sent as one layered encoding`() {
        val asked = mutableListOf<List<RtpParameters.Encoding>>()
        val got = addCameraSender(
            addWithEncodings = { asked += it; sender },
            addPlain = { Any() },
            simulcast = false,
        )
        assertEquals("temporal layers", got.accepted)
        assertEquals(1, asked.single().size)
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
