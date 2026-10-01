// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import me.lakko.app.chat.GroupPollOutcome
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.chat.AttachmentMediaMeta
import me.lakko.app.chat.UploadStage
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.calls.CallKind
import me.lakko.app.calls.GroupCallStartOutcome
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.GroupMember
import uniffi.lakko_ffi_uniffi.OutgoingOwner
import uniffi.lakko_ffi_uniffi.Role
import uniffi.lakko_ffi_uniffi.membershipRefusalRetryMaySucceed
import uniffi.lakko_ffi_uniffi.RoleTable

/**
 * State + dispatcher for the per-thread [ChatScreen].
 *
 * Combines the [ChatRepository.state]-side thread (looked up by
 * [groupIdHex]) with VM-local composer state (the current
 * draft input + the send-busy / send-error flags) into a
 * single [ChatScreenUiState] flow.
 *
 * Manual refresh only — the polling loop lands in 1.15e7 (the
 * same sub-sprint that wires background polling for the
 * contacts inbox).
 */
public class ChatViewModel(
    private val repo: ChatRepository,
    public val groupIdHex: String,
    /** SFU group-call admission needs the account's session bearer to
     *  mint a call JWT. Threaded in from `MainActivity` (where the
     *  `AuthRepository` lives) as a closure so the VM has no auth-layer
     *  dependency. Returns `null` when signed out — `startGroupCall`
     *  then refuses with a clear log line. Defaults to `{ null }` so
     *  existing call sites + tests compile unchanged. */
    private val sessionTokenProvider: () -> String? = { null },
    /** Resolves `@username -> (accountId, signaturePubkey)` through the
     *  G-4-verified contacts path for the add-member flow (A4). Threaded
     *  from `MainActivity` as `{ contactsRepository.resolveVerifiedRecipient(it) }`.
     *  Defaults to a fail-closed stub so existing call sites + tests compile
     *  unchanged (add-member then refuses). */
    private val resolveRecipient: suspend (String) -> Pair<ByteArray, ByteArray> = {
        throw IllegalStateException("no @username resolver wired — add-member unavailable")
    },
    /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String =
        { uniffi.lakko_ffi_uniffi.validateUsername(it) },
    /** The process-wide 1x1 call host ([me.lakko.app.calls.AppCallHost]). The
     *  1x1 call does not live in this view model any more: it must keep ringing
     *  and keep running when this chat is not on screen. `null` in tests — the
     *  1x1 call button then does nothing. */
    private val callHost: me.lakko.app.calls.OneToOneCallHost<*>? = null,
    /** The process-wide group-call host ([me.lakko.app.calls.AppCallHost.group]).
     *  The group call does not live in this view model either: it keeps
     *  running, and keeps its key current, when this chat is not on screen.
     *  `null` in tests — the group call buttons then report that calls are
     *  unavailable. */
    private val groupCallHost: me.lakko.app.calls.GroupCallHost? = null,
    /** The call occupying the app's one call slot
     *  ([me.lakko.app.calls.AppCallHost.occupant]); `null` in tests. */
    callOccupant: StateFlow<CallKind?>? = null,
    /** Ends whatever call occupies the slot (the busy notice's "End current
     *  call"). */
    private val endCurrentCall: () -> Unit = {},
) : ViewModel() {

    private val composer: MutableStateFlow<ComposerState> =
        MutableStateFlow(ComposerState())

    // -------------------------------------------------------------
    // `docs/ux/message-actions.md` section 8 — Select (bulk Delete / Forward)
    // -------------------------------------------------------------

    private val mutableSelection: MutableStateFlow<me.lakko.app.chat.MessageSelection?> =
        MutableStateFlow(null)

    /**
     * The live selection while the screen is in selection mode, `null`
     * otherwise. Every rule of the mode is in `MessageSelection.kt`; this
     * holds the value so a rotation keeps it and so the source conversation
     * still has it when the picker on top of it hands back an outcome.
     * Nothing about it is persisted (section 8, "Leaving").
     */
    public val selection: StateFlow<me.lakko.app.chat.MessageSelection?> = mutableSelection.asStateFlow()

    private val mutableBulkBusy = MutableStateFlow(false)

    /** `true` while a bulk delete is running; the action bar waits. */
    public val bulkBusy: StateFlow<Boolean> = mutableBulkBusy.asStateFlow()

    private val mutableBulkStatus = MutableStateFlow<UiText?>(null)

    /**
     * "Deleted K of N." / "Forwarded K of N." after a run stopped short,
     * shown under the action bar; `null` otherwise. Cleared by the next
     * action and by leaving the mode.
     */
    public val bulkStatus: StateFlow<UiText?> = mutableBulkStatus.asStateFlow()

    init {
        // Section 8 — the picker leaves a selection's forward outcome in the
        // slot. It is read HERE rather than by the screen because the source
        // screen is not composed while the picker (or the target chat) is on
        // top of it, and this view model is: it lives with the back-stack
        // entry, so the selection is cleared or narrowed at once.
        viewModelScope.launch {
            me.lakko.app.chat.PendingForwardOutcome.pending.collect { outcome ->
                if (outcome != null && outcome.sourceGroupIdHex == groupIdHex) {
                    me.lakko.app.chat.PendingForwardOutcome.take(groupIdHex)?.let(::applyForwardOutcome)
                }
            }
        }
    }

    /** `Select` was chosen on [message]: enter the mode with it selected. */
    public fun onEnterSelection(message: me.lakko.app.chat.ChatMessage) {
        if (!me.lakko.app.chat.canSelectMessage(message)) return
        if (mutableBulkBusy.value) return
        mutableBulkStatus.value = null
        mutableSelection.value = me.lakko.app.chat.MessageSelection.starting(
            messageId = message.messageId,
            isOwn = message.direction == me.lakko.app.chat.Direction.Outgoing,
        )
    }

    /** A tap or long press on a row while selecting. A row that cannot be
     *  acted on is not selectable and the tap does nothing. */
    public fun onToggleSelection(message: me.lakko.app.chat.ChatMessage) {
        val current = mutableSelection.value ?: return
        if (mutableBulkBusy.value) return
        if (!me.lakko.app.chat.canSelectMessage(message)) return
        mutableSelection.value = current.toggle(
            messageId = message.messageId,
            isOwn = message.direction == me.lakko.app.chat.Direction.Outgoing,
        )
    }

    /** `Cancel`, Back, or navigating away: leave the mode and clear it. A run
     *  in flight finishes first — its sends have already left. */
    public fun onCancelSelection() {
        if (mutableBulkBusy.value) return
        mutableSelection.value = null
        mutableBulkStatus.value = null
    }

    /** The selected messages, oldest first — the order both actions run in.
     *  The screen turns these into forward requests, because the author
     *  label a request carries is the timeline's to resolve. */
    public fun selectedMessagesInOrder(): List<me.lakko.app.chat.ChatMessage> {
        val current = mutableSelection.value ?: return emptyList()
        return me.lakko.app.chat.selectedInTimelineOrder(current, state.value.messages)
    }

    /**
     * Section 8's Delete, after the confirmation: one section-3 delete per
     * selected message, oldest first, each applied locally as the single
     * delete is. Stops at the first failure, keeps the unsent selected and
     * says "Deleted K of N."; full success leaves the mode.
     */
    public fun onDeleteSelected() {
        val current = mutableSelection.value ?: return
        if (!current.deleteEnabled) return
        if (!mutableBulkBusy.compareAndSet(expect = false, update = true)) return
        mutableBulkStatus.value = null
        val ordered = me.lakko.app.chat.selectedInTimelineOrder(current, state.value.messages)
        viewModelScope.launch {
            val result = try {
                me.lakko.app.chat.performInOrder(ordered) { message ->
                    repo.deleteMessage(groupIdHex = groupIdHex, targetMessageId = message.messageId)
                }
            } finally {
                mutableBulkBusy.value = false
            }
            if (result.complete) {
                mutableSelection.value = null
                mutableBulkStatus.value = null
            } else {
                mutableSelection.value = mutableSelection.value?.retain(result.unsent.map { it.messageId })
                mutableBulkStatus.value = me.lakko.app.chat.progressLabel(
                    me.lakko.app.chat.BulkAction.DELETE,
                    result.done,
                    result.total,
                )
                android.util.Log.w("ChatViewModel", "onDeleteSelected stopped short", result.failure)
            }
        }
    }

    private fun applyForwardOutcome(outcome: me.lakko.app.chat.ForwardOutcome) {
        val current = mutableSelection.value ?: return
        if (outcome.complete) {
            mutableSelection.value = null
            mutableBulkStatus.value = null
        } else {
            mutableSelection.value = current.retain(outcome.unsentMessageIds)
            mutableBulkStatus.value = me.lakko.app.chat.progressLabel(
                me.lakko.app.chat.BulkAction.FORWARD,
                outcome.done,
                outcome.total,
            )
        }
    }

    // -------------------------------------------------------------
    // ADR-0052 / `docs/ux/message-actions.md` section 3.1 — Reply
    // -------------------------------------------------------------

    private val mutableReplyingTo: MutableStateFlow<me.lakko.app.chat.ReplyArmed?> =
        MutableStateFlow(null)

    /**
     * The reply the composer has armed, or `null`. The strip above the
     * composer renders it; the next send carries its snapshot. Section 3.1's
     * exclusivity rules live in the handlers below: choosing Reply on another
     * message replaces it, starting an edit drops it, the close control, a
     * send of any kind and leaving the conversation clear it.
     */
    public val replyingTo: StateFlow<me.lakko.app.chat.ReplyArmed?> = mutableReplyingTo.asStateFlow()

    /** Counts arms, so arming the same message twice still refocuses the field. */
    private var replyArmSeq: Long = 0

    /**
     * Reply was chosen on [message]. [authorLabel] is what the timeline calls
     * its author ("You" for an own message); the screen resolves it because the
     * timeline is where that resolution lives. The quote is snapshotted HERE
     * and never re-read (section 3.1). A row with no DS id has nothing to point
     * at and is ignored — the menu does not offer Reply on one.
     */
    public fun onReplyToMessage(message: me.lakko.app.chat.ChatMessage, authorLabel: String) {
        if (message.messageId == 0L) return
        replyArmSeq += 1
        mutableReplyingTo.value = me.lakko.app.chat.ReplyArmed(
            messageId = message.messageId,
            snapshot = me.lakko.app.chat.replySnapshotFor(message, authorLabel),
            armedSeq = replyArmSeq,
        )
    }

    /** The strip's close control, and leaving the conversation. */
    public fun onCancelReply() {
        mutableReplyingTo.value = null
    }

    /** Section 3.1 — choosing Edit while a reply is armed drops the reply. */
    public fun onEditStarted() {
        mutableReplyingTo.value = null
    }

    /**
     * Takes the armed reply for a send: returns its snapshot and clears the
     * strip in the same step, so the message that goes out is the only one
     * that carries it.
     */
    private fun takeArmedReply(): me.lakko.app.chat.ReplyRef? =
        mutableReplyingTo.getAndUpdate { null }?.snapshot

    /**
     * ADR-0048 — the one name resolver. Declared ABOVE [derived]: the first
     * `buildState` runs in that initialiser and can already resolve a typing
     * member's name, so this must be assigned first.
     */
    private val profileNames: me.lakko.app.profile.ProfileDirectory =
        me.lakko.app.profile.ProfileDirectory.get(repo.context)

    /** Bumped whenever a resolved name may have changed; the screen reads it
     *  so author labels, the header and the roster update in place. */
    public val nameVersion: StateFlow<Long> = profileNames.version

    // -------------------------------------------------------------
    // ADR-0045 UX §6.2 — the end of your own membership
    //
    // Every rule and every string lives in `me.lakko.app.chat.MembershipEnded`;
    // what is here is the wiring that feeds it live group state, the gates that
    // consult it, and the driver for the one-shot teardown it decides.
    // -------------------------------------------------------------

    /**
     * §6.2.3 — the durable `leaveRequestedAt` stamp, cached.
     *
     * Only THIS device ever writes it (at leave-publish), so a read at
     * construction plus a re-read after [onLeaveGroup] is complete — there is
     * no third party that can make it appear. Cached rather than re-read per
     * emission because it is a `SharedPreferences` hit on a path that runs on
     * every repository update.
     */
    private var leaveRequestedAtMs: Long? = repo.leaveRequestedAt(groupIdHex)

    private val mutableMembership: MutableStateFlow<me.lakko.app.chat.MembershipState> =
        MutableStateFlow(
            me.lakko.app.chat.membershipState(
                isActive = repo.isGroupActive(groupIdHex),
                leaveRequestedAtMs = leaveRequestedAtMs,
            ),
        )

    /**
     * §6.2 — is this device still a member, and (if not) did it ask to go?
     *
     * `internal` because [me.lakko.app.chat.MembershipState] is, which is the
     * same posture the sibling rule modules take: the RULES are module-private
     * and the composable in this module renders them.
     *
     * The state is TERMINAL and survives a restart, so this answers the same at
     * launch as it did in the batch that carried the removal — there is no
     * transient window to debounce and nothing to re-check later.
     */
    internal val membership: StateFlow<me.lakko.app.chat.MembershipState> =
        mutableMembership.asStateFlow()

    /** §6.2 — the standing gate. Public because the screen's poll loop and its
     *  header consult it; the narrowing (left vs removed) stays in
     *  [membership]. */
    public val membershipEnded: StateFlow<Boolean> =
        mutableMembership
            .map { me.lakko.app.chat.membershipEnded(it) }
            .stateIn(
                scope = viewModelScope,
                started = SharingStarted.Eagerly,
                initialValue = me.lakko.app.chat.membershipEnded(mutableMembership.value),
            )

    private val mutableBeaconTickerRunning: MutableStateFlow<Boolean> =
        MutableStateFlow(true)

    /**
     * §6.2.8 item 2 — the beacon ticker's own run flag.
     *
     * The ticker lives in the composable (it is `repeatOnLifecycle`-scoped,
     * which IS this platform's foreground gate), so the teardown cannot cancel
     * a job it does not own. It flips this instead and the ticker breaks out.
     * The emit path is gated independently
     * ([me.lakko.app.chat.BeaconEmitVerdict.MEMBERSHIP_ENDED]), so this stops
     * the wasted wake while the gate is what makes a surviving ticker harmless.
     */
    public val beaconTickerRunning: StateFlow<Boolean> =
        mutableBeaconTickerRunning.asStateFlow()

    /**
     * §6.2.8 — claims each one-shot step BEFORE its first suspension point.
     *
     * See [me.lakko.app.chat.EndedTeardownLatch] for why the claim needs a real
     * lock on this platform: the retire suspends into `Dispatchers.IO` and the
     * collector below can be re-entered while it is in flight, so "exactly
     * once" has to be decided synchronously rather than on completion.
     */
    private val teardownLatch = me.lakko.app.chat.EndedTeardownLatch()

    private val derived: MutableStateFlow<ChatScreenUiState> =
        MutableStateFlow(buildState(repo.state.value, composer.value, typingText = null))

    /**
     * The typing line last computed off the main thread by the repository and
     * profile collectors. The composer collector runs on every keystroke and
     * reuses it rather than reading the live roster (a Rust call that waits on
     * the group's mutex) on the main thread.
     */
    private var lastTypingText: UiText? = null

    public val state: StateFlow<ChatScreenUiState> = derived.asStateFlow()

    // -------------------------------------------------------------
    // Phase 3a10 — call surface
    // -------------------------------------------------------------

    /**
     * The app-wide 1x1 call slot (ANY chat's call), or `null`. The call itself,
     * its ringing banner and its in-call screen are app-level
     * (`OneToOneCallOverlay` in `MainActivity`); this chat reads the slot only
     * to hide its call buttons and keep media capture out of the way while a
     * call owns the microphone.
     */
    public val oneToOneCall: StateFlow<me.lakko.app.calls.OneToOneCall<*>?> =
        callHost?.active ?: MutableStateFlow(null)

    /**
     * The thread-state collectors. Started ONLY from the `init` block at the
     * very end of this class — see the note there for why the call site's
     * position is load-bearing and must not move back up here.
     */
    private fun startThreadCollectors() {
        // ADR-0042 — chat-open push-routing provisioning (mirror of the web
        // Chat.tsx onMount `provisionPushRouting`): owner + no secret ->
        // mint + persist + distribute + subscribe under the opaque
        // `push_gid`; member with a secret -> subscribe; else defer.
        // Idempotent + race-safe; fire-and-forget inside the repository.
        repo.ensurePushRouting(groupIdHex)
        // ADR-0048 trigger 4 — chat-open profile request: if some member still
        // reads `Member` on this device, ask the group to introduce itself (at
        // most once a day per group; the core's rule decides). Fire-and-forget
        // inside the repository, like the provisioning above.
        repo.requestProfilesIfNeeded(groupIdHex)
        // Manually combine two StateFlows — kotlinx-coroutines
        // has no synchronous `combine` for two StateFlows;
        // two parallel collectors keep the derived flow hot
        // for the ViewModel's lifetime.
        // ADR-0051 — a name accepted by the poll bumps the store version; the
        // members panel re-titles in place without being reopened.
        viewModelScope.launch {
            me.lakko.app.chat.GroupNameStore.version.collect { refreshGroupName() }
        }
        // ADR-0052 decision 3 — a pin accepted by the poll, or set here, bumps
        // the store version; the bar re-derives in place.
        viewModelScope.launch {
            me.lakko.app.chat.PinStore.version.collect { refreshPins() }
        }
        viewModelScope.launch {
            repo.state.collect { repoState ->
                // The live roster reads below are synchronous Rust calls that
                // take the group's mutex, which a poll or sweep on the MLS
                // dispatcher may be holding: never on the main thread.
                val live = withContext(kotlinx.coroutines.Dispatchers.Default) {
                    LiveGroupRead(
                        typingText = typingTextFor(repoState),
                        otherIdentities = otherChatIdentities(),
                        isActive = repo.isGroupActive(groupIdHex),
                    )
                }
                lastTypingText = live.typingText
                derived.value = buildState(repoState, composer.value, live.typingText)
                mutableOtherMemberIdentities.value = live.otherIdentities
                // ADR-0052 decision 3 — the local half of the shared-pin
                // standing, re-read with the roster (a role change or a
                // membership change moves it). Same off-main-thread rule.
                mutablePinAuthority.value = withContext(kotlinx.coroutines.Dispatchers.Default) {
                    repo.mayPinForEveryone(groupIdHex)
                }
                // ADR-0045 UX §6.2 — re-read the core's own membership
                // predicate after every repository emission. Nothing in the
                // receive path announces the transition
                // (`processIncomingVerifyingRoles` returns the same
                // content-less `Commit` for a removing commit as for any
                // other), so this is the only place it can be noticed.
                applyMembership(live.isActive)
                // ADR-0045 UX §7 — a repository emission follows every poll,
                // and a poll is how this device learns that somebody ELSE's
                // sweep drained the queue. `refreshMembers` covers the local
                // half; without this one a request refused here would wait for
                // the panel to be reopened.
                evaluateArmedRetries()
            }
        }
        // ADR-0045 UX §6.2.8 — the one-shot half of the teardown. Driven off
        // the membership flow rather than off the repository's, so it runs once
        // on the transition; the latch makes a re-run harmless either way.
        viewModelScope.launch {
            mutableMembership.collect { state ->
                me.lakko.app.chat.runEndedTeardown(
                    state = state,
                    latch = teardownLatch,
                    stopBeaconTicker = { mutableBeaconTickerRunning.value = false },
                    retirePushSubscription = {
                        repo.retirePushSubscriptionAfterEviction(groupIdHex)
                    },
                )
            }
        }
        viewModelScope.launch {
            composer.collect { composerState ->
                derived.value = buildState(repo.state.value, composerState, lastTypingText)
            }
        }
        // ADR-0048 — a profile arriving or a username verifying changes the
        // typing line's name; rebuild so it updates in place.
        viewModelScope.launch {
            profileNames.version.collect {
                val repoState = repo.state.value
                val typing = withContext(kotlinx.coroutines.Dispatchers.Default) { typingTextFor(repoState) }
                lastTypingText = typing
                derived.value = buildState(repoState, composer.value, typing)
            }
        }
        // ADR-0024 — read-receipt SEND. Each time the thread's
        // highest received messageId advances past our own
        // last-published watermark, publish a throttled
        // `read_up_to` so peers learn we have read up to it.
        // Distinct-on-the-highest-id so a no-op repo emission
        // (e.g. a typing fold) doesn't re-trigger.
        viewModelScope.launch {
            repo.state
                .distinctUntilChanged { a, b ->
                    highestMessageIdFor(a) == highestMessageIdFor(b)
                }
                .collect { repoState ->
                    scheduleReadReceipt(highestMessageIdFor(repoState))
                }
        }
        // The group call — its mid-call rekey (G-18 / ADR-0029), its camera and
        // microphone, and the 1x1 host's busy flag — lives in the app-level
        // `GroupCallHost` now, so none of it stops when this chat does.
        // Incoming 1x1 call signaling is NOT attached here any more: the
        // repository hands every one-to-one body, from every poll of every
        // group, to the app-level call host.
    }

    /** The chat screen's own long-poll loop started; see [ChatRepository.claimScreenPoll]. */
    public fun claimScreenPoll() {
        repo.claimScreenPoll(groupIdHex)
    }

    /** The chat screen's own long-poll loop ended. */
    public fun releaseScreenPoll() {
        repo.releaseScreenPoll(groupIdHex)
    }

    /** The chat screen became visible (resumed) or stopped being visible. */
    public fun setOnScreen(onScreen: Boolean) {
        repo.setThreadOnScreen(groupIdHex, onScreen)
    }

    /**
     * Answer the incoming call in the slot (`docs/ux/calls.md` section 2.1.1).
     *
     * The quiet panel this chat draws for a MUTED conversation's call is a
     * second surface for the same call, not a second call: it drives the very
     * entry point the full-screen ring screen drives, so there is one accept
     * path on this client and a change to what accepting means cannot reach one
     * surface and miss the other.
     */
    public fun acceptIncomingCall(withVideo: Boolean) {
        callHost?.accept(withVideo = withVideo)
    }

    /** Refuse the incoming call in the slot; the caller is told, as ever. */
    public fun declineIncomingCall() {
        callHost?.decline()
    }


    /**
     * Tap-to-call from the chat header.
     *
     * `docs/ux/calls.md` section 3 — [video] says which header button was
     * pressed, and the host turns that into the surface that opens: a voice
     * call is DIALLED here and now, and only a video call on a device with a
     * camera stops at the camera check. Before this both buttons prepared a
     * session and left it sitting undialled behind a settings card titled "Join
     * call", which section 3.1 removed outright.
     *
     * A video button pressed on a device with no camera places a voice call and
     * says so in one line, rather than opening a check with nothing in it.
     */
    public fun startCall(audio: Boolean = true, video: Boolean = false) {
        // `audio` has no say: a call is always dialled with an audio section,
        // because the call screen's Mute button must work without a
        // renegotiation. See `immediateDialMedia` / `offeredCallMedia`.
        @Suppress("UNUSED_PARAMETER") val ignoredAudio = audio
        val host = callHost ?: return
        val kind = me.lakko.app.calls.requestedCallKind(video)
        val hasCamera = me.lakko.app.calls.CameraVideoCapture.hasUsableCamera(repo.context)
        // The host builds the session, arms the SFrame seed from the MLS
        // exporter (G3-2-call-key) and dials unless a camera check is due; the
        // app-level overlay presents whichever of the two follows. Refused
        // while any other call is up — one call at a time.
        if (!host.startOutgoing(groupIdHex, kind, hasCamera = hasCamera)) {
            showBusyNotice(me.lakko.app.calls.CallVerb.START)
            return
        }
        me.lakko.app.calls.callEntryNotice(kind, hasCamera)?.let(::showCallNotice)
    }

    // -------------------------------------------------------------
    // Calls — the one call slot
    //
    // Every call (1x1, group, call link) lives in the app-level host
    // (`me.lakko.app.calls.AppCallHost`) and is drawn from the activity root, so
    // leaving this chat neither hides nor ends it. This chat reads the slot to
    // hide its call buttons, asks the host to start or join, and projects the
    // group-call ledger into its Join banner.
    // -------------------------------------------------------------

    /**
     * The call occupying the app's one call slot — any chat's 1x1 call, a group
     * call anywhere, or a call-link call — or `null` when it is free. While it
     * is set the header's call buttons are hidden: returning to the chat of a
     * live call shows that call (the host's in-call screen), never an offer to
     * start a second one.
     */
    public val callOccupant: StateFlow<CallKind?> = callOccupant ?: MutableStateFlow(null)

    private val mutableJoinableGroupCall: MutableStateFlow<JoinableGroupCall?> =
        MutableStateFlow(null)

    /**
     * A live group call on this chat that this device is not in, or `null`.
     * Drives the "Group call in progress" banner, and makes the header call
     * buttons join that call rather than start a second one.
     */
    public val joinableGroupCall: StateFlow<JoinableGroupCall?> =
        mutableJoinableGroupCall.asStateFlow()

    /**
     * The group-call collectors. Started ONLY from the `init` block at the very
     * end of this class, for the same reason as [startThreadCollectors].
     */
    private fun startGroupCallCollectors() {
        // Project the ledger into the banner. Liveness decays with time alone
        // (both windows only close), so while a live record is shown the
        // projection is re-taken on a slow tick and stops ticking once the call
        // is no longer live; any new signal re-emits the record and restarts it.
        viewModelScope.launch {
            combine(
                repo.groupCalls.map { it[groupIdHex] }.distinctUntilChanged(),
                this@ChatViewModel.callOccupant,
                groupCallHost?.slot ?: MutableStateFlow(null),
            ) { record, occupant, groupSlot -> Triple(record, occupant, groupSlot) }
                .collectLatest { (record, occupant, groupSlot) ->
                    while (true) {
                        val now = System.currentTimeMillis()
                        val ownDeviceKey = repo.ownGroupCallMemberKey()
                        val joinable = me.lakko.app.calls.joinableGroupCall(
                            record = record,
                            nowMs = now,
                            anyCallActive = occupant != null,
                            activeGroupCallIdHex = groupSlot?.callIdHex,
                            ownDeviceKey = ownDeviceKey,
                        )
                        mutableJoinableGroupCall.value = joinable?.let {
                            JoinableGroupCall(
                                callIdHex = it.callIdHex,
                                // Not in this call (else no banner), so this
                                // device's own entry is never counted.
                                joinedCount = me.lakko.app.calls.freshGroupCallPeopleCount(
                                    it,
                                    now,
                                    ignoreDeviceKey = ownDeviceKey,
                                ),
                                video = it.video,
                            )
                        }
                        if (record == null) break
                        val ignored = me.lakko.app.calls.groupCallIgnoredDeviceKey(
                            record,
                            ownDeviceKey,
                            groupSlot?.callIdHex,
                        )
                        if (!me.lakko.app.calls.isGroupCallLive(record, now, ignored)) break
                        // While the banner offers a call to join, ask the SFU
                        // whether it is still there; the host closes the record
                        // when the answer is `ended` (`SfuCallState.kt`).
                        val host = groupCallHost
                        val offered = joinable?.callIdHex
                        if (host != null && offered != null) {
                            launch { host.probeCallState(groupIdHex, offered) }
                        }
                        delay(GROUP_CALL_BANNER_TICK_MS)
                    }
                }
        }
    }

    /** `true` when this conversation is a group (ADR-0062, read from the
     *  group itself) — the call button then routes through the SFU
     *  group-call path, even for a two-person group; a one-to-one stays on
     *  the 1x1 P2P call. A group this device cannot classify yet (no
     *  handle) follows [me.lakko.app.chat.conversationKindOf]. */
    public val isGroupChat: Boolean
        get() = repo.conversationKind(groupIdHex) == me.lakko.app.chat.ConversationKind.Group

    // --- ADR-0059 section 6: Share my channels with this group ---

    /** Whether this conversation is a group (the switch is a group's). */
    public fun isGroupConversation(): Boolean =
        isGroupConversation(repo, groupIdHex)

    /** The shared-channels row; the switch reads `groupIdHex in shareOn`. */
    public val sharedChannelsRow: StateFlow<me.lakko.app.discover.SharedChannelsRow>
        get() = repo.sharedChannels.row

    private val mutableShareChannelsBusy = MutableStateFlow(false)
    public val shareChannelsBusy: StateFlow<Boolean> = mutableShareChannelsBusy.asStateFlow()
    private val mutableShareChannelsError = MutableStateFlow<UiText?>(null)
    public val shareChannelsError: StateFlow<UiText?> = mutableShareChannelsError.asStateFlow()

    /** Load the row for this account (the sheet calls it when shown). */
    public fun openSharedChannels() {
        viewModelScope.launch { runCatching { repo.sharedChannels.open() } }
    }

    /** Turn the switch on or off. It changes only once the send succeeded. */
    public fun setShareChannels(on: Boolean) {
        if (mutableShareChannelsBusy.value) return
        mutableShareChannelsBusy.value = true
        mutableShareChannelsError.value = null
        viewModelScope.launch {
            try {
                repo.sharedChannels.setSharing(groupIdHex, on)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatViewModel", "share channels switch failed", cause)
                mutableShareChannelsError.value = when ((cause as? me.lakko.app.discover.SharingRefused)?.reason) {
                    me.lakko.app.discover.SharingRefused.Reason.SIGNED_OUT -> UiText("common.error.session_ended")
                    me.lakko.app.discover.SharingRefused.Reason.NOT_A_MEMBER -> UiText("chat.ended.no_longer_member")
                    null -> UiText("discover.share.error")
                }
            } finally {
                mutableShareChannelsBusy.value = false
            }
        }
    }

    // --- Group membership (A4) ---

    private val mutableMembers: MutableStateFlow<List<GroupMember>> =
        MutableStateFlow(emptyList())

    /** Live MLS roster for this group at the current epoch. */
    public val members: StateFlow<List<GroupMember>> = mutableMembers.asStateFlow()

    /**
     * The other people in this chat, re-read after every repository emission.
     *
     * The header's "N members" and the Read tick both need the live roster, and
     * a membership change arrives without anything the screen already watches
     * changing: a creator's own proposal is swept at the tail of a poll, so the
     * group grew while the header kept "2 members" until the chat was reopened
     * (seen live on 2026-09-17). A `StateFlow` drops equal sets, so the screen
     * recomposes only when the roster actually changed.
     */
    private val mutableOtherMemberIdentities: MutableStateFlow<Set<me.lakko.app.chat.MemberIdentity>> =
        MutableStateFlow(emptySet())
    public val otherMemberIdentities: StateFlow<Set<me.lakko.app.chat.MemberIdentity>> =
        mutableOtherMemberIdentities.asStateFlow()

    private val mutableMembersPanelOpen: MutableStateFlow<Boolean> =
        MutableStateFlow(false)

    /** `true` while the members sheet is shown. */
    public val membersPanelOpen: StateFlow<Boolean> = mutableMembersPanelOpen.asStateFlow()

    private val mutableAddMemberInput: MutableStateFlow<String> = MutableStateFlow("")

    /** `@username` typed into the add-member field. */
    public val addMemberInput: StateFlow<String> = mutableAddMemberInput.asStateFlow()

    private val mutableMembershipBusy: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** `true` while an add / remove commit is in flight. */
    public val membershipBusy: StateFlow<Boolean> = mutableMembershipBusy.asStateFlow()

    private val mutableMembershipError: MutableStateFlow<UiText?> = MutableStateFlow(null)

    /** Last add / remove error surfaced to the sheet, else `null`. */
    public val membershipError: StateFlow<UiText?> = mutableMembershipError.asStateFlow()

    private val mutableMembershipNotice: MutableStateFlow<UiText?> = MutableStateFlow(null)

    /** ADR-0045 — feedback for a membership request that was accepted locally
     *  and published. Distinct from [membershipError] because a request is not
     *  an outcome: the change has NOT happened yet, and the copy must not
     *  suggest it has. */
    public val membershipNotice: StateFlow<UiText?> = mutableMembershipNotice.asStateFlow()

    private val mutableCanManageMembers: MutableStateFlow<Boolean> = MutableStateFlow(true)

    /** ADR-0022 — `true` when the local member may add / remove (owner, admin,
     *  or an open/legacy group with no role table); `false` for a plain member.
     *  The sheet hides the add field + per-member Remove when this is `false`. */
    public val canManageMembers: StateFlow<Boolean> = mutableCanManageMembers.asStateFlow()

    private val mutableIsOwner: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** ADR-0022 — `true` when the local member is the owner (only the owner sees
     *  the per-member promote / demote toggle). */
    public val isOwner: StateFlow<Boolean> = mutableIsOwner.asStateFlow()

    private val mutableRoleTable: MutableStateFlow<RoleTable?> = MutableStateFlow(null)

    /** The group's current role table (owner + admins), or `null` for an open
     *  group — the members sheet reads it reactively to label + gate the
     *  per-member promote / demote toggle. */
    public val roleTable: StateFlow<RoleTable?> = mutableRoleTable.asStateFlow()

    private val mutablePendingJoins: MutableStateFlow<List<ByteArray>> =
        MutableStateFlow(emptyList())

    /** ADR-0027 (G-3) — pending external-join requests. Each entry is a joiner
     *  leaf signature key (the `proposalRef` for approve / reject). The members
     *  sheet renders an owner/admin-only section from this; empty hides it. The
     *  MLS-core retained set (`group.pendingExternalJoins()`) is the source of
     *  truth — re-read on [refreshMembers] and on each poll while the sheet is
     *  open. */
    public val pendingJoins: StateFlow<List<ByteArray>> = mutablePendingJoins.asStateFlow()

    private val mutableUnkeptJoinRequest: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** ADR-0027 / UX §7.1 — an external-join request arrived while the
     *  quarantine was full, so the core did not keep it. It is in
     *  [pendingJoins] for nobody and can be approved by nobody; the only honest
     *  thing left is to say it happened.
     *
     *  Session-scoped and cleared when the admin acts on the queue (approve or
     *  reject), because acting is precisely what may have made room. There is
     *  no count: `MAX_PENDING_EXTERNAL_JOINS` is module-private in the core,
     *  and a client-side literal is the stale-drift class §7.1 flags. */
    public val unkeptJoinRequest: StateFlow<Boolean> = mutableUnkeptJoinRequest.asStateFlow()

    private val mutablePendingRequests:
        MutableStateFlow<List<me.lakko.app.chat.PendingRequest>> = MutableStateFlow(emptyList())

    /**
     * ADR-0045 — the pending MEMBERSHIP requests (proposals awaiting a
     * committer sweep), already filtered by the UX §10 visibility gate: the
     * owner and admins see the whole set, every other member sees only requests
     * they made themselves.
     *
     * Distinct from [pendingJoins], which is the ADR-0027 EXTERNAL-join queue —
     * a stranger asking to be let in, which an admin acts on. These are requests
     * an existing member already published, which nobody approves; they simply
     * apply when a committer sweeps.
     */
    public val pendingRequests: StateFlow<List<me.lakko.app.chat.PendingRequest>> =
        mutablePendingRequests.asStateFlow()

    private val mutableNotApplied:
        MutableStateFlow<List<me.lakko.app.chat.UnappliedRequest>> = MutableStateFlow(emptyList())

    /**
     * ADR-0045 S7 residual 1 / ADR-0050 section 2.5a point 5 — own requests a
     * sweep dropped whose automatic re-request then stopped (the author lost
     * the role it needs, the core refuses it, an approved join's KeyPackage is
     * gone). A re-request in flight is shown nowhere. Emptied with the rest of
     * the pending surface once the membership has ended.
     */
    public val notApplied: StateFlow<List<me.lakko.app.chat.UnappliedRequest>> =
        mutableNotApplied.asStateFlow()

    private val mutablePendingNotice:
        MutableStateFlow<me.lakko.app.chat.PendingNotice?> = MutableStateFlow(null)

    /** ADR-0045 / UX §5.1 — the panel-level "N membership changes are waiting"
     *  notice, or `null` when nothing has been outstanding past the quiet
     *  window. */
    public val pendingNotice: StateFlow<me.lakko.app.chat.PendingNotice?> =
        mutablePendingNotice.asStateFlow()

    /** ADR-0028 OQ2 (G-3) — advisory attribution for a pending request: which
     *  member minted the `/g/` link it arrived on, and whether that issuer is in
     *  the current roster. */
    public data class JoinAttributionUi(
        val issuerFingerprint: String,
        val issuerInRoster: Boolean,
        /** ADR-0048 — the issuer's signature key (base64), for its name. */
        val issuerKeyB64: String? = null,
    )

    private val mutableJoinAttributions:
        MutableStateFlow<Map<String, JoinAttributionUi>> = MutableStateFlow(emptyMap())

    /** ADR-0028 OQ2 (G-3) — keyed by joiner signature key (full hex). Refreshed
     *  alongside [pendingJoins]; a missing entry means no attribution was posted. */
    public val joinAttributions: StateFlow<Map<String, JoinAttributionUi>> =
        mutableJoinAttributions.asStateFlow()

    private val mutableInviteLink: MutableStateFlow<String?> = MutableStateFlow(null)

    /** ADR-0028 (G-3) — the minted group invite-link (the owner/admin "create
     *  invite link" affordance), or `null` before it is generated. */
    public val inviteLink: StateFlow<String?> = mutableInviteLink.asStateFlow()

    /** Update the add-member text field. */
    public fun setAddMemberInput(value: String) {
        mutableAddMemberInput.value = value
    }

    // --- ADR-0052 decision 3 — pins ------------------------------------------

    private val mutableSharedPins: MutableStateFlow<Map<Long, me.lakko.app.chat.StoredPin>> =
        MutableStateFlow(emptyMap())

    /** The conversation's shared pin map as this device holds it (unpins included). */
    public val sharedPins: StateFlow<Map<Long, me.lakko.app.chat.StoredPin>> = mutableSharedPins.asStateFlow()

    private val mutablePersonalPins: MutableStateFlow<Map<Long, ULong>> = MutableStateFlow(emptyMap())

    /** This device's personal pins here, `messageId -> pinnedAtMs`. */
    public val personalPins: StateFlow<Map<Long, ULong>> = mutablePersonalPins.asStateFlow()

    private val mutablePinAuthority: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** Whether this device may set or clear a shared pin (section 4.1's
     *  second row). Re-read off the main thread after every repository
     *  emission, so the sheet never pays a Rust call per bubble. */
    public val mayPinForEveryone: StateFlow<Boolean> = mutablePinAuthority.asStateFlow()

    /** Re-read both pin stores. */
    private fun refreshPins() {
        mutableSharedPins.value = repo.sharedPins(groupIdHex)
        mutablePersonalPins.value = repo.personalPins(groupIdHex)
    }

    /** Section 4.1 — the row's word and the sheet's choices for [messageId]. */
    public fun pinMenuFor(messageId: Long): me.lakko.app.chat.PinMenu =
        me.lakko.app.chat.pinMenuFor(
            sharedPinned = mutableSharedPins.value[messageId]?.pinned == true,
            personalPinned = messageId in mutablePersonalPins.value,
            mayShared = mutablePinAuthority.value,
        )

    /** Perform one pin action on [messageId]. A personal action is local and
     *  immediate; a shared one is sent, and applied locally once it went out. */
    public fun onPinAction(messageId: Long, action: me.lakko.app.chat.PinAction) {
        when (action) {
            me.lakko.app.chat.PinAction.PIN_ME -> repo.setPersonalPin(groupIdHex, messageId, pinned = true)
            me.lakko.app.chat.PinAction.UNPIN_ME -> repo.setPersonalPin(groupIdHex, messageId, pinned = false)
            me.lakko.app.chat.PinAction.PIN_EVERYONE, me.lakko.app.chat.PinAction.UNPIN_EVERYONE -> {
                val pinned = action == me.lakko.app.chat.PinAction.PIN_EVERYONE
                viewModelScope.launch {
                    try {
                        repo.setSharedPin(groupIdHex, messageId, pinned)
                    } catch (err: Throwable) {
                        composer.update { it.copy(sendError = chatError(err, "chat.error.pin")) }
                        android.util.Log.w("ChatViewModel", "onPinAction failed", err)
                    }
                }
            }
        }
    }

    // --- ADR-0051 — the shared group name -----------------------------------

    private val mutableGroupName: MutableStateFlow<String> = MutableStateFlow("")

    /** The group's shared name as this device holds it; empty when none. */
    public val groupName: StateFlow<String> = mutableGroupName.asStateFlow()

    private val mutableGroupNameDraft: MutableStateFlow<String> = MutableStateFlow("")

    /** The `Group name` field's text while editing. */
    public val groupNameDraft: StateFlow<String> = mutableGroupNameDraft.asStateFlow()

    private val mutableGroupNameBusy: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** `true` while a rename is being sent. */
    public val groupNameBusy: StateFlow<Boolean> = mutableGroupNameBusy.asStateFlow()

    private val mutableGroupNameError: MutableStateFlow<UiText?> = MutableStateFlow(null)

    /** The last rename failure, else `null`. */
    public val groupNameError: StateFlow<UiText?> = mutableGroupNameError.asStateFlow()

    /** Re-read the stored shared name (and reset the draft to it). */
    private fun refreshGroupName() {
        val stored = repo.groupName(groupIdHex)?.name.orEmpty()
        mutableGroupName.value = stored
        if (!mutableGroupNameBusy.value) mutableGroupNameDraft.value = stored
    }

    public fun setGroupNameDraft(value: String) {
        if (value.length <= GroupPresentationStore.MAX_NAME_CHARS) {
            mutableGroupNameDraft.value = value
            mutableGroupNameError.value = null
        }
    }

    /**
     * ADR-0051 §3 — Save: send the draft into the group (an empty draft
     * clears the name). Gated on the LIVE role — a stale edit control tapped
     * by a demoted admin reads "Only admins can rename this group". The local
     * title changes only once the send succeeded.
     */
    public fun saveGroupName() {
        if (mutableGroupNameBusy.value) return
        val draft = mutableGroupNameDraft.value.trim()
        if (draft == mutableGroupName.value.trim()) return
        if (repo.ownRole(groupIdHex) == Role.MEMBER) {
            mutableGroupNameError.value = me.lakko.app.chat.GROUP_NAME_ONLY_ADMINS
            return
        }
        mutableGroupNameBusy.value = true
        mutableGroupNameError.value = null
        viewModelScope.launch {
            try {
                repo.setGroupName(groupIdHex, draft)
                mutableGroupName.value = draft
                mutableGroupNameDraft.value = draft
            } catch (e: uniffi.lakko_ffi_uniffi.FfiException) {
                // The composer refused the text (line break, control character,
                // too long): the message names the rule.
                mutableGroupNameError.value = groupNameErrorText(e)
                android.util.Log.w("ChatViewModel", "saveGroupName refused", e)
            } catch (e: Throwable) {
                mutableGroupNameError.value = me.lakko.app.chat.GROUP_NAME_SEND_FAILED
                android.util.Log.w("ChatViewModel", "saveGroupName failed", e)
            } finally {
                mutableGroupNameBusy.value = false
            }
        }
    }

    /** Open / close the members sheet; refresh the roster on open. */
    public fun toggleMembersPanel() {
        val opening = !mutableMembersPanelOpen.value
        mutableMembersPanelOpen.value = opening
        if (opening) {
            mutableGroupNameError.value = null
            mutableMembershipError.value = null
            // A notice describes one action's outcome, so it must not outlive
            // the panel that produced it: the pending row annotation is the
            // durable signal, not this.
            mutableMembershipNotice.value = null
            // ADR-0045 §6.1.5 — and neither does a half-filled transfer step.
            // Reopening the panel must not resume a confirmation the user walked
            // away from with a successor already picked.
            mutableTransferStepOpen.value = false
            refreshMembers()
        }
    }

    /**
     * ADR-0045 UX §6.2 — re-read the core's own membership predicate.
     *
     * `Group.isActive()` is the variable `createApplicationMessage`,
     * `exportSecret` and `unprotect_message` each consult before refusing, so a
     * composer withdrawn on it is withdrawn for the reason the send would have
     * failed. A torn read answers "still a member", which is the status quo
     * rather than a false claim the user cannot check.
     */
    /**
     * ADR-0045 UX §6 / §12 item 6 — the one funnel every membership handler
     * reports a failure through.
     *
     * Before this, all nine handlers were
     * `mutableMembershipError.value = e.message ?: e.toString()`, so a member
     * who tapped `Add` was shown `proposeAddMemberFromWire failed`, or — one
     * layer in — the core's own `add proposal refused: proposer is not owner
     * or admin`, a sentence written for a developer reading a log.
     *
     * The **log keeps that sentence**, because that is who it is for. The
     * screen gets the copy the reason selects, and the reason is a typed value
     * rather than anything read out of the message.
     *
     * A non-refusal keeps its own message unchanged: collapsing a dead socket
     * into "This change could not be requested." would be a worse lie than the
     * raw text, and §6 row 7 is about not leaking a *refusal's* internal
     * reason, not about hiding every error.
     */
    private fun reportMembershipFailure(e: Throwable, op: String, fallbackKey: String) {
        // ADR-0061 section 6: the reason, the token pool or the device clock
        // pick the sentence; anything else is the action's own key. The raw
        // message stays in the log, for the people it was written for.
        mutableMembershipError.value = me.lakko.app.chat.chatErrorText(
            e,
            fallbackKey,
            ::membershipRefusalRetryMaySucceed,
            // ADR-0054 - adding by username spends a lookup token.
            allowance = me.lakko.app.chat.AllowanceKind.LOOKUP,
        )
        android.util.Log.w("ChatViewModel", "$op failed", e)
    }

    /** ADR-0061 section 6 — a failed chat action's sentence, [fallbackKey] unless the error names itself. */
    private fun chatError(err: Throwable, fallbackKey: String): UiText {
        // The raw message is for the log. A host JVM test has no
        // android.util.Log, and the sentence must not depend on one.
        try {
            android.util.Log.w("ChatViewModel", "chat action failed ($fallbackKey)", err)
        } catch (_: RuntimeException) {
        }
        return me.lakko.app.chat.chatErrorText(err, fallbackKey, ::membershipRefusalRetryMaySucceed)
    }

    /** A rename's refusal from its kind (`chat.group_name.error.*`), never the core's text. */
    private fun groupNameErrorText(e: uniffi.lakko_ffi_uniffi.FfiException): UiText = when (e) {
        is uniffi.lakko_ffi_uniffi.FfiException.DisplayNameLineBreak -> UiText("chat.group_name.error.line_break")
        is uniffi.lakko_ffi_uniffi.FfiException.DisplayNameControlOrFormatCharacter ->
            UiText("chat.group_name.error.character")
        is uniffi.lakko_ffi_uniffi.FfiException.DisplayNameTooLong,
        is uniffi.lakko_ffi_uniffi.FfiException.DisplayNameTooManyBytes,
        -> UiText("chat.group_name.error.too_long")
        else -> UiText("chat.group_name.error.generic")
    }

    // -----------------------------------------------------------------------
    // ADR-0045 UX §7 — the automatic re-send of a queue-full refusal
    //
    // Every rule lives in `me.lakko.app.chat.MembershipRetry` and is tested
    // there; what is here is the wiring that feeds it live group state and
    // re-issues what it decides to fire.
    // -----------------------------------------------------------------------

    private val retryStore = me.lakko.app.chat.MembershipRetryStore()

    /** The `(account, group)` scope every armed retry is filed under, or
     *  `null` outside a session, where there is nothing to file. */
    private fun retryScope(): String? {
        val account = repo.currentAccountIdB64() ?: return null
        return me.lakko.app.chat.MembershipRetryStore.scopeKey(account, groupIdHex)
    }

    /**
     * The group as the retry verdict reads it, captured in ONE pass — the
     * roster and the pending set must come from the same moment, because the
     * merge that empties one is the merge that moves the other.
     *
     * A `null` roster is a TORN read, not an empty group, and the verdict
     * treats it as claiming nothing.
     */
    private fun retrySnapshot(): me.lakko.app.chat.RetryGroupSnapshot {
        val roster = repo.members(groupIdHex)?.map {
            me.lakko.app.chat.RetryRosterMember(
                leafIndex = it.leafIndex,
                signatureKeyB64 = me.lakko.app.chat.outboxB64(it.signaturePublicKey),
            )
        }
        val table = repo.groupRoles(groupIdHex)?.let {
            me.lakko.app.chat.RetryRoleTable(
                ownerKeyB64 = me.lakko.app.chat.outboxB64(it.owner),
                adminKeysB64 = it.admins.map { key -> me.lakko.app.chat.outboxB64(key) },
            )
        }
        return me.lakko.app.chat.RetryGroupSnapshot(
            roster = roster,
            pendingKeys = repo.pendingProposalKeys(groupIdHex),
            pendingCount = repo.pendingProposalCount(groupIdHex),
            roleTable = table,
            membershipEnded = membershipEnded.value,
            retainedJoinKeys = retainedJoinKeysNow(),
        )
    }

    /**
     * ADR-0027 — the joiner key of every request the core still holds in its
     * retained external-join quarantine, or `null` on a torn read.
     *
     * Deliberately NOT `repo.pendingExternalJoins`: that one folds a failed
     * read into an empty list (correct for a panel, which then renders nothing)
     * and its caller gates on `membershipEnded`. Here an empty list and a
     * failed read are different answers — the first says the request is gone,
     * the second says nothing — and conflating them would drop a live retry.
     */
    private fun retainedJoinKeysNow(): List<String>? =
        repo.retainedExternalJoinKeys(groupIdHex)?.map { me.lakko.app.chat.outboxB64(it) }

    /**
     * Claim the slot for [request] before a MANUAL attempt starts.
     *
     * This is half of the exactly-once property and the half that is easy to
     * miss: the re-issue goes through the repository's own per-group commit
     * lock, so the driver cannot hold that across its own last check. A user
     * tapping again therefore takes the record away rather than racing an
     * in-flight retry for it.
     */
    private fun disarmRetry(request: me.lakko.app.chat.RetryableRequest) {
        retryScope()?.let { retryStore.claim(it, request.key) }
    }

    /**
     * Arm an automatic re-send if — and only if — the CORE says this refusal
     * resolves on its own.
     *
     * Nothing here lists a reason. `armedRetryFor` asks
     * `membershipRefusalRetryMaySucceed`, so a `DoNotRetry` refusal, a
     * `NotPermitted` one, an unrecognised reason from a newer core and an
     * ordinary error all take the same path: no record, nothing retried.
     */
    private fun armRetryIfQueueFull(
        request: me.lakko.app.chat.RetryableRequest,
        error: Throwable,
        pendingCountAtRefusal: Int,
    ) {
        val scope = retryScope() ?: return
        val armed = me.lakko.app.chat.armedRetryFor(
            error = error,
            request = request,
            pendingCountAtRefusal = pendingCountAtRefusal,
            retryMaySucceed = ::membershipRefusalRetryMaySucceed,
        ) ?: return
        retryStore.arm(scope, armed)
    }

    /**
     * Run one membership request, arming a re-send if a full queue refused it.
     *
     * Wrapping rather than editing each handler's body keeps the arm and the
     * disarm adjacent to each other and to the call, which is what stops a new
     * handler from arming without disarming.
     */
    private suspend fun <T> withMembershipRetry(
        request: me.lakko.app.chat.RetryableRequest,
        run: suspend () -> T,
    ): T {
        disarmRetry(request)
        val before = repo.pendingProposalCount(groupIdHex)
        try {
            return run()
        } catch (e: Throwable) {
            armRetryIfQueueFull(request, e, before)
            throw e
        }
    }

    /**
     * The approve-an-external-join twin of [withMembershipRetry].
     *
     * Separate because the arming gate is: `armedApproveJoinRetry` also asks
     * `externalJoinAffordance` whether the request is one an approve could act
     * on at all, and the `retained` answer is read LIVE off the core's
     * quarantine rather than assumed. A torn read of that set answers `false`,
     * which is the fail-safe direction — it can only decline to arm, never arm
     * a retry the core has nothing to look up.
     */
    private suspend fun <T> withApproveJoinRetry(
        proposalRef: ByteArray,
        run: suspend () -> T,
    ): T {
        val subjectKeyB64 = me.lakko.app.chat.outboxB64(proposalRef)
        disarmRetry(me.lakko.app.chat.RetryableRequest.ApproveJoin(subjectKeyB64))
        val before = repo.pendingProposalCount(groupIdHex)
        try {
            return run()
        } catch (e: Throwable) {
            val scope = retryScope()
            if (scope != null) {
                val armed = me.lakko.app.chat.armedApproveJoinRetry(
                    error = e,
                    subjectKeyB64 = subjectKeyB64,
                    pendingCountAtRefusal = before,
                    retained = retainedJoinKeysNow()?.contains(subjectKeyB64) ?: false,
                    retryMaySucceed = ::membershipRefusalRetryMaySucceed,
                )
                if (armed != null) retryStore.arm(scope, armed)
            }
            throw e
        }
    }

    /**
     * Re-issue one armed request, reconstructed from its descriptor.
     *
     * **Reconstructed, never a stored lambda.** The descriptor is exactly what
     * the request is; anything a lambda would additionally capture (a roster
     * snapshot, a leaf index, a KeyPackage) is stale by the time the queue
     * drains. In particular a removal's LEAF is resolved from the live roster
     * here, because OpenMLS hands a vacated leaf to the next member added and a
     * stored one would eventually remove the wrong person.
     */
    private suspend fun reissueMembershipRequest(
        request: me.lakko.app.chat.RetryableRequest,
    ) {
        when (request) {
            is me.lakko.app.chat.RetryableRequest.Add ->
                repo.addMemberToGroup(
                    groupIdHex,
                    me.lakko.app.chat.outboxUnb64(request.subjectAccountIdB64),
                    me.lakko.app.chat.outboxUnb64(request.subjectKeyB64),
                )
            is me.lakko.app.chat.RetryableRequest.Remove,
            is me.lakko.app.chat.RetryableRequest.Leave,
            -> {
                val key = when (request) {
                    is me.lakko.app.chat.RetryableRequest.Remove -> request.subjectKeyB64
                    is me.lakko.app.chat.RetryableRequest.Leave -> request.subjectKeyB64
                    else -> return
                }
                // The verdict already dropped a request whose subject has left,
                // so an unresolvable leaf here is a torn read between the
                // verdict and now. Refusing to guess is the only safe answer:
                // any fallback leaf names somebody, and for a removal that
                // somebody is a real person.
                val leaf = repo.members(groupIdHex)
                    ?.firstOrNull { me.lakko.app.chat.outboxB64(it.signaturePublicKey) == key }
                    ?.leafIndex ?: return
                repo.removeMemberFromGroup(groupIdHex, leaf)
            }
            is me.lakko.app.chat.RetryableRequest.SetAdmin ->
                repo.setMemberAdmin(
                    groupIdHex,
                    me.lakko.app.chat.outboxUnb64(request.subjectKeyB64),
                    request.makeAdmin,
                )
            is me.lakko.app.chat.RetryableRequest.TransferOwnership ->
                repo.transferGroupOwnership(
                    groupIdHex,
                    me.lakko.app.chat.outboxUnb64(request.subjectKeyB64),
                    request.outgoing,
                )
            is me.lakko.app.chat.RetryableRequest.ApproveJoin -> {
                // The joiner's signature key IS the `proposal_ref` handle, so
                // the descriptor reconstructs the call whole. The KeyPackage it
                // needs is the one the core retained — which is also why the
                // verdict refuses to fire once that request has left the
                // quarantine.
                repo.approveExternalJoin(
                    groupIdHex,
                    me.lakko.app.chat.outboxUnb64(request.subjectKeyB64),
                )
                // `unkeptJoinRequest` is deliberately NOT cleared here. The
                // manual handler clears it as feedback for something the user
                // just did; a background re-send is not that, and the notice's
                // action item — ask that person to send their request again —
                // stays true regardless, because a request the quarantine never
                // kept is not recoverable by approving a different one.
                Unit
            }
        }
    }

    /**
     * The driver: evaluate every armed request against the group as it is now.
     *
     * **Driven by observation, never by a timer.** It runs from the repository
     * emission that follows every poll and from `refreshMembers`, i.e. from
     * exactly the events that can drain a queue. A quiet group produces no
     * attempts at all.
     *
     * Free when nothing is armed, which is the overwhelmingly common case.
     */
    private fun evaluateArmedRetries() {
        val scope = retryScope() ?: return
        // Stand down while a MANUAL membership operation is in flight.
        //
        // The disarm-on-tap covers every handler that knows its descriptor up
        // front, but `onAddMember` cannot: the request only becomes nameable
        // once the typed handle has resolved, so between the tap and the
        // resolution there is a window in which a retry for the same person
        // could fire behind the user's own attempt. Standing down costs at
        // most one poll — the repository emission after the next one
        // re-evaluates.
        if (mutableMembershipBusy.value) return
        val armed = retryStore.armed(scope)
        if (armed.isEmpty()) return
        val view = retrySnapshot()
        for (entry in armed) {
            val verdict = me.lakko.app.chat.retryVerdict(entry, view)
            if (verdict is me.lakko.app.chat.RetryVerdict.Wait) continue
            // Claim first, decide second — the claim is the exactly-once step,
            // and a `Drop` claims for the same reason a `Fire` does: leaving a
            // dropped record in the store would re-evaluate it on every future
            // refresh.
            val claimed = retryStore.claim(scope, entry.request.key) ?: continue
            if (verdict is me.lakko.app.chat.RetryVerdict.Drop) continue
            viewModelScope.launch {
                try {
                    reissueMembershipRequest(claimed.request)
                    repo.sweepIfCommitter(groupIdHex)
                    refreshMembers()
                } catch (e: kotlinx.coroutines.CancellationException) {
                    // The screen went away mid-retry. NOT a failure of the
                    // request and not something to report: swallowing a
                    // `CancellationException` into an error message is the same
                    // class of mistake ADR-0044 recorded on the commit path.
                    // The record is already claimed, so this attempt is simply
                    // lost — which is exactly the in-memory contract.
                    throw e
                } catch (e: Throwable) {
                    // A second queue-full refusal re-arms against the NEW count
                    // and waits for the next drain — silently, because nothing
                    // has changed for the user since the refusal they were
                    // already shown.
                    //
                    // An approve re-arms through its OWN helper, so the
                    // affordance gate runs on this path too. Defence in depth
                    // rather than a hole being closed — `approve_external_join`
                    // looks the request up BEFORE it proposes, so a queue-full
                    // refusal proves it was still retained — but the re-arm is
                    // part of the retry path, and a second way to arm one that
                    // skipped a gate the first way applies is the shape that
                    // eventually diverges.
                    val request = claimed.request
                    val count = repo.pendingProposalCount(groupIdHex)
                    val rearmed = if (request is me.lakko.app.chat.RetryableRequest.ApproveJoin) {
                        me.lakko.app.chat.armedApproveJoinRetry(
                            error = e,
                            subjectKeyB64 = request.subjectKeyB64,
                            pendingCountAtRefusal = count,
                            retained =
                                retainedJoinKeysNow()?.contains(request.subjectKeyB64) ?: false,
                            retryMaySucceed = ::membershipRefusalRetryMaySucceed,
                        )
                    } else {
                        me.lakko.app.chat.armedRetryFor(
                            error = e,
                            request = request,
                            pendingCountAtRefusal = count,
                            retryMaySucceed = ::membershipRefusalRetryMaySucceed,
                        )
                    }
                    if (rearmed != null) {
                        retryStore.arm(scope, rearmed)
                    } else {
                        // Anything else is a real failure of a request the user
                        // made and was not present for, so it replaces the
                        // stale message rather than being logged away.
                        reportMembershipFailure(e, "membership retry", "chat.error.membership_request")
                    }
                }
            }
        }
    }

    private fun refreshMembership() {
        applyMembership(repo.isGroupActive(groupIdHex))
    }

    /** [refreshMembership] with the core's `isActive` already read (off main). */
    private fun applyMembership(isActive: Boolean?) {
        mutableMembership.value = me.lakko.app.chat.membershipState(
            isActive = isActive,
            leaveRequestedAtMs = leaveRequestedAtMs,
        )
    }

    /** Re-read the live MLS roster from the repository. */
    public fun refreshMembers() {
        // ADR-0045 UX §6.2.7 — decided FIRST, because every gate below depends
        // on it and reading the roster does not change the answer.
        refreshMembership()
        val ended = membershipEnded.value
        mutableMembers.value = repo.members(groupIdHex) ?: emptyList()
        // ADR-0051 — the shared name rides the same refresh as the roster.
        refreshGroupName()
        val role = repo.ownRole(groupIdHex)
        // ADR-0045 UX §6.2.7 — and NOBODY may manage members once their own
        // membership has ended. `Add member`, the invite link, every `Remove`
        // and every admin toggle are all refused by the core for an inactive
        // group, and one gate cannot be partly applied. Note the difference
        // from role-gating, which hides what SOMEONE ELSE may do — this hides
        // what nobody in this group may do any more.
        mutableCanManageMembers.value = role != Role.MEMBER && !ended
        // §6.2.7 — and the owner-only admin toggle goes with it. Defence in
        // depth rather than a reachable case: §6.2.9 establishes that a sitting
        // owner cannot reach this state at all (`authorise` refuses a `Remove`
        // targeting the owner and refuses an owner's own leave). But `isOwner`
        // gates a control independently of `canManageMembers`, and a gate that
        // is only correct because of a property proved somewhere else is a gate
        // that breaks silently when that property moves.
        mutableIsOwner.value = role == Role.OWNER && !ended
        mutableRoleTable.value = repo.groupRoles(groupIdHex)
        // ADR-0027 (G-3) — refresh the pending external-join list alongside the
        // roster (the retained set is the source of truth).
        //
        // §6.2.7 — emptied once the membership has ended: approving needs
        // `add_member`, which the core refuses for an inactive group, so the
        // section would be a list of buttons that all fail.
        mutablePendingJoins.value =
            if (ended) emptyList() else repo.pendingExternalJoins(groupIdHex)
        // ADR-0027 / §7.1 — the request the quarantine turned away is in no
        // list, so it can only come from the repository's own observation.
        mutableUnkeptJoinRequest.value = !ended && repo.sawUnkeptExternalJoin(groupIdHex)
        // ADR-0045 — and the pending MEMBERSHIP requests, gated by the UX §10
        // visibility rule. Read after the role above, so a just-changed role is
        // reflected in what this member is allowed to see.
        //
        // §6.2.7 — emptied at the SOURCE once the membership has ended, so the
        // notice, the row annotations, `Waiting to be added` and the exit
        // block's `waitingRoleChange` all fall silent together rather than one
        // render site at a time. The pending set describes changes that will
        // apply to a group that no longer contains us.
        val visible = if (ended) {
            emptyList()
        } else {
            me.lakko.app.chat.visibleRequests(
                repo.pendingMembershipRequests(groupIdHex),
                mutableCanManageMembers.value,
            )
        }
        mutablePendingRequests.value = visible
        mutablePendingNotice.value = me.lakko.app.chat.pendingNotice(visible)
        // ADR-0045 S7 residual 1 / ADR-0050 section 2.5a — rows are written by
        // the repository's superseded pass when a re-request stops; this only
        // reads them.
        mutableNotApplied.value = if (ended) emptyList() else repo.notAppliedRequests(groupIdHex)
        refreshJoinAttributions()
        // ADR-0045 UX §6.1.9 — decided from the roster + role table just read,
        // so a torn read says nothing rather than claiming a promotion.
        refreshNewOwnerNotice()
        // ADR-0045 UX §7 — and the last thing, against the state just read: a
        // queue that drained since a refusal is exactly what this pass would
        // have observed.
        evaluateArmedRetries()
    }

    // -----------------------------------------------------------------------
    // ADR-0045 UX §6.1 — ownership transfer
    //
    // Every rule and every string lives in `me.lakko.app.chat.OwnershipTransfer`;
    // what is here is the wiring that feeds it live group state, and what is in
    // `MembersSheet` renders its verdicts.
    // -----------------------------------------------------------------------

    /** The roster's OWN identity treatment, in the roster's own order, as the
     *  picker reads it. Built from [memberLabel] rather than re-derived, so the
     *  picker and the members list cannot render the same person differently —
     *  that is the one thing the user cross-checks. */
    internal fun rosterViews(): List<me.lakko.app.chat.RosterEntryView> =
        mutableMembers.value.map {
            me.lakko.app.chat.RosterEntryView(
                leafIndex = it.leafIndex,
                signatureKeyB64 = me.lakko.app.chat.outboxB64(it.signaturePublicKey),
                label = memberLabel(it),
            )
        }

    /** This device's own signature key, base64 — resolved THROUGH the roster
     *  rather than stored, so a leaf renumber cannot make it name someone
     *  else. */
    private fun ownSignatureKeyB64(): String? {
        val leaf = ownLeafIndex() ?: return null
        return rosterViews().firstOrNull { it.leafIndex == leaf }?.signatureKeyB64
    }

    private val mutableNewOwnerNotice: MutableStateFlow<me.lakko.app.chat.NewOwnerNotice?> =
        MutableStateFlow(null)

    /** ADR-0045 UX §6.1.9 — the once-per-transfer, dismissible notice telling
     *  the NEW owner they are the new owner. `null` when there is nothing to
     *  say, which is the overwhelmingly common case. */
    public val newOwnerNotice: StateFlow<me.lakko.app.chat.NewOwnerNotice?> =
        mutableNewOwnerNotice.asStateFlow()

    /** Dismiss the §6.1.9 notice. It never comes back on its own: the owner it
     *  reported has been remembered, so the next reading is not a change. */
    public fun dismissNewOwnerNotice() {
        mutableNewOwnerNotice.value = null
    }

    /**
     * ADR-0045 UX §6.1.9 — tell the new owner, best-effort and fail-open.
     *
     * Nothing on the wire announces a transfer: the role table simply changes
     * and every other device learns of it by re-reading. So this is a
     * client-side INFERENCE from an observed change, and it is structurally
     * unable to fire on a device that first observes the table already naming it
     * owner — a fresh device, a restore, a first sync. Hence fail-open
     * everywhere, and hence the notice must never be the only way a user learns
     * their role; the panel's affordances stay the primary signal, exactly as
     * they are for admin today.
     *
     * The notice is decided BEFORE the new owner is remembered, or the
     * comparison would always be against itself. It then LIVES in the flow until
     * dismissed rather than being recomputed: the very next refresh would see
     * the remembered owner equal the current one and correctly return `null`, so
     * a recomputed notice would vanish on the next poll.
     */
    private fun refreshNewOwnerNotice() {
        val table = mutableRoleTable.value ?: return
        val currentOwnerB64 = me.lakko.app.chat.outboxB64(table.owner)
        val previous = repo.lastObservedOwner(groupIdHex)
        // Resolved through the LIVE roster; `null` when the previous owner has
        // left in a later commit (transfer and leave are necessarily separate
        // ones).
        val previousLabel = previous?.let { key ->
            mutableMembers.value
                .firstOrNull { me.lakko.app.chat.outboxB64(it.signaturePublicKey) == key }
                ?.let { memberLabel(it) }
        }
        me.lakko.app.chat.newOwnerNotice(
            lastObservedOwnerB64 = previous,
            currentOwnerB64 = currentOwnerB64,
            ownSignatureKeyB64 = ownSignatureKeyB64(),
            previousOwnerLabel = previousLabel,
        )?.let { mutableNewOwnerNotice.value = it }
        repo.rememberObservedOwner(groupIdHex, currentOwnerB64)
    }

    private val mutableTransferStepOpen: MutableStateFlow<Boolean> = MutableStateFlow(false)
    private val mutableTransferPick: MutableStateFlow<String?> = MutableStateFlow(null)
    private val mutableTransferOutgoing: MutableStateFlow<OutgoingOwner> =
        MutableStateFlow(me.lakko.app.chat.PRESELECTED_OUTGOING_OWNER)

    /**
     * ADR-0045 UX §6.1.5 — is the transfer step showing?
     *
     * It lives here rather than in the composable — unlike the `Leave group`
     * confirmation, which is purely local — because [onTransferOwnership] must
     * close it only once the proposal is actually published. A step that closed
     * on tap would discard the picked successor and the role choice on every
     * refusal, and the commonest refusal (the C-65 collision) is one the owner
     * would want to retry unchanged a moment later.
     */
    public val transferStepOpen: StateFlow<Boolean> = mutableTransferStepOpen.asStateFlow()

    /** The picked successor's SIGNATURE KEY (base64), or `null` for no pick yet
     *  — which is what keeps the action disabled. Never a leaf index: OpenMLS
     *  hands a vacated leaf to the next Add. */
    public val transferPick: StateFlow<String?> = mutableTransferPick.asStateFlow()

    /** §6.1.4 — what becomes of THIS device's role. Pre-selected, never
     *  defaulted; see [me.lakko.app.chat.OUTGOING_OWNER_OPTIONS]. */
    public val transferOutgoing: StateFlow<OutgoingOwner> = mutableTransferOutgoing.asStateFlow()

    /** Open the transfer step. Resets both choices, so a step reopened after a
     *  cancel never carries a stale successor, and clears the panel's transient
     *  lines so an earlier action's message does not sit under a fresh
     *  irreversibility warning. */
    public fun openTransferStep() {
        mutableMembershipError.value = null
        mutableMembershipNotice.value = null
        mutableTransferPick.value = null
        mutableTransferOutgoing.value = me.lakko.app.chat.PRESELECTED_OUTGOING_OWNER
        mutableTransferStepOpen.value = true
    }

    /** §6.1.5 — `Cancel` is a PRE-PUBLISH exit, and it is the last one. Nothing
     *  here is a withdraw: once published, the proposal is in every member's
     *  store and dies only when some commit lands. */
    public fun cancelTransferStep() {
        mutableTransferStepOpen.value = false
    }

    public fun setTransferPick(signatureKeyB64: String) {
        mutableTransferPick.value = signatureKeyB64
    }

    public fun setTransferOutgoing(outgoing: OutgoingOwner) {
        mutableTransferOutgoing.value = outgoing
    }

    /**
     * ADR-0045 UX §6.1 — publish the ownership transfer, then sweep it.
     *
     * The shape is [onToggleAdmin]'s, deliberately and for its stated reason: a
     * role change is owner-only and the owner is rank 0, so the sweep that
     * follows is this same device and in practice the change is immediate. A
     * transfer IS a role change — not a new commit type, just the owner-only
     * role-table rewrite with the owner slot moved — so anything else here would
     * be a second, differently-shaped route into membership.
     *
     * The sweep is driven from HERE rather than from the repository because
     * `sweepIfCommitter` takes the same per-group lock the publish does and a
     * `Mutex` is not reentrant; the two must run in turn, not nested.
     *
     * The outcome copy names the transferor's own new role only when the sweep
     * actually committed: half the panel changes under them in one tap, and a
     * request that has not landed must not claim it has.
     *
     * [outgoing] has no default. The core refuses to default it because either
     * silent choice is wrong in one direction, and this signature keeps that
     * property rather than undoing with a parameter default what the core
     * protected with a type.
     */
    public fun onTransferOwnership(
        successor: me.lakko.app.chat.TransferCandidate,
        outgoing: OutgoingOwner,
    ) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        mutableMembershipNotice.value = null
        viewModelScope.launch {
            try {
                val result = withMembershipRetry(
                    me.lakko.app.chat.RetryableRequest.TransferOwnership(
                        subjectKeyB64 = successor.signatureKeyB64,
                        outgoing = outgoing,
                    ),
                ) {
                    repo.transferGroupOwnership(
                        groupIdHex,
                        me.lakko.app.chat.outboxUnb64(successor.signatureKeyB64),
                        outgoing,
                    )
                }
                // Closed only now — the proposal is on the wire, so there is
                // nothing left to reconsider. A throw above leaves the step open
                // with the pick and the role choice intact.
                mutableTransferStepOpen.value = false
                val swept = repo.sweepIfCommitter(groupIdHex)
                val landed = result is me.lakko.app.chat.MembershipOutcome.Committed ||
                    swept is me.lakko.app.chat.SweepOutcome.Committed
                mutableMembershipNotice.value = me.lakko.app.chat.transferOutcomeNotice(
                    outcome = if (landed) {
                        me.lakko.app.chat.TransferOutcome.COMMITTED
                    } else {
                        me.lakko.app.chat.TransferOutcome.PROPOSED
                    },
                    successorLabel = successor.label,
                    outgoing = outgoing,
                )
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onTransferOwnership", "chat.error.transfer")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0045 S4b — one committer-liveness beacon tick.
     *
     * Driven by the chat screen on a `repeatOnLifecycle(RESUMED)` loop, which is
     * this platform's reading of "foregrounded": the coroutine simply does not
     * run while the app is stopped. The flag is passed through as well rather
     * than assumed, so the rule the emit gate enforces is written down in one
     * place and is testable — and so a future caller that is NOT lifecycle-scoped
     * cannot accidentally inherit a beacon it has no right to send.
     *
     * Best-effort: nobody is watching a beacon, and a failed tick only means this
     * device does not claim the committer duty on this pass.
     */
    public suspend fun emitCommitterBeaconTick(foreground: Boolean = true) {
        try {
            repo.emitCommitterBeaconIfDue(groupIdHex, foreground)
        } catch (_: Throwable) {
            // best-effort
        }
    }

    /** ADR-0053 — the pending set the current attribution map was read for,
     *  and the deferred re-evaluation when the per-group floor held a read. */
    private var joinAttributionsFetchedFor: Set<String>? = null
    private var deferredJoinAttributions: kotlinx.coroutines.Job? = null

    /** ADR-0028 OQ2 (G-3) — refresh the advisory join-attribution map for the
     *  current pending requests. Best-effort; owner/admin only (the role-gated
     *  approval gate is the boundary). ADR-0053 — the read covers the whole
     *  knock queue, so it runs only when the pending set changed and at most
     *  once per 10 s per group ([me.lakko.app.chat.joinAttributionDecision]). */
    private fun refreshJoinAttributions() {
        if (!mutableCanManageMembers.value) {
            deferredJoinAttributions?.cancel()
            joinAttributionsFetchedFor = null
            mutableJoinAttributions.value = emptyMap()
            return
        }
        val pending = mutablePendingJoins.value.map(::joinerKeyHex).toSet()
        val now = System.currentTimeMillis()
        when (
            val decision = me.lakko.app.chat.joinAttributionDecision(
                pending = pending,
                fetchedFor = joinAttributionsFetchedFor,
                lastFetchMs = me.lakko.app.chat.JoinAttributionReads.lastFetchMs(groupIdHex),
                nowMs = now,
            )
        ) {
            me.lakko.app.chat.JoinAttributionDecision.Clear -> {
                deferredJoinAttributions?.cancel()
                joinAttributionsFetchedFor = null
                mutableJoinAttributions.value = emptyMap()
            }
            me.lakko.app.chat.JoinAttributionDecision.Keep -> Unit
            is me.lakko.app.chat.JoinAttributionDecision.Defer -> {
                if (deferredJoinAttributions?.isActive == true) return
                deferredJoinAttributions = viewModelScope.launch {
                    kotlinx.coroutines.delay(decision.waitMs)
                    refreshJoinAttributions()
                }
            }
            me.lakko.app.chat.JoinAttributionDecision.Fetch -> {
                deferredJoinAttributions?.cancel()
                me.lakko.app.chat.JoinAttributionReads.markFetched(groupIdHex, now)
                joinAttributionsFetchedFor = pending
                viewModelScope.launch {
                    val attrs = repo.fetchJoinAttributions(groupIdHex)
                    mutableJoinAttributions.value = attrs.associate { a ->
                        joinerKeyHex(a.joinerSignatureKey) to
                            JoinAttributionUi(
                                issuerFingerprint = a.issuerFingerprint,
                                issuerInRoster = a.issuerInRoster,
                                issuerKeyB64 = a.issuerSignatureKey
                                    .takeIf { it.isNotEmpty() }
                                    ?.let { me.lakko.app.chat.outboxB64(it) },
                            )
                    }
                }
            }
        }
    }

    private fun joinerKeyHex(key: ByteArray): String =
        key.joinToString("") { b -> "%02x".format(b.toInt() and 0xff) }

    /** Our own leaf index — the roster marks it "(you)" and blocks self-removal. */
    public fun ownLeafIndex(): UInt? = repo.ownLeafIndex(groupIdHex)

    /**
     * Human label for a roster member: a matched contact's chosen name (or its
     * fingerprint), else a short fingerprint of the signature key. "(you)" is
     * appended for the local leaf. Mirrors the ADR-0021 call-roster resolution,
     * keyed on the member's signature public key.
     */
    public fun memberLabel(member: GroupMember): String {
        // The local leaf is not in the contact directory, so the resolver
        // would fall back to "Member"; the row is simply "You".
        if (ownLeafIndex() == member.leafIndex) return Localization.strings.value["chat.author.you"]
        // ADR-0048 UX §3 — the one resolver: rename, profile name, verified
        // @username, fingerprint (never `@` + hex).
        return profileNames.resolve(member.signaturePublicKey).listLabel
    }

    /**
     * ADR-0048 — the resolved name for a member IDENTITY (signature key,
     * base64) — message author labels, reactions, read receipts. The name
     * alone, as chat bubbles show it (UX §3).
     */
    public fun nameOf(identity: me.lakko.app.chat.MemberIdentity): String =
        profileNames.resolve(identity).primary

    /**
     * ADR-0052 decision 2 / UX 5.1 "Tap to jump" — where a forwarded
     * message's attribution takes the person, or `null` for nowhere: the
     * origin conversation and message id when this device holds both. The
     * rule is [me.lakko.app.chat.forwardJumpTarget]; this reads it over the
     * repository's current threads.
     */
    public fun forwardJumpTarget(forward: me.lakko.app.chat.ForwardRef): Pair<String, Long>? =
        me.lakko.app.chat.forwardJumpTarget(forward, repo.state.value.threads)

    /**
     * ADR-0048 UX §5 — the chat header title: in a two-member chat the OTHER
     * person's resolved name; in a group `null` (the group label is unchanged).
     * `null` too while the roster cannot be read.
     */
    public fun oneToOnePeerName(): String? =
        repo.oneToOnePeerKey(groupIdHex)?.let { profileNames.resolve(it).primary }

    /**
     * UX 5.2 — the other person's VERIFIED username in a two-member chat (the
     * header's second line), or `null` when there is none or this is a group.
     */
    public fun oneToOnePeerUsername(): String? =
        repo.oneToOnePeerKey(groupIdHex)?.let { profileNames.resolve(it).verifiedUsername }

    /**
     * docs/ux/conversation-info.md section 1 — the other person's signature
     * key in a two-member chat, base64url, in the encoding the contact screen
     * is keyed on. `null` for a group, or before the roster reads.
     *
     * A leaf key names one of that person's DEVICES (ADR-0050), so this is the
     * weaker of the two ways to find their contact record and
     * `conversationInfo` tries the chat first.
     */
    public fun oneToOnePeerRouteKey(): String? =
        repo.oneToOnePeerKey(groupIdHex)?.let { me.lakko.app.auth.base64UrlEncodeNoPad(it) }

    /**
     * UX section 3 — the conversation kind, read from the group (ADR-0062):
     * fixed by its creator, the same on every member's device.
     */
    public fun conversationKind(): me.lakko.app.chat.ConversationKind =
        repo.conversationKind(groupIdHex)

    /**
     * ADR-0045 / UX §5.2 — the identity treatment for a PROSPECTIVE member: a
     * matched contact's chosen name, else a short fingerprint of the signature
     * key the Add proposal carries.
     *
     * Deliberately NOT [memberLabel]: the subject of a pending add is not in the
     * roster at all — that is the whole point of giving it its own section rather
     * than a roster row — so there is no `GroupMember` to read, and no "(you)"
     * case (nobody sees a request to add themselves to a group they are already
     * in).
     */
    /**
     * ADR-0053 — the label of a pending join request: the contact's name
     * resolved through the joiner's ACCOUNT key (a request is keyed on the
     * device leaf key, contacts on the account key); the existing leaf-key
     * label for a joiner this device does not know.
     */
    public fun pendingJoinLabel(joinerSignatureKey: ByteArray): String =
        me.lakko.app.chat.pendingJoinLabel(
            accountKey = repo.pendingExternalJoinAccountKey(groupIdHex, joinerSignatureKey),
            leafKey = joinerSignatureKey,
            knownName = { key -> profileNames.resolve(key).takeIf { !it.isPlaceholder }?.listLabel },
            fallbackLabel = { key -> nameOfKeyB64(me.lakko.app.chat.outboxB64(key)) },
        )

    public fun nameOfKeyB64(keyB64: String?): String {
        val key = try {
            keyB64?.let { android.util.Base64.decode(it, android.util.Base64.DEFAULT) }
        } catch (_: Throwable) {
            null
        } ?: return Localization.strings.value["chat.members.unnamed"]
        // ADR-0048 — a prospective member resolves like anyone else: a profile
        // is only known if one reached this device, so this is usually the
        // fingerprint (without `@`).
        return profileNames.resolve(key).listLabel
    }

    /**
     * Resolve the typed `@username` (G-4-verified) and add it to this group:
     * publish the add commit to the journal (own-marked) + ferry the Welcome.
     * Fail-closed — a resolve / verify / publish failure surfaces an error and
     * leaves the group untouched.
     */
    public fun onAddMember() {
        val typed = mutableAddMemberInput.value
        if (typed.isBlank() || mutableMembershipBusy.value) return
        // ADR-0048 §4 — a name failing the rule is never looked up.
        val username = when (val decision = me.lakko.app.profile.searchDecision(typed, validateUsername)) {
            is me.lakko.app.profile.SearchDecision.Refuse -> {
                mutableMembershipError.value = decision.text
                return
            }
            is me.lakko.app.profile.SearchDecision.Lookup -> decision.normalized
        }
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                val (accountId, signaturePubkey) = resolveRecipient(username)
                // ADR-0045 UX §7 — the descriptor is only knowable once the
                // typed handle has resolved, which is why it is built here and
                // not before: re-resolving a username later could name a
                // different account if the handle has since been
                // re-registered, and adding the wrong person is not a retry.
                withMembershipRetry(
                    me.lakko.app.chat.RetryableRequest.Add(
                        subjectKeyB64 = me.lakko.app.chat.outboxB64(signaturePubkey),
                        subjectAccountIdB64 = me.lakko.app.chat.outboxB64(accountId),
                    ),
                ) { repo.addMemberToGroup(groupIdHex, accountId, signaturePubkey) }
                mutableAddMemberInput.value = ""
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onAddMember", "chat.error.add_member")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * Remove a member by leaf index: publish the removal commit to the journal
     * (own-marked). Refuses to remove the local leaf. Fail-closed on error.
     */
    public fun onRemoveMember(leafIndex: UInt) {
        if (ownLeafIndex() == leafIndex) {
            mutableMembershipError.value = UiText("chat.refusal.fallback")
            return
        }
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                // ADR-0045 UX §7 — armed against the member's SIGNATURE key,
                // resolved now and re-resolved to a leaf at fire time. A torn
                // roster read means no retry for this attempt rather than a
                // retry against a guess.
                val targetKeyB64 = repo.members(groupIdHex)
                    ?.firstOrNull { it.leafIndex == leafIndex }
                    ?.let { me.lakko.app.chat.outboxB64(it.signaturePublicKey) }
                if (targetKeyB64 == null) {
                    repo.removeMemberFromGroup(groupIdHex, leafIndex)
                } else {
                    withMembershipRetry(
                        me.lakko.app.chat.RetryableRequest.Remove(targetKeyB64),
                    ) { repo.removeMemberFromGroup(groupIdHex, leafIndex) }
                }
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onRemoveMember", "chat.error.remove_member")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0045 — whether to offer `Leave group`, and whether it is live.
     *
     * One rule, two consumers: `MembersSheet` renders from
     * [me.lakko.app.chat.leaveGroupAffordance] over the state it has already
     * collected (a Compose read, so it recomposes), and [onLeaveGroup] re-asks
     * here before publishing. A hidden control is not a guarantee — [isOwner] is
     * only as fresh as the last roster refresh — so the action must not assume
     * the gate it was drawn behind still holds.
     *
     * `roleTable != null` IS the proposal-path predicate: `ChatRepository`'s own
     * `usesProposalPath` is `group.groupRoles() != null` under the same
     * catch-Throwable-to-null, and [groupRoles] is that read.
     */
    internal fun leaveAffordance(): me.lakko.app.chat.LeaveGroupAffordance =
        me.lakko.app.chat.leaveGroupAffordance(
            usesProposalPath = mutableRoleTable.value != null,
            isOwner = mutableIsOwner.value,
            ownLeaf = ownLeafIndex(),
            busy = mutableMembershipBusy.value,
            requests = mutablePendingRequests.value,
        )

    /**
     * ADR-0045 — ask to leave the group.
     *
     * Reachable for the first time because of that ADR: naming this device's OWN
     * leaf in a Remove proposal needs no role, where ADR-0022 had recorded
     * self-leave as impossible because OpenMLS `CannotRemoveSelf` stops a member
     * committing their own departure. A proposal is not a commit, so there is
     * nothing to commit and nothing to refuse.
     *
     * Deliberately NOT routed through [onRemoveMember], whose own-leaf guard is
     * still correct for the direct-commit path a table-less group takes.
     *
     * The caller is expected to have confirmed first: a published request cannot
     * be withdrawn — there is no wire withdraw, and evicting it locally would
     * leave it live in every peer's store — so the confirmation is the last exit
     * and this method is past it.
     */
    public fun onLeaveGroup() {
        when (val affordance = leaveAffordance()) {
            is me.lakko.app.chat.LeaveGroupAffordance.Hidden -> {
                // Reached only if the gate went stale between draw and tap.
                // Answer in this feature's own vocabulary rather than letting
                // the core's prose surface to a user who never asked about
                // proposals.
                mutableMembershipError.value = affordance.reason.message
                return
            }
            // Busy, or we have already asked. Asking twice would put two
            // identical Remove proposals into one sweep.
            me.lakko.app.chat.LeaveGroupAffordance.Disabled -> return
            me.lakko.app.chat.LeaveGroupAffordance.Enabled -> Unit
        }
        val leaf = ownLeafIndex() ?: return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        mutableMembershipNotice.value = null
        viewModelScope.launch {
            try {
                // The repository records the own-request observation itself, so
                // unlike the web reference there is nothing to do here afterwards
                // but re-read the roster — which must still contain us, annotated.
                // ADR-0045 UX §7 — keyed on our own SIGNATURE key, never on
                // the leaf: OpenMLS hands a vacated leaf to the next member
                // added, so a retry armed against a leaf could come to name
                // somebody else entirely.
                val ownKeyB64 = ownSignatureKeyB64()
                if (ownKeyB64 == null) {
                    repo.removeMemberFromGroup(groupIdHex, leaf)
                } else {
                    withMembershipRetry(
                        me.lakko.app.chat.RetryableRequest.Leave(ownKeyB64),
                    ) { repo.removeMemberFromGroup(groupIdHex, leaf) }
                }
                // ADR-0045 UX §6.2.3 — the ONE thing that later tells
                // "You left this group." from "You are no longer a member of
                // this group.", and it has to be its own durable field rather
                // than a read of the observation the repository just wrote.
                // That observation lives under `remove:{ownLeaf}` and is pruned
                // the moment the core stops holding the request — which is
                // exactly what the sweep that removes us does. The poll batch
                // carrying the eviction therefore destroys the only evidence
                // that we asked for it, and the order of those two steps is an
                // implementation detail no test pins.
                //
                // Written AFTER the publish returns: an unpublished request is
                // not a leave. Fail-safe when absent — the fallback string is
                // true either way.
                repo.noteOwnLeaveRequested(groupIdHex)
                leaveRequestedAtMs = repo.leaveRequestedAt(groupIdHex)
                mutableMembershipNotice.value = UiText("chat.leave.requested")
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onLeaveGroup", "chat.error.leave")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0045 UX §6.2.4 — `Delete this chat`.
     *
     * Reachable only from the exit block's `Ended` arm, so this never removes a
     * live conversation. It is also the ONE control in this whole design that
     * destroys user data on request: everything else in §6.2 removes a claim
     * the UI should not have been making, this removes a transcript that exists
     * nowhere else, on one confirmation, with no undo and no export. The
     * confirmation says so in as many words, and that is the only protection
     * there is.
     *
     * Synchronous, so the caller can navigate away on the next line: the
     * repository's work is local (a message-log rewrite plus four
     * `SharedPreferences` rows) and the push subscription is already gone by
     * this point — the §6.2.8 teardown retires it at the moment of the
     * eviction, not at the moment of the delete, so a user who never presses
     * this button still stops being woken.
     */
    public fun onDeleteChat() {
        repo.deleteEndedChat(groupIdHex)
    }

    /**
     * Promote / demote a member as admin (owner only — the toggle is shown only
     * when [isOwner]). Publishes the `setGroupRoles` commit. Fail-closed.
     */
    public fun onToggleAdmin(member: GroupMember, makeAdmin: Boolean) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                withMembershipRetry(
                    me.lakko.app.chat.RetryableRequest.SetAdmin(
                        subjectKeyB64 = me.lakko.app.chat.outboxB64(member.signaturePublicKey),
                        makeAdmin = makeAdmin,
                    ),
                ) { repo.setMemberAdmin(groupIdHex, member.signaturePublicKey, makeAdmin) }
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onToggleAdmin", "chat.error.change_admin")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0027 (G-3), on the ADR-0045 proposal path since S7 — approve a pending
     * external-join request (owner/admin only). Publishes ONE artifact, an
     * ordinary admin-authored `Add` proposal; there is no commit, no epoch
     * advance and no Welcome here. The joiner is admitted when a committer
     * sweeps the proposal, which is what posts the `WelcomeRendezvous` they are
     * polling for — and ADR-0045 decision 2 gives that step no deadline, so the
     * notice must not promise it has happened.
     *
     * The sweep is attempted immediately so the common single-admin case still
     * reads as "tap, done"; when this device is standing down it simply does not
     * fire, and the copy says so. Fail-closed on error.
     */
    public fun onApproveJoin(proposalRef: ByteArray) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        mutableMembershipNotice.value = null
        viewModelScope.launch {
            try {
                // ADR-0045 UX §7 — an approve refused by a full pending set now
                // carries its reason (the core stopped flattening it into prose
                // in `21f6c8c`), so it re-sends itself when a committer drains
                // the queue. The wrapper also disarms first, so tapping Approve
                // again takes the record rather than racing an in-flight
                // re-send for it.
                withApproveJoinRetry(proposalRef) {
                    repo.approveExternalJoin(groupIdHex, proposalRef)
                }
                val swept = repo.sweepIfCommitter(groupIdHex)
                mutableMembershipNotice.value = if (swept is me.lakko.app.chat.SweepOutcome.Committed) {
                    UiText("chat.members.join.approved_added")
                } else {
                    // ADR-0045 S7 residual 2 — an approve that did not apply has
                    // grown a queue only a sweep drains, and the core's own
                    // feedback (its cap refusal) lands on whoever proposes NEXT.
                    // Naming the depth here puts it in front of the person who
                    // caused it. Read from the core, not from `pendingRequests`,
                    // which has not been refreshed yet.
                    val queue = me.lakko.app.chat.waitingQueueLine(
                        repo.pendingProposalCount(groupIdHex),
                    )
                    val approved = UiText("chat.members.join.approved_requested")
                    if (queue == null) {
                        approved
                    } else {
                        UiText("common.sentence_pair", parts = mapOf("first" to approved, "second" to queue))
                    }
                }
                // Acting on the queue is what may have made room for the
                // request the quarantine turned away, so the notice about it
                // stops being current.
                repo.clearUnkeptExternalJoin(groupIdHex)
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onApproveJoin", "chat.error.approve_join")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0027 (G-3) — reject a pending external-join request (owner/admin only).
     * Local-only drop of the retained proposal; no commit / journal post.
     * Fail-closed on error.
     */
    /**
     * ADR-0045 S7 residual 1 — dismiss one `Not applied` row, by
     * [me.lakko.app.chat.UnappliedRequest.rowKey]. It stays dismissed.
     */
    public fun onDismissNotApplied(rowKey: String) {
        repo.dismissNotApplied(groupIdHex, rowKey)
        mutableNotApplied.value = repo.notAppliedRequests(groupIdHex)
    }

    public fun onRejectJoin(proposalRef: ByteArray) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                // ADR-0045 UX §7 — a reject is the user saying no, so any armed
                // re-send of an approve for the same request goes with it. The
                // verdict would drop it anyway once it saw the emptied
                // quarantine (`JOIN_REQUEST_GONE`), but leaving a record that
                // contradicts a decision the user just made is not a state to
                // keep for a poll cycle.
                disarmRetry(
                    me.lakko.app.chat.RetryableRequest.ApproveJoin(
                        me.lakko.app.chat.outboxB64(proposalRef),
                    ),
                )
                repo.rejectExternalJoin(groupIdHex, proposalRef)
                // Same reason as in `onApproveJoin`: the queue moved.
                repo.clearUnkeptExternalJoin(groupIdHex)
                refreshMembers()
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onRejectJoin", "chat.error.reject_join")
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0028 (G-3) — mint a group invite-link (owner/admin only) and publish
     * it to [inviteLink] for the members sheet to show + copy. Fail-closed.
     */
    public fun onCreateInviteLink() {
        mutableMembershipError.value = null
        mutableMembershipNotice.value = null
        viewModelScope.launch {
            try {
                mutableInviteLink.value = repo.createGroupInviteLink(groupIdHex)
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onCreateInviteLink", "chat.error.create_link")
            }
        }
    }

    /**
     * ADR-0028 revoke — revoke the currently-shown group invite link (issuer
     * only). A pointer link carries a ledger blob the server deletes; an inline
     * link has nothing to revoke (we just clear the field). Fail-closed.
     */
    public fun onRevokeInviteLink() {
        val link = mutableInviteLink.value ?: return
        mutableMembershipError.value = null
        mutableMembershipNotice.value = null
        viewModelScope.launch {
            try {
                val revoked = repo.revokeGroupInviteLink(link)
                mutableInviteLink.value = null
                mutableMembershipNotice.value = inviteLinkRevokeNotice(revoked)
            } catch (e: Throwable) {
                reportMembershipFailure(e, "onRevokeInviteLink", "chat.error.revoke_link")
            }
        }
    }

    private val mutableCallNotice: MutableStateFlow<UiText?> = MutableStateFlow(null)

    /** A call-related message for the user — why a call could not start or
     *  join. `null` when there is nothing to say. Cleared by [dismissCallNotice]. */
    public val callNotice: StateFlow<UiText?> = mutableCallNotice.asStateFlow()

    private val mutableCallNoticeOffersEnd: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** The [callNotice] is a busy refusal: offer to end the call occupying the
     *  slot, since the refusal is otherwise the whole outcome. */
    public val callNoticeOffersEndCurrentCall: StateFlow<Boolean> = mutableCallNoticeOffersEnd.asStateFlow()

    public fun dismissCallNotice() {
        mutableCallNotice.value = null
        mutableCallNoticeOffersEnd.value = false
    }

    /** Surface a call failure the user would otherwise only see as nothing
     *  happening. */
    public fun showCallNotice(message: UiText) {
        mutableCallNoticeOffersEnd.value = false
        mutableCallNotice.value = message
    }

    /** The busy notice's "End current call". */
    public fun onEndCurrentCall() {
        dismissCallNotice()
        endCurrentCall()
    }

    private fun showBusyNotice(verb: me.lakko.app.calls.CallVerb) {
        val occupant = callOccupant.value
        if (occupant == null) {
            showCallNotice(UiText("chat.call.busy"))
            return
        }
        mutableCallNotice.value = me.lakko.app.calls.callBusyMessage(occupant, verb)
        mutableCallNoticeOffersEnd.value = true
    }

    /**
     * The header call buttons. A live group call on this chat is JOINED, never
     * duplicated by a second start; otherwise a group chat starts a group call
     * and a two-member chat a 1x1 call. The caller has already obtained the
     * microphone (and, for [video], camera) permission.
     */
    public fun onCallButton(video: Boolean) {
        val live = liveGroupCallRecord()
        when (me.lakko.app.calls.callButtonAction(live, isGroupChat)) {
            me.lakko.app.calls.CallButtonAction.JOIN_LIVE_GROUP_CALL -> joinGroupCall(video)
            me.lakko.app.calls.CallButtonAction.START_GROUP_CALL -> startGroupCall(video)
            me.lakko.app.calls.CallButtonAction.START_ONE_TO_ONE_CALL ->
                startCall(audio = true, video = video)
        }
    }

    private fun liveGroupCallRecord(): me.lakko.app.calls.GroupCallRecord? {
        val record = repo.groupCalls.value[groupIdHex] ?: return null
        val ignored = me.lakko.app.calls.groupCallIgnoredDeviceKey(
            record,
            repo.ownGroupCallMemberKey(),
            groupCallHost?.slot?.value?.callIdHex,
        )
        return record.takeIf {
            me.lakko.app.calls.isGroupCallLive(it, System.currentTimeMillis(), ignored)
        }
    }

    /**
     * Start a multi-party SFU group call for this (group) chat and announce it,
     * through the app-level host: it mints the admission JWT, derives the
     * per-call SFrame seed from the MLS exporter, publishes `GroupCallStart` and
     * connects. Refused while any call occupies the slot; fails closed (a notice,
     * no call) on any missing prerequisite (security invariant #4).
     */
    public fun startGroupCall(video: Boolean = false) {
        requestGroupCall(video = video, joinCallIdHex = null)
    }

    /**
     * Join the live group call recorded for this chat (the banner's Join and the
     * header buttons while a call is in progress). A call that stopped being
     * live between the tap and the host's check is simply not joined — silently
     * starting a different call would surprise the user.
     */
    public fun joinGroupCall(video: Boolean = false) {
        val record = liveGroupCallRecord() ?: run {
            showCallNotice(UiText("chat.call.group_ended"))
            return
        }
        requestGroupCall(video = video, joinCallIdHex = record.callIdHex)
    }

    private fun requestGroupCall(video: Boolean, joinCallIdHex: String?) {
        val host = groupCallHost ?: run {
            showCallNotice(UiText("chat.call.unavailable"))
            return
        }
        viewModelScope.launch {
            when (val outcome = host.start(groupIdHex, video = video, joinCallIdHex = joinCallIdHex)) {
                GroupCallStartOutcome.Connected,
                GroupCallStartOutcome.AlreadyInThisCall,
                -> Unit
                is GroupCallStartOutcome.Busy -> {
                    mutableCallNotice.value = outcome.message
                    mutableCallNoticeOffersEnd.value = true
                }
                GroupCallStartOutcome.Ended -> showCallNotice(UiText("chat.call.group_ended"))
                is GroupCallStartOutcome.Failed -> showCallNotice(outcome.message)
            }
        }
    }

    // -------------------------------------------------------------
    // Phase 2a3e2b-Android — cross-device draft sync
    // -------------------------------------------------------------

    /** Attached via [attachSync] from the screen. `null` until
     *  the screen lifecycle wires it in. */
    private var deviceGroupSync: DeviceGroupSyncRepository? = null
    /** Raw group_id bytes for the active thread — cached so
     *  the broadcast / seed paths don't re-decode the hex
     *  string on every keystroke. */
    private var groupIdBytes: ByteArray? = null
    /** Last draft text the broadcast path published — dedups
     *  echoes that loop back through the poll loop. */
    private var lastBroadcastDraft: String? = null
    private var draftBroadcastJob: Job? = null
    private var didSeedDraft: Boolean = false
    private val DRAFT_DEBOUNCE_MS: Long = 500L

    /** Phase 2a3e2c-Android — read-cursor broadcast tracker.
     *  Highest broadcast messageId; the per-`messages` effect
     *  schedules a fresh `ReadCursor` payload only when the
     *  newest visible row's id exceeds this value. */
    private var lastBroadcastReadId: Long = -1L
    private var readCursorJob: Job? = null
    private val READ_CURSOR_DEBOUNCE_MS: Long = 1500L

    // -------------------------------------------------------------
    // ADR-0024 — typing indicator + read-receipt SEND state
    // -------------------------------------------------------------

    /** ADR-0024 — wall-clock time (millis) we last published a
     *  `typing:true`; the throttle suppresses a fresh `true` until
     *  [TYPING_SEND_THROTTLE_MS] has elapsed. */
    private var lastTypingTrueSentMs: Long = 0L
    /** `true` between the first throttled `typing:true` and the
     *  matching `typing:false`; lets us send `false` exactly once
     *  on blur / send (no `false` spam when nothing is in flight). */
    private var typingActive: Boolean = false
    private val TYPING_SEND_THROTTLE_MS: Long = 4_000L

    /** ADR-0024 — our own last-PUBLISHED read-receipt watermark.
     *  We re-publish `read_up_to` only when the highest received
     *  messageId advances past this (monotone + de-duped). */
    private var lastPublishedReadReceipt: Long = -1L
    private var readReceiptJob: Job? = null
    private val READ_RECEIPT_DEBOUNCE_MS: Long = 1_200L

    /**
     * Bind to the process-scoped [DeviceGroupSyncRepository]
     * (Phase 2a3e2b). Called from the Compose ChatScreen on
     * `LaunchedEffect(Unit)`. Subscribes to:
     *
     *   * incoming `Draft` payloads keyed on this thread —
     *     seed the composer once on first mount when local
     *     is empty (avoids stomping mid-typing on this
     *     device).
     *   * local `draft` changes — debounce 500 ms then
     *     broadcast.
     *
     * Idempotent: a second call replaces the prior reference
     * but the in-flight collectors keep running.
     */
    public fun attachSync(sync: DeviceGroupSyncRepository) {
        if (deviceGroupSync === sync) return
        deviceGroupSync = sync
        val gid = hexToBytes(groupIdHex) ?: return
        groupIdBytes = gid

        viewModelScope.launch {
            sync.drafts.collect { drafts ->
                if (didSeedDraft) return@collect
                val incoming = drafts[groupIdHex] ?: return@collect
                if (composer.value.draft.isEmpty()) {
                    composer.update { it.copy(draft = incoming.text) }
                }
                didSeedDraft = true
            }
        }

        viewModelScope.launch {
            composer
                .asStateFlow()
                .distinctUntilChanged { a, b -> a.draft == b.draft }
                .collect { snapshot ->
                    scheduleDraftBroadcast(snapshot.draft, gid)
                }
        }

        // Phase 2a3e2c-Android — read-cursor broadcast. Every
        // time the repo's per-thread message list grows past
        // the previously-broadcast value, schedule a fresh
        // `ReadCursor` payload pointing at the newest visible
        // message-id. Debounced 1500 ms.
        viewModelScope.launch {
            repo.state
                .distinctUntilChanged { a, b ->
                    val ar = a.threads.firstOrNull { it.groupIdHex == groupIdHex }
                    val br = b.threads.firstOrNull { it.groupIdHex == groupIdHex }
                    ar?.messages?.size == br?.messages?.size
                }
                .collect { state ->
                    val thread = state.threads.firstOrNull {
                        it.groupIdHex == groupIdHex
                    } ?: return@collect
                    scheduleReadCursorBroadcast(
                        messages = thread.messages,
                        groupIdBytes = gid,
                    )
                }
        }
    }

    private fun scheduleReadCursorBroadcast(
        messages: List<ChatMessage>,
        groupIdBytes: ByteArray,
    ) {
        if (messages.isEmpty()) return
        // Find the highest server-assigned messageId. Outgoing
        // rows with messageId == 0 (pre-ack) get skipped — the
        // ack path triggers a fresh effect.
        var highest: Long = -1L
        for (m in messages) {
            if (m.messageId > highest) highest = m.messageId
        }
        if (highest <= lastBroadcastReadId) return
        val target = highest
        readCursorJob?.cancel()
        readCursorJob = viewModelScope.launch {
            delay(READ_CURSOR_DEBOUNCE_MS)
            lastBroadcastReadId = target
            // 8 big-endian bytes — wire shape stable across
            // platforms.
            val idBytes = ByteArray(8)
            var v = target
            for (i in 7 downTo 0) {
                idBytes[i] = (v and 0xff).toByte()
                v = v ushr 8
            }
            val now = System.currentTimeMillis().toULong()
            try {
                deviceGroupSync?.setReadCursor(
                    groupId = groupIdBytes,
                    upToMessageId = idBytes,
                    readAtMs = now,
                )
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "ChatViewModel",
                    "setReadCursor failed",
                    cause,
                )
            }
        }
    }

    /** ADR-0024 — the highest server-assigned messageId in this
     *  thread (the read-receipt watermark candidate). `-1` when
     *  there is no acked row yet (a pre-ack outgoing row has
     *  messageId == 0 and is skipped — the ack path re-triggers). */
    private fun highestMessageIdFor(repoState: me.lakko.app.chat.ChatRepositoryState): Long {
        val thread = repoState.threads.firstOrNull { it.groupIdHex == groupIdHex }
            ?: return -1L
        var highest = -1L
        for (m in thread.messages) {
            if (m.messageId > highest) highest = m.messageId
        }
        return highest
    }

    /**
     * ADR-0024 — publish a read receipt when [highest] advances
     * past our own last-published watermark. Monotone (a lower /
     * equal value is a no-op) + throttled (debounced
     * [READ_RECEIPT_DEBOUNCE_MS] so a burst of incoming rows
     * collapses to one publish). Tracks [lastPublishedReadReceipt]
     * so re-sync / repo re-emissions never re-publish.
     */
    private fun scheduleReadReceipt(highest: Long) {
        if (highest <= 0L) return
        if (highest <= lastPublishedReadReceipt) return
        val target = highest
        readReceiptJob?.cancel()
        readReceiptJob = viewModelScope.launch {
            delay(READ_RECEIPT_DEBOUNCE_MS)
            // Re-check under the post-delay state — a newer publish
            // may have landed while we waited.
            if (target <= lastPublishedReadReceipt) return@launch
            lastPublishedReadReceipt = target
            repo.sendReadReceipt(groupIdHex = groupIdHex, upTo = target)
        }
    }

    private fun scheduleDraftBroadcast(text: String, gid: ByteArray) {
        if (text == lastBroadcastDraft) return
        draftBroadcastJob?.cancel()
        draftBroadcastJob = viewModelScope.launch {
            delay(DRAFT_DEBOUNCE_MS)
            lastBroadcastDraft = text
            try {
                deviceGroupSync?.setDraft(groupId = gid, text = text)
            } catch (cause: Throwable) {
                // Best-effort — the next keystroke retries.
                android.util.Log.w(
                    "ChatViewModel",
                    "setDraft failed",
                    cause,
                )
            }
        }
    }

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        var i = 0
        while (i < hex.length) {
            val hi = Character.digit(hex[i], 16)
            val lo = Character.digit(hex[i + 1], 16)
            if (hi < 0 || lo < 0) return null
            out[i / 2] = ((hi shl 4) or lo).toByte()
            i += 2
        }
        return out
    }

    /**
     * ADR-0024 — render the typing indicator line from the set of members
     * currently typing, or `null` when nobody is.
     *
     *   * We are never shown ("you typing" is noise).
     *   * 1×1 (a single other typist) → a bare "typing…" (the
     *     name is redundant in a two-person chat).
     *   * Exactly one named other in a group → "<name> is typing…"
     *     resolved via the same contact / fingerprint resolution
     *     the roster uses ([memberLabel], stripped of the "(you)"
     *     suffix it never carries for a non-self leaf).
     *   * More than one → "Several people are typing…".
     */
    private fun typingIndicatorText(
        typingMembers: Set<me.lakko.app.chat.MemberIdentity>,
    ): UiText? {
        if (typingMembers.isEmpty()) return null
        // The LIVE roster read, not the last-refreshed members snapshot: this
        // runs on every state emission, including before the members sheet has
        // ever been opened.
        val own = repo.ownMemberIdentity(groupIdHex)
        val others = typingMembers.filter { it != own }
        if (others.isEmpty()) return null
        // 1×1 (ADR-0062 kind): the only other person is the typist, so a
        // single other typist needs no name. A two-person GROUP still names.
        val isOneToOne = repo.conversationKind(groupIdHex) == me.lakko.app.chat.ConversationKind.Chat
        if (others.size == 1) {
            if (isOneToOne) return UiText("chat.typing.one_to_one")
            val name = labelForIdentity(others.first())
            return if (name != null) {
                UiText("chat.typing.named", texts = mapOf("name" to name))
            } else {
                UiText("chat.typing.one_to_one")
            }
        }
        return UiText("chat.typing.several")
    }

    /** ADR-0024 — resolve a single member IDENTITY to its label (contact name
     *  or fingerprint), or `null` when that member is not in the live roster.
     *  Reuses [memberLabel].
     *
     *  Keyed on the signature key rather than the leaf so a member who left
     *  cannot be relabelled with the name of whoever took their slot. */
    private fun labelForIdentity(identity: me.lakko.app.chat.MemberIdentity): String? {
        val member = repo.members(groupIdHex)?.firstOrNull {
            me.lakko.app.chat.outboxB64(it.signaturePublicKey) == identity
        } ?: return null
        // The name alone — a typing line is not a list row (UX §3).
        return profileNames.resolve(member.signaturePublicKey).primary
    }

    /** The roster reads one repository emission needs, taken together off the
     *  main thread. */
    private class LiveGroupRead(
        val typingText: UiText?,
        val otherIdentities: Set<me.lakko.app.chat.MemberIdentity>,
        val isActive: Boolean?,
    )

    /** The typing line for [repoState]. Reads the live roster: call it off
     *  the main thread. */
    private fun typingTextFor(repoState: me.lakko.app.chat.ChatRepositoryState): UiText? =
        typingIndicatorText(
            repoState.threads.firstOrNull { it.groupIdHex == groupIdHex }?.typingMembers ?: emptySet(),
        )

    private fun buildState(
        repoState: me.lakko.app.chat.ChatRepositoryState,
        composer: ComposerState,
        typingText: UiText?,
    ): ChatScreenUiState {
        val thread: ChatThread? =
            repoState.threads.firstOrNull { it.groupIdHex == groupIdHex }
        return ChatScreenUiState(
            groupIdHex = groupIdHex,
            messages = thread?.messages ?: emptyList(),
            draft = composer.draft,
            sending = composer.sending,
            sendError = composer.sendError,
            pendingAttachments = composer.pendingAttachments,
            uploading = composer.inFlight.isNotEmpty(),
            inFlightUploads = composer.inFlight,
            pendingPreviews = composer.pendingPreviews,
            // ADR-0024 — typing indicator + read-state surface.
            typingText = typingText,
            readUpToByMember = thread?.readUpToByMember ?: emptyMap(),
            // We always render the screen as "exists" — if the
            // repo doesn't carry this thread (no Group handle
            // built yet, post-restart pre-bootstrap), the
            // messages list is just empty and the composer
            // still works (sendMessage will throw with a clear
            // error which we surface as `sendError`).
            threadKnown = thread != null,
        )
    }

    public fun onDraftChange(value: String) {
        composer.update { it.copy(draft = value) }
        // ADR-0024 — a non-empty edit means the user is actively
        // typing: publish `typing:true` (throttled to at most once
        // per ~4 s). Clearing the field to empty is treated as a
        // stop (publish `typing:false` once).
        if (value.isNotEmpty()) {
            maybeSendTypingTrue()
        } else {
            sendTypingStop()
        }
    }

    /**
     * ADR-0024 — publish a throttled `typing:true`. At most one
     * `true` per [TYPING_SEND_THROTTLE_MS]; a live typist's
     * keystrokes inside that window collapse to a single signal.
     * Marks [typingActive] so the matching `false` fires exactly
     * once on blur / send.
     */
    private fun maybeSendTypingTrue() {
        val now = System.currentTimeMillis()
        if (typingActive && now - lastTypingTrueSentMs < TYPING_SEND_THROTTLE_MS) return
        lastTypingTrueSentMs = now
        typingActive = true
        viewModelScope.launch {
            repo.sendTyping(groupIdHex = groupIdHex, isTyping = true)
        }
    }

    /**
     * ADR-0024 — publish `typing:false` exactly once (best-effort
     * clear). A no-op when no `typing:true` is outstanding, so a
     * blur / send / field-clear can never spam `false`.
     */
    private fun sendTypingStop() {
        if (!typingActive) return
        typingActive = false
        viewModelScope.launch {
            repo.sendTyping(groupIdHex = groupIdHex, isTyping = false)
        }
    }

    /**
     * ADR-0024 — the composer's focus changed. On focus-loss we
     * clear any outstanding typing signal (`typing:false` once).
     * The screen wires this through `onFocusChanged`.
     */
    public fun onComposerFocusChanged(focused: Boolean) {
        if (!focused) sendTypingStop()
    }

    public fun onSendClicked() {
        val snapshot = composer.value
        if (snapshot.sending) return
        val trimmed = snapshot.draft.trim()
        val stagedAttachments = snapshot.pendingAttachments
        // 1.19c4: attachment-only messages are valid (empty
        // text + ≥1 attachment); pre-1.19 text-only messages
        // still valid (≥1 char text + zero attachments). The
        // only invalid input is the all-empty composer.
        if (trimmed.isEmpty() && stagedAttachments.isEmpty()) return

        // ADR-0024 — sending a message clears any outstanding
        // typing signal (`typing:false` once).
        sendTypingStop()

        // ADR-0052 — the reply goes with this message and the strip clears
        // now, for the same reason the draft does: the quote lives on the
        // row from this moment, and a failed send keeps it there with Retry.
        val reply = takeArmedReply()

        // Cleared HERE rather than on success, because the message is now in
        // the conversation from this moment: the words live in the row, which
        // keeps them and offers Retry if the send does not land. They used to
        // be cleared only on success - which was right while a failure left
        // nothing in the conversation, and is wrong now that it would leave
        // the same text in two places.
        composer.update {
            it.copy(
                sending = true,
                sendError = null,
                draft = "",
                pendingAttachments = emptyList(),
                pendingPreviews = emptyMap(),
            )
        }
        viewModelScope.launch {
            try {
                repo.sendMessage(
                    groupIdHex = groupIdHex,
                    plaintext = trimmed,
                    attachments = stagedAttachments,
                    reply = reply,
                )
                composer.update { it.copy(sending = false, sendError = null) }
            } catch (err: Throwable) {
                // One sentence; the action is on the row, next to the message
                // it belongs to. Same words as the web client
                // (`chat/send-failure.ts`) and iOS. A refusal that names
                // itself keeps its own words - "you are no longer a member of
                // this group" is the difference between retrying usefully and
                // retrying forever.
                composer.update {
                    it.copy(
                        sending = false,
                        sendError = chatError(err, "chat.send.failed"),
                    )
                }
            }
        }
    }

    /** Send a message that did not get there. Drives the row's Retry. */
    public fun onRetrySend(localId: Long) {
        composer.update { it.copy(sendError = null) }
        viewModelScope.launch {
            try {
                repo.retrySend(groupIdHex = groupIdHex, localId = localId)
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = chatError(err, "chat.send.failed")) }
            }
        }
    }

    /**
     * ADR-0023 — edit one of OUR OWN messages. Composes + publishes
     * the EDIT op + locally replaces the target's text (the DS
     * never echoes our own application messages back). A blank /
     * unchanged edit is a no-op. Failures surface through
     * [ChatScreenUiState.sendError].
     */
    public fun onEditMessage(targetMessageId: Long, newText: String) {
        val trimmed = newText.trim()
        if (trimmed.isEmpty()) return
        viewModelScope.launch {
            try {
                repo.editMessage(
                    groupIdHex = groupIdHex,
                    targetMessageId = targetMessageId,
                    newText = trimmed,
                )
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = chatError(err, "chat.error.edit")) }
                android.util.Log.w("ChatViewModel", "onEditMessage failed", err)
            }
        }
    }

    /**
     * ADR-0023 — delete one of OUR OWN messages. Composes +
     * publishes the DELETE op + locally tombstones the target.
     */
    public fun onDeleteMessage(targetMessageId: Long) {
        viewModelScope.launch {
            try {
                repo.deleteMessage(
                    groupIdHex = groupIdHex,
                    targetMessageId = targetMessageId,
                )
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = chatError(err, "chat.error.delete_message")) }
                android.util.Log.w("ChatViewModel", "onDeleteMessage failed", err)
            }
        }
    }

    /**
     * ADR-0023 — react to ANY message with [emoji]. Composes +
     * publishes the REACTION op + locally toggles our own
     * `(ownLeaf, emoji)` on the target. The FFI compose rejects an
     * empty / over-long emoji.
     */
    public fun onReactToMessage(targetMessageId: Long, emoji: String) {
        // ADR-0047 §3 — a reaction is exactly one emoji grapheme, checked
        // before anything is composed (the wire only bounds the byte length).
        if (!me.lakko.app.media.isSingleEmojiGrapheme(emoji)) return
        viewModelScope.launch {
            try {
                repo.reactToMessage(
                    groupIdHex = groupIdHex,
                    targetMessageId = targetMessageId,
                    emoji = emoji,
                )
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = chatError(err, "chat.error.react")) }
                android.util.Log.w("ChatViewModel", "onReactToMessage failed", err)
            }
        }
    }

    /** ADR-0023 — our own IDENTITY in this chat group, used by the UI to
     *  render which reaction chips we contributed (and can toggle off). `null`
     *  when there is no live handle.
     *
     *  Not our leaf: a leaf compared against a stored reactor would start
     *  matching a stranger the moment somebody is added into a slot we once
     *  held. */
    public fun ownChatIdentity(): me.lakko.app.chat.MemberIdentity? =
        repo.ownMemberIdentity(groupIdHex)

    /** ADR-0024 — the live roster's member IDENTITIES minus our own. The
     *  Bubble's read-state ("Read" only when EVERY other member has read it) is
     *  gated on every member in this set. Empty when there is no live handle /
     *  no other member. */
    public fun otherChatIdentities(): Set<me.lakko.app.chat.MemberIdentity> {
        val own = repo.ownMemberIdentity(groupIdHex)
        return repo.members(groupIdHex)
            ?.map { me.lakko.app.chat.outboxB64(it.signaturePublicKey) }
            ?.filter { it != own }
            ?.toSet()
            ?: emptySet()
    }

    /** Serializes picked-item uploads: every item shows its chip at once,
     *  but the encrypt + PUT runs one at a time so the chips stay honest. */
    private val attachmentUploadLock = kotlinx.coroutines.sync.Mutex()
    private val nextPickId = java.util.concurrent.atomic.AtomicLong(1)

    /** Preview decoding runs here, detached from the pick coroutine, so a
     *  native decoder that never returns cannot hold the send (UX spec §10). */
    private val visualPrepScope = kotlinx.coroutines.CoroutineScope(
        kotlinx.coroutines.SupervisorJob() + kotlinx.coroutines.Dispatchers.IO,
    )

    /**
     * UX spec §10 — attach picked items. Each item gets its upload chip
     * IMMEDIATELY, before it is read or previewed, and leaves the tray only as
     * a staged attachment or with a readable error; never silently.
     */
    public fun onPickedUris(uris: List<android.net.Uri>) {
        for (uri in uris) onPickedUri(uri)
    }

    private fun onPickedUri(uri: android.net.Uri) {
        val id = nextPickId.getAndIncrement()
        val placeholder = Localization.strings.value[me.lakko.app.media.PICKED_ITEM_PLACEHOLDER_KEY]
        addInFlight(UploadProgress(id = id, fileName = placeholder, stage = null))
        viewModelScope.launch {
            val read = try {
                withContext(kotlinx.coroutines.Dispatchers.IO) {
                    me.lakko.app.media.readPickedUri(repo.context, uri, AttachmentBridge.MAX_ATTACHMENT_BYTES)
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                removeInFlight(id)
                throw e
            }
            when (read) {
                is me.lakko.app.media.PickedRead.Failed -> failPick(id, read.name, read.error)
                is me.lakko.app.media.PickedRead.Ok -> {
                    val payload = read.payload
                    updateInFlight(id) { it.copy(fileName = payload.name ?: placeholder) }
                    val visual = prepareVisualBounded(payload.bytes, payload.mime)
                    updateInFlight(id) { it.copy(previewBytes = visual?.thumb) }
                    uploadPicked(id, payload.bytes, payload.mime, payload.name, visual)
                }
            }
        }
    }

    /**
     * Drive the encrypt + upload flow for one item whose bytes are already
     * read. Kept for callers that resolve the bytes themselves.
     */
    public fun onAttachmentPicked(
        bytes: ByteArray,
        mime: String?,
        name: String?,
        /** UX spec §10 — pixel size and thumb for a photo or video; `null`
         *  for a plain file. */
        visual: me.lakko.app.media.VisualPrep? = null,
    ) {
        val id = nextPickId.getAndIncrement()
        addInFlight(
            UploadProgress(
                id = id,
                fileName = name?.takeUnless { it.isEmpty() } ?: Localization.strings.value[me.lakko.app.media.PICKED_ITEM_PLACEHOLDER_KEY],
                stage = null,
                previewBytes = visual?.thumb,
            ),
        )
        viewModelScope.launch { uploadPicked(id, bytes, mime, name, visual) }
    }

    /** The §10 size + thumb, bounded by [me.lakko.app.media.VISUAL_PREP_TIMEOUT_MS];
     *  on timeout the item is sent without them. */
    private suspend fun prepareVisualBounded(bytes: ByteArray, mime: String?): me.lakko.app.media.VisualPrep? {
        if (me.lakko.app.media.inlineKindOf(mime, me.lakko.app.media.coreAllowlist) == null) return null
        val scratch = repo.context.cacheDir
        val prep = me.lakko.app.media.awaitBounded(visualPrepScope, me.lakko.app.media.VISUAL_PREP_TIMEOUT_MS) {
            me.lakko.app.media.prepareVisualMedia(bytes, mime, scratch)
        }
        if (prep == null) {
            android.util.Log.w("ChatViewModel", "preview preparation timed out or failed; sending without it")
        }
        return prep
    }

    private suspend fun uploadPicked(
        id: Long,
        bytes: ByteArray,
        mime: String?,
        name: String?,
        visual: me.lakko.app.media.VisualPrep?,
    ) {
        try {
            attachmentUploadLock.withLock {
                // UX spec §10 — the thumb goes up first so the ref can carry
                // it. A thumb that fails to upload is dropped; the send goes
                // ahead without it.
                val thumbRef = visual?.thumb?.let { thumb ->
                    try {
                        AttachmentBridge.encryptAndUploadThumb(
                            bytes = thumb,
                            mime = "image/jpeg",
                            width = visual.thumbSize?.width?.toUInt(),
                            height = visual.thumbSize?.height?.toUInt(),
                        )
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        android.util.Log.w("ChatViewModel", "thumb upload failed: ${e.javaClass.simpleName}")
                        null
                    }
                }
                val meta = if (visual != null && (visual.size != null || thumbRef != null)) {
                    AttachmentMediaMeta(
                        width = visual.size?.width?.toUInt(),
                        height = visual.size?.height?.toUInt(),
                        thumb = thumbRef,
                    )
                } else {
                    null
                }
                val ref = AttachmentBridge.encryptAndUpload(
                    bytes = bytes,
                    mime = mime,
                    name = name,
                    media = meta,
                    onStage = { stage -> updateInFlight(id) { it.copy(stage = stage) } },
                )
                composer.update { state ->
                    val thumb = visual?.thumb
                    state.copy(
                        inFlight = state.inFlight.filterNot { it.id == id },
                        pendingAttachments = state.pendingAttachments + ref,
                        pendingPreviews = if (thumb != null) {
                            state.pendingPreviews + (AttachmentBridge.hex(ref.blobId) to thumb)
                        } else {
                            state.pendingPreviews
                        },
                    )
                }
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            removeInFlight(id)
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("ChatViewModel", "attachment failed: ${e.javaClass.simpleName}")
            failPick(id, name, e)
        }
    }

    private fun addInFlight(item: UploadProgress) {
        composer.update { it.copy(inFlight = it.inFlight + item, sendError = null) }
    }

    private fun updateInFlight(id: Long, change: (UploadProgress) -> UploadProgress) {
        composer.update { state ->
            state.copy(inFlight = state.inFlight.map { if (it.id == id) change(it) else it })
        }
    }

    private fun removeInFlight(id: Long) {
        composer.update { state -> state.copy(inFlight = state.inFlight.filterNot { it.id == id }) }
    }

    private fun failPick(id: Long, name: String?, error: Throwable?) {
        composer.update { state ->
            state.copy(
                inFlight = state.inFlight.filterNot { it.id == id },
                sendError = me.lakko.app.media.attachFailureMessage(name, error),
            )
        }
    }

    /** Drop a staged attachment from the composer's pending
     *  tray. Index matches the order in
     *  [ChatScreenUiState.pendingAttachments]. */
    public fun onRemovePendingAttachment(index: Int) {
        composer.update {
            if (index !in it.pendingAttachments.indices) it
            else it.copy(
                pendingAttachments = it.pendingAttachments.toMutableList().apply {
                    removeAt(index)
                },
                pendingPreviews = it.pendingPreviews - AttachmentBridge.hex(it.pendingAttachments[index].blobId),
            )
        }
    }

    // -------------------------------------------------------------
    // ADR-0047 — voice messages, video notes, GIFs
    // -------------------------------------------------------------

    /**
     * Outgoing media still in flight. Each is its own message, sent the moment
     * it is finished; the bubble shows an upload ring and, on failure,
     * `Not sent · Retry`. Held by the view model so a rotation does not lose a
     * take mid-upload.
     */
    private val mediaQueue: me.lakko.app.media.MediaSendQueue =
        me.lakko.app.media.MediaSendQueue(
            scope = viewModelScope,
            uploadThumb = { poster ->
                AttachmentBridge.encryptAndUploadThumb(
                    bytes = poster.bytes,
                    mime = poster.mime,
                    width = poster.width.toUInt(),
                    height = poster.height.toUInt(),
                )
            },
            uploadMain = { bytes, mime, name, meta, onProgress ->
                AttachmentBridge.encryptAndUpload(
                    bytes = bytes,
                    mime = mime,
                    name = name,
                    media = meta,
                    onProgress = onProgress,
                )
            },
            publish = { ref, reply ->
                repo.sendMessage(
                    groupIdHex = groupIdHex,
                    plaintext = "",
                    attachments = listOf(ref),
                    reply = reply,
                )
            },
            onLog = { line -> android.util.Log.w("ChatViewModel", line) },
        )

    public val pendingMedia: StateFlow<List<me.lakko.app.media.PendingMediaRow>> = mediaQueue.rows

    /** A finished voice message or video note the user chose to send. */
    public fun sendRecordedTake(take: me.lakko.app.media.RecordedTake) {
        val stamp = System.currentTimeMillis()
        val kind = if (take.kind == me.lakko.app.media.CaptureKind.VOICE) {
            me.lakko.app.media.KIND_VOICE
        } else {
            me.lakko.app.media.KIND_VIDEO_NOTE
        }
        val name = if (kind == me.lakko.app.media.KIND_VOICE) "voice-$stamp.m4a" else "video-note-$stamp.mp4"
        // The take was recorded inside the chat screen's media cache, which is
        // deleted when the screen closes — possibly before the upload reads it.
        // A rename (same filesystem, no copy) moves it out of that lifetime;
        // the upload reads it once and deletes it.
        val outbox = java.io.File(repo.context.cacheDir, MEDIA_OUTBOX_DIR).apply { mkdirs() }
        val moved = java.io.File(outbox, java.util.UUID.randomUUID().toString())
        val file = if (take.file.renameTo(moved)) moved else take.file
        mediaQueue.enqueue(
            me.lakko.app.media.MediaSendQueue.Job(
                tempId = mediaQueue.newTempId(),
                kind = kind,
                mime = take.mime,
                name = name,
                durationMs = take.durationMs,
                waveform = take.waveform,
                width = take.width,
                height = take.height,
                poster = take.poster,
                // Section 3.1 — "Recording a voice message with a reply armed
                // sends the take as the reply; the strip clears the same way."
                reply = takeArmedReply(),
                loadBytes = {
                    kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                        try {
                            if (file.length() > AttachmentBridge.MAX_ATTACHMENT_BYTES) {
                                null
                            } else {
                                file.readBytes() to take.mime
                            }
                        } catch (_: java.io.IOException) {
                            null
                        } finally {
                            // Read once: the job keeps the bytes for a retry.
                            file.delete()
                        }
                    }
                },
            ),
        )
    }

    /** UX spec §5 — a tapped GIF is sent immediately: downloaded through the
     *  proxy, encrypted, uploaded like any file. Recipients never contact the
     *  proxy or GIPHY. */
    public fun sendGif(item: me.lakko.app.media.GifItem) {
        mediaQueue.enqueue(
            me.lakko.app.media.MediaSendQueue.Job(
                tempId = mediaQueue.newTempId(),
                kind = me.lakko.app.media.KIND_GIF,
                mime = "image/gif",
                name = "gif-${System.currentTimeMillis()}.gif",
                durationMs = null,
                waveform = null,
                width = item.width,
                height = item.height,
                poster = null,
                // A tapped GIF is a sent message like a take; it goes as the
                // reply and clears the strip the same way.
                reply = takeArmedReply(),
                loadBytes = {
                    when (val r = gifClient.media(item.fullUrl, me.lakko.app.media.MAX_GIF_FULL_BYTES)) {
                        is me.lakko.app.media.GifResult.Ok -> r.value.bytes to r.value.mime
                        else -> null
                    }
                },
            ),
        )
    }

    public fun retryMediaSend(tempId: Long): Unit = mediaQueue.retry(tempId)

    public fun discardMediaSend(tempId: Long): Unit = mediaQueue.discard(tempId)

    /** The GIF proxy client, bound to this account's session token. Built on
     *  first use, so opening a chat that never shows the GIF tab builds none. */
    private val gifClientLazy: Lazy<me.lakko.app.media.GifClient> = lazy {
        me.lakko.app.media.GifClient(sessionToken = sessionTokenProvider)
    }
    private val gifClient: me.lakko.app.media.GifClient by gifClientLazy

    /** The GIF tab's state for this chat. A proxy that answers "not
     *  configured" hides the tab for the life of this view model. */
    public val gifPicker: me.lakko.app.media.GifPickerModel by lazy {
        me.lakko.app.media.GifPickerModel(
            scope = viewModelScope,
            fetch = { query, offset -> gifClient.page(query, offset) },
        )
    }

    private val gifPreviewCache = object : LinkedHashMap<String, ByteArray>(32, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, ByteArray>?): Boolean =
            size > GIF_PREVIEW_CACHE_ENTRIES
    }

    /** A picker tile's preview, through the proxy (never GIPHY's CDN), kept in
     *  a small in-memory cache for scrolling back. `null` on failure: the tile
     *  stays a neutral placeholder. */
    public suspend fun gifPreview(url: String): ByteArray? {
        synchronized(gifPreviewCache) { gifPreviewCache[url] }?.let { return it }
        val result = gifClient.media(url, me.lakko.app.media.MAX_GIF_PREVIEW_BYTES)
        val bytes = (result as? me.lakko.app.media.GifResult.Ok)?.value?.bytes ?: return null
        synchronized(gifPreviewCache) { gifPreviewCache[url] = bytes }
        return bytes
    }

    /** One immediate (non-waiting) poll of this group — the first tick of the
     *  screen's receive loop. Receiving is otherwise automatic; there is no
     *  Refresh button. */
    public fun pollNow() {
        viewModelScope.launch { repo.pollGroup(groupIdHex) }
    }

    /**
     * ADR-0040 (G-35) — run the durable pending-commit outbox sweep for this
     * thread ONCE on open, BEFORE the first poll. Re-publishes any commit that
     * crashed mid-window on a previous run (re-arming the own-echo filter so the
     * first poll drops the re-fetched commit instead of hitting
     * `CannotDecryptOwnMessage`) and discards a torn-write orphan. Best-effort —
     * a failure leaves the entries for the next open's sweep.
     */
    public suspend fun sweepPendingCommitsOnOpen() {
        try {
            repo.sweepPendingCommits(groupIdHex)
        } catch (_: Throwable) {
            // best-effort — the next open retries.
        }
    }

    /**
     * One long-poll tick for the per-thread continuous-polling
     * loop the composable drives via
     * `LaunchedEffect(Unit) { while (isActive) { pollOnce(...) } }`.
     */
    /** What the poll did; see [GroupPollOutcome]. Joins a poll already in
     *  flight rather than returning at once, so the loop never spins. */
    public suspend fun pollOnceLongPoll(waitSecs: Int = DEFAULT_LONG_POLL_SECS): GroupPollOutcome =
        repo.pollGroup(groupIdHex, waitSecs = waitSecs, joinInFlight = true)

    /**
     * ADR-0045 UX §6.2.8 item 4 — may this screen offer a call?
     *
     * The per-call SFrame seed comes from `exportSecret("lakko/call/v1", …)`,
     * which is gated on `is_active()` and now raises, so the button fails
     * closed — which is correct, and is not enough. A button that always errors
     * is the shape §6.1.1 rejects, and §6.2.11 forbids any live indicator on a
     * dead group for a stronger reason: a call control asserts a live E2EE
     * session, and there is not one.
     */
    public fun callsAvailable(): Boolean = !membershipEnded.value

    // -------------------------------------------------------------
    // Construction — keep this the LAST thing before the companion.
    // -------------------------------------------------------------

    /**
     * Start every collector, AFTER every property of this class is assigned.
     *
     * The position of this block is the whole fix, so it must stay below the
     * last property declaration. Kotlin runs property initialisers and `init`
     * blocks in textual order, and the collectors do not wait for the
     * constructor to finish: `viewModelScope` runs on
     * `Dispatchers.Main.immediate`, a ViewModel is built on the main thread
     * where that dispatcher needs no dispatch, so each `launch` starts
     * executing inline, and `StateFlow.collect` hands over its current value
     * before its first suspension. A collector started any earlier reads the
     * properties declared below it while they are still a JVM `null` behind a
     * non-null type.
     *
     * That is exactly what used to happen: the collectors lived in an `init`
     * block near the top of the class and read the group-call state then kept
     * here (declared hundreds of lines further down) on their first
     * emission. The NPE was thrown inside the coroutine, reached the main
     * thread's uncaught-exception handler, and killed the process every time a
     * chat was opened. Moving the reads one by one is not enough — the retry
     * evaluation a collector calls reaches `mutableMembershipBusy` and
     * `retryStore`, and the read-receipt path reaches properties near the end
     * of the class, where an unassigned primitive silently reads as 0 rather
     * than failing. Starting everything here removes the whole class of bug.
     *
     * Pinned by `ChatViewModelConstructionTest`.
     */
    init {
        startThreadCollectors()
        startGroupCallCollectors()
    }

    override fun onCleared() {
        visualPrepScope.cancel()
        super.onCleared()
        // Every call deliberately outlives the chat it started in (it lives in
        // the app-level call host), so nothing here touches one.
        // Only close a client that was actually built.
        if (gifClientLazy.isInitialized()) gifClient.close()
    }

    public companion object {
        /** DS long-poll budget — same value as
         *  [me.lakko.app.ui.contacts.ContactsViewModel]. */
        public const val DEFAULT_LONG_POLL_SECS: Int = 30

        /** Picker previews kept in memory; each is at most 2 MiB and in
         *  practice a few hundred KiB. */
        private const val GIF_PREVIEW_CACHE_ENTRIES: Int = 48

        /** Finished takes waiting to be read by their upload (see
         *  [sendRecordedTake]). */
        internal const val MEDIA_OUTBOX_DIR: String = "chat-media-outbox"
    }

    /** VM-local composer + transient flags. Persistent timeline
     *  data lives in [ChatRepository]. */
    private data class ComposerState(
        val draft: String = "",
        val sending: Boolean = false,
        val sendError: UiText? = null,
        /** Phase 1.19c4 — attachments staged in the composer.
         *  The user picks a file / photo; each call to
         *  [ChatViewModel.onAttachmentPicked] encrypts +
         *  uploads to storage immediately so the ref is ready
         *  by the time Send is tapped. Cleared on successful
         *  Send, on per-chip remove, and on screen disposal
         *  (ViewModel.onCleared). */
        val pendingAttachments: List<AttachmentRef> = emptyList(),
        /** Every picked item not yet staged: being read, previewed,
         *  encrypted or uploaded. Send is disabled while non-empty. */
        val inFlight: List<UploadProgress> = emptyList(),
        /** UX spec §10 — thumbs of staged photos / videos, keyed by the
         *  hex blob id, for the composer chips. */
        val pendingPreviews: Map<String, ByteArray> = emptyMap(),
    )
}

