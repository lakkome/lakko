// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * What it means for the phone to ring, written once, with no Android types.
 *
 * ## The defect this closes
 *
 * `docs/ux/calls.md` section 2.1 asks for a call that rings with the app
 * backgrounded, closed, or the phone locked. Two halves were missing. The
 * outbound half landed with [shouldWakeForCallSignaling]: an offer and an end
 * now request a high-priority push wake. This file is the receiving half's
 * vocabulary — every decision about ringing that does NOT need a
 * [android.content.Context], so it can be unit tested on a plain JVM where
 * neither the framework nor `liblakko_ffi_uniffi.so` exists.
 *
 * ## One owner per verb
 *
 * There are exactly two verbs, and each has exactly one implementation
 * ([me.lakko.app.calls.IncomingCallRinger]):
 *
 *   * **an offer arrived, ring for it** — [IncomingCallRingPort.startRinging],
 *   * **stop ringing for this call id** — [IncomingCallRingPort.stopRinging].
 *
 * Two independent paths reach them, which is why the verbs are a port rather
 * than a call at each site:
 *
 *   1. the **FCM wake**, whose background decrypt classifies the offer
 *      (`me.lakko.app.push`), and
 *   2. the app's **own poll**, whose one-to-one sink is
 *      [OneToOneCallHost]. This path is not a fallback: the test stack has no
 *      FCM credentials, so on the emulator it is the ONLY path that ever runs,
 *      and a ring wired only to the push would be untestable there.
 *
 * Both may ring the same call — a wake and a poll can race — so the ring is
 * idempotent per call id.
 */

/** The ring window shared by both sides of a call, `docs/ux/calls.md` section 2.1. */
public const val CALL_RING_WINDOW_MS: Long = 45_000L

/** An incoming call the phone should be ringing for. */
public data class IncomingCallRing(
    /** The DS journal the offer arrived on — needed to publish the decline. */
    val groupIdHex: String,
    /** The 16-byte call id as lowercase hex; the identity of the ring. */
    val callIdHex: String,
    /** The caller as this device already knows them, or `null` when it does not. */
    val callerName: String?,
    /** The offer asked for video. */
    val video: Boolean,
    /** The offer's own `created_at`, or `null` when it could not be read. */
    val offeredAtMs: Long?,
)

/** The two verbs, so the host and the push path share one implementation. */
public interface IncomingCallRingPort {
    /** An offer arrived: ring for it. Idempotent for a call id already ringing. */
    public fun startRinging(ring: IncomingCallRing)

    /** Stop ringing for this call id. A call id that is not ringing is a no-op. */
    public fun stopRinging(callIdHex: String)
}

/** What the incoming-call notification says. */
public data class CallRingContent(val title: String, val body: String)

/**
 * The notification's two lines, per `docs/ux/calls.md` section 2.2: the caller's
 * name, and one line saying what kind of call it is.
 *
 * The body is the kind and nothing else — never "Incoming Lakko call", never a
 * message preview. A caller this device cannot name falls back to the same word
 * the in-app banner has always used rather than to an account id, which would
 * be both meaningless and a leak onto a locked screen.
 */
public fun callRingContent(callerName: String?, video: Boolean): CallRingContent = CallRingContent(
    title = callerName?.trim()?.takeIf { it.isNotEmpty() } ?: "Someone",
    body = if (video) "Video call" else "Voice call",
)

/**
 * Milliseconds left in the ring window for an offer stamped [offeredAtMs], or
 * `0` when it has run out.
 *
 * An offer with no readable stamp rings for the whole window: the push wake
 * that carried it was sent when the offer was published, so treating it as
 * expired would silently drop a live call. A stamp in the future (the caller's
 * clock runs ahead) is clamped to the full window rather than trusted.
 */
public fun callRingRemainingMs(
    offeredAtMs: Long?,
    nowMs: Long,
    windowMs: Long = CALL_RING_WINDOW_MS,
): Long {
    if (offeredAtMs == null) return windowMs
    val age = nowMs - offeredAtMs
    if (age <= 0L) return windowMs
    if (age >= windowMs) return 0L
    return windowMs - age
}

/**
 * One call-signaling frame, as much of it as the ring cares about.
 *
 * Only an offer and an end reach here: an answer, an ICE candidate and the
 * group-control bodies belong to a call whose peer is already awake, and
 * [shouldWakeForCallSignaling] does not wake anyone for them.
 */
public sealed interface CallRingSignal {
    /** The call id this signal names. */
    public val callIdHex: String

    /** A call is being offered to this device. */
    public data class Offer(
        override val callIdHex: String,
        val video: Boolean,
        val offeredAtMs: Long?,
        val callerName: String? = null,
    ) : CallRingSignal

    /** The call is over: the caller cancelled, or it was answered elsewhere. */
    public data class End(override val callIdHex: String) : CallRingSignal
}

/**
 * What a batch of call-signaling frames means for the ring.
 *
 * @param ring the call to ring for, or `null` when the batch rings for nothing.
 * @param cancelled call ids that must stop ringing, in arrival order.
 */
public data class CallRingOutcome(
    val ring: IncomingCallRing?,
    val cancelled: List<String>,
)

