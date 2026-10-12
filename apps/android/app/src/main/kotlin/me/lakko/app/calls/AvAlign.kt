// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.AvAlign
import uniffi.lakko_ffi_uniffi.AvAudioReading
import uniffi.lakko_ffi_uniffi.AvAudioReason
import uniffi.lakko_ffi_uniffi.AvPlan
import uniffi.lakko_ffi_uniffi.AvReadings
import uniffi.lakko_ffi_uniffi.AvVideoPlan
import uniffi.lakko_ffi_uniffi.AvVideoReading
import uniffi.lakko_ffi_uniffi.AvVideoReason
import uniffi.lakko_ffi_uniffi.avAlignCapability
import uniffi.lakko_ffi_uniffi.avAlignReadingMs
import uniffi.lakko_ffi_uniffi.avAudioReasonName
import uniffi.lakko_ffi_uniffi.avCapabilityName
import uniffi.lakko_ffi_uniffi.avVideoReasonName
import kotlin.math.roundToLong

/**
 * Keeping a remote person's sound and picture together in a group call: the
 * Android side of the shared rule (ADR-0070, note of 2026-10-12). The twin of
 * the web `av-align.ts`.
 *
 * A subscriber gets, per remote person, one audio track and up to three video
 * tracks: main (stream id `<peer>`), middle (`<peer>~mid`) and light
 * (`<peer>~light`). libwebrtc's own lip sync covers the main track only (it
 * shares the audio's stream id). The rule that ties the others to the sound is
 * written once, in Rust (`crates/lakko-ui-rules`, `av_align`), and reached
 * here over UniFFI as [AvAlign]. Nothing of the rule is restated in this file.
 *
 * What is here is what the rule cannot know:
 *
 *  * which receiver is which (person, audio | main | middle | light):
 *    [avReceiverOf];
 *  * the receivers' statistics as the rule's inputs: [inboundSamples],
 *    [trackInput];
 *  * which track a tile draws ([ShownTrackBoard]);
 *  * whether this device can set a receiver's playout delay
 *    ([ReceiverDelaySetter]), and the write itself;
 *  * the log lines ([describeSync], [describeCapability]).
 *
 * ## What Android can do
 *
 * The pinned SDK (`io.github.webrtc-sdk:android:125.6422.07`) binds no way
 * to set a receiver's playout delay. `org.webrtc.RtpReceiver` is, in full:
 * `track()`, `getParameters()`, `id()`, `dispose()`, `SetObserver(Observer)`
 * and `setFrameDecryptor(FrameDecryptor)`. The engine behind it has one,
 * `RtpReceiverInterface::SetJitterBufferMinimumDelay`, and Lakko's native
 * module reaches it (`me.lakko.sframe.ReceiverPlayoutDelay`; the evidence is
 * in `apps/shared/sframe-bridge/receiver-delay.h` and ADR-0070, note of
 * 2026-10-12). That module offers it only when the engine library loaded in
 * this process is the binary the evidence was read from, and stops for the
 * whole process if the engine's object fails its probe.
 *
 * So the setter is [EngineReceiverDelaySetter] where the module says it is
 * available and [NoReceiverDelaySetter] where it does not
 * ([receiverDelaySetterFor]); with the second the rule is fed, the skew is
 * measured and logged, and nothing is written.
 *
 * No Android or libwebrtc type: the host JVM tests drive all of it with a
 * fake [AvAlignRules], since they cannot load the native library.
 */

/** One of a remote person's receivers. */
internal enum class AvRole(val wireName: String) {
    AUDIO("audio"),
    MAIN("main"),
    MIDDLE("middle"),
    LIGHT("light"),
}

/** The video roles in the order the rule and the log line name them. */
internal val AV_VIDEO_ROLES: List<AvRole> = listOf(AvRole.MAIN, AvRole.MIDDLE, AvRole.LIGHT)

/** Whose receiver, and which of theirs. */
internal data class AvReceiverRef(val peerId: String, val role: AvRole)

/**
 * The person and the role of a remote track from the MSID stream id it
 * arrived under, the way the tile routing reads it ([splitStreamId]): audio
 * and the main picture under `<peer>`, the middle track under `<peer>~mid`,
 * the light companion under `<peer>~light`. `null` for a track that arrived
 * in no stream, and for an audio track under a copy's stream id (the SFU
 * sends none).
 */
