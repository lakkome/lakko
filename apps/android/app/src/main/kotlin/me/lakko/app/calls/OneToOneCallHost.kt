// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The app-level owner of the 1x1 call: one slot for the whole signed-in
 * session, fed by call signaling from EVERY receive path.
 *
 * ## The defect this replaces
 *
 * The 1x1 [CallSession] and its ringing banner used to live in `ChatViewModel`
 * / `ChatScreen`, and `ChatRepository` had ONE `callSignalingSink` slot, set by
 * whichever chat view model was created last and filtered by that chat's
 * group. The chat list long-polls every group, so an offer to a chat that was
 * not open reached a sink that dropped it while the journal cursor advanced
 * past the row: the call never rang and could never ring. Opening chat A and
 * then chat B also left A's view model (cached per group) with no sink at all.
 * Leaving the chat hid the in-call screen.
 *
 * ## What the host guarantees
 *
 *  - **Every path feeds it.** `ChatRepository` hands every one-to-one body to
 *    [deliver] from the open chat's poll, the app-level all-groups poll and the
 *    committer sweep's apply pass alike. Nothing is filtered by which screen is
 *    showing.
 *  - **Exactly once, in order, per signaling row.** [deliver] drops a row whose
 *    DS message id is not above the last one delivered for that group (a held
 *    cursor re-fetches a batch; the sweep and the poll consume disjoint rows),
 *    and enqueues it into one FIFO consumed by one coroutine. The chat view used
 *    to launch a coroutine per row, so an ICE candidate could be handled before
 *    the offer's session existed and be dropped.
 *  - **One call at a time.** A fresh offer while a call occupies the slot (or
 *    a group call or call-link call is starting or live on this device) is
 *    answered `End{busy}` so the caller stops ringing.
 *  - **The stale-offer rule.** An offer whose DS row is older than
 *    [STALE_OFFER_MS] does not ring and is not answered busy — nobody is waiting
 *    on it. An unreadable row timestamp counts as stale.
 *  - **Ring and establish timeouts.** A ringing call ends `timedOut` after
 *    [OneToOneCallTiming.ringTimeoutMs]; a dialed or accepted call that has not
 *    connected ends `timedOut` after [OneToOneCallTiming.establishTimeoutMs].
 *    The End is published, so the other side stops too.
 *  - **C-11.** A group-control body never reaches the 1x1 session: the
 *    repository routes those to the group-call ledger, and [routeOneToOneSignaling]
 *    drops one that arrives here anyway.
 *  - **C-12, widened.** An answer, ICE candidate or End is applied only when it
 *    names the call in the slot AND arrives on the journal of the group that
 *    call lives in.
 *  - **Backgrounded offers.** An offer that would ring while the app is not in
 *    the foreground is held instead of rung, and re-judged when the app comes
 *    back: it rings then if it is still fresh. Ringing a call nobody can see
 *    would burn its ring timeout off-screen.
 *
 * ## Threading
 *
 * Every state change happens on ONE consumer coroutine: inbound rows, UI
 * commands ([accept], [decline], [dial], ...), session state changes and timer
 * expiries are all events on the same queue. [deliver] only checks the
 * high-water mark and enqueues, so it is safe from inside the poll's critical
 * section. Published outbound signaling goes through a second single consumer,
 * so the peer receives it in the order the session produced it.
 *
 * Production runs the host on the main dispatcher ([AppCallHost]); the unit
 * suite drives it on a test scheduler with fake sessions.
 */

/** What the host needs from a 1x1 call session. [CallSession] implements it. */
public interface OneToOneCallSession {
    public val callId: ByteArray
    public val state: StateFlow<CallSession.WebCallState>

    public fun setCallKey(seed: ByteArray, localLeaf: Int)

    public fun applyIncomingSignaling(body: FfiCallSignalingBody)

    public fun dial(audio: Boolean = true, video: Boolean = false)

    public fun acceptCall(withVideo: Boolean = false)

    public fun declineCall()

    public fun hangUp(reason: CallSession.WebCallEndReason = CallSession.WebCallEndReason.HungUp)
}

/** The per-call SFrame key material the MLS exporter produced. */
public class OneToOneCallKey(public val seed: ByteArray, public val localLeaf: Int)

/** The platform effects the host drives: session construction, keying, publish. */
public interface OneToOneCallPort<S : OneToOneCallSession> {
    /** A new session for a call in [groupIdHex]; its outbound signaling goes to [outbound]. */
    public fun createSession(groupIdHex: String, callId: ByteArray, outbound: (ByteArray) -> Unit): S

    /** The per-call SFrame key from the group's MLS exporter, or `null` (the call then refuses). */
    public suspend fun deriveCallKey(groupIdHex: String, callId: ByteArray): OneToOneCallKey?

    /** Publish call-signaling bytes to [groupIdHex]'s journal. May throw. */
    public suspend fun publish(groupIdHex: String, bytes: ByteArray)

    /** Compose an `End` body, or `null` when composing failed. */
    public fun composeEnd(callId: ByteArray, reason: FfiCallEndReason): ByteArray?