/** Polish 1.x — composer-tray progress chip payload.
 *  Mirror of the iOS `ChatViewModel.UploadStage` enum and the
 *  web client's `uploadProgress` signal. */
public data class UploadProgress(
    /** Identifies one picked item across its stages. */
    val id: Long = 0,
    /** Display name of the file currently being processed. */
    val fileName: String,
    /** Which pipeline stage the bridge is in; `null` while the item is
     *  still being read and previewed. */
    val stage: UploadStage?,
    /** UX spec §10 — the photo / video thumb shown in the upload chip. */
    val previewBytes: ByteArray? = null,
)

/** Combined snapshot the [ChatScreen] renders. */
public data class ChatScreenUiState(
    val groupIdHex: String,
    val messages: List<ChatMessage>,
    val draft: String,
    val sending: Boolean,
    val sendError: UiText?,
    /** 1.19c4 — staged attachments below the text composer. */
    val pendingAttachments: List<AttachmentRef> = emptyList(),
    /** 1.19c4 — `true` while a picked item is being encrypted
     *  + uploaded. */
    val uploading: Boolean = false,
    /** Every picked item still in flight, in pick order. */
    val inFlightUploads: List<UploadProgress> = emptyList(),
    /** UX spec §10 — thumbs of staged attachments, by hex blob id. */
    val pendingPreviews: Map<String, ByteArray> = emptyMap(),
    /** `false` when the repository has no thread for this
     *  groupIdHex — e.g. immediately after process restart
     *  before snapshot rehydration has rebuilt the per-group
     *  handles. The UI still renders the composer (sendMessage
     *  surfaces a clear error if the handle is missing). */
    val threadKnown: Boolean,
    /** ADR-0024 — the typing-indicator line ("<name> is typing…" /
     *  "typing…" / "Several people are typing…"), or `null` when
     *  nobody is typing. Rendered just above the input bar. */
    val typingText: UiText? = null,
    /** ADR-0024 — the monotone per-MEMBER read watermark, keyed on the
     *  member's signature key. The Bubble renders "Read" on an OWN message once
     *  EVERY other member's watermark `>=` that message's id (else "Sent"). */
    val readUpToByMember: Map<me.lakko.app.chat.MemberIdentity, Long> = emptyMap(),
)

