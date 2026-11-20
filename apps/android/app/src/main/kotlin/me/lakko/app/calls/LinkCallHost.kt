// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.atomic.AtomicBoolean
import me.lakko.app.l10n.UiText
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
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
 * The host also owns the call's INVITE LINK ([invite], [copyInviteLink],
 * [sendInvite]) — `docs/ux/call-links.md` section 3. The link belongs to the
 * call, not to a screen, so its one-per-call mint lives here beside the call;
 * what the control reads is in `InCallInvite.kt` and who is offered it in
 * `CallInviteLink.kt`.
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
    /**
     * The `/c/` slug the call was started from, to return to its landing, or
     * `null` for a call started from the Calls destination's `New call`
     * (`docs/ux/call-links.md` section 2), which has no landing to return to
     * and no link at all until somebody asks for one.
     */
    public val slug: String?,
    public val accountIdentitySeed: ByteArray,
    public val sessionToken: String,
    /** The raw account id, to read the own profile the call announces. */
    public val accountId: ByteArray,
    /** The name a guest typed on the link page, their name for this call
     *  (`docs/ux/calls.md` section 4.1, `docs/ux/call-links.md` section 9.1),
     *  already normalised; `null` for a real account, which starts under its
     *  profile name. */
    public val initialCallName: String? = null,
)

/**
 * What minting an invite link needs from the signed-in session
 * (`docs/ux/call-links.md` section 3).
 *
 * Read at the moment `Invite` is pressed rather than kept by the host for the
 * call's lifetime: the seed is the account's root secret, and the only reason
 * to hold it would be to save a lookup. Reading it late also answers section
 * 5's "signed out mid-flow" row for free — there is simply nothing to read.
 */
public class CallInviteIdentity(
    /** ADR-0017 phrase seed; `Identity.fromSeed` derives the signing key. */
    public val accountIdentitySeed: ByteArray,
    public val accountId: ByteArray,
    public val sessionToken: String,
)

/** The orchestrator's reports, as [MultiJoinerCall.Options] carries them. */
public class LinkCallEvents(
    public val onJoined: () -> Unit,
    public val onEnded: () -> Unit,
    public val onNoCommitter: () -> Unit,
    public val onError: (Throwable) -> Unit,
    /**
     * The link's owner ended the call for everyone (`docs/ux/calls.md`
     * section 4.1.2). Reported only for a message whose MLS-authenticated
     * sender holds the owner key of the signed capability. [starterName] is
     * the name on the owner's tile, or `null` when this device cannot name
     * them. The host ends the call.
     */
    public val onEndedForEveryone: (starterName: String?) -> Unit = {},
)

/** What the host needs from a running link call. */
public interface LinkCallHandle {
    /** Tear the call down and free what it holds. Idempotent. */
    public fun hangUp()

    /**
     * Tell every member the call has been ended for everyone
     * (`docs/ux/calls.md` section 4.1.2): a `CallEnd{EndedForAll}` sealed into
     * the call's own group and relayed to the call. Only the link's owner
     * sends it; every other member ignores it from anyone else. Best effort and
     * never throws: the host hangs up whether or not it went out.
     */
    public fun endForEveryone()

    public suspend fun publishVideo()

    public fun setCameraEnabled(on: Boolean)

    public fun setMicrophoneEnabled(on: Boolean)

    /** Use the camera chosen in the call's Settings sheet; `null` is the
     *  default one. */
    public fun selectCamera(cameraName: String?) {}
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

    /**
     * Sign and upload a `/c/` capability for the call [callId] names, and
     * return its URL (`docs/ux/call-links.md` sections 3 and 4).
     *
     * Costs a network round trip, which is why it
     * runs at the first `Invite` and not at the call's start. May throw; the
     * host turns the cause into the one sentence the user reads.
     */
    public suspend fun mintInviteLink(callId: ByteArray, identity: CallInviteIdentity): String
}