internal fun avReceiverOf(streamId: String?, audio: Boolean): AvReceiverRef? {
    if (streamId.isNullOrEmpty()) return null
    val id = splitStreamId(streamId)
    if (audio) {
        return if (id.role == RemoteStreamRole.MAIN) AvReceiverRef(id.peerId, AvRole.AUDIO) else null
    }
    val role = when (id.role) {
        RemoteStreamRole.MAIN -> AvRole.MAIN
        RemoteStreamRole.MIDDLE -> AvRole.MIDDLE
        RemoteStreamRole.LIGHT -> AvRole.LIGHT
    }
    return AvReceiverRef(id.peerId, role)
}

/** A remote track of the connection: its id, the stream it arrived in and
 *  its kind. What the peer connection recorded when the track arrived. */
internal data class RemoteTrackRef(val trackId: String, val streamId: String, val audio: Boolean)

// ---------------------------------------------------------------------------
// Statistics as the rule's inputs
// ---------------------------------------------------------------------------

/** The cumulative counters of one `inbound-rtp` entry the rule's inputs are
 *  made of. */
internal data class InboundSample(
    val packetsReceived: Double,
    /** Seconds, summed over everything that left the jitter buffer. */
    val jitterBufferDelay: Double,
    val jitterBufferEmittedCount: Double,
    /** Seconds, the same sum for the delay the network alone would need;
     *  zero where the report does not carry it. */
    val jitterBufferMinimumDelay: Double,
)

private fun number(stat: RtcStat, key: String): Double =
    (stat.members[key] as? Number)?.toDouble()?.takeIf { it.isFinite() } ?: 0.0

/**
 * The `inbound-rtp` entries of a connection's statistics report, by the id of
 * the track each belongs to (`trackIdentifier`). An entry that does not name
 * its track is left out: nothing else ties it to a person.
 */
internal fun inboundSamples(stats: Iterable<RtcStat>): Map<String, InboundSample> {
    val out = LinkedHashMap<String, InboundSample>()
    for (stat in stats) {
        if (stat.type != "inbound-rtp") continue
        val trackId = (stat.members["trackIdentifier"] as? String)?.takeIf { it.isNotEmpty() } ?: continue
        out[trackId] = InboundSample(
            packetsReceived = number(stat, "packetsReceived"),
            jitterBufferDelay = number(stat, "jitterBufferDelay"),
            jitterBufferEmittedCount = number(stat, "jitterBufferEmittedCount"),
            jitterBufferMinimumDelay = number(stat, "jitterBufferMinimumDelay"),
        )
    }
    return out
}

/** What one receiver reports for one interval, as the rule takes it. */
internal data class TrackInput(
    /** Packets arrived in the interval. */
    val flowing: Boolean,
    /** Average time in the jitter buffer over the interval, whole
     *  milliseconds; `null` when nothing left the buffer. */
    val delayMs: UInt?,
    /** The delay the network alone would need, whole milliseconds; `null`
     *  when not measured. */
    val networkMs: UInt?,
)

/** A receiver with no reading: not flowing, nothing measured. */
internal val NO_TRACK_INPUT: TrackInput = TrackInput(flowing = false, delayMs = null, networkMs = null)

/** The largest figure handed to the rule, milliseconds: far above anything a
 *  receiver reports, and inside what a `UInt` holds. The rule bounds its
 *  readings itself; this only keeps the conversion total. */
private const val INPUT_MAX_MS: Long = 1_000_000L

/**
 * One interval of one receiver: [cur] against [prev]. With no [prev], or with
 * counters that went backwards (a stream replaced under the same track), the
 * interval is measured from zero. With no [cur] the receiver reported nothing.
 */
internal fun trackInput(prev: InboundSample?, cur: InboundSample?): TrackInput {
    if (cur == null) return NO_TRACK_INPUT
    val base = prev?.takeIf {
        it.packetsReceived <= cur.packetsReceived && it.jitterBufferEmittedCount <= cur.jitterBufferEmittedCount
    }
    fun d(read: (InboundSample) -> Double): Double = maxOf(0.0, read(cur) - (base?.let(read) ?: 0.0))
    val emitted = d { it.jitterBufferEmittedCount }
    fun perEmittedMs(seconds: Double): UInt =
        (seconds / emitted * 1000).roundToLong().coerceIn(0L, INPUT_MAX_MS).toUInt()
    val network = d { it.jitterBufferMinimumDelay }
    return TrackInput(
        flowing = d { it.packetsReceived } > 0,
        delayMs = if (emitted > 0) perEmittedMs(d { it.jitterBufferDelay }) else null,
        networkMs = if (emitted > 0 && network > 0) perEmittedMs(network) else null,
    )
}

