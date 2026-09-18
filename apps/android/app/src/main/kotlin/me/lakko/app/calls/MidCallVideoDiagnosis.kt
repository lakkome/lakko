// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Why a mid-call camera-on produced no picture on the other phone.
 *
 * ## Why this exists
 *
 * Live, Android calling iOS: the caller presses Camera on a voice call, their
 * own screen fills with their camera, the renegotiated SDP reaches iOS, iOS
 * attaches the remote video track — and then waits for a first frame that never
 * comes, for over a minute.
 *
 * Every one of the three plausible causes looks **identical** from either end:
 * a sender that never encodes, a transceiver left in a direction that does not
 * send (or a renegotiation whose answer never landed), and a video sender whose
 * frames are not SFrame-sealed, so the peer's decryptor drops all of them in
 * silence. The sender sees its own local preview in all three cases, because
 * the preview renders the capture and never touches the peer connection; the
 * receiver sees a track and no frames in all three.
 *
 * So the next live call must not require guessing. [midCallVideoVerdict] reads
 * three facts a few seconds after the camera-on and names which one it was;
 * [midCallVideoReport] is the single line the console prints.
 *
 * Pure and Android-free: the facts are gathered by the platform and the
 * conclusion is drawn here, so the reasoning is unit-tested on the host JVM
 * rather than only on a two-phone desk.
 */

/**
 * How long after the RENEGOTIATION ANSWER the probe reads the counters.
 *
 * The anchor matters more than the number, and getting it wrong wasted a live
 * run: the probe used to fire from the camera button, and on a real call the
 * answer landed LATER than the delay did. Every field it printed then described
 * a sender that had not been negotiated yet — `currentDirection=none`,
 * `codec=none`, `framesEncoded=0` — so it reported NOT_NEGOTIATED_AS_SENDING
 * for a call that was about to be fine, and `NO_KEYFRAME_SENT` could never
 * print at all, because the direction check sits ahead of it and the direction
 * is always absent before the answer.
 *
 * So the probe is anchored to the answer, where the sender exists, and it runs
 * twice: once soon enough to catch a sender that never starts, and once late
 * enough that `keyFramesEncoded` and `pliCount` describe a sender that has been
 * sending for a while. Chosen, not measured.
 */
public const val MID_CALL_VIDEO_PROBE_DELAY_MS: Long = 2_000L

/** The second read, for counters that only mean something after some traffic. */
public const val MID_CALL_VIDEO_LATE_PROBE_DELAY_MS: Long = 6_000L

/** Which of the three it was. */
public enum class MidCallVideoVerdict {
    /** Encoding and on the wire. If the peer still shows nothing, the fault is
     *  past this device — the peer's decryptor or its renderer. */
    FLOWING,

    /**
     * No SFrame encryptor on the video sender.
     *
     * The frames leave sealed with nothing, the peer's decryptor cannot open
     * them, and it drops every one without an error — the quietest of the three
     * failures and the reason this check comes first. A mid-call video sender
     * must be sealed with the same per-call seed as the audio one and its own
     * stream index (ADR-0035), exactly as a call that starts as video seals its
     * video sender at connect.
     */
    NOT_SEALED,

    /**
     * The transceiver is not negotiated as sending.
     *
     * Either the direction is not a sending one, or `currentDirection` is still
     * absent, which means the renegotiation has not completed — an offer whose
     * answer never came back, or never got applied.
     */
    NOT_NEGOTIATED_AS_SENDING,

    /** Nothing has been encoded: the capture or the encoder never started. */
    NOT_ENCODING,

    /** Frames were encoded and nothing left the socket. */
    ENCODED_BUT_NOT_SENT,

    /**
     * Frames are being encoded and sent, and not one of them is a key frame.
     *
     * A receiver that joined the stream in progress has nothing to start from
     * and throws away everything that arrives — packets climbing, frames stuck
     * at zero, and nothing ever reaching its decryptor because the frame is
     * discarded before decryption. This is what a mid-call camera-on looks like
     * when the one IDR the encoder produced was sent before the peer's receiver
     * existed.
     */
    NO_KEYFRAME_SENT,

    /** No outbound-rtp video stats yet — too early, or no video sender at all. */
    UNKNOWN,
}

