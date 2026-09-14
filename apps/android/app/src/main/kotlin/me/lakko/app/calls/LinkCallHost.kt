// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The app-level owner of the call-link (`/c/<slug>`, ADR-0032) call — the
 * Kotlin counterpart of the link slot in `apps/web/src/calls/CallHostContext.tsx`.
 * The decisions live in `LinkCallHostRules.kt`.
 *
 * The landing (`JoinCallScreen`) decodes and verifies the capability, runs the
 * guest sign-up and asks [start]; from then on it only shows [slot]. The
 * orchestrator ([MultiJoinerCall], behind [LinkCallPort]), its UI state, the
 * controls and the hang-up live here on the process scope, so leaving the
 * landing keeps the call (a status line on other screens says where it is), and
 * a second link cannot start a second call while this one holds the slot.
 *
 * The protocol is unchanged and entirely [MultiJoinerCall]'s: owner-founds,
 * committer election with failover and reclaim, per-peer and per-signature-key
 * admission dedup, per-admission rekey, the post-join owner-membership check,
 * the admission timeout, media only after the Welcome (invariant #4).
 *
 * Public entry points are called on the main thread; see [GroupCallHost].
 */

/** A verified call-invite capability plus the live session, as the landing hands it over. */
public class LinkCallRequest(
    public val callId: ByteArray,
    public val ownerAccountId: ByteArray,
    public val ownerSignatureKey: ByteArray,
    public val mediaVideo: Boolean,
    /** The local identity IS the capability owner: found + host. */
    public val isOwner: Boolean,
    /** The `/c/` slug the call was started from, to return to its landing. */
    public val slug: String,
    public val accountIdentitySeed: ByteArray,
    public val sessionToken: String,
)

/** The orchestrator's reports, as [MultiJoinerCall.Options] carries them. */
public class LinkCallEvents(
    public val onJoined: () -> Unit,
    public val onEnded: () -> Unit,
    public val onNoCommitter: () -> Unit,
    public val onError: (Throwable) -> Unit,
)

/** What the host needs from a running link call. */
public interface LinkCallHandle {
    /** Tear the call down and free what it holds. Idempotent. */
    public fun hangUp()

    public suspend fun publishVideo()

    public fun setCameraEnabled(on: Boolean)

    public fun setMicrophoneEnabled(on: Boolean)
}

/** The platform effects the host drives. */
public interface LinkCallPort {
    public fun sfuBaseUrl(): String?

    /** The SFrame native shim is present (invariant #4). */
    public fun callingAvailable(): Boolean

    /** Found (owner) or join the call. May throw; events may fire before it returns. */
    public suspend fun start(
        request: LinkCallRequest,
        sfuBaseUrl: String,
        ui: CallUiState,
        events: LinkCallEvents,
    ): LinkCallHandle
}

/** The link call in the host's slot. */
public data class LinkCallSlot(
    /** Lowercase hex of the capability's 16-byte `call_id`. */
    val callIdHex: String,
    val slug: String,
    val owner: Boolean,
    val phase: LinkCallPhase,
    /** The reason, for [LinkCallPhase.ERROR]. */
    val errorMessage: String?,
)

/** What a start / join came to. */
public sealed interface LinkCallStartOutcome {
    public data object Started : LinkCallStartOutcome

    public data object AlreadyInThisCall : LinkCallStartOutcome

    public data class Busy(val message: String) : LinkCallStartOutcome

    public data class Failed(val message: String) : LinkCallStartOutcome
}

public class LinkCallHost(
    private val scope: CoroutineScope,
    /** The call occupying the slot other than a link call, or `null`. */
    private val otherOccupant: () -> CallKind?,
    /** Called with whether a link call now occupies the slot. */
    private val onSlotChanged: (occupied: Boolean) -> Unit,
    private val warn: (String, Throwable?) -> Unit = ::logLinkCallWarning,
) {
    @Volatile
    private var port: LinkCallPort? = null

    public fun attach(port: LinkCallPort) {
        this.port = port
    }

    /** The in-call UI state the hosted link call drives. */
    public val ui: CallUiState = CallUiState()

    private val mutableSlot = MutableStateFlow<LinkCallSlot?>(null)

    /** The link call occupying the slot, or kept after it ended so its outcome
     *  can be shown; `null` otherwise. */
    public val slot: StateFlow<LinkCallSlot?> = mutableSlot.asStateFlow()

    private val mutableOverlay = MutableStateFlow(false)

    /** The in-call screen is up: from the moment the call is live until the
     *  screen's own exit after it ended. */
    public val overlayVisible: StateFlow<Boolean> = mutableOverlay.asStateFlow()

    private val mutableLandingClaims = MutableStateFlow(0)

    /** How many landings are showing the link call. */
    public val landingClaims: StateFlow<Int> = mutableLandingClaims.asStateFlow()

    private var handle: LinkCallHandle? = null

    /** Identity of the current start; reports from an abandoned one are ignored. */
    private var attempt: Any? = null

    /** A link call is starting, waiting for admission or live. */
    public val occupiesSlot: Boolean
        get() = mutableSlot.value?.let { linkCallOccupiesSlot(it.phase) } == true

    init {
        // Controls, only once the call is live: a joiner still waiting for its
        // Welcome has published nothing (invariant #4) and must not start
        // announcing camera state to a room it has not been admitted to.
        scope.launch {
            ui.participants
                .map { list -> list.firstOrNull { it.isLocal }?.camOn ?: false }
                .distinctUntilChanged()
                .collect { camOn ->
                    val h = liveHandle() ?: return@collect
                    if (camOn) {
                        try {
                            h.publishVideo()
                        } catch (cancel: CancellationException) {
                            throw cancel
                        } catch (error: Throwable) {
                            ui.setCamera(false)
                            warn("link call: mid-call publishVideo failed, camera reverted", error)
                        }
                    } else {
                        h.setCameraEnabled(false)
                    }
                }
        }
        scope.launch {
            ui.participants
                .map { list -> list.firstOrNull { it.isLocal }?.micOn }
                .distinctUntilChanged()
                .collect { micOn -> if (micOn != null) liveHandle()?.setMicrophoneEnabled(micOn) }
        }
    }

    private fun liveHandle(): LinkCallHandle? =
        handle?.takeIf { mutableSlot.value?.phase == LinkCallPhase.LIVE }

    /** Why starting or joining now would be refused, or `null` when the slot is free. */
    public fun busyMessage(verb: CallVerb, requestedCallIdHex: String): String? {
        val decision = decideLinkCallStart(
            occupant = occupant(),
            activeLinkCallIdHex = mutableSlot.value?.takeIf { linkCallOccupiesSlot(it.phase) }?.callIdHex,
            requestedCallIdHex = requestedCallIdHex,
            verb = verb,
        )
        return (decision as? LinkCallStartDecision.Busy)?.message
    }

    /** Found (owner) or join the call a `/c/` link names. Never throws. */
    public suspend fun start(request: LinkCallRequest): LinkCallStartOutcome =
        scope.async { runStart(request) }.await()

    /** Leave the link call (or stop waiting to be admitted). No outcome to show. */
    public fun leave() {
        attempt = null
        val h = handle
        handle = null
        setSlot(null)
        h?.hangUp()
    }

    /** Forget a link call's outcome once it has been shown. */
    public fun dismissOutcome() {
        val s = mutableSlot.value ?: return
        if (!linkCallOccupiesSlot(s.phase)) setSlot(null)
    }

    /** A landing is showing the link call; the host does not repeat its status
     *  elsewhere meanwhile. Returns the release. */
    public fun claimLanding(): () -> Unit {
        mutableLandingClaims.update { it + 1 }
        val released = AtomicBoolean(false)
        return {
            if (released.compareAndSet(false, true)) mutableLandingClaims.update { (it - 1).coerceAtLeast(0) }
        }
    }

    /** The in-call screen's exit, after the ended state was shown. */
    public fun onOverlayExit() {
        mutableOverlay.value = false
        // The screen only exits once the session ended; a slot still claiming the
        // call would otherwise hold it with nothing left to show or end it. An
        // ordinary end was just shown on the in-call screen, so it is not
        // repeated as a status line.
        if (occupiesSlot) {
            leave()
        } else if (mutableSlot.value?.phase == LinkCallPhase.ENDED) {
            dismissOutcome()
        }
    }

    /** Sign-out: tear down locally. */
    public fun reset() {
        leave()
        mutableOverlay.value = false
    }

    private fun occupant(): CallKind? {
        val other = otherOccupant()
        return callSlotOccupant(
            oneToOneInCall = other == CallKind.ONE_TO_ONE,
            groupCallActive = other == CallKind.GROUP,
            linkCallActive = occupiesSlot,
        )
    }

    private suspend fun runStart(request: LinkCallRequest): LinkCallStartOutcome {
        val p = port ?: return LinkCallStartOutcome.Failed("Calls are not available right now.")
        val callIdHex = callIdHexOf(request.callId)
        val verb = if (request.isOwner) CallVerb.START else CallVerb.JOIN
        // No suspension between this decision and the slot write below, so a
        // double tap finds the slot taken rather than starting twice.
        when (
            val decision = decideLinkCallStart(
                occupant = occupant(),
                activeLinkCallIdHex = mutableSlot.value?.takeIf { linkCallOccupiesSlot(it.phase) }?.callIdHex,
                requestedCallIdHex = callIdHex,
                verb = verb,
            )
        ) {
            LinkCallStartDecision.Start -> Unit
            LinkCallStartDecision.AlreadyInThisCall -> return LinkCallStartOutcome.AlreadyInThisCall
            is LinkCallStartDecision.Busy -> return LinkCallStartOutcome.Busy(decision.message)
        }
        val base = p.sfuBaseUrl()
            ?: return LinkCallStartOutcome.Failed("This client has no SFU endpoint — set lakko.sfu.url and rebuild.")
        // Fail-closed: no SFrame native shim, no call (invariant #4).
        if (!p.callingAvailable()) {
            return LinkCallStartOutcome.Failed("Secure media encryption is unavailable on this device.")
        }

        val thisAttempt = Any()
        attempt = thisAttempt
        handle = null
        ui.reset()
        setSlot(
            LinkCallSlot(
                callIdHex = callIdHex,
                slug = request.slug,
                owner = request.isOwner,
                phase = LinkCallPhase.STARTING,
                errorMessage = null,
            ),
        )
        val events = LinkCallEvents(
            onJoined = { apply(thisAttempt, LinkCallEvent.Joined) },
            onEnded = { apply(thisAttempt, LinkCallEvent.Ended) },
            onNoCommitter = { apply(thisAttempt, LinkCallEvent.NoCommitter) },
            onError = { error -> apply(thisAttempt, LinkCallEvent.Error, error.message ?: error.toString()) },
        )
        return try {
            // Owner founds + hosts (`onJoined` fires inside); a joiner connects
            // signaling-only, relays its KeyPackage and waits for a Welcome.
            val h = p.start(request, base, ui, events)
            val s = mutableSlot.value
            if (attempt !== thisAttempt || s == null || !linkCallOccupiesSlot(s.phase)) {
                // Left, replaced, or already over while the start was in flight.
                h.hangUp()
            } else {
                handle = h
                apply(thisAttempt, LinkCallEvent.SignalingOpen(owner = request.isOwner))
            }
            LinkCallStartOutcome.Started
        } catch (cancel: CancellationException) {
            if (attempt === thisAttempt) leave()
            throw cancel
        } catch (error: Throwable) {
            val message = error.message ?: error.toString()
            warn("link call: start failed", error)
            apply(thisAttempt, LinkCallEvent.Error, message)
            LinkCallStartOutcome.Failed(message)
        }
    }

    private fun apply(forAttempt: Any, event: LinkCallEvent, errorMessage: String? = null) {
        if (attempt !== forAttempt) return
        val s = mutableSlot.value ?: return
        val next = reduceLinkCallPhase(s.phase, event)
        val message = if (next == LinkCallPhase.ERROR && s.phase != LinkCallPhase.ERROR) {
            errorMessage ?: s.errorMessage
        } else {
            s.errorMessage
        }
        if (next == s.phase && message == s.errorMessage) return
        setSlot(s.copy(phase = next, errorMessage = message))
        if (next == LinkCallPhase.LIVE) mutableOverlay.value = true
        if (!linkCallOccupiesSlot(next)) {
            // The orchestrator already closed its session; free what the call
            // still holds (its identity), outside the callback that reported it.
            val h = handle
            handle = null
            if (h != null) scope.launch { h.hangUp() }
        }
    }

    private fun setSlot(next: LinkCallSlot?) {
        mutableSlot.value = next
        onSlotChanged(next != null && linkCallOccupiesSlot(next.phase))
    }
}

private fun logLinkCallWarning(message: String, error: Throwable?) {
    try {
        android.util.Log.w("LinkCallHost", message, error)
    } catch (_: Throwable) {
        // No Android logger (host unit tests).
    }
}