// ---------------------------------------------------------------------------
// Which track a tile draws
// ---------------------------------------------------------------------------

/**
 * Which track every remote tile of a call draws now, by SFU peer id.
 *
 * A person can be drawn by several tiles at once (the grid and the stage), on
 * different tracks, so each tile registers a question and the board asks them
 * all. A tile answers `null` while it draws nothing (released, or the app is
 * not in the foreground). Thread-safe: tiles come and go on the main thread
 * and the call's reading loop asks from its own scope.
 */
internal class ShownTrackBoard {
    private val tiles = LinkedHashMap<Any, Pair<String, () -> AvRole?>>()

    /** [tile] (any object that is the same for the tile's lifetime) draws a
     *  picture of [peerId]; [drawn] says which track, when asked. */
    @Synchronized
    fun register(tile: Any, peerId: String, drawn: () -> AvRole?) {
        tiles[tile] = peerId to drawn
    }

    /** The tile is gone. */
    @Synchronized
    fun unregister(tile: Any) {
        tiles.remove(tile)
    }

    /** The tracks of [peerId] some tile draws now. */
    fun shownOf(peerId: String): Set<AvRole> {
        val asks = synchronized(this) { tiles.values.filter { it.first == peerId }.map { it.second } }
        val out = HashSet<AvRole>()
        for (ask in asks) {
            val role = try {
                ask()
            } catch (cause: RuntimeException) {
                // The tile's rule was released under the question.
                null
            }
            if (role != null) out += role
        }
        return out
    }

    /** Forget every tile: a new call starts. */
    @Synchronized
    fun reset() {
        tiles.clear()
    }
}

/** The board of the call this process is in: the tiles write to it, the SFU
 *  session's reading loop asks it. */
internal val shownTrackBoard: ShownTrackBoard = ShownTrackBoard()

// ---------------------------------------------------------------------------
// The shared rule, as this module reaches it
// ---------------------------------------------------------------------------

/** One person's rule: the part of the shared [AvAlign] this module calls. */
internal interface AvAlignRule {
    fun read(nowMs: ULong, readings: AvReadings): AvPlan

    fun receiversReplaced()

    /** Free the rule. Nothing is asked of it afterwards. */
    fun close()
}

/**
 * The shared rule and the words it has for its answers, behind an interface:
 * production passes [SharedAvAlignRules], which is the binding and nothing
 * else, and a host test passes a fake. The rule itself is tested where it is
 * written, in `crates/lakko-ui-rules/src/av_align.rs`.
 */
internal interface AvAlignRules {
    fun create(audioSettable: Boolean, videoSettable: Boolean): AvAlignRule

    /** How often the rule expects to be read, milliseconds. */
    fun readingMs(): Long

    fun capabilityName(audioSettable: Boolean, videoSettable: Boolean): String

    fun audioReasonName(reason: AvAudioReason): String

    fun videoReasonName(reason: AvVideoReason): String
}

/** The rule every client shares, over UniFFI. Never re-implemented here. */
internal object SharedAvAlignRules : AvAlignRules {
    override fun create(audioSettable: Boolean, videoSettable: Boolean): AvAlignRule {
        val rule = AvAlign(audioSettable, videoSettable)
        return object : AvAlignRule {
            override fun read(nowMs: ULong, readings: AvReadings): AvPlan = rule.read(nowMs, readings)

            override fun receiversReplaced() = rule.receiversReplaced()

            override fun close() = rule.close()
        }
    }

    override fun readingMs(): Long = avAlignReadingMs().toLong()

    override fun capabilityName(audioSettable: Boolean, videoSettable: Boolean): String =
        avCapabilityName(avAlignCapability(audioSettable, videoSettable))

    override fun audioReasonName(reason: AvAudioReason): String = avAudioReasonName(reason)

    override fun videoReasonName(reason: AvVideoReason): String = avVideoReasonName(reason)
}

/** The reading interval used when the rule cannot be asked or answers
 *  something a timer cannot run at, milliseconds. */