/** ADR-0024 — read state of an OWN outgoing message, derived from
 *  the per-leaf read watermarks. [None] for a not-yet-acked row
 *  (messageId == 0). */
public enum class OwnReadState { None, Sent, Read }

/**
 * ADR-0024 — compute the read state of an OWN message.
 *
 *   * [OwnReadState.None] when the message has no real DS id yet
 *     (messageId == 0) — no "Sent"/"Read" tick is shown.
 *   * [OwnReadState.Read] when EVERY OTHER member's read watermark
 *     `>=` this message's id (the whole group has read it).
 *   * [OwnReadState.Sent] otherwise.
 *
 * [own] (excluded from the all-others check) + [others] (the live roster minus
 * self) are supplied by the screen so the helper is pure + unit-test-friendly.
 *
 * Every one of the three is a [me.lakko.app.chat.MemberIdentity] — a signature
 * key — and not an MLS leaf. The watermark map is monotone and never reset on a
 * membership change, so under leaf keys a member added into a departed
 * member's vacated slot inherited that member's watermark outright, and this
 * function then answered [OwnReadState.Read] for a message the newcomer had
 * never seen.
 */
public fun ownReadStateFor(
    messageId: Long,
    own: me.lakko.app.chat.MemberIdentity?,
    others: Set<me.lakko.app.chat.MemberIdentity>,
    readUpToByMember: Map<me.lakko.app.chat.MemberIdentity, Long>,
): OwnReadState {
    if (messageId == 0L) return OwnReadState.None
    val peers = others.filter { it != own }
    if (peers.isEmpty()) return OwnReadState.Sent
    val allRead = peers.all { who ->
        (readUpToByMember[who] ?: -1L) >= messageId
    }
    return if (allRead) OwnReadState.Read else OwnReadState.Sent
}

