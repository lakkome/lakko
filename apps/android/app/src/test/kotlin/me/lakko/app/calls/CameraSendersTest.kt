// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.RtpParameters

/**
 * The group call's camera senders (`CameraSenders.kt`, ADR-0070 steps 1 and
 * 5): what each is created with, the order they are added in, the fallbacks
 * when libwebrtc refuses, and what an offer names. `RtpParameters.Encoding`
 * is a plain Java class, so the encodings are real; the peer connection is a
 * lambda.
 */
class CameraSendersTest {

    // ---- the numbers -------------------------------------------------------

    @Test
    fun `the sizes, ceilings and stream indices are the web client's`() {
        assertEquals("L1T3", CAMERA_SCALABILITY_MODE)
        assertEquals(4.0, CAMERA_LIGHT_SCALE_DOWN, 0.0)
        assertEquals(150_000, CAMERA_LIGHT_MAX_BITRATE_BPS)
        assertEquals(2.0, CAMERA_MIDDLE_SCALE_DOWN, 0.0)
        assertEquals(500_000, CAMERA_MIDDLE_MAX_BITRATE_BPS)
        assertEquals(0, SFRAME_STREAM_AUDIO)
        assertEquals(1, CameraSenderKind.MAIN.streamIndex)
        assertEquals(3, CameraSenderKind.LIGHT.streamIndex)
        assertEquals(4, CameraSenderKind.MIDDLE.streamIndex)
    }

    @Test
    fun `no two senders share an SFrame stream index`() {
        val indices = CAMERA_SENDER_KINDS.map { it.streamIndex } + SFRAME_STREAM_AUDIO
        assertEquals(indices.size, indices.toSet().size)
    }

    @Test
    fun `the copies are added light first, and the diagnostics name main, middle, light`() {
        assertEquals(listOf(CameraCopyKind.LIGHT, CameraCopyKind.MIDDLE), CAMERA_COPY_KINDS)
        assertEquals(listOf("main", "middle", "light"), CAMERA_SENDER_KINDS.map { it.wireName })
        assertSame(CameraSenderKind.LIGHT, CameraCopyKind.LIGHT.sender)
        assertSame(CameraSenderKind.MIDDLE, CameraCopyKind.MIDDLE.sender)
    }

    // ---- the encodings -----------------------------------------------------

    @Test
    fun `the main sender is one full-size encoding in temporal layers, with no rid`() {
        val encoding = cameraMainEncoding()
        assertNull(encoding.rid)
        assertTrue(encoding.active)
        assertNull(encoding.scaleResolutionDownBy)
        assertNull(encoding.maxBitrateBps)
        assertEquals("L1T3", encoding.scalabilityMode)
    }

    @Test
    fun `a copy is one ordinary encoding with its size and ceiling, not a simulcast layer`() {
        val light = cameraCopyEncoding(CameraCopyKind.LIGHT)
        assertNull(light.rid)
        assertTrue(light.active)
        assertEquals(1.0, light.scaleResolutionDownBy!!, 0.0)
        assertEquals(150_000, light.maxBitrateBps)
        assertNull(light.scalabilityMode)

        val middle = cameraCopyEncoding(CameraCopyKind.MIDDLE, active = false)
        assertNull(middle.rid)
        assertFalse(middle.active)
        assertEquals(1.0, middle.scaleResolutionDownBy!!, 0.0)
        assertEquals(500_000, middle.maxBitrateBps)
        assertTrue(cameraCopyEncoding(CameraCopyKind.MIDDLE, active = true).active)
    }

    // ---- the plan ----------------------------------------------------------

    @Test
    fun `all three senders exist at any count, and the middle one is active from the rule's threshold`() {
        val asked = ArrayList<Int>()
        val rule: (Int) -> Boolean = { people ->
            asked += people
            people >= 3
        }
        val two = cameraSenderPlan(2, rule)
        val three = cameraSenderPlan(3, rule)
        assertEquals(listOf(2, 3), asked)
        for (plan in listOf(two, three)) {
            assertEquals(
                listOf(CameraSenderKind.MAIN, CameraSenderKind.LIGHT, CameraSenderKind.MIDDLE),
                plan.map { it.kind },
            )
            assertEquals(listOf(1, 3, 4), plan.map { it.streamIndex })
            assertEquals(listOf(1.0, 4.0, 2.0), plan.map { it.scaleDown })
            assertEquals(listOf(null, 150_000, 500_000), plan.map { it.maxBitrateBps })
            // The main and the light sender are always created sending.
            assertTrue(plan[0].active)
            assertTrue(plan[1].active)
        }
        assertFalse(two[2].active)
        assertTrue(three[2].active)
    }