internal const val AV_READING_FALLBACK_MS: Long = 1_000L

/**
 * The rule's reading interval as a timer period: what the rule says, or
 * [AV_READING_FALLBACK_MS] when it cannot say or says something outside a
 * quarter of a second to two seconds (the longest interval the rule counts in
 * full).
 */
internal fun avReadingIntervalMs(rules: AvAlignRules): Long =
    try {
        rules.readingMs().takeIf { it in 250L..2_000L } ?: AV_READING_FALLBACK_MS
    } catch (cause: RuntimeException) {
        AV_READING_FALLBACK_MS
    } catch (cause: LinkageError) {
        AV_READING_FALLBACK_MS
    }

/**
 * How many readings of the rule make one interval of the media counters'
 * log: the statistics are read once per reading, and the counters' lines are
 * written every this many readings. At least one.
 */
internal fun readingsPerCountersInterval(readingMs: Long, countersMs: Long): Long =
    if (readingMs <= 0L) 1L else ((countersMs + readingMs / 2) / readingMs).coerceAtLeast(1L)

// ---------------------------------------------------------------------------
// Setting a receiver's playout delay
// ---------------------------------------------------------------------------

/**
 * What this device can set on a receiver. Decided in Lakko's code from what
 * the media SDK offers, never assumed of the engine.
 */
internal interface ReceiverDelaySetter {
    /** An audio receiver's target playout delay can be set. */
    val audioSettable: Boolean

    /** A video receiver's target playout delay can be set. */
    val videoSettable: Boolean

    /**
     * Set the target playout delay of [peerId]'s [role] receiver (the one of
     * track [trackId]) to [ms] milliseconds; `0` gives the target back to the
     * engine. May throw: the caller reports it once and stops asking for that
     * kind of that person's receivers.
     */
    fun set(peerId: String, role: AvRole, trackId: String, ms: UInt)
}

/**
 * The setter of a process in which the engine's method cannot be reached
 * (see the file's header). Asked to set, it refuses, so a caller that got
 * the capability wrong finds out.
 */
internal object NoReceiverDelaySetter : ReceiverDelaySetter {
    override val audioSettable: Boolean = false
    override val videoSettable: Boolean = false

    override fun set(peerId: String, role: AvRole, trackId: String, ms: UInt): Unit =
        throw UnsupportedOperationException("no playout-delay setter is available in this process")
}

/** A set the native module did not carry out: the receiver is gone, or the
 *  setter is not available in this process. */
internal class ReceiverDelayNotAvailable : IllegalStateException("not-available")

/** A set the native module refused because the engine's object failed its
 *  probe; nothing is set for the rest of the process. */
internal class ReceiverDelayProbeFailed : IllegalStateException("probe-failed")

/** What [EngineReceiverDelaySetter] is told when a set took. The native
 *  module's `ReceiverPlayoutDelay.STATUS_OK`. */
internal const val RECEIVER_DELAY_OK: Int = 0

/** The native module's `ReceiverPlayoutDelay.STATUS_PROBE_FAILED`. */
internal const val RECEIVER_DELAY_PROBE_FAILED: Int = 2

/**
 * The setter that reaches the engine's
 * `RtpReceiverInterface::SetJitterBufferMinimumDelay` through Lakko's native
 * module. Both kinds go through the one engine method, so both are settable.
 *
 * [setOn] carries one set out on the receiver of a track (`trackId`, whether
 * it is an audio track, milliseconds; `0` gives the target back) and returns
 * the module's status. Anything but [RECEIVER_DELAY_OK] is thrown, which is
 * how [CallAvAlign] learns to stop asking for that kind of that person.
 *
 * No Android type here: the peer connection supplies [setOn].
 */
internal class EngineReceiverDelaySetter(
    private val setOn: (trackId: String, audio: Boolean, ms: Int) -> Int,
) : ReceiverDelaySetter {
    override val audioSettable: Boolean = true
    override val videoSettable: Boolean = true

    override fun set(peerId: String, role: AvRole, trackId: String, ms: UInt) {
        // The rule asks for at most a few seconds; the engine's own ceiling
        // is ten. Kept inside an Int whatever is asked.
        val bounded = ms.coerceAtMost(RECEIVER_DELAY_MAX_MS).toInt()
        when (setOn(trackId, role == AvRole.AUDIO, bounded)) {
            RECEIVER_DELAY_OK -> Unit
            RECEIVER_DELAY_PROBE_FAILED -> throw ReceiverDelayProbeFailed()
            else -> throw ReceiverDelayNotAvailable()
        }
    }
}