/** What the platform managed to read about the local video sender. */
public data class MidCallVideoFacts(
    /** `framesEncoded` from the outbound-rtp video stream, `null` if absent. */
    val framesEncoded: Long?,
    /** `bytesSent` from the same stream, `null` if absent. */
    val bytesSent: Long?,
    /**
     * `keyFramesEncoded` — how many key frames this sender has produced.
     *
     * The number that decides whether a receiver joining mid-stream can decode
     * anything at all: without one it discards every delta frame that follows.
     */
    val keyFramesEncoded: Long?,
    /** RTCP picture-loss requests received from the peer. */
    val pliCount: Long?,
    /** RTCP full-intra-refresh requests received from the peer. */
    val firCount: Long?,
    /** The transceiver's configured direction, e.g. `SEND_RECV`. */
    val direction: String?,
    /** Its negotiated direction. `null` until a renegotiation completes. */
    val currentDirection: String?,
    /** An SFrame encryptor is attached to the video sender. */
    val sframeArmed: Boolean,
    /**
     * The KID the video encryptor stamps: `(leaf << 8) | streamIndex`
     * (ADR-0035), or `null` when nothing is armed.
     *
     * Logged because it is the first thing to compare between the two arming
     * paths, and safe to log because it is exactly what travels in the clear in
     * every frame header.
     *
     * Worth knowing what a mismatch here would and would not explain: the peer
     * derives its opening key from the KID it READS off the frame, not from one
     * it assumed, so two paths stamping different stream indices would still
     * decrypt on both sides. A wrong KID breaks the roster mapping — which tile
     * a stream belongs to — rather than the decryption.
     */
    val sealKid: Long?,
    /** The leaf half of [sealKid]. */
    val sealLeaf: Int?,
    /** The stream half of [sealKid]: 0 audio, 1 video. */
    val sealStream: Int?,
    /**
     * libwebrtc fetched the encryptor's native pointer, so it reached the
     * sender's media channel rather than only having had a setter called.
     * `false` with [sframeArmed] true is the "attached but not in the frame
     * path" case, which sends unsealed frames to a peer expecting sealed ones.
     */
    val encryptorInFramePath: Boolean,
    /** The negotiated outbound video codec, e.g. `video/VP8`. */
    val codec: String?,
)

/** A direction that actually sends. */
private fun sends(direction: String?): Boolean =
    direction == "SEND_RECV" || direction == "SEND_ONLY"

/**
 * The verdict for [facts].
 *
 * The order is deliberate and is the order of how silently each fault fails.
 * An unsealed sender is first because it produces healthy-looking stats on this
 * device and total silence on the other; the direction comes next because it
 * explains both a wrong direction and an unfinished renegotiation; only then do
 * the counters speak, and they can only be trusted once the first two are ruled
 * out.
 */
public fun midCallVideoVerdict(facts: MidCallVideoFacts): MidCallVideoVerdict = when {
    !facts.sframeArmed -> MidCallVideoVerdict.NOT_SEALED
    // Armed but never fetched: libwebrtc holds no pointer to it, so the frames
    // going out are not the ones this encryptor sealed.
    !facts.encryptorInFramePath -> MidCallVideoVerdict.NOT_SEALED
    !sends(facts.direction) || facts.currentDirection == null ->
        MidCallVideoVerdict.NOT_NEGOTIATED_AS_SENDING
    !sends(facts.currentDirection) -> MidCallVideoVerdict.NOT_NEGOTIATED_AS_SENDING
    facts.framesEncoded == null && facts.bytesSent == null -> MidCallVideoVerdict.UNKNOWN
    (facts.framesEncoded ?: 0L) <= 0L -> MidCallVideoVerdict.NOT_ENCODING
    (facts.bytesSent ?: 0L) <= 0L -> MidCallVideoVerdict.ENCODED_BUT_NOT_SENT
    // Checked only once the bytes are known to be moving: a sender that is not
    // sending has no key frames for the uninteresting reason.
    facts.keyFramesEncoded != null && facts.keyFramesEncoded <= 0L ->
        MidCallVideoVerdict.NO_KEYFRAME_SENT
    else -> MidCallVideoVerdict.FLOWING
}

/**
 * Whether to force another key frame.
 *
 * The peer asking (a picture-loss request arrived) is the only evidence this
 * device can have that its frames are not being decoded — it cannot see the
 * peer's decoder. So one unanswered ask is the signal to key again.
 *
 * Deliberately not "retry if the peer has not acknowledged": nothing
 * acknowledges frames, and a retry loop on a condition that never clears would
 * key the stream forever.
 */
public fun shouldForceAnotherKeyframe(facts: MidCallVideoFacts): Boolean =
    (facts.pliCount ?: 0L) > 0L || (facts.firCount ?: 0L) > 0L ||
        midCallVideoVerdict(facts) == MidCallVideoVerdict.NO_KEYFRAME_SENT

/**
 * The one sentence a give-up should print, naming which half of the problem
 * this device is looking at.
 *
 * The fork is decisive and the two halves have different owners. A receiver
 * that cannot decode asks for a picture, repeatedly. If those asks are landing
 * here and the encoder still will not key, the fault is in the encoder or the
 * lever reaching it — this device's problem. If they are NOT landing here while
 * the receiver insists it is asking, the requests are being lost between the
 * two, which is a transport question and no amount of local reconfiguring
 * touches it.
 */
