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
 *    a group call is live on this device) is answered `End{busy}` so the caller
 *    stops ringing.
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
    groupCallBusy: Boolean,
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
    if (occupant != null || groupCallBusy) return OneToOneRoute.BUSY
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
     * occupies the slot or a group call is live; the consumer re-checks.
     */
    public fun startOutgoing(groupIdHex: String): Boolean {
        if (hasCall || groupCalls.isNotEmpty()) return false
        send(Event.StartOutgoing(groupIdHex))
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

    /** Hang up the call in the slot. */
    public fun hangUp(): Unit = send(Event.HangUp)

    /** Remove an ended call from the slot (the in-call screen's exit). */
    public fun clearFinished(): Unit = send(Event.ClearFinished)

    /** The app came to (or left) the foreground. */
    public fun setForeground(foreground: Boolean): Unit = send(Event.Foreground(foreground))

    private val groupCalls: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** A group (SFU) call identified by [key] is live on this device, or no longer. */
    public fun setGroupCallActive(key: String, active: Boolean) {
        if (active) groupCalls.add(key) else groupCalls.remove(key)
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
        data class StartOutgoing(val groupIdHex: String) : Event
        data class Dial(val audio: Boolean, val video: Boolean) : Event
        data object CancelPrepared : Event
        data class Accept(val withVideo: Boolean) : Event
        data object Decline : Event
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
            is Event.StartOutgoing -> onStartOutgoing(event.groupIdHex)
            is Event.Dial -> onDial(event)
            Event.CancelPrepared -> onCancelPrepared()
            is Event.Accept -> onAccept(event.withVideo)
            Event.Decline -> liveSlot()?.takeIf { it.call.incoming && isRinging(it) }?.call?.session?.declineCall()
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
            groupCallBusy = groupCalls.isNotEmpty(),
        )
        when (route) {
            OneToOneRoute.DROP -> Unit
            OneToOneRoute.APPLY -> applyToOccupant(row)
            OneToOneRoute.BUSY -> publishEnd(row.groupIdHex, callIdOfBody(row.body), FfiCallEndReason.BUSY)
            OneToOneRoute.RING -> if (foreground) ring(row) else holdOffer(row)
        }
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
            live.call.session.applyIncomingSignaling(row.body)
            return
        }
        // The occupant is a held offer. Its End means the caller gave up, and
        // the offer must never ring. Its ICE candidates are kept for the call
        // that rings on foreground; anything else means nothing yet.
        val held = deferred ?: return
        when (row.body) {
            is FfiCallSignalingBody.End -> {
                clearDeferred()
                rememberEnded(callIdHexOfBody(held.body))
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
    }

    private suspend fun onStartOutgoing(groupIdHex: String) {
        if (occupant(clock()) != null || groupCalls.isNotEmpty()) return
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
                call = OneToOneCall(groupIdHex, session, incoming = false, remoteOffersVideo = false),
                callIdHex = callIdHexOf(callId),
                keyArmed = key != null,
            ),
        )
    }

    private suspend fun onDial(event: Event.Dial) {
        val s = liveSlot() ?: return
        if (s.call.incoming || s.call.session.state.value != CallSession.WebCallState.Idle) return
        armKeyIfMissing(s)
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
            groupCallBusy = groupCalls.isNotEmpty(),
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
            CallSession.WebCallState.Connected,
            CallSession.WebCallState.Ending,
            -> cancelTimer(s)
            CallSession.WebCallState.Done -> {
                cancelTimer(s)
                rememberEnded(s.callIdHex)
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