/** The engine clamps a minimum playout delay to ten seconds; nothing larger
 *  is sent. */
internal const val RECEIVER_DELAY_MAX_MS: UInt = 10_000u

/**
 * The setter of this process: the engine's when the native module says the
 * setter is [available], none otherwise.
 */
internal fun receiverDelaySetterFor(
    available: Boolean,
    setOn: (trackId: String, audio: Boolean, ms: Int) -> Int,
): ReceiverDelaySetter =
    if (available) EngineReceiverDelaySetter(setOn) else NoReceiverDelaySetter

// ---------------------------------------------------------------------------
// The log lines
// ---------------------------------------------------------------------------

/** What the last reading said of a person's audio. */
internal data class AudioSyncState(val delayMs: UInt?, val askedMs: UInt, val reason: String)

/** What the last reading said of one of a person's video tracks. */
internal data class VideoSyncState(
    val role: AvRole,
    val delayMs: UInt?,
    val askedMs: UInt,
    /** The track's delay minus the audio's; `null` when either was not
     *  measured. */
    val skewMs: Int?,
    val reason: String,
)

/** What the last reading said of one person. */
internal data class PeerSyncState(
    val audio: AudioSyncState,
    /** The video tracks the person has, main first. */
    val video: List<VideoSyncState>,
)

/**
 * One person in one line (ASCII, no names; [peerId] is the SFU's per-call
 * peer id). The audio as `delay/ask=asked/reason`, each video track the
 * person has as `delay/ask=asked/skew=skew/reason`, main first; a figure
 * that was not measured is `-`, `ask=0` is nothing asked. The reasons are the
 * shared rule's own words. The web client's `describeSync` fields. For
 * example:
 *
 * `call media interval sync peer=p7 audio=420/ask=0/no-setter main=431/ask=0/skew=11/engine-synced light=291/ask=0/skew=-129/no-setter`
 */
internal fun describeSync(label: String, peerId: String, state: PeerSyncState): String {
    val audio = "audio=${state.audio.delayMs ?: "-"}/ask=${state.audio.askedMs}/${state.audio.reason}"
    val video = AV_VIDEO_ROLES.mapNotNull { role -> state.video.firstOrNull { it.role == role } }.map { v ->
        "${v.role.wireName}=${v.delayMs ?: "-"}/ask=${v.askedMs}/skew=${v.skewMs ?: "-"}/${v.reason}"
    }
    return (listOf("$label sync peer=$peerId", audio) + video).joinToString(" ")
}

private fun yesNo(value: Boolean): String = if (value) "yes" else "no"

/**
 * What the rule can achieve on this device, in one line, logged once at the
 * start of a call. `capability` is the shared rule's word for the two
 * setters. For example:
 *
 * `call media sync capability=observe-only audio_setter=no video_setter=no interval_ms=1000`
 */
internal fun describeCapability(
    label: String,
    capability: String,
    audioSettable: Boolean,
    videoSettable: Boolean,
    intervalMs: Long,
): String =
    "$label sync capability=$capability audio_setter=${yesNo(audioSettable)}" +
        " video_setter=${yesNo(videoSettable)} interval_ms=$intervalMs"

// ---------------------------------------------------------------------------
// One call
// ---------------------------------------------------------------------------

/**
 * The alignment of every remote person of one call.
 *
 * Once per reading interval the caller hands over the people of the call, the
 * remote tracks the connection knows, the receivers' samples and a way to ask
 * what is shown ([read]); the rule's answers are written through the
 * [ReceiverDelaySetter] before it returns. A person's rule is built at their
 * first reading and freed when they leave ([removePeer], or their absence
 * from the people of a later reading) or the call ends ([dispose]).
 *
 * A write that throws is warned of once per person and kind, and that
 * person's rule is built again as one that cannot set that kind, so it goes
 * on measuring and stops asking.
 *
 * Not thread-safe: one caller, one thread (the call's scope).
 */