    /**
     * The other person's name in [groupIdHex] as this device already knows it,
     * for the ring's title (`docs/ux/calls.md` section 2.2), or `null`.
     *
     * Defaulted rather than required: a host driven by fake sessions has no
     * contacts to resolve against, and a ring with no name still rings.
     */
    public fun peerName(groupIdHex: String): String? = null

    /**
     * A call in [groupIdHex] finished: write its row into that conversation
     * (`docs/ux/calls.md` section 6, [CallTranscriptRow.kt]).
     *
     * Local to this device — nothing is composed and nothing is published, so
     * this is a side effect on the transcript store and never on the wire.
     * Defaulted to a no-op for the same reason as [peerName]: a host driven by
     * fake sessions has no conversation to write into.
     */
    public fun recordCall(groupIdHex: String, entry: CallTranscriptEntry) {
        // No conversation store attached.
    }
}

/** The call occupying the slot, as the UI renders it. */
public data class OneToOneCall<S : OneToOneCallSession>(
    val groupIdHex: String,
    val session: S,
    /** `true` for a call this device is answering. */
    val incoming: Boolean,
    /** The offer advertised video, so "Accept with video" can negotiate it. */
    val remoteOffersVideo: Boolean,
    /** The user accepted this incoming call (the session may still read
     *  `Ringing` while its peer connection is being built). */
    val accepted: Boolean = false,
    /**
     * For an outgoing call, the kind the caller chose in the chat header
     * (`docs/ux/calls.md` section 3). It decides one thing: whether the pre-join
     * opens with the camera on. Meaningless for an incoming call, which skips
     * the pre-join entirely, so it stays at the safe default there.
     */
    val requestedKind: RequestedCallKind = RequestedCallKind.VOICE,
)

/** Timeouts bounding a 1x1 call's non-terminal states. Web parity: chosen, not measured. */
public data class OneToOneCallTiming(
    val ringTimeoutMs: Long = 45_000L,
    val establishTimeoutMs: Long = 45_000L,
)

/** An offer whose DS row is older than this does not ring. */
internal const val STALE_OFFER_MS: Long = 60_000L

/** What happens to one inbound one-to-one body. */
internal enum class OneToOneRoute {
    /** Not addressed to the call in the slot, stale, ended, or a group-control body. */
    DROP,

    /** Hand it to the call in the slot. */
    APPLY,

    /** Publish `End{busy}` to the offering group; do not ring. */
    BUSY,

    /** Open a ringing answerer session. */
    RING,
}

/** The call occupying the slot, as routing compares it. */
internal data class OneToOneSlotKey(val groupIdHex: String, val callIdHex: String)

/** Is an offer with this DS row timestamp too old to ring for? `null` = unreadable = stale. */
internal fun isStaleOffer(rowCreatedAtMs: Long?, nowMs: Long, maxAgeMs: Long = STALE_OFFER_MS): Boolean =
    rowCreatedAtMs == null || nowMs - rowCreatedAtMs > maxAgeMs

/** Decide one inbound body. Pure; see the host documentation for the rules. */
internal fun routeOneToOneSignaling(
    body: FfiCallSignalingBody,
    groupIdHex: String,
    rowCreatedAtMs: Long?,
    nowMs: Long,
    occupant: OneToOneSlotKey?,
    endedCallIds: Set<String>,
    otherCallBusy: Boolean,
): OneToOneRoute {
    val callIdHex = when (body) {
        is FfiCallSignalingBody.Offer -> callIdHexOf(body.callId)
        is FfiCallSignalingBody.Answer -> callIdHexOf(body.callId)
        is FfiCallSignalingBody.IceCandidate -> callIdHexOf(body.callId)
        is FfiCallSignalingBody.End -> callIdHexOf(body.callId)
        // C-11 — group-control bodies never advance a 1x1 call.
        is FfiCallSignalingBody.GroupStart,
        is FfiCallSignalingBody.GroupJoin,
        is FfiCallSignalingBody.GroupLeave,
        -> return OneToOneRoute.DROP
    }
    // C-12 — the same call id on another group's journal is not this call.
    if (occupant != null && occupant.callIdHex == callIdHex && occupant.groupIdHex == groupIdHex) {
        return OneToOneRoute.APPLY
    }
    if (body !is FfiCallSignalingBody.Offer) return OneToOneRoute.DROP
    if (callIdHex in endedCallIds) return OneToOneRoute.DROP
    if (isStaleOffer(rowCreatedAtMs, nowMs)) return OneToOneRoute.DROP
    if (occupant != null || otherCallBusy) return OneToOneRoute.BUSY
    return OneToOneRoute.RING
}

/** How the app-wide overlay presents the call in the slot. */
public enum class OneToOneCallPresentation {
    /** An incoming call not yet answered: the ringing banner above any screen. */
    RINGING_BANNER,

    /** The prejoin of an outgoing call, a call being set up or live, or the
     *  ended state awaiting dismissal: the full-screen in-call screen. */
    IN_CALL_SCREEN,
}

/** The presentation for a session in [state]; an accepted call leaves the banner at once. */
public fun oneToOneCallPresentation(state: CallSession.WebCallState, accepted: Boolean): OneToOneCallPresentation =
    if (state == CallSession.WebCallState.Ringing && !accepted) {
        OneToOneCallPresentation.RINGING_BANNER
    } else {
        OneToOneCallPresentation.IN_CALL_SCREEN
    }