/** How often the Join banner re-checks a shown call's liveness. The windows it
 *  checks are a minute and four hours wide, so this is precision, not load. */
/** The banner's re-read tick, and how often it asks the SFU whether the call
 *  it offers is still there ([me.lakko.app.calls.SFU_CALL_STATE_PROBE_MS]). */
private const val GROUP_CALL_BANNER_TICK_MS: Long = me.lakko.app.calls.SFU_CALL_STATE_PROBE_MS

/** The chat's Join banner: a live group call this device is not in. */
public data class JoinableGroupCall(
    val callIdHex: String,
    /** Distinct people (person keys) with a fresh device in the call at the last tick. */
    val joinedCount: Int,
    /** The initiator announced a video call. */
    val video: Boolean,
)

/**
 * What the group info says after Revoke on its invite link, the web rule
 * (`Chat.tsx` `onRevokeInviteLink`): a pointer link's ledger row was deleted,
 * so the link no longer works (`revoked`); an inline sealed link (the
 * censored-region fallback) has no server record, so the field was only
 * cleared (`cleared`). [revoked] is what `ChatRepository.revokeGroupInviteLink`
 * returns.
 */
internal fun inviteLinkRevokeNotice(revoked: Boolean): UiText =
    if (revoked) UiText("chat.invite_link.revoked") else UiText("chat.invite_link.cleared")
