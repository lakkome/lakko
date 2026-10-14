// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AvAudioPlan
import uniffi.lakko_ffi_uniffi.AvAudioReason
import uniffi.lakko_ffi_uniffi.AvPlan
import uniffi.lakko_ffi_uniffi.AvReadings
import uniffi.lakko_ffi_uniffi.AvVideoPlan
import uniffi.lakko_ffi_uniffi.AvVideoReason

/**
 * The Android wiring around the shared audio/video rule (`AvAlign.kt`).
 *
 * The rule itself is Rust (`crates/lakko-ui-rules/src/av_align.rs`) and is
 * tested there; the host JVM cannot load the native library. So these tests
 * put a scripted [AvAlignRules] in its place and check what this client ASKS
 * the rule (which receiver is whose, what the statistics become, what is
 * shown) and what it does with the answer (the writes, the log line, the
 * lifecycle). They never check what the rule answers.
 */
class AvAlignTest {

    /** One person's scripted rule: records every question, answers [plan]. */
    private class ScriptedRule(val audioSettable: Boolean, val videoSettable: Boolean) : AvAlignRule {
        var plan: AvPlan = quietPlan()
        val readings = ArrayList<Pair<ULong, AvReadings>>()
        var replaced = 0
        var closed = false

        override fun read(nowMs: ULong, readings: AvReadings): AvPlan {
            check(!closed) { "AvAlign object has already been destroyed" }
            this.readings += nowMs to readings
            return plan
        }

        override fun receiversReplaced() {
            check(!closed) { "AvAlign object has already been destroyed" }
            replaced += 1
        }

        override fun close() {
            closed = true
        }
    }

    /** The shared rules, scripted: every rule built is kept, in order. */
    private class FakeRules(
        var readingMs: Long = 1_000L,
        var failCreate: Boolean = false,
    ) : AvAlignRules {
        val built = ArrayList<ScriptedRule>()

        /** What the next rule built answers. */
        var nextPlan: AvPlan = quietPlan()

        override fun create(audioSettable: Boolean, videoSettable: Boolean): AvAlignRule {
            if (failCreate) throw UnsatisfiedLinkError("no native library on the host")
            return ScriptedRule(audioSettable, videoSettable).also {
                it.plan = nextPlan
                built += it
            }
        }

        override fun readingMs(): Long = readingMs

        override fun capabilityName(audioSettable: Boolean, videoSettable: Boolean): String =
            "cap(${audioSettable},${videoSettable})"

        override fun audioReasonName(reason: AvAudioReason): String = "a-${reason.name.lowercase()}"

        override fun videoReasonName(reason: AvVideoReason): String = "v-${reason.name.lowercase()}"
    }

    /** A setter that records, and throws for the kinds in [failing]. */
    private class FakeSetter(
        override val audioSettable: Boolean = true,
        override val videoSettable: Boolean = true,
    ) : ReceiverDelaySetter {
        val writes = ArrayList<String>()
        val failing = HashSet<AvRole>()

        override fun set(peerId: String, role: AvRole, trackId: String, ms: UInt) {
            if (role in failing) throw IllegalStateException("RtpReceiver has been disposed.")
            writes += "$peerId/${role.wireName}/$trackId=$ms"
        }
    }

    /** Writes that wait until the test carries them out, as a thread of
     *  their own would make them wait. */
    private class HeldWrites : ReceiverDelayWrites {
        val waiting = ArrayDeque<() -> Unit>()
        var closed = false

        override fun submit(write: () -> Unit) {
            if (!closed) waiting += write
        }

        override fun close() {
            closed = true
            waiting.clear()
        }

        /** Carry out everything waiting, in the order it was submitted. */
        fun runAll() {
            while (true) (waiting.removeFirstOrNull() ?: return)()
        }
    }

    private companion object {
        fun quietPlan(
            audio: AvAudioPlan = AvAudioPlan(setMs = null, askedMs = 0u, reason = AvAudioReason.FREE),
            main: AvVideoPlan = video(AvVideoReason.ENGINE_SYNCED),
            middle: AvVideoPlan = video(AvVideoReason.NOT_FLOWING),
            light: AvVideoPlan = video(AvVideoReason.NOT_FLOWING),
        ) = AvPlan(audio = audio, main = main, middle = middle, light = light)

        fun video(reason: AvVideoReason, set: UInt? = null, asked: UInt = 0u, skew: Int? = null) =
            AvVideoPlan(setMs = set, askedMs = asked, skewMs = skew, reason = reason)

        fun inbound(trackId: String?, packets: Number, delay: Number, emitted: Number, minimum: Number? = null) =
            RtcStat(
                type = "inbound-rtp",
                members = buildMap {
                    if (trackId != null) put("trackIdentifier", trackId)
                    put("packetsReceived", packets)
                    put("jitterBufferDelay", delay)
                    put("jitterBufferEmittedCount", emitted)
                    if (minimum != null) put("jitterBufferMinimumDelay", minimum)
                },
                id = "in-$trackId",
            )

        fun sample(packets: Number, delay: Number, emitted: Number, minimum: Number = 0) = InboundSample(
            packetsReceived = packets.toDouble(),
            jitterBufferDelay = delay.toDouble(),
            jitterBufferEmittedCount = emitted.toDouble(),
            jitterBufferMinimumDelay = minimum.toDouble(),
        )

        /** One person's four tracks, as the connection records them. */
        fun tracksOf(peer: String, middle: Boolean = true, light: Boolean = true) = buildList {
            add(RemoteTrackRef("$peer-a", peer, audio = true))
            add(RemoteTrackRef("$peer-v", peer, audio = false))
            if (middle) add(RemoteTrackRef("$peer-m", "$peer~mid", audio = false))
            if (light) add(RemoteTrackRef("$peer-l", "$peer~light", audio = false))
        }
    }