/** The link call in the host's slot. */
public data class LinkCallSlot(
    /** Lowercase hex of the capability's 16-byte `call_id`. */
    val callIdHex: String,
    /** The `/c/` landing this call came from, or `null` — see [LinkCallRequest.slug]. */
    val slug: String?,
    val owner: Boolean,
    val phase: LinkCallPhase,
    /** The reason, for [LinkCallPhase.ERROR], from the kind of failure (ADR-0061); `null` names none. */
    val error: UiText?,
)

/** What a start / join came to. */
public sealed interface LinkCallStartOutcome {
    public data object Started : LinkCallStartOutcome

    public data object AlreadyInThisCall : LinkCallStartOutcome

    public data class Busy(val message: UiText) : LinkCallStartOutcome

    /** [message] is diagnostic English for the log; a screen shows its own catalog line. */
    public data class Failed(val message: String) : LinkCallStartOutcome
}

public class LinkCallHost(
    private val scope: CoroutineScope,
    /** The call occupying the slot other than a link call, or `null`. */
    private val otherOccupant: () -> CallKind?,
    /** Called with whether a link call now occupies the slot. */
    private val onSlotChanged: (occupied: Boolean) -> Unit,
    private val warn: (String, Throwable?) -> Unit = ::logLinkCallWarning,
    /**
     * The call this device was in was ended for everyone by the link's owner
     * (`docs/ux/calls.md` section 4.1.2); see
     * [LinkCallEvents.onEndedForEveryone].
     */
    private val notifyEndedForEveryone: (starterName: String?) -> Unit = {},
    /**
     * File a finished call in the Calls list (`docs/ux/navigation.md` section
     * 5.1.1): the account the call was started under, and its row. Called at
     * most once per call, only for a call that went live, on the main thread;
     * the guest rule and the store write are the caller's. Never throws into
     * the host: a failure is logged.
     */
    private val recordHistory: (accountId: ByteArray, entry: CallHistoryEntry) -> Unit = { _, _ -> },
    /** The wall clock the history row is stamped with. */
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    @Volatile
    private var port: LinkCallPort? = null

    /** The Calls-list row of the current start, until it is filed. */
    private class PendingHistory(
        val attempt: Any,
        val accountId: ByteArray,
        val recorder: LinkCallHistoryRecorder,
    )

    private var pendingHistory: PendingHistory? = null

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

    /** The in-call screen wants to be up: from the moment the call is live until
     *  the screen's own exit after it ended. Whether it is actually drawn is
     *  this AND [collapsed] being false. */
    public val overlayVisible: StateFlow<Boolean> = mutableOverlay.asStateFlow()

    private val mutableCollapsed = MutableStateFlow(false)

    /**
     * `docs/ux/calls.md` section 4.5 — this call has been put away into the bar
     * across the top of the app. See [GroupCallHost.collapsed] for why it is
     * cleared where the call goes live and never where it ends.
     */
    public val collapsed: StateFlow<Boolean> = mutableCollapsed.asStateFlow()

    /** Put the call away (the chevron, or the system back). Never ends it. */
    public fun collapse() {
        mutableCollapsed.value = true
    }

    /** Bring the call back (a tap on the bar). */
    public fun expand() {
        mutableCollapsed.value = false
    }

    /** See [CallUiState.connectedSinceMs] — the running time the bar shows. */
    public val connectedSinceMs: StateFlow<Long?> get() = ui.connectedSinceMs

    private val mutableLandingClaims = MutableStateFlow(0)

    /** How many landings are showing the link call. */
    public val landingClaims: StateFlow<Int> = mutableLandingClaims.asStateFlow()

    private var handle: LinkCallHandle? = null

    /** The session of a call this device just ended for everyone, kept up
     *  until its last frame has left ([endForEveryone]); `null` otherwise. */
    private var flushing: LinkCallHandle? = null

    /** Identity of the current start; reports from an abandoned one are ignored. */
    private var attempt: Any? = null

    /** The last start, in flight or finished. [leave] cancels one in flight;
     *  the next start waits for a cancelled one to finish ([start]). */
    private var startJob: Job? = null

    private val mutableInvite = MutableStateFlow<InCallInvite>(InCallInvite.Idle)

    /**
     * What `Invite` is doing for the call in the slot
     * (`docs/ux/call-links.md` section 3).
     *
     * Reset at every start, so a link minted for one call is never offered as
     * the next call's.
     */
    public val invite: StateFlow<InCallInvite> = mutableInvite.asStateFlow()

    private val mutableInviteLineShown = MutableStateFlow(false)

    /**
     * Whether the line under the status row that says where the link went is
     * on screen ([inviteStatusHoldMs]). Held here, with the call, and not in
     * the call screen: a screen that is collapsed into the bar and opened
     * again is a new composition, and a timer of its own showed an old
     * "Link copied" for another few seconds on every return (seen on the
     * Pixel 7a, 2026-11-10). iOS (`CallInviteOffer.lineShown`) and web
     * (`CallHostContext`) hold it with the call too.
     */
    public val inviteLineShown: StateFlow<Boolean> = mutableInviteLineShown.asStateFlow()

    /** Takes the line down once its hold has passed. */
    private var inviteHoldJob: Job? = null

    /** The live call's raw `call_id`, which the link has to carry to lead here. */
    private var liveCallId: ByteArray? = null

    /** A link call is starting, waiting for admission or live. */
    public val occupiesSlot: Boolean
        get() = mutableSlot.value?.let { linkCallOccupiesSlot(it.phase) } == true

    init {
        scope.launch {
            mutableInvite.collect { next ->
                inviteHoldJob?.cancel()
                inviteHoldJob = null
                mutableInviteLineShown.value = next !is InCallInvite.Idle
                val hold = inviteStatusHoldMs(next) ?: return@collect
                inviteHoldJob = scope.launch {
                    delay(hold)
                    mutableInviteLineShown.value = false
                }
            }
        }
        // Controls, only once the call is live: a joiner still waiting for its
        // Welcome has published nothing (invariant #4) and must not start
        // announcing camera state to a room it has not been admitted to.
        scope.launch {
            ui.participants
                .map { list -> list.firstOrNull { it.isLocal }?.camOn ?: false }
                .distinctUntilChanged()
                .collect { camOn ->
                    val h = liveHandle() ?: return@collect
                    if (camOn) pendingHistory?.recorder?.sawVideo()
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
        // The camera chosen in the Settings sheet (docs/ux/calls.md section
        // 4.1). Applied when chosen and again when the call goes live, so a
        // choice made while waiting to be admitted is the camera that opens.
        scope.launch {
            combine(ui.devices.camera, mutableSlot.map { it?.phase == LinkCallPhase.LIVE }.distinctUntilChanged()) {
                    camera, live ->
                camera to live
            }.collect { (camera, live) -> if (live) handle?.selectCamera(camera) }
        }
    }

    private fun liveHandle(): LinkCallHandle? =
        handle?.takeIf { mutableSlot.value?.phase == LinkCallPhase.LIVE }

    /** Why starting or joining now would be refused, or `null` when the slot is free. */
    public fun busyMessage(verb: CallVerb, requestedCallIdHex: String): UiText? {
        val decision = decideLinkCallStart(
            occupant = occupant(),
            activeLinkCallIdHex = mutableSlot.value?.takeIf { linkCallOccupiesSlot(it.phase) }?.callIdHex,
            requestedCallIdHex = requestedCallIdHex,
            verb = verb,
        )
        return (decision as? LinkCallStartDecision.Busy)?.message
    }

    /** Whether this device may offer an invite link for the call it is in
     *  ([showsCallInvite]). */
    public fun canInvite(): Boolean =
        mutableSlot.value?.let { showsCallInvite(it.owner, it.phase) } == true

    /**
     * `Copy link` (`docs/ux/call-links.md` section 3): the link to the live
     * call goes to the clipboard, minted first when this is the first use, and
     * [invite] says so.
     *
     * [copy] puts a string on the clipboard, or is `null` where there is none
     * to reach; the link is then shown to be copied by hand. When there is no
     * link to give, [invite] already says why. The call is never affected.
     *
     * Runs on the host's own scope, so leaving the screen mid-upload does not
     * throw the work away.
     */
    public suspend fun copyInviteLink(identity: CallInviteIdentity?, copy: ((String) -> Unit)?): Unit =
        scope.async {
            val callId = liveCallId
            val url = linkForCall(identity) ?: return@async
            if (liveCallId !== callId) return@async
            mutableInvite.value = InCallInvite.Ready(url, copyCallLink(url, copy))
        }.await()

    /**
     * `Invite a contact` (`docs/ux/call-links.md` section 3): send the
     * invitation into the conversation the person picked.
     *
     * [send] publishes one plain text message whose whole text is the link it
     * is given, which is what lets every client draw it as an invitation with a
     * `Join` button ([callInviteOf]). `true` when it was sent, and [invite]
     * then names [conversationTitle]; `false` when it was not, so the picker
     * stays open with the reason. A failed send keeps the link: the next use
     * re-offers it and nothing is minted twice. The call is untouched either
     * way.
     */
    public suspend fun sendInvite(
        identity: CallInviteIdentity?,
        conversationTitle: String,
        send: suspend (url: String) -> Unit,
    ): Boolean =
        scope.async {
            val callId = liveCallId
            val url = linkForCall(identity) ?: return@async false
            try {
                send(url)
            } catch (cancel: CancellationException) {
                if (liveCallId === callId) {
                    mutableInvite.value = InCallInvite.Failed(CallInviteFailure.NOT_SENT, url)
                }
                throw cancel
            } catch (error: Throwable) {
                warn("link call: sending the call invitation failed", error)
                if (liveCallId === callId) {
                    mutableInvite.value = InCallInvite.Failed(CallInviteFailure.NOT_SENT, url)
                }
                return@async false
            }
            if (liveCallId === callId) {
                mutableInvite.value = InCallInvite.Ready(url, LinkDelivery.Sent(conversationTitle))
            }
            true
        }.await()

    /**
     * Found (owner) or join the call a `/c/` link names. Never throws.
     *
     * A start the person left while it was in flight ([leave]) is cancelled
     * and comes back as [LinkCallStartOutcome.Started]: there is no outcome
     * to show, which is what the slot already says. The start after it waits
     * for that one to finish tearing down before it takes the call screen's
     * state, so nothing the abandoned start writes on its way out lands on
     * the new call.
     */
    public suspend fun start(request: LinkCallRequest): LinkCallStartOutcome {
        val abandoned = startJob?.takeIf { it.isCancelled }
        val job = scope.async {
            abandoned?.join()
            runStart(request)
        }
        startJob = job
        return try {
            job.await()
        } catch (cancel: CancellationException) {
            // The caller's own cancellation is passed on; [leave] cancelling
            // the start is the person's doing and not a failure.
            currentCoroutineContext().ensureActive()
            LinkCallStartOutcome.Started
        }
    }

    /** Leave the link call (or stop waiting to be admitted). No outcome to show. */
    public fun leave() {
        // A start still in flight stops here: what it has opened so far is
        // closed by its own cancellation, and it reports nothing.
        startJob?.cancel()
        fileHistory()
        attempt = null
        val h = handle
        handle = null
        liveCallId = null
        mutableInvite.value = InCallInvite.Idle
        setSlot(null)
        h?.hangUp()
    }

    /**
     * End the link call for every person in it (`docs/ux/calls.md` section
     * 4.1.2). Only acts for the link's owner, in a live call.
     *
     * The slot is given up at once, as on any hang-up. The session itself is
     * torn down [LINK_END_FOR_EVERYONE_FLUSH_MS] later: the end rides the
     * signaling socket, and closing that socket in the same breath would drop
     * the frame still queued on it.
     */
    public fun endForEveryone() {
        val s = mutableSlot.value ?: return
        val h = liveHandle() ?: return
        if (!s.owner) return
        h.endForEveryone()
        fileHistory()
        attempt = null
        handle = null
        liveCallId = null
        mutableInvite.value = InCallInvite.Idle
        setSlot(null)
        flushing = h
        scope.launch {
            delay(LINK_END_FOR_EVERYONE_FLUSH_MS)
            finishFlush(h)
        }
    }

    /** Tear down the session [endForEveryone] left up for its last frame. With
     *  no argument, whichever one is still up: the next call is about to take
     *  the shared UI state, or the session is being signed out. */
    private fun finishFlush(only: LinkCallHandle? = null) {
        val h = flushing ?: return
        if (only != null && only !== h) return
        flushing = null
        h.hangUp()
    }

    /** The owner ended the call this device is in for everyone: say who, then
     *  end it here the way a call that ended on its own ends. */
    private fun endedForEveryone(forAttempt: Any, starterName: String?) {
        if (attempt !== forAttempt) return
        val s = mutableSlot.value ?: return
        if (!linkCallOccupiesSlot(s.phase)) return
        notifyEndedForEveryone(starterName)
        apply(forAttempt, LinkCallEvent.Ended)
        // The line just shown says how the call ended; it is not repeated as
        // the status line an ordinary end leaves behind.
        if (mutableLandingClaims.value == 0) dismissOutcome()
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
        // repeated as a status line — unless a landing is showing the call, which
        // then says it ended and forgets it when it closes.
        if (occupiesSlot) {
            leave()
        } else if (mutableSlot.value?.phase == LinkCallPhase.ENDED && mutableLandingClaims.value == 0) {
            dismissOutcome()
        }
    }

    /** Sign-out: tear down locally. */
    public fun reset() {
        leave()
        finishFlush()
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

    /**
     * The link to the call in the slot, minted on first use.
     *
     * `null` when there is none to give: a mint is already in flight, there is
     * no call this device hosts, no session, or the mint failed — and in each
     * case [invite] already says what the user reads.
     *
     * One mint per call. The check and the move to [InCallInvite.Minting] have
     * no suspension between them, so a double tap finds the mint in flight and
     * is ignored rather than uploading a second capability; a link already
     * minted is handed back without a round trip, also after a failed send.
     * On success the state is left for the caller, which records where the
     * link went.
     */
    private suspend fun linkForCall(identity: CallInviteIdentity?): String? {
        val state = mutableInvite.value
        when (nextInvitePress(state)) {
            InvitePress.IGNORE -> return null
            InvitePress.REOFFER -> mintedInviteUrl(state)?.let { return it }
            InvitePress.MINT -> Unit
        }
        val p = port
        val callId = liveCallId
        if (p == null || callId == null || !canInvite()) {
            // Nothing to invite anyone to. Not a failure to report: the control
            // is only drawn while there is a live call this device hosts.
            return null
        }
        if (identity == null) {
            mutableInvite.value = InCallInvite.Failed(CallInviteFailure.NO_SESSION)
            return null
        }
        mutableInvite.value = InCallInvite.Minting
        val url = try {
            p.mintInviteLink(callId, identity)
        } catch (cancel: CancellationException) {
            if (liveCallId === callId) mutableInvite.value = InCallInvite.Idle
            throw cancel
        } catch (error: Throwable) {
            // The call runs over the SFU and owes the ledger nothing, so a
            // failed mint is a sentence and an unchanged call.
            warn("link call: minting the invite link failed", error)
            if (liveCallId === callId) {
                mutableInvite.value = InCallInvite.Failed(classifyCallInviteFailure(error))
            }
            return null
        }
        // The call may have ended, or another begun, while the mint was in
        // flight; its link is not this call's.
        if (liveCallId !== callId) return null
        if (!canInvite()) {
            mutableInvite.value = InCallInvite.Idle
            return null
        }
        return url
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

        // A call ended for everyone a moment ago still closes its session; do
        // it now, before this call takes the UI state that close would write.
        finishFlush()
        // A row still owed by the previous call is filed before this one
        // replaces it.
        fileHistory()
        val thisAttempt = Any()
        attempt = thisAttempt
        pendingHistory = PendingHistory(
            attempt = thisAttempt,
            accountId = request.accountId.copyOf(),
            recorder = LinkCallHistoryRecorder(
                callIdHex = callIdHex,
                outgoing = request.isOwner,
                video = request.mediaVideo,
            ),
        )
        handle = null
        liveCallId = request.callId.copyOf()
        mutableInvite.value = InCallInvite.Idle
        ui.reset()
        setSlot(
            LinkCallSlot(
                callIdHex = callIdHex,
                slug = request.slug,
                owner = request.isOwner,
                phase = LinkCallPhase.STARTING,
                error = null,
            ),
        )
        val events = LinkCallEvents(
            onJoined = { apply(thisAttempt, LinkCallEvent.Joined) },
            onEnded = { apply(thisAttempt, LinkCallEvent.Ended) },
            onNoCommitter = { apply(thisAttempt, LinkCallEvent.NoCommitter) },
            onError = { error -> apply(thisAttempt, LinkCallEvent.Error, callErrorReason(error)) },
            onEndedForEveryone = { name -> endedForEveryone(thisAttempt, name) },
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
            // The person left while the start was in flight and it failed
            // under the teardown: nothing to report.
            if (attempt !== thisAttempt) return LinkCallStartOutcome.Started
            val message = error.message ?: error.toString()
            warn("link call: start failed", error)
            apply(thisAttempt, LinkCallEvent.Error, callErrorReason(error))
            LinkCallStartOutcome.Failed(message)
        }
    }

    private fun apply(forAttempt: Any, event: LinkCallEvent, error: UiText? = null) {
        if (attempt !== forAttempt) return
        val s = mutableSlot.value ?: return
        val next = reduceLinkCallPhase(s.phase, event)
        val reason = if (next == LinkCallPhase.ERROR && s.phase != LinkCallPhase.ERROR) {
            error ?: s.error
        } else {
            s.error
        }
        if (next == s.phase && reason == s.error) return
        setSlot(s.copy(phase = next, error = reason))
        if (next == LinkCallPhase.LIVE) {
            pendingHistory?.takeIf { it.attempt === forAttempt }?.recorder?.live(nowMs())
            mutableOverlay.value = true
            // A call is born expanded — see [collapsed].
            mutableCollapsed.value = false
        }
        if (!linkCallOccupiesSlot(next)) {
            fileHistory(forAttempt)
            // The orchestrator already closed its session; free what the call
            // still holds (its identity), outside the callback that reported it.
            val h = handle
            handle = null
            if (h != null) scope.launch { h.hangUp() }
        }
    }

    /**
     * File the current call's Calls-list row, once: when this device leaves the
     * call or the call ends for it, whichever comes first. With [onlyAttempt],
     * only when that start is still the current one. Nothing for a call that
     * never went live.
     */
    private fun fileHistory(onlyAttempt: Any? = null) {
        val pending = pendingHistory ?: return
        if (onlyAttempt != null && pending.attempt !== onlyAttempt) return
        pendingHistory = null
        val entry = pending.recorder.finish(nowMs()) ?: return
        try {
            recordHistory(pending.accountId, entry)
        } catch (error: Throwable) {
            warn("link call: filing the call history row failed", error)
        }
    }

    private fun setSlot(next: LinkCallSlot?) {
        mutableSlot.value = next
        onSlotChanged(next != null && linkCallOccupiesSlot(next.phase))
    }
}

/**
 * How long the owner's session stays up after it sent the end for everyone,
 * so the frame leaves before the socket closes. Chosen, not measured: writing
 * one queued signaling frame is local work.
 */
public const val LINK_END_FOR_EVERYONE_FLUSH_MS: Long = 300L

private fun logLinkCallWarning(message: String, error: Throwable?) {
    try {
        android.util.Log.w("LinkCallHost", message, error)
    } catch (_: Throwable) {
        // No Android logger (host unit tests).
    }
}