/** Parse a DS row's `created_at` (RFC 3339) to epoch millis, `null` when unreadable. */
internal fun parseRowCreatedAtMs(value: String): Long? =
    try {
        java.time.Instant.parse(value).toEpochMilli()
    } catch (_: java.time.format.DateTimeParseException) {
        try {
            java.time.OffsetDateTime.parse(value).toInstant().toEpochMilli()
        } catch (_: java.time.format.DateTimeParseException) {
            null
        }
    }

public class OneToOneCallHost<S : OneToOneCallSession>(
    private val scope: CoroutineScope,
    private val clock: () -> Long = { System.currentTimeMillis() },
    private val timing: OneToOneCallTiming = OneToOneCallTiming(),
    private val warn: (String, Throwable?) -> Unit = ::logWarning,
) {
    @Volatile
    private var port: OneToOneCallPort<S>? = null

    /** Install the platform effects. Re-attaching (an activity recreate) replaces them. */
    public fun attach(port: OneToOneCallPort<S>) {
        this.port = port
    }

    @Volatile
    private var ringPort: IncomingCallRingPort? = null

    /**
     * Install what makes the phone ring ([IncomingCallRinger] in production).
     *
     * Separate from [attach] because the ring outlives the activity that
     * attached the port, and because a host driven by fakes must be able to
     * observe the ring without an Android notification manager.
     */
    public fun attachRinger(ringer: IncomingCallRingPort?) {
        this.ringPort = ringer
    }

    private val mutableActive = MutableStateFlow<OneToOneCall<S>?>(null)

    /** The call in the slot (any state, including ended until cleared), or `null`. */
    public val active: StateFlow<OneToOneCall<S>?> = mutableActive.asStateFlow()

    /** A 1x1 call is ringing, being set up, or live. */
    public val hasCall: Boolean
        get() = mutableActive.value?.let { it.session.state.value != CallSession.WebCallState.Done } == true

    // ---- Inbound ----------------------------------------------------------

    private val highWater = HashMap<String, Long>()

    /**
     * Hand one received one-to-one body to the host. Called from inside a
     * poll's critical section: checks the per-group high-water mark and
     * enqueues, nothing more.
     *
     * @param messageId the DS message id of the row that carried [body].
     * @param createdAt the DS row's server timestamp.
     */
    public fun deliver(groupIdHex: String, messageId: Long, body: FfiCallSignalingBody, createdAt: String) {
        synchronized(highWater) {
            val last = highWater[groupIdHex]
            if (last != null && messageId <= last) return
            highWater[groupIdHex] = messageId
            // Enqueued under the same monitor as the check, so two groups'
            // polls cannot reorder a group's rows between the two.
            send(Event.Row(groupIdHex, body, parseRowCreatedAtMs(createdAt)))
        }
    }

    // ---- UI commands --------------------------------------------------------

    /**
     * Prepare an outgoing call in [groupIdHex]: the session is built and keyed
     * but not dialed (the prejoin). Returns `false` when a call already
     * occupies the slot or another kind of call is live; the consumer re-checks.
     */
    public fun startOutgoing(
        groupIdHex: String,
        /** What the caller asked for; opens the pre-join's camera toggle. */
        kind: RequestedCallKind = RequestedCallKind.VOICE,
    ): Boolean {
        if (hasCall || otherCalls.isNotEmpty()) return false
        send(Event.StartOutgoing(groupIdHex, kind))
        return true
    }

    /** Dial the prepared outgoing call. */
    public fun dial(audio: Boolean, video: Boolean): Unit = send(Event.Dial(audio, video))

    /** Abandon a prepared, undialed outgoing call. */
    public fun cancelPrepared(): Unit = send(Event.CancelPrepared)

    /** Accept the ringing call. */
    public fun accept(withVideo: Boolean): Unit = send(Event.Accept(withVideo))

    /** Decline the ringing call. */
    public fun decline(): Unit = send(Event.Decline)

    /**
     * Accept the call named by [callIdHex], from the ring screen.
     *
     * Named rather than implicit because the ring can be answered before this
     * device has a session for it: an offer that arrived while the app was
     * backgrounded is only HELD, and a cold start has not polled yet. Either
     * way the request is remembered and taken the moment that call rings in the
     * app, and a request for a call that never arrives simply expires with it.
     */
    public fun acceptRinging(callIdHex: String, withVideo: Boolean): Unit =
        send(Event.AcceptRinging(callIdHex, withVideo))

    /**
     * Decline the call named by [callIdHex] on [groupIdHex], from the ring
     * screen or the notification action.
     *
     * Also publishes the `End` for a call this device only holds, which is the
     * whole point: a held offer has no session to decline through, and without
     * the `End` the caller rings on to its own timeout.
     */
    public fun declineRinging(groupIdHex: String, callIdHex: String): Unit =
        send(Event.DeclineRinging(groupIdHex, callIdHex))

    /**
     * The ring window ran out for [callIdHex]. Forget a held offer for it, so
     * that bringing the app to the foreground later cannot ring for a call the
     * user has already watched time out.
     */
    public fun dismissRingingOffer(callIdHex: String): Unit = send(Event.DismissRinging(callIdHex))

    /** Hang up the call in the slot. */
    public fun hangUp(): Unit = send(Event.HangUp)

    /** Remove an ended call from the slot (the in-call screen's exit). */
    public fun clearFinished(): Unit = send(Event.ClearFinished)

    /** The app came to (or left) the foreground. */
    public fun setForeground(foreground: Boolean): Unit = send(Event.Foreground(foreground))

    private val otherCalls: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /**
     * Another kind of call — a group (SFU) call or a call-link call, identified
     * by [key] — occupies the one call slot on this device, or no longer does.
     * While any does, an offer is answered busy and [startOutgoing] refuses
     * (`CallSlot.kt`).
     */
    public fun setOtherCallActive(key: String, active: Boolean) {
        if (active) otherCalls.add(key) else otherCalls.remove(key)
    }

    /** Logout / forget device: hang up, forget every call, start clean. */
    public fun reset() {
        synchronized(highWater) {
            highWater.clear()
            send(Event.Reset)
        }
    }

    // ---- The consumer --------------------------------------------------------

    private sealed interface Event {
        data class Row(val groupIdHex: String, val body: FfiCallSignalingBody, val createdAtMs: Long?) : Event
        data class StartOutgoing(val groupIdHex: String, val kind: RequestedCallKind) : Event
        data class Dial(val audio: Boolean, val video: Boolean) : Event
        data object CancelPrepared : Event
        data class Accept(val withVideo: Boolean) : Event
        data object Decline : Event
        data class AcceptRinging(val callIdHex: String, val withVideo: Boolean) : Event
        data class DeclineRinging(val groupIdHex: String, val callIdHex: String) : Event
        data class DismissRinging(val callIdHex: String) : Event
        data object HangUp : Event
        data object ClearFinished : Event
        data class Foreground(val foreground: Boolean) : Event
        data object Reset : Event
        class SessionState(val session: OneToOneCallSession, val state: CallSession.WebCallState) : Event
        class Timeout(val session: OneToOneCallSession, val generation: Long) : Event
    }

    private class Outbound(val groupIdHex: String, val bytes: ByteArray)

    private enum class TimerKind { RING, ESTABLISH }

    private inner class Slot(
        var call: OneToOneCall<S>,
        val callIdHex: String,
        var keyArmed: Boolean,
    ) {
        var accepted: Boolean = false
        var lastState: CallSession.WebCallState? = null
        var timerKind: TimerKind? = null
        var timerGeneration: Long = 0
        var timerJob: Job? = null
        var watchJob: Job? = null

        // ---- What the transcript row is built from (section 6) --------------
        // Everything below is observed by this host from events it already
        // handles, so recording a call needs nothing new from the session.

        /** The outgoing call was dialed. An undialed prepared session (the
         *  pre-join, cancelled) is not a call and writes no row. */
        var dialed: Boolean = false

        /** Wall clock at the first `Connected`, or `null` if media never flowed.
         *  The only source of a duration; its absence is what keeps an
         *  unanswered call from being given one. */
        var connectedAtMs: Long? = null

        /** The user pressed Decline on this device. */
        var declinedHere: Boolean = false

        /** The other side answered with a decline. */
        var declinedByPeer: Boolean = false

        /** The row has been written. A call ends once, and several paths can
         *  observe that (the terminal state, a decline, a reset), so the write
         *  is latched rather than trusted to happen on exactly one of them. */
        var recorded: Boolean = false
    }

    private val events = Channel<Event>(Channel.UNLIMITED)
    private val outbound = Channel<Outbound>(Channel.UNLIMITED)
    private val started = AtomicBoolean(false)

    // Consumer-owned state: read and written only by [handle].
    private var slot: Slot? = null
    private var deferred: Event.Row? = null

    /**
     * ICE candidates of the [deferred] offer's call, in arrival order. The
     * caller trickles them right behind the offer; dropping them while the
     * offer is held would leave the call, once it rings on foreground, without
     * the candidates the caller already sent. Bounded by
     * [RemoteCandidateBuffer.DEFAULT_CAPACITY]; always cleared with [deferred].
     */
    private val deferredCandidates = ArrayList<Event.Row>()
    private var foreground: Boolean = false
    private val ended = LinkedHashSet<String>()

    /**
     * An answer taken on the ring screen for a call this device has no session
     * for yet, kept until that call rings in the app. Dropped whenever the
     * ring's call is declined or another call takes the slot, so it can never
     * answer a later, unrelated call.
     */
    private var pendingAccept: Pair<String, Boolean>? = null

    private fun send(event: Event) {
        ensureStarted()
        events.trySend(event)
    }

    private fun ensureStarted() {
        if (!started.compareAndSet(false, true)) return
        scope.launch {
            for (event in events) {
                try {
                    handle(event)
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (error: Throwable) {
                    warn("call host: event handling failed", error)
                }
            }
        }
        scope.launch {
            for (item in outbound) {
                val p = port ?: continue
                try {
                    p.publish(item.groupIdHex, item.bytes)
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (error: Throwable) {
                    warn("call host: signaling publish failed", error)
                }
            }
        }
    }

    private suspend fun handle(event: Event) {
        when (event) {
            is Event.Row -> onRow(event)
            is Event.StartOutgoing -> onStartOutgoing(event.groupIdHex, event.kind)
            is Event.Dial -> onDial(event)
            Event.CancelPrepared -> onCancelPrepared()
            is Event.Accept -> onAccept(event.withVideo)
            is Event.AcceptRinging -> onAcceptRinging(event)
            is Event.DeclineRinging -> onDeclineRinging(event)
            is Event.DismissRinging -> onDismissRinging(event.callIdHex)
            Event.Decline -> liveSlot()?.takeIf { it.call.incoming && isRinging(it) }?.let { s ->
                s.declinedHere = true
                s.call.session.declineCall()
            }
            Event.HangUp -> liveSlot()?.call?.session?.hangUp(CallSession.WebCallEndReason.HungUp)
            Event.ClearFinished -> slot?.takeIf { it.call.session.state.value == CallSession.WebCallState.Done }
                ?.let { clearSlot() }
            is Event.Foreground -> onForeground(event.foreground)
            Event.Reset -> onReset()
            is Event.SessionState -> onSessionState(event)
            is Event.Timeout -> onTimeout(event)
        }
    }

    /** The slot, unless its call has ended. */
    private fun liveSlot(): Slot? = slot?.takeIf { it.call.session.state.value != CallSession.WebCallState.Done }

    private fun isRinging(s: Slot): Boolean = s.call.session.state.value == CallSession.WebCallState.Ringing

    /** Who occupies the slot for routing: a live call, else a still-fresh held offer. */
    private fun occupant(nowMs: Long): OneToOneSlotKey? {
        liveSlot()?.let { return OneToOneSlotKey(it.call.groupIdHex, it.callIdHex) }
        val held = deferred ?: return null
        if (isStaleOffer(held.createdAtMs, nowMs)) {
            clearDeferred()
            return null
        }
        return OneToOneSlotKey(held.groupIdHex, callIdHexOfBody(held.body))
    }

    private suspend fun onRow(row: Event.Row) {
        val now = clock()
        val route = routeOneToOneSignaling(
            body = row.body,
            groupIdHex = row.groupIdHex,
            rowCreatedAtMs = row.createdAtMs,
            nowMs = now,
            occupant = occupant(now),
            endedCallIds = ended,
            otherCallBusy = otherCalls.isNotEmpty(),
        )
        when (route) {
            OneToOneRoute.DROP -> Unit
            OneToOneRoute.APPLY -> applyToOccupant(row)
            OneToOneRoute.BUSY -> {
                publishEnd(row.groupIdHex, callIdOfBody(row.body), FfiCallEndReason.BUSY)
                // Refused because this device was already in a call. The user
                // never saw it ring, which is exactly what makes the row worth
                // writing: it is the only way they learn it happened.
                recordSessionlessOffer(row, CallOutcome.MISSED)
            }
            OneToOneRoute.RING -> {
                // Ring the phone whatever screen is showing, including none.
                // The notification channel is the only thing on Android that
                // produces a ringtone and a lock-screen answer button, and the
                // in-app banner has never produced either; with the app open
                // the platform renders it as a heads-up above that banner
                // rather than as the full-screen ring screen.
                startRinging(row)
                if (foreground) ring(row) else holdOffer(row)
            }
        }
    }

    /** An offer arrived: ring for it. The other half of the pair is [stopRinging]. */
    private fun startRinging(row: Event.Row) {
        val ringer = ringPort ?: return
        val offer = row.body as? FfiCallSignalingBody.Offer ?: return
        ringer.startRinging(
            IncomingCallRing(
                groupIdHex = row.groupIdHex,
                callIdHex = callIdHexOf(offer.callId),
                callerName = try {
                    port?.peerName(row.groupIdHex)
                } catch (error: Throwable) {
                    warn("call host: resolving the caller's name failed", error)
                    null
                },
                video = offer.media.video,
                // The offer's own stamp, not this device's clock: a wake that
                // took twenty seconds to arrive has twenty-five seconds of
                // ringing left, not forty-five.
                offeredAtMs = offer.createdAtMs.toLong(),
            ),
        )
    }

    /** Stop ringing for this call id. */
    private fun stopRinging(callIdHex: String) {
        ringPort?.stopRinging(callIdHex)
    }

    private suspend fun onAcceptRinging(event: Event.AcceptRinging) {
        stopRinging(event.callIdHex)
        val live = liveSlot()
        if (live != null && live.callIdHex == event.callIdHex) {
            onAccept(event.withVideo)
            return
        }
        // No session for it yet: the offer is held (backgrounded), or this
        // process has not polled since a cold start. Remember the answer and
        // take it the moment that call rings in the app.
        pendingAccept = event.callIdHex to event.withVideo
        // A held offer rings in-app on the next foreground; nothing else to do.
    }

    private fun onDeclineRinging(event: Event.DeclineRinging) {
        stopRinging(event.callIdHex)
        pendingAccept = null
        val live = liveSlot()
        if (live != null && live.callIdHex == event.callIdHex) {
            live.declinedHere = true
            live.call.session.declineCall()
            return
        }
        // A held offer has no session to decline through, so the End is
        // published straight to the offering group — and its row is written
        // here, for the same reason.
        val held = deferred?.takeIf { callIdHexOfBody(it.body) == event.callIdHex }
        if (held != null) {
            clearDeferred()
            recordSessionlessOffer(held, CallOutcome.DECLINED)
        }
        rememberEnded(event.callIdHex)
        val callId = callIdBytesFromHex(event.callIdHex) ?: return
        publishEnd(event.groupIdHex, callId, FfiCallEndReason.DECLINED)
    }

    private fun onDismissRinging(callIdHex: String) {
        val held = deferred?.takeIf { callIdHexOfBody(it.body) == callIdHex } ?: return
        clearDeferred()
        // The ring window ran out on an offer that never got a session: a
        // missed call, and the only trace of it this device will ever have.
        recordSessionlessOffer(held, CallOutcome.MISSED)
        rememberEnded(callIdHex)
    }

    private fun holdOffer(row: Event.Row) {
        clearDeferred()
        deferred = row
    }

    private fun clearDeferred() {
        deferred = null
        deferredCandidates.clear()
    }

    private fun applyToOccupant(row: Event.Row) {
        val live = liveSlot()
        if (live != null) {
            // The peer's own words for how it ended. Read here rather than off
            // the session because the End body carries the reason and the
            // session interface does not surface it — and `Declined` is the one
            // outcome that cannot be inferred from the states this host sees.
            val end = row.body as? FfiCallSignalingBody.End
            if (end != null && end.reason == FfiCallEndReason.DECLINED) live.declinedByPeer = true
            live.call.session.applyIncomingSignaling(row.body)
            return
        }
        // The occupant is a held offer. Its End means the caller gave up, and
        // the offer must never ring. Its ICE candidates are kept for the call
        // that rings on foreground; anything else means nothing yet.
        val held = deferred ?: return
        when (row.body) {
            is FfiCallSignalingBody.End -> {
                val heldCallIdHex = callIdHexOfBody(held.body)
                clearDeferred()
                // The caller gave up on a call this device only held, or
                // another of this account's devices refused it. Either way this
                // device never took it, and the row is the only trace of it —
                // the peer's own reason decides which of the two words it uses.
                recordSessionlessOffer(
                    held,
                    if (row.body.reason == FfiCallEndReason.DECLINED) {
                        CallOutcome.DECLINED
                    } else {
                        CallOutcome.MISSED
                    },
                )
                rememberEnded(heldCallIdHex)
                // The caller gave up (or another of this account's devices
                // answered): the phone must stop ringing now, not at the
                // window's end.
                stopRinging(heldCallIdHex)
                if (pendingAccept?.first == heldCallIdHex) pendingAccept = null
            }
            is FfiCallSignalingBody.IceCandidate ->
                if (deferredCandidates.size < RemoteCandidateBuffer.DEFAULT_CAPACITY) {
                    deferredCandidates += row
                }
            else -> Unit
        }
    }

    private suspend fun ring(row: Event.Row) {
        val offer = row.body as? FfiCallSignalingBody.Offer ?: return
        val p = port ?: run {
            warn("call host: an offer arrived with no port attached, dropped", null)
            return
        }
        clearSlot()
        val session = p.createSession(row.groupIdHex, offer.callId) { bytes ->
            outbound.trySend(Outbound(row.groupIdHex, bytes))
        }
        val key = p.deriveCallKey(row.groupIdHex, offer.callId)
        if (key != null) {
            session.setCallKey(key.seed, key.localLeaf)
        } else {
            // Fail closed: accepting without a key refuses the call (invariant #4).
            warn("call host: call-key derivation failed for an incoming call", null)
        }
        session.applyIncomingSignaling(offer)
        install(
            Slot(
                call = OneToOneCall(
                    groupIdHex = row.groupIdHex,
                    session = session,
                    incoming = true,
                    remoteOffersVideo = offer.media.video,
                ),
                callIdHex = callIdHexOf(offer.callId),
                keyArmed = key != null,
            ),
        )
        // An answer already taken on the ring screen, for exactly this call.
        val pending = pendingAccept
        if (pending != null && pending.first == callIdHexOf(offer.callId)) {
            pendingAccept = null
            onAccept(pending.second)
        }
    }

    private suspend fun onStartOutgoing(groupIdHex: String, kind: RequestedCallKind) {
        if (occupant(clock()) != null || otherCalls.isNotEmpty()) return
        val p = port ?: return
        clearSlot()
        val callId = ByteArray(CALL_ID_BYTES).also { SecureRandom().nextBytes(it) }
        val session = p.createSession(groupIdHex, callId) { bytes ->
            outbound.trySend(Outbound(groupIdHex, bytes))
        }
        val key = p.deriveCallKey(groupIdHex, callId)
        if (key != null) session.setCallKey(key.seed, key.localLeaf)
        install(
            Slot(
                call = OneToOneCall(
                    groupIdHex = groupIdHex,
                    session = session,
                    incoming = false,
                    remoteOffersVideo = false,
                    // Carried so the pre-join opens the way the caller asked
                    // (section 3); it changes nothing about what is offered.
                    requestedKind = kind,
                ),
                callIdHex = callIdHexOf(callId),
                keyArmed = key != null,
            ),
        )
    }

    private suspend fun onDial(event: Event.Dial) {
        val s = liveSlot() ?: return
        if (s.call.incoming || s.call.session.state.value != CallSession.WebCallState.Idle) return
        armKeyIfMissing(s)
        s.dialed = true
        s.call.session.dial(audio = event.audio, video = event.video)
    }

    private fun onCancelPrepared() {
        val s = liveSlot() ?: return
        if (s.call.incoming || s.call.session.state.value != CallSession.WebCallState.Idle) return
        s.call.session.hangUp(CallSession.WebCallEndReason.HungUp)
        clearSlot()
    }

    private suspend fun onAccept(withVideo: Boolean) {
        val s = liveSlot() ?: return
        if (!s.call.incoming || s.accepted || !isRinging(s)) return
        stopRinging(s.callIdHex)
        s.accepted = true
        s.call = s.call.copy(accepted = true)
        mutableActive.value = s.call
        armTimer(s, TimerKind.ESTABLISH, timing.establishTimeoutMs)
        armKeyIfMissing(s)
        s.call.session.acceptCall(withVideo = withVideo)
    }

    private suspend fun armKeyIfMissing(s: Slot) {
        if (s.keyArmed) return
        val key = port?.deriveCallKey(s.call.groupIdHex, s.call.session.callId) ?: return
        s.call.session.setCallKey(key.seed, key.localLeaf)
        s.keyArmed = true
    }

    private suspend fun onForeground(nowForeground: Boolean) {
        foreground = nowForeground
        if (!nowForeground) return
        val held = deferred ?: return
        val heldCandidates = deferredCandidates.toList()
        clearDeferred()
        val now = clock()
        val route = routeOneToOneSignaling(
            body = held.body,
            groupIdHex = held.groupIdHex,
            rowCreatedAtMs = held.createdAtMs,
            nowMs = now,
            occupant = liveSlot()?.let { OneToOneSlotKey(it.call.groupIdHex, it.callIdHex) },
            endedCallIds = ended,
            otherCallBusy = otherCalls.isNotEmpty(),
        )
        when (route) {
            OneToOneRoute.RING -> {
                ring(held)
                // Only into the call this offer just rang (C-12); the session
                // keeps them until its PeerConnection exists.
                val rang = liveSlot()?.takeIf { it.callIdHex == callIdHexOfBody(held.body) }
                heldCandidates.forEach { rang?.call?.session?.applyIncomingSignaling(it.body) }
            }
            OneToOneRoute.BUSY -> publishEnd(held.groupIdHex, callIdOfBody(held.body), FfiCallEndReason.BUSY)
            OneToOneRoute.APPLY, OneToOneRoute.DROP -> Unit
        }
    }

    private fun onReset() {
        slot?.let { stopRinging(it.callIdHex) }
        deferred?.let { stopRinging(callIdHexOfBody(it.body)) }
        pendingAccept = null
        liveSlot()?.call?.session?.hangUp(CallSession.WebCallEndReason.HungUp)
        clearSlot()
        clearDeferred()
        ended.clear()
    }

    private fun onSessionState(event: Event.SessionState) {
        val s = slot ?: return
        if (s.call.session !== event.session) return
        val previous = s.lastState
        val state = event.state
        if (previous == state) return
        s.lastState = state
        when (state) {
            CallSession.WebCallState.Idle -> Unit
            CallSession.WebCallState.Ringing ->
                if (!s.accepted) armTimer(s, TimerKind.RING, timing.ringTimeoutMs)
            CallSession.WebCallState.Outgoing -> armTimer(s, TimerKind.ESTABLISH, timing.establishTimeoutMs)
            // An accept already armed the establish timer; a dial's keeps running.
            CallSession.WebCallState.Connecting ->
                if (s.timerKind != TimerKind.ESTABLISH) armTimer(s, TimerKind.ESTABLISH, timing.establishTimeoutMs)
            CallSession.WebCallState.Connected -> {
                cancelTimer(s)
                // The only place a duration can start. Latched: a reconnect
                // passes through `Connected` again and must not restart the
                // clock on a call that has been up for ten minutes.
                if (s.connectedAtMs == null) s.connectedAtMs = clock()
            }
            CallSession.WebCallState.Ending -> cancelTimer(s)
            CallSession.WebCallState.Done -> {
                cancelTimer(s)
                recordFinishedCall(s)
                rememberEnded(s.callIdHex)
                // Declined, answered, timed out, or the caller hung up: every
                // way a call ends is a reason to stop ringing for it.
                stopRinging(s.callIdHex)
                if (pendingAccept?.first == s.callIdHex) pendingAccept = null
                // An incoming call that ended before it was accepted (declined,
                // timed out, the caller hung up) was only ever a banner: there
                // is no in-call screen to dismiss, so the slot clears itself.
                if (s.call.incoming && !s.accepted) clearSlot()
            }
        }
    }

    private fun onTimeout(event: Event.Timeout) {
        val s = slot ?: return
        if (s.call.session !== event.session || s.timerGeneration != event.generation) return
        val state = s.call.session.state.value
        val expired = when (s.timerKind) {
            TimerKind.RING -> state == CallSession.WebCallState.Ringing && !s.accepted
            TimerKind.ESTABLISH ->
                state == CallSession.WebCallState.Outgoing ||
                    state == CallSession.WebCallState.Connecting ||
                    (state == CallSession.WebCallState.Ringing && s.accepted)
            null -> false
        }
        s.timerKind = null
        s.timerJob = null
        if (expired) s.call.session.hangUp(CallSession.WebCallEndReason.TimedOut)
    }

    /**
     * Write the transcript row for a call that has ended (`docs/ux/calls.md`
     * section 6).
     *
     * Two calls never write a row, and both are deliberate: a prepared but
     * **undialed** outgoing session is a pre-join the user cancelled, not a
     * call; and a slot already recorded is not recorded twice.
     */
    private fun recordFinishedCall(s: Slot) {
        if (s.recorded) return
        if (!s.call.incoming && !s.dialed) return
        s.recorded = true
        val outcome = callOutcome(
            incoming = s.call.incoming,
            answered = if (s.call.incoming) s.accepted else s.dialed,
            declinedHere = s.declinedHere,
            declinedByPeer = s.declinedByPeer,
        )
        recordCall(
            groupIdHex = s.call.groupIdHex,
            entry = CallTranscriptEntry(
                outcome = outcome,
                video = if (s.call.incoming) {
                    s.call.remoteOffersVideo
                } else {
                    s.call.requestedKind == RequestedCallKind.VIDEO
                },
                durationSecs = callDurationSecs(s.connectedAtMs, clock()),
            ),
        )
    }

    /** Write one row, never letting a transcript failure break a call teardown. */
    private fun recordCall(groupIdHex: String, entry: CallTranscriptEntry) {
        try {
            port?.recordCall(groupIdHex, entry)
        } catch (error: Throwable) {
            warn("call host: recording the call row failed", error)
        }
    }

    /**
     * An offer this device only ever HELD is over: there is no session and so no
     * terminal transition to hang the row on, but the user did miss (or refuse)
     * a real call and section 6 makes no exception for it.
     */
    private fun recordSessionlessOffer(row: Event.Row, outcome: CallOutcome) {
        val offer = row.body as? FfiCallSignalingBody.Offer ?: return
        recordCall(
            groupIdHex = row.groupIdHex,
            entry = CallTranscriptEntry(outcome = outcome, video = offer.media.video),
        )
    }

    private fun install(s: Slot) {
        slot = s
        mutableActive.value = s.call
        val session = s.call.session
        s.watchJob = scope.launch {
            session.state.collect { state -> events.trySend(Event.SessionState(session, state)) }
        }
    }

    private fun clearSlot() {
        val s = slot ?: return
        cancelTimer(s)
        s.watchJob?.cancel()
        slot = null
        mutableActive.value = null
    }

    private fun armTimer(s: Slot, kind: TimerKind, delayMs: Long) {
        s.timerJob?.cancel()
        s.timerGeneration += 1
        s.timerKind = kind
        val generation = s.timerGeneration
        val session = s.call.session
        s.timerJob = scope.launch {
            delay(delayMs)
            events.trySend(Event.Timeout(session, generation))
        }
    }

    private fun cancelTimer(s: Slot) {
        s.timerJob?.cancel()
        s.timerJob = null
        s.timerKind = null
        s.timerGeneration += 1
    }

    private fun publishEnd(groupIdHex: String, callId: ByteArray, reason: FfiCallEndReason) {
        val bytes = port?.composeEnd(callId, reason) ?: return
        outbound.trySend(Outbound(groupIdHex, bytes))
    }

    private fun rememberEnded(callIdHex: String) {
        ended.remove(callIdHex)
        ended.add(callIdHex)
        while (ended.size > MAX_ENDED_CALL_IDS) {
            val oldest = ended.iterator()
            oldest.next()
            oldest.remove()
        }
    }

    private fun callIdOfBody(body: FfiCallSignalingBody): ByteArray = when (body) {
        is FfiCallSignalingBody.Offer -> body.callId
        is FfiCallSignalingBody.Answer -> body.callId
        is FfiCallSignalingBody.IceCandidate -> body.callId
        is FfiCallSignalingBody.End -> body.callId
        is FfiCallSignalingBody.GroupStart -> body.callId
        is FfiCallSignalingBody.GroupJoin -> body.callId
        is FfiCallSignalingBody.GroupLeave -> body.callId
    }

    private fun callIdHexOfBody(body: FfiCallSignalingBody): String = callIdHexOf(callIdOfBody(body))

    private companion object {
        const val CALL_ID_BYTES = 16
        const val MAX_ENDED_CALL_IDS = 256
    }
}

private fun logWarning(message: String, error: Throwable?) {
    try {
        android.util.Log.w("OneToOneCallHost", message, error)
    } catch (_: Throwable) {
        // No Android logger (host unit tests): nothing to report to.
    }
}