    // ---- which receiver is whose -------------------------------------------

    @Test
    fun `a track is a person's audio, main, middle or light by the stream it arrived in`() {
        assertEquals(AvReceiverRef("p7", AvRole.AUDIO), avReceiverOf("p7", audio = true))
        assertEquals(AvReceiverRef("p7", AvRole.MAIN), avReceiverOf("p7", audio = false))
        assertEquals(AvReceiverRef("p7", AvRole.MIDDLE), avReceiverOf("p7~mid", audio = false))
        assertEquals(AvReceiverRef("p7", AvRole.LIGHT), avReceiverOf("p7~light", audio = false))
    }

    @Test
    fun `a track in no stream, and audio under a copy's stream, belong to nobody`() {
        assertNull(avReceiverOf(null, audio = true))
        assertNull(avReceiverOf("", audio = false))
        assertNull(avReceiverOf("p7~light", audio = true))
        assertNull(avReceiverOf("p7~mid", audio = true))
    }

    // ---- statistics -> readings ---------------------------------------------

    @Test
    fun `only inbound entries that name their track become samples`() {
        val samples = inboundSamples(
            listOf(
                inbound("t1", packets = 10, delay = 1.5, emitted = 5, minimum = 0.5),
                inbound(null, packets = 99, delay = 9, emitted = 9),
                RtcStat("outbound-rtp", mapOf("trackIdentifier" to "t2", "packetsReceived" to 3)),
                inbound("t3", packets = 4L, delay = 0, emitted = 0),
            ),
        )
        assertEquals(setOf("t1", "t3"), samples.keys)
        assertEquals(sample(10, 1.5, 5, 0.5), samples["t1"])
        // A report without the network-only figure reads it as zero.
        assertEquals(sample(4, 0, 0, 0), samples["t3"])
    }

    @Test
    fun `an interval is the growth of the counters, as whole milliseconds per emitted`() {
        val input = trackInput(
            prev = sample(packets = 100, delay = 10.0, emitted = 1000, minimum = 4.0),
            cur = sample(packets = 150, delay = 10.0 + 21.0, emitted = 1050, minimum = 4.0 + 6.0),
        )
        // 21 s over 50 emitted is 420 ms; 6 s over 50 is 120 ms.
        assertEquals(TrackInput(flowing = true, delayMs = 420u, networkMs = 120u), input)
    }

    @Test
    fun `nothing emitted is no delay, and no packets is not flowing`() {
        val same = sample(packets = 100, delay = 10, emitted = 1000, minimum = 4)
        assertEquals(TrackInput(flowing = false, delayMs = null, networkMs = null), trackInput(same, same))
        // Packets arrived, nothing left the buffer yet.
        assertEquals(
            TrackInput(flowing = true, delayMs = null, networkMs = null),
            trackInput(same, same.copy(packetsReceived = 101.0)),
        )
    }

    @Test
    fun `a report without the network-only figure gives no network need`() {
        val input = trackInput(sample(0, 0, 0), sample(packets = 10, delay = 2.0, emitted = 10))
        assertEquals(TrackInput(flowing = true, delayMs = 200u, networkMs = null), input)
    }

    @Test
    fun `the first sample, and counters that went back, are measured from zero`() {
        val cur = sample(packets = 10, delay = 3.0, emitted = 10, minimum = 1.0)
        val fromZero = TrackInput(flowing = true, delayMs = 300u, networkMs = 100u)
        assertEquals(fromZero, trackInput(null, cur))
        assertEquals(fromZero, trackInput(sample(packets = 500, delay = 99, emitted = 900, minimum = 9), cur))
        assertEquals(fromZero, trackInput(sample(packets = 5, delay = 99, emitted = 900, minimum = 9), cur))
    }

    @Test
    fun `a receiver that did not report has no reading`() {
        assertEquals(NO_TRACK_INPUT, trackInput(sample(1, 1, 1), null))
        assertEquals(TrackInput(flowing = false, delayMs = null, networkMs = null), NO_TRACK_INPUT)
    }

    // ---- the interval ---------------------------------------------------------

    @Test
    fun `the reading interval is the rule's, or one second when it cannot say`() {
        assertEquals(1_000L, avReadingIntervalMs(FakeRules(readingMs = 1_000L)))
        assertEquals(500L, avReadingIntervalMs(FakeRules(readingMs = 500L)))
        assertEquals(2_000L, avReadingIntervalMs(FakeRules(readingMs = 2_000L)))
        assertEquals(AV_READING_FALLBACK_MS, avReadingIntervalMs(FakeRules(readingMs = 0L)))
        assertEquals(AV_READING_FALLBACK_MS, avReadingIntervalMs(FakeRules(readingMs = 60_000L)))
        val broken = object : AvAlignRules by FakeRules() {
            override fun readingMs(): Long = throw UnsatisfiedLinkError("no native library")
        }
        assertEquals(AV_READING_FALLBACK_MS, avReadingIntervalMs(broken))
    }