internal class CallAvAlign(
    private val rules: AvAlignRules = SharedAvAlignRules,
    private val setter: ReceiverDelaySetter = NoReceiverDelaySetter,
    private val warn: (String) -> Unit = {},
    info: (String) -> Unit = {},
    label: String = "call media",
) {
    private class Slot(val trackId: String) {
        var prev: InboundSample? = null
    }

    private class Person {
        var rule: AvAlignRule? = null
        var audioWriteFailed = false
        var videoWriteFailed = false
        val slots = LinkedHashMap<AvRole, Slot>()
        var last: PeerSyncState? = null
    }

    private val people = LinkedHashMap<String, Person>()
    private var disposed = false

    init {
        val capability = try {
            rules.capabilityName(setter.audioSettable, setter.videoSettable)
        } catch (cause: RuntimeException) {
            "unknown"
        } catch (cause: LinkageError) {
            "unknown"
        }
        info(describeCapability(label, capability, setter.audioSettable, setter.videoSettable, readingMs()))
    }

    /** How often [read] is to be called, milliseconds. */
    fun readingMs(): Long = avReadingIntervalMs(rules)

    /** The people a rule is held for now. */
    fun peers(): Set<String> = people.keys.toSet()

    /** [peerId] left the call: their rule is freed. */
    fun removePeer(peerId: String) {
        val person = people.remove(peerId) ?: return
        freeRule(person)
    }

    /** Every receiver of the call was recreated: nothing has been told to the
     *  new ones, and their counters start again. */
    fun receiversReplaced() {
        for (person in people.values) replaced(person)
    }

    private fun replaced(person: Person) {
        try {
            person.rule?.receiversReplaced()
        } catch (cause: RuntimeException) {
            // Freed under us; the next reading builds it again.
            person.rule = null
        }
        for (slot in person.slots.values) slot.prev = null
    }

    /**
     * The remote tracks the connection knows now, for the people of the call.
     * A track of someone not in [peerIds] is ignored and a person no longer
     * in it is dropped. A different track under a role a person already had
     * means their receiver was replaced: the rule is told.
     */
    private fun sync(peerIds: Set<String>, tracks: Iterable<RemoteTrackRef>) {
        for (gone in people.keys.filter { it !in peerIds }) removePeer(gone)
        // A later track under the same person and role wins.
        val current = LinkedHashMap<AvReceiverRef, String>()
        for (track in tracks) {
            val ref = avReceiverOf(track.streamId, track.audio) ?: continue
            if (ref.peerId in peerIds) current[ref] = track.trackId
        }
        for ((ref, trackId) in current) {
            val person = people.getOrPut(ref.peerId) { Person() }
            val before = person.slots[ref.role]
            if (before?.trackId == trackId) continue
            person.slots[ref.role] = Slot(trackId)
            if (before != null) replaced(person)
        }
    }

    /**
     * One reading interval, taken at [nowMs] (one monotonic clock, whole
     * milliseconds). [peerIds] are the other people of the call, [tracks] the
     * connection's remote tracks, [byTrack] the sample of every receiver that
     * reported ([inboundSamples]); [shown] says whether a tile draws a track
     * of a person now.
     */
    fun read(
        nowMs: Long,
        peerIds: Set<String>,
        tracks: Iterable<RemoteTrackRef>,
        byTrack: Map<String, InboundSample>,
        shown: (peerId: String, role: AvRole) -> Boolean,
    ) {
        if (disposed) return
        sync(peerIds, tracks)
        for ((peerId, person) in people) {
            if (person.slots.isEmpty()) {
                person.last = null
                continue
            }
            val rule = ruleOf(person)
            // The samples move on whether or not the rule could be asked, so
            // the first interval it is asked about is one interval long.
            val inputs = HashMap<AvRole, TrackInput>()
            for ((role, slot) in person.slots) {
                val cur = byTrack[slot.trackId]
                inputs[role] = trackInput(slot.prev, cur)
                if (cur != null) slot.prev = cur
            }
            if (rule == null) {
                person.last = null
                continue
            }
            fun video(role: AvRole): AvVideoReading {
                val input = inputs[role] ?: NO_TRACK_INPUT
                return AvVideoReading(
                    flowing = input.flowing,
                    shown = role in person.slots && shown(peerId, role),
                    // libwebrtc synchronises the main track itself: it shares
                    // the audio's stream id.
                    engineSynced = role == AvRole.MAIN,
                    delayMs = input.delayMs,
                    networkMs = input.networkMs,
                )
            }
            val audio = inputs[AvRole.AUDIO] ?: NO_TRACK_INPUT
            val plan = try {
                rule.read(
                    nowMs.coerceAtLeast(0L).toULong(),
                    AvReadings(
                        audio = AvAudioReading(audio.flowing, audio.delayMs, audio.networkMs),
                        main = video(AvRole.MAIN),
                        middle = video(AvRole.MIDDLE),
                        light = video(AvRole.LIGHT),
                    ),
                )
            } catch (cause: RuntimeException) {
                person.last = null
                continue
            }
            fun planOf(role: AvRole): AvVideoPlan = when (role) {
                AvRole.MIDDLE -> plan.middle
                AvRole.LIGHT -> plan.light
                else -> plan.main
            }
            person.last = PeerSyncState(
                audio = AudioSyncState(audio.delayMs, plan.audio.askedMs, audioReason(plan.audio.reason)),
                video = AV_VIDEO_ROLES.filter { it in person.slots }.map { role ->
                    val p = planOf(role)
                    VideoSyncState(role, inputs[role]?.delayMs, p.askedMs, p.skewMs, videoReason(p.reason))
                },
            )
            // The answers are taken before any is written: a write that
            // throws builds the person's rule again.
            val sets = ArrayList<Pair<AvRole, UInt>>()
            plan.audio.setMs?.let { sets += AvRole.AUDIO to it }
            for (role in AV_VIDEO_ROLES) planOf(role).setMs?.let { sets += role to it }
            for ((role, ms) in sets) write(peerId, person, role, ms)
        }
    }

    /** One line per person the last reading covered, in the order they
     *  joined. */
    fun lines(label: String): List<String> =
        people.mapNotNull { (peerId, person) -> person.last?.let { describeSync(label, peerId, it) } }

    /** The call is over: every rule is freed. Nothing is read or written
     *  afterwards. */
    fun dispose() {
        for (person in people.values) freeRule(person)
        people.clear()
        disposed = true
    }

    private fun audioReason(reason: AvAudioReason): String =
        try {
            rules.audioReasonName(reason)
        } catch (cause: RuntimeException) {
            "-"
        } catch (cause: LinkageError) {
            "-"
        }

    private fun videoReason(reason: AvVideoReason): String =
        try {
            rules.videoReasonName(reason)
        } catch (cause: RuntimeException) {
            "-"
        } catch (cause: LinkageError) {
            "-"
        }

    /** The person's rule, built on first use; `null` while the rule cannot
     *  be built. */
    private fun ruleOf(person: Person): AvAlignRule? {
        person.rule?.let { return it }
        person.rule = try {
            rules.create(
                setter.audioSettable && !person.audioWriteFailed,
                setter.videoSettable && !person.videoWriteFailed,
            )
        } catch (cause: RuntimeException) {
            null
        } catch (cause: LinkageError) {
            null
        }
        return person.rule
    }

    private fun freeRule(person: Person) {
        val rule = person.rule
        person.rule = null
        person.last = null
        try {
            rule?.close()
        } catch (cause: RuntimeException) {
            // Already freed; nothing is held either way.
        }
    }

    /** Write one answer. A write that throws is reported once for that
     *  person and kind, and their rule is built again without that setter. */
    private fun write(peerId: String, person: Person, role: AvRole, ms: UInt) {
        val audio = role == AvRole.AUDIO
        if (if (audio) person.audioWriteFailed else person.videoWriteFailed) return
        val slot = person.slots[role] ?: return
        try {
            setter.set(peerId, role, slot.trackId, ms)
        } catch (cause: RuntimeException) {
            if (audio) person.audioWriteFailed = true else person.videoWriteFailed = true
            val kind = if (audio) "audio" else "video"
            val capability = try {
                rules.capabilityName(
                    setter.audioSettable && !person.audioWriteFailed,
                    setter.videoSettable && !person.videoWriteFailed,
                )
            } catch (inner: RuntimeException) {
                "unknown"
            } catch (inner: LinkageError) {
                "unknown"
            }
            warn(
                "call media sync peer=$peerId could not set the $kind playout delay" +
                    " (${cause.javaClass.simpleName}); continuing as $capability",
            )
            // The rule takes what it can set when it is built, so it is built
            // again; it starts as for a person who has just joined.
            val last = person.last
            freeRule(person)
            person.last = last
            for (s in person.slots.values) s.prev = null
        }
    }
}
