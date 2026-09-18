// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which of the three ways a mid-call camera-on fails, named from the facts.
 *
 * The live case this was written against: Android pressed Camera on a voice
 * call, its own screen filled with its camera, the SDP reached iOS, iOS
 * attached the remote video track and then waited for a first frame that never
 * arrived. Those two observations are produced by all three causes, which is
 * why the verdict has to be drawn from what the sender can measure rather than
 * from what either screen shows.
 */
class MidCallVideoDiagnosisTest {

    @Suppress("LongParameterList")
    private fun facts(
        framesEncoded: Long? = 120L,
        bytesSent: Long? = 90_000L,
        keyFramesEncoded: Long? = 1L,
        pliCount: Long? = 0L,
        firCount: Long? = 0L,
        direction: String? = "SEND_RECV",
        currentDirection: String? = "SEND_ONLY",
        sframeArmed: Boolean = true,
        sealKid: Long? = 0x0301L,
        sealLeaf: Int? = 3,
        sealStream: Int? = 1,
        encryptorInFramePath: Boolean = true,
        codec: String? = "video/VP8",
        headerExtensions: List<String> = emptyList(),
    ) = MidCallVideoFacts(
        framesEncoded = framesEncoded,
        bytesSent = bytesSent,
        keyFramesEncoded = keyFramesEncoded,
        pliCount = pliCount,
        firCount = firCount,
        direction = direction,
        currentDirection = currentDirection,
        sframeArmed = sframeArmed,
        sealKid = sealKid,
        sealLeaf = sealLeaf,
        sealStream = sealStream,
        encryptorInFramePath = encryptorInFramePath,
        codec = codec,
        headerExtensions = headerExtensions,
    )

    @Test
    fun `a healthy mid-call camera-on reads as flowing`() {
        assertEquals(MidCallVideoVerdict.FLOWING, midCallVideoVerdict(facts()))
    }

    @Test
    fun `an unsealed video sender is named first`() {
        // It is the quietest failure: the counters look perfect on this device
        // and the peer drops every frame without an error, so healthy-looking
        // numbers must not be allowed to outvote it.
        assertEquals(
            MidCallVideoVerdict.NOT_SEALED,
            midCallVideoVerdict(facts(sframeArmed = false)),
        )
    }

    @Test
    fun `a transceiver left receive-only is named`() {
        assertEquals(
            MidCallVideoVerdict.NOT_NEGOTIATED_AS_SENDING,
            midCallVideoVerdict(facts(direction = "RECV_ONLY", currentDirection = "RECV_ONLY")),
        )
    }

    @Test
    fun `a renegotiation whose answer never landed is named`() {
        // `currentDirection` stays null until an answer is applied, so an offer
        // that went out and was never answered — or was answered by a colliding
        // offer the impolite peer dropped — reads as not negotiated, not as a
        // broken encoder.
        assertEquals(
            MidCallVideoVerdict.NOT_NEGOTIATED_AS_SENDING,
            midCallVideoVerdict(facts(currentDirection = null)),
        )
    }

    @Test
    fun `a sender negotiated as sending but encoding nothing is named`() {
        assertEquals(
            MidCallVideoVerdict.NOT_ENCODING,
            midCallVideoVerdict(facts(framesEncoded = 0L, bytesSent = 0L)),
        )
    }

    @Test
    fun `frames encoded but nothing on the wire is its own answer`() {
        assertEquals(
            MidCallVideoVerdict.ENCODED_BUT_NOT_SENT,
            midCallVideoVerdict(facts(framesEncoded = 90L, bytesSent = 0L)),
        )
    }

    @Test
    fun `no stats at all is unknown, not a diagnosis`() {
        // Never pretend: absent counters mean the probe learned nothing, and
        // reporting NOT_ENCODING there would send the next reader hunting a
        // working encoder.
        assertEquals(
            MidCallVideoVerdict.UNKNOWN,
            midCallVideoVerdict(facts(framesEncoded = null, bytesSent = null)),
        )
    }

    @Test
    fun `send-recv and send-only both count as sending`() {
        listOf("SEND_RECV", "SEND_ONLY").forEach { d ->
            assertEquals(
                d,
                MidCallVideoVerdict.FLOWING,
                midCallVideoVerdict(facts(direction = d, currentDirection = d)),
            )
        }
    }