    @Test
    fun `the counters are logged every whole number of readings`() {
        assertEquals(2L, readingsPerCountersInterval(1_000L, MEDIA_COUNTERS_INTERVAL_MS))
        assertEquals(1L, readingsPerCountersInterval(2_000L, MEDIA_COUNTERS_INTERVAL_MS))
        assertEquals(4L, readingsPerCountersInterval(500L, MEDIA_COUNTERS_INTERVAL_MS))
        assertEquals(1L, readingsPerCountersInterval(5_000L, MEDIA_COUNTERS_INTERVAL_MS))
        assertEquals(1L, readingsPerCountersInterval(0L, MEDIA_COUNTERS_INTERVAL_MS))
    }

    // ---- which track a tile draws -----------------------------------------------

    @Test
    fun `the board gathers what every tile of a person draws now`() {
        val board = ShownTrackBoard()
        val grid = Any()
        val stage = Any()
        var gridDraws: AvRole? = AvRole.LIGHT
        board.register(grid, "a") { gridDraws }
        board.register(stage, "a") { AvRole.MAIN }
        board.register(Any(), "b") { AvRole.MIDDLE }

        assertEquals(setOf(AvRole.LIGHT, AvRole.MAIN), board.shownOf("a"))
        assertEquals(setOf(AvRole.MIDDLE), board.shownOf("b"))
        assertEquals(emptySet<AvRole>(), board.shownOf("nobody"))

        // A tile that draws nothing now (the app is away) shows nothing.
        gridDraws = null
        assertEquals(setOf(AvRole.MAIN), board.shownOf("a"))
        board.unregister(stage)
        assertEquals(emptySet<AvRole>(), board.shownOf("a"))
        board.reset()
        assertEquals(emptySet<AvRole>(), board.shownOf("b"))
    }

    @Test
    fun `a tile whose rule was released under the question shows nothing`() {
        val board = ShownTrackBoard()
        board.register(Any(), "a") { throw IllegalStateException("LayerDisplay object has already been destroyed") }
        assertEquals(emptySet<AvRole>(), board.shownOf("a"))
    }

    // ---- the log lines -------------------------------------------------------------

    @Test
    fun `one sync line names the audio and each video track the person has`() {
        val state = PeerSyncState(
            audio = AudioSyncState(delayMs = 420u, askedMs = 0u, reason = "no-setter"),
            video = listOf(
                VideoSyncState(AvRole.LIGHT, delayMs = 291u, askedMs = 0u, skewMs = -129, reason = "no-setter"),
                VideoSyncState(AvRole.MAIN, delayMs = 431u, askedMs = 0u, skewMs = 11, reason = "engine-synced"),
            ),
        )
        assertEquals(
            "call media interval sync peer=p7 audio=420/ask=0/no-setter" +
                " main=431/ask=0/skew=11/engine-synced light=291/ask=0/skew=-129/no-setter",
            describeSync(MEDIA_COUNTERS_INTERVAL_LABEL, "p7", state),
        )
    }

    @Test
    fun `a figure that was not measured is a dash`() {
        val state = PeerSyncState(
            audio = AudioSyncState(delayMs = null, askedMs = 300u, reason = "not-flowing"),
            video = listOf(VideoSyncState(AvRole.MIDDLE, null, askedMs = 120u, skewMs = null, reason = "no-sound")),
        )
        assertEquals(
            "x sync peer=q audio=-/ask=300/not-flowing middle=-/ask=120/skew=-/no-sound",
            describeSync("x", "q", state),
        )
    }

    @Test
    fun `the capability is said once, when the call's alignment is built`() {
        val said = ArrayList<String>()
        CallAvAlign(rules = FakeRules(), setter = NoReceiverDelaySetter, info = { said += it })
        assertEquals(
            listOf("call media sync capability=cap(false,false) audio_setter=no video_setter=no interval_ms=1000"),
            said,
        )
        said.clear()
        CallAvAlign(rules = FakeRules(readingMs = 500L), setter = FakeSetter(videoSettable = false), info = { said += it })
        assertEquals(
            listOf("call media sync capability=cap(true,false) audio_setter=yes video_setter=no interval_ms=500"),
            said,
        )
    }

    @Test
    fun `a process without the setter says so when asked to set`() {
        assertFalse(NoReceiverDelaySetter.audioSettable)
        assertFalse(NoReceiverDelaySetter.videoSettable)
        var refused = false
        try {
            NoReceiverDelaySetter.set("a", AvRole.LIGHT, "t", 100u)
        } catch (cause: UnsupportedOperationException) {
            refused = true
        }
        assertTrue(refused)
    }

    // ---- the engine's setter -------------------------------------------------------------