    @Test
    fun `while the rule cannot be asked the middle sender is created off`() {
        val plan = cameraSenderPlan(5) { throw IllegalStateException("not loaded") }
        assertFalse(plan.single { it.kind == CameraSenderKind.MIDDLE }.active)
        assertTrue(plan.single { it.kind == CameraSenderKind.LIGHT }.active)
    }

    // ---- adding the main sender --------------------------------------------

    @Test
    fun `the main sender is added with its one encoding`() {
        val seen = ArrayList<List<RtpParameters.Encoding>>()
        val result = addCameraSender(
            addWithEncodings = { encodings ->
                seen += encodings
                "sender"
            },
            addPlain = { throw AssertionError("not reached") },
        )
        assertEquals("sender", result.sender)
        assertTrue(result.layered)
        assertEquals(1, seen.single().size)
        assertEquals("L1T3", seen.single().single().scalabilityMode)
    }

    @Test
    fun `a refused encoding falls back to the plain add, and says why`() {
        val refusals = ArrayList<RuntimeException>()
        val result = addCameraSender<String>(
            addWithEncodings = { throw IllegalStateException("C++ addTransceiver failed.") },
            addPlain = { "plain" },
            onRefused = { refusals += it },
        )
        assertEquals("plain", result.sender)
        assertFalse(result.layered)
        assertEquals("C++ addTransceiver failed.", refusals.single().message)
    }

    @Test
    fun `temporal layers are asked for afterwards on a plainly added sender`() {
        val encodings = listOf(RtpParameters.Encoding(null, true, null))
        var applied = 0
        assertEquals(
            CameraLayersOutcome.APPLIED,
            requestCameraLayers(encodings) {
                applied++
                true
            },
        )
        assertEquals("L1T3", encodings.single().scalabilityMode)
        // Already set: nothing is sent again.
        assertEquals(CameraLayersOutcome.ALREADY_SET, requestCameraLayers(encodings) { throw AssertionError() })
        assertEquals(1, applied)
        assertEquals(CameraLayersOutcome.NO_ENCODING, requestCameraLayers(emptyList()) { true })
        assertEquals(
            CameraLayersOutcome.REFUSED,
            requestCameraLayers(listOf(RtpParameters.Encoding(null, true, null))) { false },
        )
        assertEquals(
            CameraLayersOutcome.REFUSED,
            requestCameraLayers(listOf(RtpParameters.Encoding(null, true, null))) { throw IllegalStateException() },
        )
    }

    // ---- adding a copy -----------------------------------------------------

    @Test
    fun `a copy is added with its encoding, the middle one active or not as told`() {
        val seen = ArrayList<RtpParameters.Encoding>()
        val added = addCopySender(
            kind = CameraCopyKind.MIDDLE,
            active = false,
            addWithEncoding = { encoding ->
                seen += encoding
                "middle"
            },
            addPlain = { throw AssertionError("not reached") },
        )!!
        assertEquals("middle", added.sender)
        assertTrue(added.configured)
        assertFalse(seen.single().active)
        assertEquals(1.0, seen.single().scaleResolutionDownBy!!, 0.0)
        assertEquals(500_000, seen.single().maxBitrateBps)
    }

    @Test
    fun `a copy whose encoding is refused is added plainly and left for the caller to configure`() {
        val added = addCopySender<String>(
            kind = CameraCopyKind.LIGHT,
            active = null,
            addWithEncoding = { throw IllegalStateException("refused") },
            addPlain = { "plain" },
        )!!
        assertEquals("plain", added.sender)
        assertFalse(added.configured)
    }