    @Test
    fun `inactive and stopped are not sending`() {
        listOf("INACTIVE", "STOPPED").forEach { d ->
            assertEquals(
                d,
                MidCallVideoVerdict.NOT_NEGOTIATED_AS_SENDING,
                midCallVideoVerdict(facts(currentDirection = d)),
            )
        }
    }

    @Test
    fun `an encryptor attached but never fetched is not sealing`() {
        // `setFrameEncryptor` returning cleanly proves only that a setter was
        // called. If libwebrtc never took the native pointer, the frames going
        // out are not the ones this encryptor sealed — and a peer armed to
        // expect sealed frames drops every one of them, which is
        // indistinguishable from a wrong key.
        assertEquals(
            MidCallVideoVerdict.NOT_SEALED,
            midCallVideoVerdict(facts(encryptorInFramePath = false)),
        )
    }

    @Test
    fun `the stamped KID is reported so the two arming paths can be compared`() {
        val line = midCallVideoReport(facts())
        assertTrue(line, line.contains("sealKid=0x301"))
        assertTrue(line, line.contains("sealLeaf=3"))
        assertTrue(line, line.contains("sealStream=1"))
        assertTrue(line, line.contains("codec=video/VP8"))
    }

    @Test
    fun `the label names which arming path the line came from`() {
        assertTrue(midCallVideoReport(facts()).startsWith("mid-call camera-on: "))
        assertTrue(
            midCallVideoReport(facts(), "call-start-with-video")
                .startsWith("call-start-with-video: "),
        )
    }

    @Test
    fun `sending frames with no key frame among them is its own answer`() {
        // The live case: packets climbing on the peer, frames stuck at zero,
        // and nothing ever reaching its decryptor — because a receiver that
        // joined the stream in progress discards every delta frame until an IDR
        // arrives, and on a mid-call camera-on the encoder's only IDR went out
        // before that receiver existed.
        assertEquals(
            MidCallVideoVerdict.NO_KEYFRAME_SENT,
            midCallVideoVerdict(facts(keyFramesEncoded = 0L)),
        )
    }

    @Test
    fun `a key frame among the encoded frames reads as flowing`() {
        assertEquals(MidCallVideoVerdict.FLOWING, midCallVideoVerdict(facts(keyFramesEncoded = 1L)))
    }

    @Test
    fun `the keyframe check never outranks a sender that is not sending`() {
        // A sender with no key frames because it has no frames at all must be
        // reported for the reason that actually stops it, not for the symptom.
        assertEquals(
            MidCallVideoVerdict.NOT_ENCODING,
            midCallVideoVerdict(facts(framesEncoded = 0L, bytesSent = 0L, keyFramesEncoded = 0L)),
        )
        assertEquals(
            MidCallVideoVerdict.NOT_SEALED,
            midCallVideoVerdict(facts(sframeArmed = false, keyFramesEncoded = 0L)),
        )
    }

    @Test
    fun `the peer asking for a picture is reason to key again`() {
        // The only evidence this device can have that its frames are not being
        // decoded: it cannot see the peer's decoder, but it can see the peer
        // asking.
        assertTrue(shouldForceAnotherKeyframe(facts(pliCount = 3L)))
        assertTrue(shouldForceAnotherKeyframe(facts(firCount = 1L)))
        assertTrue(shouldForceAnotherKeyframe(facts(keyFramesEncoded = 0L)))
    }

    @Test
    fun `a healthy stream is not keyed again`() {
        // A retry on a condition that never clears would key the stream for the
        // rest of the call.
        assertFalse(shouldForceAnotherKeyframe(facts()))
    }

    @Test
    fun `the report carries the keyframe and picture-loss counts`() {
        val line = midCallVideoReport(facts(keyFramesEncoded = 2L, pliCount = 5L, firCount = 1L))
        assertTrue(line, line.contains("keyFramesEncoded=2"))
        assertTrue(line, line.contains("pliCount=5"))
        assertTrue(line, line.contains("firCount=1"))
    }

    // ---- The keyframe watchdog ------------------------------------------------

    @Test
    fun `nothing is forced before the encoder has produced a frame`() {
        // The live finding this whole watchdog exists for: a reconfiguration
        // fired the instant the renegotiation answer landed reported success
        // and produced no key frame, because there was no running encoder to
        // reconfigure. The receiver then asked 78 times and got nothing.
        assertEquals(
            KeyframeAction.WAIT,
            keyframeAction(facts(framesEncoded = 0L, keyFramesEncoded = 0L), attemptsMade = 0),
        )
    }