public fun keyframeGiveUpDiagnosis(facts: MidCallVideoFacts): String {
    val asks = (facts.pliCount ?: 0L) + (facts.firCount ?: 0L)
    return if (asks > 0L) {
        "the peer asked $asks time(s) and this encoder would not key - encoder or lever"
    } else {
        "the peer's picture requests are not arriving here - transport, not the encoder"
    }
}

/** What the keyframe watchdog should do with the sender it just measured. */
public enum class KeyframeAction {
    /**
     * The encoder has produced nothing yet. Asking it to reconfigure now is
     * asking nothing of nothing — which is exactly what a force fired at the
     * moment the answer landed did on the live run: it reported success and no
     * IDR followed, because there was no running encoder to reconfigure.
     */
    WAIT,

    /** Frames are flowing and none of them is a key frame. Reconfigure. */
    FORCE,

    /**
     * Forcing has already been tried and the sender is still not keying, so
     * the in-place lever is not reaching this encoder. Try the other one.
     */
    ESCALATE,

    /** A key frame exists. Nothing more is owed. */
    SATISFIED,

    /** Every lever has been tried. Say so rather than retrying forever. */
    GIVE_UP,
}

/** Attempts before the watchdog stops. */
public const val MAX_KEYFRAME_ATTEMPTS: Int = 3

/** How often the watchdog re-measures the sender. */
public const val KEYFRAME_WATCHDOG_INTERVAL_MS: Long = 1_500L

/**
 * How many times it re-measures before stopping.
 *
 * The product is the window in which a key frame must appear: long enough that
 * a slow encoder start is not mistaken for a dead one, short enough that the
 * watchdog is not still running when the user has given up and hung up.
 */
public const val KEYFRAME_WATCHDOG_TICKS: Int = 8

/**
 * What to do about a sender that may or may not have produced a key frame.
 *
 * The ordering encodes the live finding: **a force before the first encoded
 * frame is wasted**, so [WAIT][KeyframeAction.WAIT] comes before
 * [FORCE][KeyframeAction.FORCE] and the watchdog is allowed to do nothing for a
 * while. Absent counters are treated as "not yet", never as "no key frame" —
 * an unmeasured sender must not be reconfigured on a guess.
 */
public fun keyframeAction(facts: MidCallVideoFacts, attemptsMade: Int): KeyframeAction = when {
    (facts.keyFramesEncoded ?: 0L) > 0L -> KeyframeAction.SATISFIED
    // No stats at all, or nothing encoded yet: the encoder is not running, and
    // the lever needs a running encoder.
    facts.framesEncoded == null || facts.framesEncoded <= 0L -> KeyframeAction.WAIT
    attemptsMade <= 0 -> KeyframeAction.FORCE
    attemptsMade < MAX_KEYFRAME_ATTEMPTS -> KeyframeAction.ESCALATE
    else -> KeyframeAction.GIVE_UP
}

/**
 * The one line the console prints after a mid-call camera-on, carrying the
 * verdict and every number it was drawn from — so a reader can disagree with
 * the conclusion without running the call again.
 *
 * Plain ASCII, no emoji: this goes through `tracing`-equivalent logging and the
 * house rule is that log lines are plain text.
 */
public fun midCallVideoReport(
    facts: MidCallVideoFacts,
    /**
     * Which arming path produced these facts. The two paths print the same
     * line so they can be compared field by field on one console instead of by
     * reading two functions and hoping they agree.
     */
    label: String = "mid-call camera-on",
): String = buildString {
    append(label).append(": ")
    append(midCallVideoVerdict(facts).name)
    append(" (framesEncoded=").append(facts.framesEncoded ?: -1L)
    append(", bytesSent=").append(facts.bytesSent ?: -1L)
    append(", direction=").append(facts.direction ?: "none")
    append(", currentDirection=").append(facts.currentDirection ?: "none")
    append(", sframeArmed=").append(facts.sframeArmed)
    append(", sealKid=").append(facts.sealKid?.let { "0x" + it.toString(16) } ?: "none")
    append(", sealLeaf=").append(facts.sealLeaf ?: -1)
    append(", sealStream=").append(facts.sealStream ?: -1)
    append(", inFramePath=").append(facts.encryptorInFramePath)
    append(", keyFramesEncoded=").append(facts.keyFramesEncoded ?: -1L)
    append(", pliCount=").append(facts.pliCount ?: -1L)
    append(", firCount=").append(facts.firCount ?: -1L)
    append(", codec=").append(facts.codec ?: "none")
    append(')')
}