    @Test
    fun `a copy libwebrtc will not add at all is absent, and says why`() {
        val refusals = ArrayList<String>()
        val added = addCopySender<String>(
            kind = CameraCopyKind.LIGHT,
            active = null,
            addWithEncoding = { throw IllegalStateException("refused") },
            addPlain = { throw IllegalStateException("refused again") },
            refused = { what, _ -> refusals += what },
        )
        assertNull(added)
        assertEquals(listOf("a light camera transceiver was refused"), refusals)
    }

    @Test
    fun `a plainly added copy is given its size and ceiling on the sender`() {
        val encodings = listOf(RtpParameters.Encoding(null, true, null))
        assertTrue(configureCopyEncoding(encodings, CameraCopyKind.MIDDLE, active = false) { true })
        assertEquals(1.0, encodings.single().scaleResolutionDownBy!!, 0.0)
        assertEquals(500_000, encodings.single().maxBitrateBps)
        assertFalse(encodings.single().active)

        // `null` leaves the flag as the platform has it.
        val light = listOf(RtpParameters.Encoding(null, true, null))
        assertTrue(configureCopyEncoding(light, CameraCopyKind.LIGHT, active = null) { true })
        assertTrue(light.single().active)
        assertEquals(1.0, light.single().scaleResolutionDownBy!!, 0.0)
    }

    @Test
    fun `a copy whose platform encoding carried a scale is reset to 1`() {
        val encodings = listOf(RtpParameters.Encoding(null, true, 4.0))
        assertTrue(configureCopyEncoding(encodings, CameraCopyKind.LIGHT, active = null) { true })
        assertEquals(1.0, encodings.single().scaleResolutionDownBy!!, 0.0)
    }

    @Test
    fun `a copy's source is the capture divided by the copy's factor`() {
        assertEquals(320 to 180, cameraCopySize(CameraCopyKind.LIGHT, 1280, 720))
        assertEquals(640 to 360, cameraCopySize(CameraCopyKind.MIDDLE, 1280, 720))
        assertEquals(1 to 1, cameraCopySize(CameraCopyKind.LIGHT, 2, 2))
    }

    @Test
    fun `a copy that cannot be configured is reported so, never left at full size`() {
        assertFalse(configureCopyEncoding(emptyList(), CameraCopyKind.LIGHT, null) { true })
        assertFalse(
            configureCopyEncoding(listOf(RtpParameters.Encoding(null, true, null)), CameraCopyKind.LIGHT, null) { false },
        )
        assertFalse(
            configureCopyEncoding(listOf(RtpParameters.Encoding(null, true, null)), CameraCopyKind.LIGHT, null) {
                throw IllegalStateException()
            },
        )
    }

    // ---- what an offer names -----------------------------------------------

    @Test
    fun `an offer names a copy's media line once it has a mid, and nothing before`() {
        assertEquals(listOf("2"), copyMids("2"))
        assertEquals(emptyList<String>(), copyMids(null))
        assertEquals(emptyList<String>(), copyMids(""))
    }

    // ---- the codec an encryptor is told (ADR-0071) ---------------------------

    @Test
    fun `an encryptor is told the first negotiated codec, and nothing before negotiation`() {
        assertEquals("VP8", senderCodecName(listOf("VP8", "rtx", "red")))
        // A wrapper first is what is said: it has no rule, so nothing is left clear.
        assertEquals("red", senderCodecName(listOf("red", "VP8")))
        assertNull(senderCodecName(emptyList()))
        assertNull(senderCodecName(listOf(null, "VP8")))
        assertNull(senderCodecName(listOf("", "VP8")))
    }

    @Test
    fun anOfferOnHeldTransceiversNeverAsksForTheList() {
        // Asking disposes the held ones: the offer then names no light or
        // middle line, and every sender of the group call stops working.
        val used = offerTransceivers(listOf("main", "light")) {
            throw AssertionError("the transceiver list must not be asked for")
        }
        assertEquals(listOf("main", "light"), used)
    }

    @Test
    fun anOfferWithNothingHeldUsesTheList() {
        assertEquals(listOf("audio"), offerTransceivers(emptyList<String>()) { listOf("audio") })
    }
}
