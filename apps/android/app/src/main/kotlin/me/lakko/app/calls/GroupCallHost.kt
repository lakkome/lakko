// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.security.SecureRandom
import me.lakko.app.l10n.UiText
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The app-level owner of the group (SFU) call — the Kotlin counterpart of the
 * group slot in `apps/web/src/calls/CallHostContext.tsx`. The decisions live in
 * `GroupCallHostRules.kt`; what is here is the state and the effects.
 *
 * ## What it guarantees
 *
 *  - **The call outlives the chat.** The SFU session, its UI state, the
 *    group-control publishes and the hang-up live here, on the process scope
 *    ([AppCallHost]). A chat asks [start] and reads [slot]; the in-call screen
 *    is drawn from the activity root, so a destination change, a push tap or a
 *    `/c/` link never hides or orphans the call.
 *  - **One call at a time.** [start] refuses while any call occupies the slot
 *    ([decideGroupCallStart]); [onSlotChanged] tells the 1x1 host, which then
 *    answers an incoming offer `End{busy}`.
 *  - **The key follows the epoch while the chat is closed.** [onGroupSettled]
 *    is fed from `ChatRepository.addGroupMlsIdleListener`, which reports a
 *    group only when its per-group lock drains — after the open chat's poll,
 *    the app-level all-groups poll, a commit or the committer sweep, never
 *    between two queued regions. The read is re-checked as settled under the
 *    provider lock ([SettledGroupRead.settled]); an unsettled or unreadable
 *    state keeps the current key (fail-closed, invariant #4), and a group this
 *    device has left ends the call locally ([decideGroupCallEpochChange]).
 *
 * ## Threading
 *
 * Public entry points are called on the main thread; the production scope runs
 * on `Dispatchers.Main.immediate`, so every state change happens there.
 * [onGroupSettled] may be called from any thread and only launches onto the
 * scope. Settled reads are serialised, so a later read can never be overtaken
 * by an earlier one.
 */

/** What the host needs from a live SFU session. [SfuCallUi] implements it in production. */
public interface GroupCallSessionHandle {
    /** The SFU session reported `FAILED`. */
    public val failed: Boolean

    /** The last time the SFU proved it was there; 0 before the link came up. */
    public val lastInboundAtMs: Long

    /** The SFU refused this device because the call had already ended. */
    public val callEndedByServer: Boolean

    public suspend fun connect()

    public fun hangUp()

    public fun rekey(seed: ByteArray)

    public suspend fun publishVideo()

    public fun setCameraEnabled(on: Boolean)

    public fun setMicrophoneEnabled(on: Boolean)

    /** Use the camera chosen in the call's Settings sheet; `null` is the
     *  default one. */
    public fun selectCamera(cameraName: String?) {}
}

/** One read of a group for the host: membership, epoch, and the call seed at that epoch. */
public class SettledGroupRead(
    /** The group is in the repository. */
    public val joined: Boolean,
    /** `Group.isActive`, `null` when it cannot be read. */
    public val isActive: Boolean?,
    /** No region on the group held or awaited its lock when this was read. */
    public val settled: Boolean,
    /** The group's epoch, `null` when it cannot be read. */
    public val epoch: ULong?,
    /** `exportSecret("lakko/call/v1", callId, 32)` at [epoch], `null` when it failed. */
    public val seed: ByteArray?,
    /** This device's leaf in the group, `null` with [seed]. */
    public val localLeaf: Int?,
) {
    public companion object {
        public fun notJoined(): SettledGroupRead =
            SettledGroupRead(joined = false, isActive = null, settled = true, epoch = null, seed = null, localLeaf = null)
    }
}

/** The inputs of a new SFU session. */
public class GroupCallSessionRequest(
    public val groupIdHex: String,
    public val callIdHex: String,
    public val sfuBaseUrl: String,
    public val jwt: String,
    public val seed: ByteArray,
    public val localLeaf: Int,
    public val video: Boolean,
    /** The TURN credential minted beside [jwt], or `null` (STUN alone). */
    public val turn: TurnCredential? = null,
)

/** The platform effects the host drives. */
public interface GroupCallPort {
    public fun sfuBaseUrl(): String?

    public fun sessionToken(): String?

    /** `Group.isActive`, `null` when unknown or not joined. */
    public fun isGroupActive(groupIdHex: String): Boolean?

    /**
     * ADR-0091 (maintainer, 2026-11-20) — `true` while this device is behind
     * in [groupIdHex]: a start or join fails with the ordinary text before
     * anything is captured, announced or connected.
     */
    public fun behindHeld(groupIdHex: String): Boolean = false

    /** Bring the group up to date before joining a call in it. May throw. */
    public suspend fun syncGroup(groupIdHex: String)

    public fun isCallLive(groupIdHex: String, callIdHex: String, nowMs: Long): Boolean

    /**
     * The fresh members of the recorded call [callIdHex] other than this
     * device's own identity, at [nowMs]; `null` when the record is gone or
     * names another call.
     */
    public fun recordOtherFreshMembers(groupIdHex: String, callIdHex: String, nowMs: Long): Set<String>?

    /**
     * This ACCOUNT started the recorded call [callIdHex] (`docs/ux/calls.md`
     * section 4.1.2): the starter the record carries is this device's own
     * person key. `false` when the record is gone, names another call or
     * carries no starter.
     */
    public fun startedByThisAccount(groupIdHex: String, callIdHex: String): Boolean

    /** The name of the person behind [personKeyB64] by the call tile-name rule
     *  (`CallTileName.kt`), or `null` when this device cannot name them. */
    public fun personName(personKeyB64: String): String?

    public suspend fun readSettledGroup(groupIdHex: String, callId: ByteArray): SettledGroupRead

    /** The call token (and the TURN credential beside it) for the room of
     *  [callId], paid for by one anonymous `call-join` token (ADR-0078
     *  section 2): no session reaches the issuer. May throw. */
    public suspend fun mintCallJwt(callId: ByteArray): MintCallJwtResult

    /** `sfuRoomForCall(callId)` (64 lower-case hex), or `null` when it cannot
     *  be computed. */
    public fun sfuRoom(callId: ByteArray): String?

    /** `GET` the SFU's room-state route at [url] (no token, no credentials)
     *  and return the body on a 200, else `null` (`SfuCallState.kt`). Never
     *  throws. */
    public suspend fun fetchSfuCallState(url: String): String?

    /** Publish a group-call body to the group and fold it into the local record. */
    public suspend fun publishSignal(groupIdHex: String, body: FfiCallSignalingBody): Boolean

    public fun createSession(request: GroupCallSessionRequest, ui: CallUiState): GroupCallSessionHandle

    /** `docs/ux/calls.md` section 6 - file the finished call's row in the conversation it happened in.
     *  [startedAtMs] is when this device started or joined the call; [callIdHex]
     *  names the call, from which the row's id is derived. */
    public fun recordCall(groupIdHex: String, entry: CallTranscriptEntry, startedAtMs: Long?, callIdHex: String?)
}

/** The group call in the host's slot. */
public data class GroupCallSlot(
    val groupIdHex: String,
    val callIdHex: String,
    /** `true` until the SFU session exists. */
    val starting: Boolean,
)

/** What a start / join came to. */
public sealed interface GroupCallStartOutcome {
    public data object Connected : GroupCallStartOutcome

    /** This call is already starting or live here; nothing new was done. */
    public data object AlreadyInThisCall : GroupCallStartOutcome

    /** Another call occupies the slot; nothing was done. */
    public data class Busy(val message: UiText) : GroupCallStartOutcome

    /** The call to join is no longer live. */
    public data object Ended : GroupCallStartOutcome

    /**
     * The person left the call while it was still connecting (they pressed
     * End). Not a failure: there is nothing to tell them.
     */
    public data object Left : GroupCallStartOutcome

    /** [message] renders from the kind of failure (ADR-0061); the diagnostic is in the log. */
    public data class Failed(val message: UiText) : GroupCallStartOutcome
}

public class GroupCallHost(
    private val scope: CoroutineScope,
    /** The call occupying the slot other than a group call, or `null`. */
    private val otherOccupant: () -> CallKind?,
    /** Called with whether a group call now occupies the slot. */
    private val onSlotChanged: (occupied: Boolean) -> Unit,
    private val clock: () -> Long = { System.currentTimeMillis() },
    private val warn: (String, Throwable?) -> Unit = ::logGroupCallWarning,
    /**
     * The call this device was in was ended for everyone by its starter
     * (`docs/ux/calls.md` section 4.1.2). [starterName] is that person's name,
     * or `null` when this device cannot name them. Called on the host scope.
     */
    private val notifyEndedForEveryone: (starterName: String?) -> Unit = {},
    /** ADR-0087 UX 9.1 - show that the call ended because this account is out
     *  of its group. Called on the host scope. */
    private val notifyNoLongerInGroup: () -> Unit = {},
) {
    @Volatile
    private var port: GroupCallPort? = null

    /** Install the platform effects. Re-attaching (an activity recreate) replaces them. */
    public fun attach(port: GroupCallPort) {
        this.port = port
    }

    /** The in-call UI state the hosted group call drives. */
    public val ui: CallUiState = CallUiState()

    private val mutableSlot = MutableStateFlow<GroupCallSlot?>(null)

    /** The group call starting or live on this device, or `null`. */
    public val slot: StateFlow<GroupCallSlot?> = mutableSlot.asStateFlow()

    private val mutableOverlay = MutableStateFlow(false)

    /** The in-call screen wants to be up: from the moment the session exists
     *  until the screen's own exit after the call ended. Whether it is actually
     *  drawn is this AND [collapsed] being false — see [collapsed]. */
    public val overlayVisible: StateFlow<Boolean> = mutableOverlay.asStateFlow()

    private val mutableCollapsed = MutableStateFlow(false)

    /**
     * `docs/ux/calls.md` section 4.5 — this call has been put away into the bar
     * across the top of the app.
     *
     * A property of the CALL, not of a screen: it is set and cleared only here,
     * so no destination change can touch it, and it is cleared where the call
     * takes the slot rather than by anything watching the call end. Both halves
     * of that matter. A call is born expanded because clearing it at the start
     * is synchronous with the very assignment that makes the screen want to be
     * up, so a new call can never flash a bar on its way to its own screen. And
     * it is NOT cleared when a call ends, because section 4.5 says a collapsed
     * call that ends must not throw its `Call ended` screen across whatever the
     * person went off to read: "a person who put the call away asked not to be
     * interrupted by it."
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

    private val mutableStartedByThisAccount = MutableStateFlow(false)

    /**
     * This account started the call in the slot, so `End` offers to end it for
     * everyone (`docs/ux/calls.md` section 4.1.2). Account-level: true on any
     * device of the starter that is in the call. Read when the session comes
     * up, from the record the `GroupCallStart` left, and cleared with the call.
     */
    public val startedByThisAccount: StateFlow<Boolean> = mutableStartedByThisAccount.asStateFlow()

    private class Active(
        val groupIdHex: String,
        val callId: ByteArray,
        val callIdHex: String,
        val ownLeaf: UInt,
        val initiator: Boolean,
        /** Whether this device asked for video when it started or joined. */
        val video: Boolean,
        var joinPublished: Boolean = false,
        var keyedEpoch: ULong?,
        var connected: Boolean = false,
        /** When this device's own media first flowed, or `null` if it never did
         *  ([groupCallTranscriptEntry]). */
        var connectedAtMs: Long? = null,
        /** When this device started or joined the call: the time the Calls
         *  list files it under (`docs/ux/navigation.md` section 6.2). */
        val startedAtMs: Long,
        /** Re-publishes the Join every [GROUP_CALL_HEARTBEAT_MS]; cancelled by [release]. */
        var heartbeat: Job? = null,
        /** This call has had at least one other participant ([groupCallAloneAction]). */
        var sawRemote: Boolean = false,
        /** The alone-end timer (`docs/ux/calls.md` section 4.3); cancelled by [release]. */
        var aloneEnd: Job? = null,
        /** The call's terminating `CallEnd` is already on the journal (its
         *  starter ended it for everyone, here or on another device), so
         *  leaving it publishes no second one. */
        var endedForEveryone: Boolean = false,
        /** Set by [release]: no Join may be published for this call any more. */
        @Volatile var stopped: Boolean = false,
        /** This device left the call as a hang-up (the person's End, or the
         *  call ended for everyone) and not because its session failed. A
         *  start still in flight then reports [GroupCallStartOutcome.Left]. */
        var hungUp: Boolean = false,
    )

    private var active: Active? = null
    private var handle: GroupCallSessionHandle? = null
    private var starting = false

    /**
     * Serialises every Join (first and heartbeat) with the Leave / End of the
     * same device, so the Leave always lands on the journal AFTER the last Join
     * this device published. A Join that has not taken the lock by the time
     * the call is released is skipped ([Active.stopped]); one already holding
     * it finishes first and the Leave waits for it ([Mutex] is fair).
     */
    private val publishLock = Mutex()

    /** Bumped by [reset]: a start that straddles it abandons itself. */
    private var generation = 0L
    private val settleLock = Mutex()

    /** A group call is starting or live. */
    public val occupiesSlot: Boolean
        get() = starting || mutableSlot.value != null

    init {
        // SFU session lifecycle -> the group journal and the leave path.
        scope.launch {
            ui.phase.collect { phase ->
                val a = active ?: return@collect
                val h = handle ?: return@collect
                when (phase) {
                    CallUiState.CallPhase.Connected -> if (!a.joinPublished) {
                        // The row's clock (`docs/ux/calls.md` section 6) starts
                        // when media flows, not when the screen opened.
                        if (a.connectedAtMs == null) a.connectedAtMs = clock()
                        a.joinPublished = true
                        val p = port ?: return@collect
                        a.heartbeat = scope.launch {
                            publishJoin(p, a)
                            // The heartbeat: the same Join, re-published while
                            // this device is in the call, so members who never
                            // see a Leave (this app killed mid-call) stop
                            // counting it once the heartbeats stop.
                            while (active === a) {
                                delay(GROUP_CALL_HEARTBEAT_MS)
                                if (active !== a) break
                                publishJoin(p, a)
                            }
                        }
                    }
                    // The session closed or failed on its own (the user's End
                    // goes through [leave], which releases the handle first).
                    is CallUiState.CallPhase.Ended ->
                        leaveWith(if (h.failed) GroupLeaveReason.ERROR else GroupLeaveReason.HUNG_UP)
                    else -> Unit
                }
            }
        }
        // Mid-call camera-on: the in-call screen has no session, so its camera
        // button only flips the store; mirror it onto the session. A failure
        // reverts the toggle and the call stays up audio-only (invariant #4).
        scope.launch {
            ui.participants
                .map { list -> list.firstOrNull { it.isLocal }?.camOn ?: false }
                .distinctUntilChanged()
                .collect { camOn ->
                    val h = handle ?: return@collect
                    if (camOn) {
                        try {
                            h.publishVideo()
                        } catch (cancel: CancellationException) {
                            throw cancel
                        } catch (error: Throwable) {
                            ui.setCamera(false)
                            warn("group call: mid-call publishVideo failed, camera reverted", error)
                        }
                    } else {
                        h.setCameraEnabled(false)
                    }
                }
        }
        // The microphone button likewise.
        scope.launch {
            ui.participants
                .map { list -> list.firstOrNull { it.isLocal }?.micOn }
                .distinctUntilChanged()
                .collect { micOn -> if (micOn != null) handle?.setMicrophoneEnabled(micOn) }
        }
        // The camera chosen in the Settings sheet (docs/ux/calls.md section 4.1).
        scope.launch {
            ui.devices.camera.collect { camera -> handle?.selectCamera(camera) }
        }
        // Everyone else left: the call ends by itself after GROUP_CALL_ALONE_END_MS
        // as a hang-up, unless someone comes back inside the window
        // (`docs/ux/calls.md` section 4.3). The initiator still waiting for the
        // first joiner is not in this case, which `sawRemote` records.
        scope.launch {
            ui.participants
                .map { list -> list.count { !it.isLocal } }
                .distinctUntilChanged()
                .collect { count ->
                    val a = active ?: return@collect
                    if (count > 0) a.sawRemote = true
                    when (groupCallAloneAction(count, a.sawRemote, a.aloneEnd?.isActive == true)) {
                        GroupCallAloneAction.ARM_END -> {
                            logGroupCallInfo("group call: everyone else left; ending in ${GROUP_CALL_ALONE_END_MS / 1000} s")
                            a.aloneEnd = scope.launch {
                                delay(GROUP_CALL_ALONE_END_MS)
                                if (active === a && ui.participants.value.none { !it.isLocal }) {
                                    logGroupCallInfo("group call: ended because everyone else left")
                                    // Publishes the Leave and, when the record shows
                                    // nobody else fresh, the End; nothing more to add.
                                    leaveWith(GroupLeaveReason.HUNG_UP)
                                }
                            }
                        }
                        GroupCallAloneAction.DISARM -> {
                            a.aloneEnd?.cancel()
                            a.aloneEnd = null
                        }
                        GroupCallAloneAction.NONE -> Unit
                    }
                }
        }
    }

    /**
     * Start (`joinCallIdHex == null`) or join a group call in [groupIdHex].
     * Never throws for a refusal or a failure; the outcome says what happened.
     * The work runs on the host scope, so a caller that goes away mid-start
     * does not leave a half-built call behind.
     */
    public suspend fun start(groupIdHex: String, video: Boolean, joinCallIdHex: String?): GroupCallStartOutcome =
        scope.async { runStart(groupIdHex, video, joinCallIdHex) }.await()

    /** Leave the group call this device is in (publishes Leave / End as owed). */
    public fun leave() {
        leaveWith(GroupLeaveReason.HUNG_UP)
    }

    /**
     * End the group call this device is in for every person in it
     * (`docs/ux/calls.md` section 4.1.2). Only acts for the call's starter.
     *
     * The end is a `CallEnd` with the reason `EndedForAll`, published into the
     * group like any call signal; every device in the call checks that it
     * comes from the call's starter before it leaves. It is handed to the
     * publish queue before this device leaves, so it is on its way while the
     * session goes down, and the Leave this device owes follows it through the
     * same queue. A failed publish is not reported: the starter has left
     * either way, and the others stay until they hang up or the call empties.
     */
    public fun endForEveryone() {
        val a = active ?: return
        val p = port ?: return
        if (!p.startedByThisAccount(a.groupIdHex, a.callIdHex)) {
            // The record no longer names this account the starter (it was
            // replaced by another call's): `End` goes back to leaving.
            mutableStartedByThisAccount.value = false
            return
        }
        if (a.endedForEveryone) return
        a.endedForEveryone = true
        scope.launch {
            publishLock.withLock {
                try {
                    val published = p.publishSignal(
                        a.groupIdHex,
                        FfiCallSignalingBody.End(a.callId, FfiCallEndReason.ENDED_FOR_ALL),
                    )
                    if (!published) warn("group call: the end for everyone could not be published", null)
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (error: Throwable) {
                    warn("group call: the end for everyone could not be published", error)
                }
            }
        }
        leaveWith(GroupLeaveReason.HUNG_UP)
    }

    /**
     * The starter of the group call [callIdHex] in [groupIdHex] ended it for
     * everyone: a `CallEnd{EndedForAll}` whose MLS sender is the starter this
     * device recorded, as the receive path judged it
     * ([groupCallEndedForEveryoneBy]). A device that is in that very call
     * leaves and says who ended it; any other device has only lost the offer
     * to join, which the closed record already took away.
     *
     * Safe from any thread: it only launches onto the host scope.
     */
    public fun onEndedForEveryone(groupIdHex: String, callIdHex: String, starterPersonKey: String) {
        scope.launch {
            val a = active ?: return@launch
            if (a.groupIdHex != groupIdHex || a.callIdHex != callIdHex) return@launch
            a.endedForEveryone = true
            val name = try {
                port?.personName(starterPersonKey)
            } catch (error: Throwable) {
                null
            }
            notifyEndedForEveryone(name)
            leaveWith(GroupLeaveReason.HUNG_UP)
        }
    }

    /**
     * ADR-0087 UX 9.1 — this account is out of [groupIdHex]: a call in it ends,
     * and the line says why. Safe from any thread: it only launches onto the
     * host scope.
     */
    public fun onMembershipEnded(groupIdHex: String) {
        scope.launch {
            val a = active ?: return@launch
            if (!a.groupIdHex.equals(groupIdHex, ignoreCase = true)) return@launch
            // Nothing can be published to a group this device is out of.
            a.endedForEveryone = true
            notifyNoLongerInGroup()
            release()
        }
    }

    /** The in-call screen's exit, after the ended state was shown. */
    public fun onOverlayExit() {
        if (handle != null) leaveWith(GroupLeaveReason.HUNG_UP)
        if (!starting) mutableOverlay.value = false
    }

    /** [groupIdHex]'s MLS work settled. Safe from any thread. */
    public fun onGroupSettled(groupIdHex: String) {
        scope.launch { settle(groupIdHex) }
    }

    /** Sign-out: nothing can be published any more, tear down locally. */
    public fun reset() {
        generation += 1
        release()
        mutableOverlay.value = false
    }

    private suspend fun runStart(groupIdHex: String, video: Boolean, joinCallIdHex: String?): GroupCallStartOutcome {
        val p = port ?: return GroupCallStartOutcome.Failed(UiText("chat.call.unavailable"))
        val other = otherOccupant()
        val occupant = callSlotOccupant(
            oneToOneInCall = other == CallKind.ONE_TO_ONE,
            groupCallActive = occupiesSlot,
            linkCallActive = other == CallKind.LINK,
        )
        val current = mutableSlot.value?.let { it.groupIdHex to it.callIdHex }
        when (val decision = decideGroupCallStart(occupant, current, groupIdHex, joinCallIdHex)) {
            GroupCallStartDecision.Start -> Unit
            GroupCallStartDecision.AlreadyInThisCall -> return GroupCallStartOutcome.AlreadyInThisCall
            is GroupCallStartDecision.Busy -> return GroupCallStartOutcome.Busy(decision.message)
        }
        val initiator = joinCallIdHex == null
        val fallbackKey = if (initiator) "call.error.start_group" else "call.error.join_group"
        // ADR-0091 — a chat this device is behind in: the ordinary
        // could-not-start (could-not-join) outcome, before the slot is taken
        // or anything is published. Nothing says why.
        if (p.behindHeld(groupIdHex)) return GroupCallStartOutcome.Failed(UiText(fallbackKey))
        val callId = if (initiator) {
            ByteArray(CALL_ID_BYTES).also { SecureRandom().nextBytes(it) }
        } else {
            callIdBytesOf(joinCallIdHex!!)?.takeIf { it.size == CALL_ID_BYTES }
                ?: return GroupCallStartOutcome.Failed(UiText(fallbackKey))
        }
        val callIdHex = callIdHexOf(callId)
        val gen = generation
        // What this start built, for the outcome of a start that was left or
        // failed under it (the fields are cleared by then).
        var startedCall: Active? = null
        var startedSession: GroupCallSessionHandle? = null
        // This run holds [starting] until the session exists.
        var holdsStarting = true
        // One call at a time, app-wide, from here on.
        starting = true
        setSlot(GroupCallSlot(groupIdHex, callIdHex, starting = true))
        ui.reset()
        // An auto-connecting call: never flash the prejoin.
        ui.setPhase(CallUiState.CallPhase.Connecting)
        try {
            val base = p.sfuBaseUrl() ?: run {
                warn("group call: SFU URL not configured (lakko.sfu.url), refusing", null)
                return GroupCallStartOutcome.Failed(UiText("chat.call.unavailable"))
            }
            // The anonymous `call-join` token that pays for the call token is
            // drawn with the session, so a call needs one.
            if (p.sessionToken() == null) {
                return GroupCallStartOutcome.Failed(UiText("call.error.signed_out_start"))
            }
            if (!initiator) {
                // Derive the key at the epoch the other participants are at, and
                // see a `CallEnd` already on the journal before joining.
                try {
                    p.syncGroup(groupIdHex)
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (error: Throwable) {
                    warn("group call: pre-join sync failed, joining on the state in hand", error)
                }
                if (!p.isCallLive(groupIdHex, callIdHex, clock())) return GroupCallStartOutcome.Ended
            }
            if (p.isGroupActive(groupIdHex) == false) {
                return GroupCallStartOutcome.Failed(UiText("call.error.not_member"))
            }
            val read = p.readSettledGroup(groupIdHex, callId)
            val seed = read.seed
            val leaf = read.localLeaf
            if (!read.joined || seed == null || leaf == null) {
                // Invariant #4: no key, no call.
                warn("group call: call-key derivation failed, refusing", null)
                return GroupCallStartOutcome.Failed(UiText("call.error.encryption_setup"))
            }
            val minted = p.mintCallJwt(callId)
            if (gen != generation) return GroupCallStartOutcome.Failed(UiText(fallbackKey))
            if (initiator) {
                // After the admission mint (a refused mint announces nothing) and
                // before connecting (members see the call as soon as it exists).
                val announced = p.publishSignal(
                    groupIdHex,
                    FfiCallSignalingBody.GroupStart(
                        callId = callId,
                        initiatorLeaf = leaf.toUInt(),
                        createdAtMs = clock().toULong(),
                        media = FfiCallMediaFlags(audio = true, video = video),
                    ),
                )
                if (!announced) {
                    return GroupCallStartOutcome.Failed(UiText(fallbackKey))
                }
            }
            if (gen != generation) return GroupCallStartOutcome.Failed(UiText(fallbackKey))
            // Set before any further suspension, so a failure from here on
            // reaches the leave path — which publishes the End an initiator owes.
            val a = Active(
                groupIdHex = groupIdHex,
                callId = callId,
                callIdHex = callIdHex,
                ownLeaf = leaf.toUInt(),
                initiator = initiator,
                video = video,
                keyedEpoch = read.epoch,
                startedAtMs = clock(),
            )
            active = a
            startedCall = a
            val h = p.createSession(
                GroupCallSessionRequest(
                    groupIdHex = groupIdHex,
                    callIdHex = callIdHex,
                    sfuBaseUrl = base,
                    jwt = minted.jwt,
                    seed = seed,
                    localLeaf = leaf,
                    video = video,
                    turn = minted.turn,
                ),
                ui,
            )
            handle = h
            startedSession = h
            // Whether `End` offers to end the call for everyone: the record
            // names its starter from the `GroupCallStart` (this device's own,
            // folded in as it was published, or the one it joined on).
            mutableStartedByThisAccount.value = p.startedByThisAccount(groupIdHex, callIdHex)
            // A camera chosen in the Settings sheet before the session existed.
            ui.devices.camera.value?.let { h.selectCamera(it) }
            starting = false
            holdsStarting = false
            setSlot(GroupCallSlot(groupIdHex, callIdHex, starting = false))
            mutableOverlay.value = true
            // A call is born expanded (see [collapsed]): cleared here, in the
            // same breath as the screen being asked for, so the previous call's
            // put-away can never suppress this one's screen.
            mutableCollapsed.value = false
            h.connect()
            // Left, ended or failed while connecting: the session is already
            // released. Which of the three decides what the person is told.
            if (active !== a) return abandonedStartOutcome(a, h, groupIdHex, callId, fallbackKey)
            a.connected = true
            // A drain that arrived while connecting found no peer connection to
            // rekey; catch up now that the session can take a new key.
            settle(groupIdHex)
            return GroupCallStartOutcome.Connected
        } catch (cancel: CancellationException) {
            if (active == null || active === startedCall) release()
            throw cancel
        } catch (error: Throwable) {
            // Only this start's own call is left below: a start that was left
            // while connecting can throw after the next call took the slot.
            val ownCallActive = active != null && active === startedCall
            // The person pressed End while the session was connecting and the
            // connect threw under the teardown: not a failure to report.
            if (startedCall?.hungUp == true) return GroupCallStartOutcome.Left
            val endedByServer = startedSession?.callEndedByServer == true
            if (endedByServer) {
                // The SFU says the call ended before this device arrived
                // (`docs/ux/calls.md` section 4.3): close the chat's record
                // for everyone rather than report a failure.
                warn("group call: the SFU says the call ended before we joined", error)
                if (ownCallActive) leaveWith(GroupLeaveReason.ERROR)
                endRecordReportedBySfu(groupIdHex, callId)
                return GroupCallStartOutcome.Ended
            }
            warn("group call: connect failed", error)
            if (ownCallActive) leaveWith(GroupLeaveReason.ERROR)
            return GroupCallStartOutcome.Failed(callErrorText(error, fallbackKey))
        } finally {
            // A start that was left while connecting can finish after the next
            // one began: it gave [starting] up when its session came up, and
            // the slot and the call screen are the newer call's by now.
            if (holdsStarting) starting = false
            if (active == null && handle == null) {
                val slotCallIdHex = mutableSlot.value?.callIdHex
                if (slotCallIdHex == callIdHex) setSlot(null)
                if (slotCallIdHex == null || slotCallIdHex == callIdHex) {
                    ui.setPhase(CallUiState.CallPhase.Ended(clock()))
                }
            }
        }
    }

    /**
     * What a start came to when its call was taken out of the slot while the
     * session was connecting. The person's own End says nothing; the SFU
     * saying the call was over closes the chat's record, as when the connect
     * throws for it; anything else is the failure it was before.
     */
    private fun abandonedStartOutcome(
        a: Active,
        h: GroupCallSessionHandle,
        groupIdHex: String,
        callId: ByteArray,
        fallbackKey: String,
    ): GroupCallStartOutcome = when {
        a.hungUp -> GroupCallStartOutcome.Left
        h.callEndedByServer -> {
            endRecordReportedBySfu(groupIdHex, callId)
            GroupCallStartOutcome.Ended
        }
        else -> GroupCallStartOutcome.Failed(UiText(fallbackKey))
    }

    private suspend fun settle(groupIdHex: String) {
        settleLock.withLock {
            val a = active ?: return
            val h = handle ?: return
            if (a.groupIdHex != groupIdHex || !a.connected) return
            val p = port ?: return
            val read = try {
                p.readSettledGroup(groupIdHex, a.callId)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (error: Throwable) {
                warn("group call: settled read failed, keeping the current key", error)
                return
            }
            if (active !== a) return
            when (
                decideGroupCallEpochChange(
                    callGroupIdHex = a.groupIdHex,
                    settledGroupIdHex = groupIdHex,
                    groupJoined = read.joined,
                    isActive = read.isActive,
                    settled = read.settled,
                    keyedEpoch = a.keyedEpoch,
                    currentEpoch = read.epoch,
                )
            ) {
                GroupCallEpochAction.NONE -> Unit
                // Nothing can be published to a group this device has left.
                GroupCallEpochAction.END_LOCALLY -> release()
                GroupCallEpochAction.REKEY -> {
                    // Fail closed: a re-derivation failure keeps the current key.
                    val seed = read.seed ?: return
                    h.rekey(seed)
                    a.keyedEpoch = read.epoch
                }
            }
        }
    }

    /** One Join (the first or a heartbeat), under [publishLock]. NonCancellable:
     *  [release] cancels the heartbeat, and a publish already under way
     *  completes rather than being cut between the ratchet advance and the DS
     *  publish — the Leave then waits for it. */
    private suspend fun publishJoin(p: GroupCallPort, a: Active) {
        try {
            withContext(NonCancellable) {
                publishLock.withLock {
                    if (a.stopped) return@withLock
                    p.publishSignal(
                        a.groupIdHex,
                        FfiCallSignalingBody.GroupJoin(
                            callId = a.callId,
                            leafIndex = a.ownLeaf,
                            joinedAtMs = clock().toULong(),
                        ),
                    )
                }
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (error: Throwable) {
            warn("group call: Join publish failed", error)
        }
    }

    /**
     * Ask the SFU whether a call this device is offering to join is still
     * there, and close its record when it is not (`SfuCallState.kt`). Never
     * throws; a probe that cannot be answered changes nothing. The call this
     * device is in needs no probe: its own socket is the proof.
     */
    public suspend fun probeCallState(groupIdHex: String, callIdHex: String) {
        val p = port ?: return
        if (active?.callIdHex == callIdHex) return
        val base = p.sfuBaseUrl() ?: return
        // ADR-0078 section 2: the probe names the room (a one-way function of
        // the call id) and carries no token, so no admission token is minted
        // to ask and the issuer never sees who probes.
        val probedCallId = unhex(callIdHex) ?: return
        val room = p.sfuRoom(probedCallId) ?: return
        val state = try {
            p.fetchSfuCallState(sfuCallStateUrl(base, room))?.let(::parseSfuCallState)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            null
        }
        if (state != SfuCallState.ENDED) return
        // Still not in it after the awaits?
        if (active?.callIdHex == callIdHex) return
        val callId = unhex(callIdHex) ?: return
        endRecordReportedBySfu(groupIdHex, callId)
    }

    /** Close a group call's record on this device and for the chat: the SFU
     *  said the call ended. Any member may say so — the End signal carries
     *  only the call id, and a record that is over on the server is over for
     *  everyone. */
    private fun endRecordReportedBySfu(groupIdHex: String, callId: ByteArray) {
        val p = port ?: return
        // `publishSignal` folds the End into this device's own record as it
        // goes out, the way every other signal this host publishes is.
        scope.launch {
            publishLock.withLock {
                try {
                    p.publishSignal(groupIdHex, FfiCallSignalingBody.End(callId, FfiCallEndReason.HUNG_UP))
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (error: Throwable) {
                    warn("group call: could not publish the End the SFU reported", error)
                }
            }
        }
    }

    /** Leave the call and tell the group what [planGroupCallLeave] decides. */
    private fun leaveWith(reason: GroupLeaveReason) {
        val p = port
        val a = active ?: run {
            release()
            return
        }
        val failed = handle?.failed == true
        a.hungUp = reason == GroupLeaveReason.HUNG_UP && !failed
        // Read before the session is released: the call ended when the link
        // was last known alive, not when this device noticed.
        val lastInbound = handle?.lastInboundAtMs ?: 0L
        val otherFresh = p?.recordOtherFreshMembers(a.groupIdHex, a.callIdHex, clock())
        release()
        val endedAtMs = callEndedAtMs(clock(), lastInbound)
        groupCallTranscriptEntry(a.initiator, a.video, a.connectedAtMs, endedAtMs)?.let { entry ->
            p?.recordCall(a.groupIdHex, entry, a.startedAtMs, a.callIdHex)
        }
        val plan = planGroupCallLeave(
            reason = reason,
            sessionFailed = failed,
            joinPublished = a.joinPublished,
            initiator = a.initiator,
            recordOtherFreshMembers = otherFresh,
        )
        // The starter's end for everyone IS the call's terminating End.
        val publishEnd = plan.publishEnd && !a.endedForEveryone
        if (p == null || (!plan.publishLeave && !publishEnd)) return
        val ffiReason = when (plan.leaveReason) {
            GroupLeaveReason.HUNG_UP -> FfiCallEndReason.HUNG_UP
            GroupLeaveReason.ERROR -> FfiCallEndReason.ERROR
        }
        scope.launch {
            // After any Join of this call still being published (see [publishLock]).
            publishLock.withLock {
                if (plan.publishLeave) {
                    p.publishSignal(a.groupIdHex, FfiCallSignalingBody.GroupLeave(a.callId, a.ownLeaf, ffiReason))
                }
                if (publishEnd) {
                    p.publishSignal(a.groupIdHex, FfiCallSignalingBody.End(a.callId, ffiReason))
                }
            }
        }
    }

    /** Take the call out of the slot and tear the session down. Synchronous and
     *  idempotent, so the several paths that can end a call act once. */
    private fun release() {
        val h = handle
        active?.let {
            it.stopped = true
            it.heartbeat?.cancel()
            it.aloneEnd?.cancel()
            it.aloneEnd = null
        }
        active = null
        mutableStartedByThisAccount.value = false
        // Cleared BEFORE `hangUp`: the session's closed transition moves the UI
        // to Ended, and the phase observer must find no handle left.
        handle = null
        h?.hangUp()
        if (!starting) setSlot(null)
    }

    private fun setSlot(next: GroupCallSlot?) {
        mutableSlot.value = next
        onSlotChanged(next != null || starting)
    }

    private companion object {
        const val CALL_ID_BYTES = 16
    }
}

private fun logGroupCallWarning(message: String, error: Throwable?) {
    try {
        android.util.Log.w("GroupCallHost", message, error)
    } catch (_: Throwable) {
        // No Android logger (host unit tests).
    }
}

private fun logGroupCallInfo(message: String) {
    try {
        android.util.Log.i("GroupCallHost", message)
    } catch (_: Throwable) {
        // No Android logger (host unit tests).
    }
}

/** The 16 bytes of a lower-case hex call id, or `null` for anything else. */
private fun unhex(hex: String): ByteArray? {
    if (hex.length != 32 || !hex.all { it in '0'..'9' || it in 'a'..'f' }) return null
    return ByteArray(16) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
}