/**
 * Fold one batch of signals into a ring decision.
 *
 * A DS fetch returns a batch, not a frame, and a batch routinely carries both
 * halves of a finished call — the wake for the end arrives while the wake for
 * the offer is still being decrypted. Deciding frame by frame would therefore
 * ring for a call that is already over, on a device the user then has to
 * silence by hand; deciding over the batch cannot.
 *
 * The rules, in the order they matter:
 *
 *  - an **end** always cancels, even with no offer in this batch: it is how a
 *    ring raised by an EARLIER wake is stopped, which is the common case;
 *  - an end cancels an offer earlier in the same batch;
 *  - an offer past its ring window never rings — a catch-up fetch from an old
 *    cursor replays offers whose callers gave up long ago;
 *  - the newest surviving offer wins, so a caller who tried twice rings once.
 */
public fun foldCallRingSignals(
    groupIdHex: String,
    signals: List<CallRingSignal>,
    nowMs: Long,
    windowMs: Long = CALL_RING_WINDOW_MS,
): CallRingOutcome {
    var pending: IncomingCallRing? = null
    val cancelled = ArrayList<String>()
    for (signal in signals) {
        when (signal) {
            is CallRingSignal.Offer -> {
                if (callRingRemainingMs(signal.offeredAtMs, nowMs, windowMs) <= 0L) continue
                if (signal.callIdHex in cancelled) continue
                pending = IncomingCallRing(
                    groupIdHex = groupIdHex,
                    callIdHex = signal.callIdHex,
                    callerName = signal.callerName,
                    video = signal.video,
                    offeredAtMs = signal.offeredAtMs,
                )
            }
            is CallRingSignal.End -> {
                if (signal.callIdHex !in cancelled) cancelled += signal.callIdHex
                if (pending?.callIdHex == signal.callIdHex) pending = null
            }
        }
    }
    return CallRingOutcome(ring = pending, cancelled = cancelled)
}

// ---------------------------------------------------------------------------
// The notification's and the ring screen's actions
// ---------------------------------------------------------------------------

/** What the user chose on the ring. */
public enum class CallRingAction {
    /** Answer, with video when the offer carried it. */
    ACCEPT,

    /** Answer a video call with the camera off (`Accept without video`). */
    ACCEPT_AUDIO_ONLY,

    /** Refuse, without opening the app. */
    DECLINE,
}

/** A decoded action, with the call it names. */
public data class CallRingCommand(
    val action: CallRingAction,
    val groupIdHex: String,
    val callIdHex: String,
) {
    /** The video flag to hand [OneToOneCallHost.acceptRinging]. */
    public val withVideo: Boolean
        get() = action == CallRingAction.ACCEPT
}

/** Intent action for "show the ring screen" — the full-screen intent and the
 *  notification body's own tap. Distinct from [CALL_RING_ACTION_ACCEPT] so that
 *  a tap on the notification's Accept BUTTON answers, while merely landing on
 *  the ring screen does not: the two arrive as the same kind of intent and
 *  cannot be told apart any other way. */
public const val CALL_RING_ACTION_SHOW: String = "me.lakko.call.ring.SHOW"

/** Intent action for `Accept`. */
public const val CALL_RING_ACTION_ACCEPT: String = "me.lakko.call.ring.ACCEPT"

/** Intent action for `Accept without video`. */
public const val CALL_RING_ACTION_ACCEPT_AUDIO_ONLY: String = "me.lakko.call.ring.ACCEPT_AUDIO_ONLY"

/** Intent action for `Decline`. */
public const val CALL_RING_ACTION_DECLINE: String = "me.lakko.call.ring.DECLINE"

/** Intent extra carrying the group id hex. */
public const val CALL_RING_EXTRA_GROUP: String = "me.lakko.call.ring.group"

/** Intent extra carrying the call id hex. */
public const val CALL_RING_EXTRA_CALL_ID: String = "me.lakko.call.ring.call_id"

/**
 * Map an Intent's action and extras to a command, or `null` when they do not
 * name one.
 *
 * Pure so the mapping is testable without the framework, and fail-closed in the
 * direction that matters: an unknown action, a missing group or a missing call
 * id yields nothing at all rather than a command aimed at whatever call happens
 * to be ringing. A notification action fires from outside the app, so its
 * extras are the only thing tying it to a call — a command that lost them would
 * accept or decline the wrong one.
 */
public fun callRingCommand(
    action: String?,
    groupIdHex: String?,
    callIdHex: String?,
): CallRingCommand? {
    val chosen = when (action) {
        CALL_RING_ACTION_ACCEPT -> CallRingAction.ACCEPT
        CALL_RING_ACTION_ACCEPT_AUDIO_ONLY -> CallRingAction.ACCEPT_AUDIO_ONLY
        CALL_RING_ACTION_DECLINE -> CallRingAction.DECLINE
        else -> return null
    }
    val group = groupIdHex?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    val call = callIdHex?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    return CallRingCommand(action = chosen, groupIdHex = group, callIdHex = call)
}

/** The 16-byte call id behind a lowercase-hex id, or `null` when it is malformed. */
public fun callIdBytesFromHex(callIdHex: String): ByteArray? {
    if (callIdHex.length % 2 != 0 || callIdHex.isEmpty()) return null
    val out = ByteArray(callIdHex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(callIdHex[i * 2], 16)
        val lo = Character.digit(callIdHex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}