    @Test
    fun `the engine's setter hands on the track, its kind and the delay, and sets both kinds`() {
        val asked = ArrayList<String>()
        val setter = EngineReceiverDelaySetter { trackId, audio, ms ->
            asked += "$trackId/$audio/$ms"
            RECEIVER_DELAY_OK
        }
        assertTrue(setter.audioSettable)
        assertTrue(setter.videoSettable)
        setter.set("a", AvRole.AUDIO, "a-a", 200u)
        setter.set("a", AvRole.LIGHT, "a-l", 0u)
        setter.set("a", AvRole.MIDDLE, "a-m", UInt.MAX_VALUE)
        assertEquals(listOf("a-a/true/200", "a-l/false/0", "a-m/false/10000"), asked)
    }

    @Test
    fun `a set the native module did not carry out is thrown, by what it said`() {
        var status = 1
        val setter = EngineReceiverDelaySetter { _, _, _ -> status }
        var thrown: RuntimeException? = null
        try {
            setter.set("a", AvRole.LIGHT, "a-l", 100u)
        } catch (cause: RuntimeException) {
            thrown = cause
        }
        assertTrue(thrown is ReceiverDelayNotAvailable)
        status = RECEIVER_DELAY_PROBE_FAILED
        thrown = null
        try {
            setter.set("a", AvRole.LIGHT, "a-l", 100u)
        } catch (cause: RuntimeException) {
            thrown = cause
        }
        assertTrue(thrown is ReceiverDelayProbeFailed)
        // A status this client does not know is not a set that took.
        status = 7
        thrown = null
        try {
            setter.set("a", AvRole.AUDIO, "a-a", 100u)
        } catch (cause: RuntimeException) {
            thrown = cause
        }
        assertTrue(thrown is ReceiverDelayNotAvailable)
    }

    @Test
    fun `the setter of a process is the engine's only where the native module offers it`() {
        val none = receiverDelaySetterFor(available = false) { _, _, _ -> RECEIVER_DELAY_OK }
        assertTrue(none === NoReceiverDelaySetter)
        val engine = receiverDelaySetterFor(available = true) { _, _, _ -> RECEIVER_DELAY_OK }
        assertTrue(engine is EngineReceiverDelaySetter)
    }

    @Test
    fun `the statuses are the native module's`() {
        assertEquals(me.lakko.sframe.ReceiverPlayoutDelay.STATUS_OK, RECEIVER_DELAY_OK)
        assertEquals(me.lakko.sframe.ReceiverPlayoutDelay.STATUS_PROBE_FAILED, RECEIVER_DELAY_PROBE_FAILED)
        assertEquals(1, me.lakko.sframe.ReceiverPlayoutDelay.STATUS_NOT_AVAILABLE)
    }

    @Test
    fun `a failed probe is said once and the person goes on observing`() {
        val rules = FakeRules()
        val warnings = ArrayList<String>()
        val setter = EngineReceiverDelaySetter { _, _, _ -> RECEIVER_DELAY_PROBE_FAILED }
        val align = CallAvAlign(rules = rules, setter = setter, warn = { warnings += it })
        rules.nextPlan = quietPlan(
            audio = AvAudioPlan(setMs = 200u, askedMs = 200u, reason = AvAudioReason.WAITING_FOR_PICTURE),
            light = video(AvVideoReason.RAISING, set = 240u),
        )
        val tracks = tracksOf("a")
        align.read(0L, setOf("a"), tracks, emptyMap()) { _, _ -> false }
        align.read(1_000L, setOf("a"), tracks, emptyMap()) { _, _ -> false }
        assertEquals(
            listOf(
                "call media sync peer=a could not set the audio playout delay" +
                    " (ReceiverDelayProbeFailed); continuing as cap(false,true)",
                "call media sync peer=a could not set the video playout delay" +
                    " (ReceiverDelayProbeFailed); continuing as cap(false,false)",
            ),
            warnings,
        )
        // The rule built after the two failures can set neither.
        val last = rules.built.last()
        assertFalse(last.audioSettable)
        assertFalse(last.videoSettable)
    }

    @Test
    fun `the pinned libwebrtc SDK is the one the setter was verified for`() {
        // The drift guard (ADR-0070, note of 2026-10-12). The setter calls
        // the engine by vtable slot; the slot, each ABI's slot encoding and
        // the calling convention were read from the machine code of this SDK
        // version. A bump ships other binaries: the native module then
        // refuses to set (their build ids are not the pinned ones) and this
        // test fails until the evidence in
        // `apps/shared/sframe-bridge/receiver-delay.h` is redone and the new
        // build ids are pinned in `lakko-sframe/src/main/cpp/sframe_jni.cc`.
        val catalog = listOf(
            java.io.File("../gradle/libs.versions.toml"),
            java.io.File("gradle/libs.versions.toml"),
        ).first { it.isFile }
        val pinned = catalog.readLines()
            .map { it.trim() }
            .single { it.startsWith("webrtc-sdk-android") && !it.contains("{") }
            .substringAfter("=").trim().trim('"')
        assertEquals(me.lakko.sframe.ReceiverPlayoutDelay.VERIFIED_SDK_VERSION, pinned)
        assertEquals("125.6422.07", pinned)
    }

    // ---- one call ----------------------------------------------------------------------