    @Test
    fun `an unmeasured sender is waited on, never forced on a guess`() {
        assertEquals(
            KeyframeAction.WAIT,
            keyframeAction(facts(framesEncoded = null, keyFramesEncoded = null), attemptsMade = 0),
        )
    }

    @Test
    fun `a running encoder with no key frame is forced`() {
        assertEquals(
            KeyframeAction.FORCE,
            keyframeAction(facts(framesEncoded = 12L, keyFramesEncoded = 0L), attemptsMade = 0),
        )
    }

    @Test
    fun `a force that did not work escalates to the other lever`() {
        assertEquals(
            KeyframeAction.ESCALATE,
            keyframeAction(facts(framesEncoded = 40L, keyFramesEncoded = 0L), attemptsMade = 1),
        )
    }

    @Test
    fun `the watchdog gives up rather than reconfiguring for the whole call`() {
        assertEquals(
            KeyframeAction.GIVE_UP,
            keyframeAction(
                facts(framesEncoded = 90L, keyFramesEncoded = 0L),
                attemptsMade = MAX_KEYFRAME_ATTEMPTS,
            ),
        )
    }

    @Test
    fun `a key frame stops the watchdog whatever else is true`() {
        listOf(0, 1, MAX_KEYFRAME_ATTEMPTS, MAX_KEYFRAME_ATTEMPTS + 5).forEach { attempts ->
            assertEquals(
                "attempts=$attempts",
                KeyframeAction.SATISFIED,
                keyframeAction(facts(keyFramesEncoded = 1L, pliCount = 78L), attempts),
            )
        }
    }

    @Test
    fun `giving up names which half of the problem this device is looking at`() {
        // Requests arriving and ignored is this device's encoder; requests not
        // arriving at all is the transport between the two, and no amount of
        // local reconfiguring touches it.
        val asked = keyframeGiveUpDiagnosis(facts(pliCount = 78L, firCount = 0L))
        assertTrue(asked, asked.contains("78"))
        assertTrue(asked, asked.contains("encoder or lever"))

        val silent = keyframeGiveUpDiagnosis(facts(pliCount = 0L, firCount = 0L))
        assertTrue(silent, silent.contains("transport"))
    }

    @Test
    fun `the late probe is anchored after the first one`() {
        // Both hang off the renegotiation answer; the second exists because
        // keyFramesEncoded and pliCount mean nothing until traffic has flowed.
        assertTrue(MID_CALL_VIDEO_LATE_PROBE_DELAY_MS > MID_CALL_VIDEO_PROBE_DELAY_MS)
    }

    @Test
    fun `the watchdog window outlasts the late probe`() {
        // Otherwise the last thing the console says about a broken sender is a
        // probe taken while the watchdog was still working on it.
        val window = KEYFRAME_WATCHDOG_INTERVAL_MS * KEYFRAME_WATCHDOG_TICKS
        assertTrue("window=$window", window > MID_CALL_VIDEO_LATE_PROBE_DELAY_MS)
    }

    // ---- The frame descriptor -------------------------------------------------

    @Test
    fun `a sender with no frame-descriptor extension is reported as such`() {
        // A frame encryptor replaces the whole encoded frame, including the
        // byte libwebrtc's VP8 receive path reads the key-frame flag from. With
        // no extension carrying that flag outside the payload, a receiver has
        // nothing left to recognise a key frame by.
        assertFalse(carriesFrameDescriptorExtension(emptyList()))
        assertFalse(
            carriesFrameDescriptorExtension(
                listOf(
                    "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
                    "urn:ietf:params:rtp-hdrext:sdes:mid",
                ),
            ),
        )
    }

    @Test
    fun `either descriptor extension counts`() {
        assertTrue(carriesFrameDescriptorExtension(listOf(DEPENDENCY_DESCRIPTOR_URI)))
        assertTrue(carriesFrameDescriptorExtension(listOf(GENERIC_FRAME_DESCRIPTOR_URI)))
    }

    @Test
    fun `the report prints the negotiated extensions and the verdict on them`() {
        val without = midCallVideoReport(facts(headerExtensions = listOf("urn:ietf:params:rtp-hdrext:sdes:mid")))
        assertTrue(without, without.contains("frameDescriptorExt=false"))
        assertTrue(without, without.contains("extmaps=[urn:ietf:params:rtp-hdrext:sdes:mid]"))

        val with = midCallVideoReport(facts(headerExtensions = listOf(DEPENDENCY_DESCRIPTOR_URI)))
        assertTrue(with, with.contains("frameDescriptorExt=true"))
    }