    @Test
    fun `each person's receivers reach their own rule, with what is shown`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = FakeSetter())
        val tracks = tracksOf("a") + tracksOf("b", middle = false, light = false)
        val first = mapOf(
            "a-a" to sample(100, 10, 1000, 4),
            "a-v" to sample(100, 10, 100, 2),
            "a-m" to sample(0, 0, 0),
            "a-l" to sample(100, 10, 100),
            "b-a" to sample(100, 10, 1000),
            "b-v" to sample(100, 10, 100),
        )
        align.read(1_000L, setOf("a", "b"), tracks, first) { _, _ -> false }
        val second = mapOf(
            "a-a" to sample(150, 10 + 21.0, 1050, 4 + 6.0),
            "a-v" to sample(130, 10 + 12.9, 130, 2 + 3.0),
            "a-m" to sample(0, 0, 0),
            "a-l" to sample(130, 10 + 8.7, 130),
            "b-a" to sample(150, 10 + 5.0, 1050),
            // b's main track did not report this time.
        )
        align.read(2_000L, setOf("a", "b"), tracks, second) { peer, role -> peer == "a" && role == AvRole.LIGHT }

        assertEquals(2, rules.built.size)
        val (at, a) = rules.built[0].readings[1]
        assertEquals(2_000uL, at)
        assertEquals(true, a.audio.flowing)
        assertEquals(420u, a.audio.delayMs)
        assertEquals(120u, a.audio.networkMs)
        // The main track: libwebrtc synchronises it; not the one drawn.
        assertTrue(a.main.flowing)
        assertTrue(a.main.engineSynced)
        assertFalse(a.main.shown)
        assertEquals(430u, a.main.delayMs)
        assertEquals(100u, a.main.networkMs)
        // The middle track is there and carries nothing.
        assertFalse(a.middle.flowing)
        assertFalse(a.middle.engineSynced)
        assertNull(a.middle.delayMs)
        // The light track is the one a tile draws.
        assertTrue(a.light.flowing)
        assertTrue(a.light.shown)
        assertFalse(a.light.engineSynced)
        assertEquals(290u, a.light.delayMs)
        assertNull(a.light.networkMs)

        val b = rules.built[1].readings[1].second
        assertEquals(100u, b.audio.delayMs)
        assertFalse(b.main.flowing)
        assertNull(b.main.delayMs)
        // Tracks b does not have are absent, and never shown.
        assertFalse(b.middle.flowing)
        assertFalse(b.light.shown)
    }

    @Test
    fun `a track the person does not have is not shown, whatever a tile says`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = FakeSetter())
        align.read(0L, setOf("a"), tracksOf("a", middle = false, light = false), emptyMap()) { _, _ -> true }
        val reading = rules.built.single().readings.single().second
        assertTrue(reading.main.shown)
        assertFalse(reading.middle.shown)
        assertFalse(reading.light.shown)
    }

    @Test
    fun `the rule is built with what this device can set`() {
        val observe = FakeRules()
        CallAvAlign(rules = observe, setter = NoReceiverDelaySetter)
            .read(0L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertFalse(observe.built.single().audioSettable)
        assertFalse(observe.built.single().videoSettable)

        val pictureOnly = FakeRules()
        CallAvAlign(rules = pictureOnly, setter = FakeSetter(audioSettable = false))
            .read(0L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertFalse(pictureOnly.built.single().audioSettable)
        assertTrue(pictureOnly.built.single().videoSettable)
    }

    @Test
    fun `the rule's answers are written to the receivers they name`() {
        val rules = FakeRules()
        val setter = FakeSetter()
        val align = CallAvAlign(rules = rules, setter = setter)
        rules.nextPlan = quietPlan(
            audio = AvAudioPlan(setMs = 300u, askedMs = 300u, reason = AvAudioReason.WAITING_FOR_PICTURE),
            middle = video(AvVideoReason.RAISING, set = 240u, asked = 240u, skew = -180),
            // Zero gives the target back.
            light = video(AvVideoReason.NO_SOUND, set = 0u),
        )
        align.read(0L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertEquals(listOf("a/audio/a-a=300", "a/middle/a-m=240", "a/light/a-l=0"), setter.writes)

        // Nothing to set: nothing is written.
        setter.writes.clear()
        rules.built.single().plan = quietPlan()
        align.read(1_000L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertEquals(emptyList<String>(), setter.writes)
    }

    @Test
    fun `the sync lines carry the rule's own words and the measured figures`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        rules.nextPlan = quietPlan(
            audio = AvAudioPlan(setMs = null, askedMs = 0u, reason = AvAudioReason.NO_SETTER),
            main = video(AvVideoReason.ENGINE_SYNCED, skew = 10),
            light = video(AvVideoReason.NO_SETTER, skew = -130),
        )
        val tracks = tracksOf("a", middle = false)
        assertEquals(emptyList<String>(), align.lines("call media interval"))
        align.read(0L, setOf("a"), tracks, mapOf("a-a" to sample(0, 0, 0), "a-v" to sample(0, 0, 0), "a-l" to sample(0, 0, 0))) { _, _ -> false }
        align.read(
            1_000L,
            setOf("a"),
            tracks,
            mapOf("a-a" to sample(50, 21.0, 50), "a-v" to sample(30, 12.9, 30), "a-l" to sample(30, 8.7, 30)),
        ) { _, _ -> false }
        assertEquals(
            listOf(
                "call media interval sync peer=a audio=420/ask=0/a-no_setter" +
                    " main=430/ask=0/skew=10/v-engine_synced light=290/ask=0/skew=-130/v-no_setter",
            ),
            align.lines("call media interval"),
        )
    }

    @Test
    fun `a write that throws is reported once and the person goes on in the reduced capability`() {
        val rules = FakeRules()
        val setter = FakeSetter().apply { failing += AvRole.LIGHT }
        val warnings = ArrayList<String>()
        val align = CallAvAlign(rules = rules, setter = setter, warn = { warnings += it })
        rules.nextPlan = quietPlan(
            audio = AvAudioPlan(setMs = 200u, askedMs = 200u, reason = AvAudioReason.WAITING_FOR_PICTURE),
            middle = video(AvVideoReason.RAISING, set = 100u),
            light = video(AvVideoReason.RAISING, set = 240u),
        )
        val tracks = tracksOf("a") + tracksOf("b")
        align.read(0L, setOf("a", "b"), tracks, emptyMap()) { _, _ -> false }

        // The audio write went through; the first video write that threw
        // stopped the video writes of that person only.
        assertEquals(2, warnings.size)
        assertEquals(
            "call media sync peer=a could not set the video playout delay" +
                " (IllegalStateException); continuing as cap(true,false)",
            warnings[0],
        )
        assertTrue(setter.writes.contains("a/audio/a-a=200"))
        assertTrue(setter.writes.contains("a/middle/a-m=100"))
        // Each person's first rule was freed and the line of the reading kept.
        assertTrue(rules.built[0].closed)
        assertEquals(2, align.lines("x").size)

        // The next reading builds each a rule that cannot set video, and
        // nothing is asked of a video receiver again.
        setter.writes.clear()
        align.read(1_000L, setOf("a", "b"), tracks, emptyMap()) { _, _ -> false }
        assertEquals(4, rules.built.size)
        assertTrue(rules.built[2].audioSettable)
        assertFalse(rules.built[2].videoSettable)
        assertEquals(listOf("a/audio/a-a=200", "b/audio/b-a=200"), setter.writes)
        assertEquals(2, warnings.size)
    }

    // ---- lifecycle ---------------------------------------------------------------------

    @Test
    fun `a person gets a rule when their first track is read and loses it when they leave`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        // In the call, no track yet: no rule.
        align.read(0L, setOf("a"), emptyList(), emptyMap()) { _, _ -> false }
        assertEquals(0, rules.built.size)

        align.read(1_000L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertEquals(1, rules.built.size)
        // Someone joins mid-call.
        align.read(2_000L, setOf("a", "b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        assertEquals(2, rules.built.size)
        assertEquals(setOf("a", "b"), align.peers())

        align.removePeer("a")
        assertTrue(rules.built[0].closed)
        assertFalse(rules.built[1].closed)
        assertEquals(setOf("b"), align.peers())
        // Their stale tracks are still on the connection: they do not come back.
        align.read(3_000L, setOf("b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        assertEquals(2, rules.built.size)
        assertEquals(2, rules.built[0].readings.size)
        assertEquals(listOf("x sync peer=b audio=-/ask=0/a-free main=-/ask=0/skew=-/v-engine_synced" +
            " middle=-/ask=0/skew=-/v-not_flowing light=-/ask=0/skew=-/v-not_flowing"), align.lines("x"))
    }

    @Test
    fun `a person missing from the people of a reading is dropped without being told`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        align.read(0L, setOf("a", "b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        align.read(1_000L, setOf("b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        assertTrue(rules.built[0].closed)
        assertEquals(1, rules.built[0].readings.size)
        assertEquals(2, rules.built[1].readings.size)
    }

    @Test
    fun `the end of the call frees every rule and nothing is read afterwards`() {
        val rules = FakeRules()
        val setter = FakeSetter()
        val align = CallAvAlign(rules = rules, setter = setter)
        align.read(0L, setOf("a", "b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        align.dispose()
        assertTrue(rules.built.all { it.closed })
        assertEquals(emptyList<String>(), align.lines("x"))

        align.read(1_000L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertEquals(2, rules.built.size)
        assertEquals(emptySet<String>(), align.peers())
        // Leaving after the end is harmless.
        align.removePeer("a")
    }

    @Test
    fun `a new track under a role a person had tells the rule its receivers were replaced`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        val before = tracksOf("a", middle = false)
        align.read(0L, setOf("a"), before, mapOf("a-l" to sample(100, 30.0, 100))) { _, _ -> false }
        assertEquals(0, rules.built.single().replaced)

        // The light track arrived again under another id; the old entry stays
        // on the connection and the later one wins.
        val after = before + RemoteTrackRef("a-l2", "a~light", audio = false)
        align.read(1_000L, setOf("a"), after, mapOf("a-l" to sample(100, 30.0, 100), "a-l2" to sample(10, 2.0, 10))) { _, _ -> false }
        val rule = rules.built.single()
        assertEquals(1, rule.replaced)
        // The new receiver is measured from zero, not against the old one's counters.
        assertEquals(200u, rule.readings[1].second.light.delayMs)

        // A track for a role the person did not have replaces nothing.
        align.read(2_000L, setOf("a"), after + RemoteTrackRef("a-m", "a~mid", audio = false), emptyMap()) { _, _ -> false }
        assertEquals(1, rule.replaced)
    }

    @Test
    fun `every receiver recreated starts every person's counters again`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        val tracks = tracksOf("a", middle = false, light = false)
        align.read(0L, setOf("a"), tracks, mapOf("a-a" to sample(100, 50.0, 100))) { _, _ -> false }
        align.receiversReplaced()
        assertEquals(1, rules.built.single().replaced)
        align.read(1_000L, setOf("a"), tracks, mapOf("a-a" to sample(110, 52.0, 110))) { _, _ -> false }
        // From zero: 52 s over 110, not 2 s over 10.
        assertEquals(473u, rules.built.single().readings[1].second.audio.delayMs)
    }

    @Test
    fun `while the rule cannot be built the samples still move on, and it is tried again`() {
        val rules = FakeRules(failCreate = true)
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        val tracks = tracksOf("a", middle = false, light = false)
        align.read(0L, setOf("a"), tracks, mapOf("a-a" to sample(100, 50.0, 100))) { _, _ -> false }
        assertEquals(emptyList<String>(), align.lines("x"))

        rules.failCreate = false
        align.read(1_000L, setOf("a"), tracks, mapOf("a-a" to sample(110, 52.0, 110))) { _, _ -> false }
        // One interval long: 2 s over 10 emitted.
        assertEquals(200u, rules.built.single().readings.single().second.audio.delayMs)
    }

    @Test
    fun `a rule that throws on a reading leaves the person without a line and the call going`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        align.read(0L, setOf("a", "b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        rules.built[0].closed = true
        align.read(1_000L, setOf("a", "b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        assertEquals(1, align.lines("x").size)
        assertTrue(align.lines("x").single().startsWith("x sync peer=b "))
    }

    @Test
    fun `a clock before zero is read as zero`() {
        val rules = FakeRules()
        val align = CallAvAlign(rules = rules, setter = NoReceiverDelaySetter)
        align.read(-5L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertEquals(0uL, rules.built.single().readings.single().first)
    }

    // ---- writes carried out off the reading thread -------------------------

    private val settingPlan = quietPlan(
        audio = AvAudioPlan(setMs = 300u, askedMs = 300u, reason = AvAudioReason.WAITING_FOR_PICTURE),
        middle = video(AvVideoReason.RAISING, set = 240u, asked = 240u, skew = -180),
        light = video(AvVideoReason.NO_SOUND, set = 0u),
    )

    @Test
    fun `a reading does not wait for its writes, and they are carried out in the rule's order`() {
        val rules = FakeRules()
        val setter = FakeSetter()
        val held = HeldWrites()
        val align = CallAvAlign(rules = rules, setter = setter, writes = held)
        rules.nextPlan = settingPlan
        align.read(0L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        // The reading returned; nothing reached the setter yet.
        assertEquals(emptyList<String>(), setter.writes)
        assertEquals(3, held.waiting.size)
        // A second reading queues behind the first.
        rules.built.single().plan = quietPlan(
            audio = AvAudioPlan(setMs = 0u, askedMs = 0u, reason = AvAudioReason.FREE),
        )
        align.read(1_000L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        held.runAll()
        assertEquals(
            listOf("a/audio/a-a=300", "a/middle/a-m=240", "a/light/a-l=0", "a/audio/a-a=0"),
            setter.writes,
        )
        // Writes that took change nothing about the person's rule.
        align.read(2_000L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        assertEquals(1, rules.built.size)
    }

    @Test
    fun `a write that fails later reduces that person's capability once, at the next reading`() {
        val rules = FakeRules()
        val setter = FakeSetter().apply { failing += AvRole.AUDIO }
        val held = HeldWrites()
        val warnings = ArrayList<String>()
        val align = CallAvAlign(rules = rules, setter = setter, warn = { warnings += it }, writes = held)
        rules.nextPlan = settingPlan
        val tracks = tracksOf("a") + tracksOf("b")
        // Two readings ask before any write has run: two audio writes of
        // each person are waiting when the first one fails.
        align.read(0L, setOf("a", "b"), tracks, emptyMap()) { _, _ -> false }
        align.read(1_000L, setOf("a", "b"), tracks, emptyMap()) { _, _ -> false }
        assertEquals(emptyList<String>(), warnings)
        assertEquals(2, rules.built.size)
        held.runAll()
        // Carried out, failed, and still nothing has changed: the failure
        // waits for the reading thread.
        assertEquals(emptyList<String>(), warnings)
        assertTrue(rules.built.none { it.closed })

        align.read(2_000L, setOf("a", "b"), tracks, emptyMap()) { _, _ -> false }
        // Once per person, though two writes of each failed.
        assertEquals(
            listOf(
                "call media sync peer=a could not set the audio playout delay" +
                    " (IllegalStateException); continuing as cap(false,true)",
                "call media sync peer=b could not set the audio playout delay" +
                    " (IllegalStateException); continuing as cap(false,true)",
            ),
            warnings,
        )
        // Each person's rule was freed and built again, as one that cannot
        // set the sound and still can set the picture.
        assertEquals(4, rules.built.size)
        assertTrue(rules.built[0].closed && rules.built[1].closed)
        for (rebuilt in rules.built.drop(2)) {
            assertFalse(rebuilt.audioSettable)
            assertTrue(rebuilt.videoSettable)
        }
        // The picture's writes took, and no audio write is asked for again.
        assertEquals(
            listOf("a/middle/a-m=240", "a/light/a-l=0", "b/middle/b-m=240", "b/light/b-l=0").sorted(),
            setter.writes.distinct().sorted(),
        )
        held.runAll()
        setter.writes.clear()
        align.read(3_000L, setOf("a", "b"), tracks, emptyMap()) { _, _ -> false }
        held.runAll()
        assertTrue(setter.writes.none { "/audio/" in it })
        assertEquals(2, warnings.size)
    }

    @Test
    fun `a late failure for a receiver that was replaced, or a person who left, changes nothing`() {
        val rules = FakeRules()
        val setter = FakeSetter().apply { failing += AvRole.LIGHT }
        val held = HeldWrites()
        val warnings = ArrayList<String>()
        val align = CallAvAlign(rules = rules, setter = setter, warn = { warnings += it }, writes = held)
        rules.nextPlan = quietPlan(light = video(AvVideoReason.RAISING, set = 240u))
        align.read(0L, setOf("a", "b"), tracksOf("a") + tracksOf("b"), emptyMap()) { _, _ -> false }
        held.runAll()
        // Before the failures are applied: a's light receiver is replaced
        // and b leaves and comes back.
        setter.failing.clear()
        val replaced = tracksOf("a", light = false) + RemoteTrackRef("a-l2", "a~light", audio = false)
        align.removePeer("b")
        align.read(1_000L, setOf("a", "b"), replaced + tracksOf("b"), emptyMap()) { _, _ -> false }
        assertEquals(emptyList<String>(), warnings)
        // Both can still set the picture: the next write goes out.
        held.runAll()
        assertTrue("a/light/a-l2=240" in setter.writes)
        assertTrue("b/light/b-l=240" in setter.writes)
        assertTrue(rules.built.last().videoSettable)
    }

    @Test
    fun `the end of the call closes the writes, and nothing waiting is carried out`() {
        val rules = FakeRules()
        val setter = FakeSetter()
        val held = HeldWrites()
        val align = CallAvAlign(rules = rules, setter = setter, writes = held)
        rules.nextPlan = settingPlan
        align.read(0L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        align.dispose()
        assertTrue(held.closed)
        held.runAll()
        align.read(1_000L, setOf("a"), tracksOf("a"), emptyMap()) { _, _ -> false }
        held.runAll()
        assertEquals(emptyList<String>(), setter.writes)
    }

    @Test
    fun `the writes' own thread carries them out in order and is not the caller's`() {
        val writes = SerialReceiverDelayWrites("test-receiver-delay")
        val seen = java.util.Collections.synchronizedList(ArrayList<Int>())
        val threads = java.util.Collections.synchronizedSet(HashSet<Thread>())
        val done = java.util.concurrent.CountDownLatch(1)
        repeat(200) { n ->
            writes.submit {
                threads += Thread.currentThread()
                seen += n
            }
        }
        writes.submit { done.countDown() }
        assertTrue(done.await(10, java.util.concurrent.TimeUnit.SECONDS))
        assertEquals((0 until 200).toList(), seen.toList())
        assertEquals(1, threads.size)
        assertFalse(Thread.currentThread() in threads)
        assertTrue(threads.single().isDaemon)
        assertEquals("test-receiver-delay", threads.single().name)
        writes.close()
        assertTrue(writes.awaitEnded(10_000L))
    }

    @Test
    fun `after close the writes' thread finishes the write in flight, drops the rest and ends`() {
        val writes = SerialReceiverDelayWrites("test-receiver-delay")
        val started = java.util.concurrent.CountDownLatch(1)
        val release = java.util.concurrent.CountDownLatch(1)
        val ran = java.util.Collections.synchronizedList(ArrayList<String>())
        writes.submit {
            started.countDown()
            release.await(10, java.util.concurrent.TimeUnit.SECONDS)
            ran += "in flight"
        }
        writes.submit { ran += "waiting" }
        assertTrue(started.await(10, java.util.concurrent.TimeUnit.SECONDS))
        // Closing does not wait for the write in flight and does not stop it.
        writes.close()
        writes.close()
        writes.submit { ran += "late" }
        assertFalse(writes.awaitEnded(50L))
        release.countDown()
        assertTrue(writes.awaitEnded(10_000L))
        assertEquals(listOf("in flight"), ran.toList())
    }

    @Test
    fun `a call that can set gets a thread for its writes, one that cannot gets none`() {
        val none = receiverDelayWritesFor(NoReceiverDelaySetter)
        assertTrue(none is InlineReceiverDelayWrites)
        var ran = 0
        none.submit { ran += 1 }
        assertEquals(1, ran)
        none.close()
        none.submit { ran += 1 }
        assertEquals(1, ran)

        val engine = receiverDelayWritesFor(EngineReceiverDelaySetter { _, _, _ -> RECEIVER_DELAY_OK })
        assertTrue(engine is SerialReceiverDelayWrites)
        engine.close()
        assertTrue((engine as SerialReceiverDelayWrites).awaitEnded(10_000L))
    }
}