    // ---- The field trials that put a descriptor on the wire -------------------

    @Test
    fun `the field-trial string is the shape libwebrtc parses`() {
        // Malformed trials are not ignored; libwebrtc validates the string at
        // init, so a stray token would break every call rather than just this
        // feature. The format is `key/value/` pairs, concatenated, with a
        // trailing slash.
        val trials = FRAME_DESCRIPTOR_FIELD_TRIALS
        assertTrue(trials, trials.endsWith("/"))
        val tokens = trials.trimEnd('/').split("/")
        assertEquals("even number of key/value tokens: $tokens", 0, tokens.size % 2)
        tokens.forEach { assertTrue("empty token in $trials", it.isNotBlank()) }
    }

    @Test
    fun `the generic descriptor is the one enabled`() {
        val tokens = FRAME_DESCRIPTOR_FIELD_TRIALS.trimEnd('/').split("/")
        val byName = tokens.chunked(2).associate { it[0] to it[1] }
        assertEquals("Enabled", byName["WebRTC-GenericDescriptorAdvertised"])
    }

    @Test
    fun `the dependency descriptor is deliberately not enabled`() {
        // Not an omission, and not an obvious improvement to add back. It is
        // NOT inert for VP8 — GenericStructure returns a MinimalisticStructure
        // for kVideoCodecVP8 unconditionally — and because it fires, and
        // because the send side prefers it whenever both are registered,
        // offering both puts the DEPENDENCY descriptor on the wire, written
        // over a structure this side only simulates. The peer enables the
        // generic one alone, so the generic one is the only descriptor both
        // ends chose on purpose.
        val tokens = FRAME_DESCRIPTOR_FIELD_TRIALS.trimEnd('/').split("/")
        val byName = tokens.chunked(2).associate { it[0] to it[1] }
        assertNull(
            "re-adding this silently outranks the generic descriptor on the wire",
            byName["WebRTC-DependencyDescriptorAdvertised"],
        )
    }

    @Test
    fun `descriptor authentication is deliberately not enabled`() {
        // It would fold the descriptor into the frame encryptor's
        // additional_data, which ADR-0013 deliberately does not bind so that
        // web and native open each other's frames.
        assertFalse(
            FRAME_DESCRIPTOR_FIELD_TRIALS,
            FRAME_DESCRIPTOR_FIELD_TRIALS.contains("GenericDescriptorAuth"),
        )
    }

    @Test
    fun `the probe recognises the extension the trial is meant to produce`() {
        // The acceptance test for the change: this line flipping to true on a
        // live call is what says the descriptor reached the wire.
        assertTrue(carriesFrameDescriptorExtension(listOf(GENERIC_FRAME_DESCRIPTOR_URI)))
        assertFalse(carriesFrameDescriptorExtension(emptyList()))
        // Still recognised if it ever appears, so the probe keeps reporting the
        // truth about a session rather than only what this build asked for.
        assertTrue(carriesFrameDescriptorExtension(listOf(DEPENDENCY_DESCRIPTOR_URI)))
    }

    @Test
    fun `the report names the verdict and every number behind it`() {
        val line = midCallVideoReport(facts(sframeArmed = false))
        assertTrue(line, line.contains("NOT_SEALED"))
        assertTrue(line, line.contains("framesEncoded=120"))
        assertTrue(line, line.contains("bytesSent=90000"))
        assertTrue(line, line.contains("direction=SEND_RECV"))
        assertTrue(line, line.contains("currentDirection=SEND_ONLY"))
        assertTrue(line, line.contains("sframeArmed=false"))
    }

    @Test
    fun `an absent number reads as absent rather than as zero`() {
        // `framesEncoded=0` and "no stats" mean different things, and a reader
        // deciding whether the encoder ran must be able to tell them apart.
        val line = midCallVideoReport(facts(framesEncoded = null, bytesSent = null))
        assertTrue(line, line.contains("framesEncoded=-1"))
        assertTrue(line, line.contains("currentDirection=SEND_ONLY"))
    }

    @Test
    fun `the report is plain ASCII`() {
        // House rule: log lines carry no emoji or pictographs.
        val line = midCallVideoReport(facts())
        assertTrue(line, line.all { it.code in 32..126 })
    }
}
