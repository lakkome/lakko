// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.peerProfileOf
import me.lakko.app.profile.toFfi
import kotlinx.coroutines.CancellationException
import io.ktor.client.request.get
import io.ktor.client.statement.bodyAsText
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.callRingWindowMs
import uniffi.lakko_ffi_uniffi.composeCallSignaling
import uniffi.lakko_ffi_uniffi.sfuRoomForCall

/**
 * The process-wide call host: the one call slot, shared by the 1x1 call
 * ([instance]), the group (SFU) call ([group]) and the call-link call ([link]).
 *
 * A process object rather than anything owned by a screen or a view model: a
 * call must outlive leaving the screen it started on, keep ringing on any
 * screen, and survive an activity recreate. `MainActivity` attaches the current
 * [ChatRepository] ([attach]) and draws every call from its root.
 *
 * The three hosts are one slot: each asks the others through `otherOccupant`
 * before starting ([callSlotOccupant]), and the group and link hosts tell the
 * 1x1 host when they occupy it, so an incoming 1x1 offer during a group or link
 * call is answered `End{busy}`.
 *
 * Runs on the main dispatcher: the sessions mutate their state flows there and
 * their entry points were always called from the main thread.
 */
public object AppCallHost {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    private const val GROUP_SLOT_KEY = "group"
    private const val LINK_SLOT_KEY = "link"

    public val instance: OneToOneCallHost<CallSession> by lazy {
        OneToOneCallHost(
            scope = scope,
            // The ring window from its one home
            // (`crates/lakko-ui-rules/src/ring.rs`), not a literal restated
            // here: the caller's phone and this one have to give up at the
            // same moment, or one files a missed call while the other rings.
            timing = OneToOneCallTiming(ringTimeoutMs = callRingWindowMs().toLong()),
        )
    }

    /**
     * `docs/ux/calls.md` section 4.1.2 — the line that says the call this
     * device was in was ended for everyone, for a few seconds after it
     * happened. One for the slot: the group and the link host both report here.
     */
    public val endedForEveryone: EndedForEveryoneNotice by lazy { EndedForEveryoneNotice(scope) }

    public val group: GroupCallHost by lazy {
        GroupCallHost(
            scope = scope,
            otherOccupant = {
                when {
                    instance.hasCall -> CallKind.ONE_TO_ONE
                    link.occupiesSlot -> CallKind.LINK
                    else -> null
                }
            },
            onSlotChanged = { occupied -> instance.setOtherCallActive(GROUP_SLOT_KEY, occupied) },
            notifyEndedForEveryone = { name -> endedForEveryone.show(name) },
            notifyNoLongerInGroup = { endedForEveryone.showLine(UiText("call.ended.no_longer_in_group")) },
        )
    }

    public val link: LinkCallHost by lazy {
        LinkCallHost(
            scope = scope,
            otherOccupant = {
                when {
                    instance.hasCall -> CallKind.ONE_TO_ONE
                    group.occupiesSlot -> CallKind.GROUP
                    else -> null
                }
            },
            onSlotChanged = { occupied -> instance.setOtherCallActive(LINK_SLOT_KEY, occupied) },
            notifyEndedForEveryone = { name -> endedForEveryone.show(name) },
        )
    }

    /** The call occupying the slot, as the UI observes it. */
    @OptIn(ExperimentalCoroutinesApi::class)
    public val occupant: StateFlow<CallKind?> by lazy {
        val oneToOneInCall = instance.active.flatMapLatest { call ->
            call?.session?.state?.map { it != CallSession.WebCallState.Done } ?: flowOf(false)
        }
        combine(oneToOneInCall, group.slot, link.slot) { oneToOne, groupSlot, linkSlot ->
            callSlotOccupant(
                oneToOneInCall = oneToOne,
                groupCallActive = groupSlot != null,
                linkCallActive = linkSlot != null && linkCallOccupiesSlot(linkSlot.phase),
            )
        }.stateIn(scope, SharingStarted.Eagerly, null)
    }

    /** The call occupying the slot right now (not waiting for a flow emission). */
    public fun occupantNow(): CallKind? = callSlotOccupant(
        oneToOneInCall = instance.hasCall,
        groupCallActive = group.occupiesSlot,
        linkCallActive = link.occupiesSlot,
    )

    private var idleSubscription: (() -> Unit)? = null

    /** Dropped when another repository attaches. */
    private var membershipEndSubscription: (() -> Unit)? = null

    /**
     * Install the platform effects for the current activity's repository. An
     * activity recreate builds a new repository, so this runs each time and
     * replaces the previous wiring.
     */
    public fun attach(
        applicationContext: Context,
        repository: ChatRepository,
        sessionTokenProvider: () -> String?,
    ) {
        instance.attach(
            ChatRepositoryCallPort(applicationContext, repository, sessionTokenProvider))
        // docs/ux/calls.md section 2.1 — the phone rings for an offer this
        // path delivers, not only for one a push wake carries. On the test
        // stack, which has no FCM credentials, this is the ONLY path that runs.
        IncomingCallRinger.attach(applicationContext)
        instance.attachRinger(IncomingCallRinger)
        repository.oneToOneCallInbound = instance::deliver
        repository.oneToOneScreenShareInbound = instance::deliverScreenShare
        repository.oneToOneMovedSink = instance::conversationMoved
        instance.callTarget = repository::oneToOneCallTargetFor
        // docs/ux/calls.md section 4.1.2 — the starter of the group call this
        // device is in ended it for everyone.
        repository.groupCallEndedForEveryone = group::onEndedForEveryone
        group.attach(ChatRepositoryGroupCallPort(applicationContext, repository, scope, sessionTokenProvider))
        link.attach(MultiJoinerLinkCallPort(applicationContext, scope))
        idleSubscription?.invoke()
        // G-18 / ADR-0029 — the hosted group call rekeys whenever its group's
        // MLS work settles, whichever receive path applied it.
        idleSubscription = repository.addGroupMlsIdleListener { groupIdHex -> group.onGroupSettled(groupIdHex) }
        // ADR-0087 UX 9.1 - this account is out of the group a call is in.
        membershipEndSubscription?.invoke()
        membershipEndSubscription = repository.addMembershipEndListener { groupIdHex ->
            group.onMembershipEnded(groupIdHex)
        }
    }

    /**
     * `docs/ux/calls.md` section 4.5 — put the call in the slot away, or bring
     * it back. Routed by kind here because the presentation belongs to the
     * call, and there is exactly one call.
     *
     * Neither touches the call: not the media, not the keys, not the timer. The
     * flag lives on the host that owns the call (cleared when a new call takes
     * the slot), so a destination change cannot reach it and a collapse
     * survives every navigation for as long as the call does.
     */
    public fun collapseCurrentCall() {
        when (occupantNow()) {
            CallKind.ONE_TO_ONE -> instance.collapse()
            CallKind.GROUP -> group.collapse()
            CallKind.LINK -> link.collapse()
            null -> Unit
        }
    }

    /** See [collapseCurrentCall]. A tap on the bar. */
    public fun expandCurrentCall() {
        when (occupantNow()) {
            CallKind.ONE_TO_ONE -> instance.expand()
            CallKind.GROUP -> group.expand()
            CallKind.LINK -> link.expand()
            null -> Unit
        }
    }

    /**
     * The collapsed bar's one control — section 4.5: "one round mute button,
     * drawn on and off exactly as the call screen draws it".
     *
     * Routed exactly as the call screen routes its own microphone button. A 1x1
     * call has a session on this side of the FFI and is told directly; a group
     * or link call's screen only ever flips the store, which its host mirrors
     * onto the session, so the bar flips the same store rather than reaching
     * past it for a handle it does not hold.
     */
    public fun setCurrentCallMicrophoneEnabled(on: Boolean) {
        when (occupantNow()) {
            CallKind.ONE_TO_ONE -> instance.active.value?.session?.setMicrophoneEnabled(on)
            CallKind.GROUP -> group.ui.setMic(on)
            CallKind.LINK -> link.ui.setMic(on)
            null -> Unit
        }
    }

    /** End whatever call occupies the slot (declines a ringing 1x1 call). */
    public fun endCurrentCall() {
        when (occupantNow()) {
            CallKind.ONE_TO_ONE -> {
                val call = instance.active.value ?: return
                val state = call.session.state.value
                when {
                    call.incoming && !call.accepted && state == CallSession.WebCallState.Ringing -> instance.decline()
                    !call.incoming && state == CallSession.WebCallState.Idle -> instance.cancelPrepared()
                    else -> instance.hangUp()
                }
            }
            CallKind.GROUP -> group.leave()
            CallKind.LINK -> link.leave()
            null -> Unit
        }
    }

    /**
     * `New call` — `docs/ux/call-links.md` sections 1 and 2.
     *
     * One kind of call: a group video call, founded here and now, with the
     * person who asked for it landing in it. Nothing is asked first and nothing
     * is uploaded: a call needs only a fresh `call_id` and an owner, both of
     * which this device already has, so the upload the invite link costs is
     * paid later by whoever actually wants a link (section 3).
     *
     * The call runs through the same [link] host a `/c/` link's landing uses —
     * the ADR-0032 owner-founds path, unchanged — so it holds the one call slot
     * and keeps running when the screen that started it goes away. It has no
     * slug, because there is no landing to go back to until a link is minted.
     */
    public suspend fun startNewCall(
        accountIdentitySeed: ByteArray,
        accountId: ByteArray,
        sessionToken: String,
        video: Boolean = CALL_INVITE_LINK_MEDIA_VIDEO,
    ): LinkCallStartOutcome {
        val ownSignatureKey = try {
            me.lakko.app.auth.SessionSecretWipe.identity(accountIdentitySeed).use { it.signaturePublicKey() }
        } catch (error: Throwable) {
            android.util.Log.w("AppCallHost", "no live identity to start a call with", error)
            return LinkCallStartOutcome.Failed("no live identity")
        }
        return link.start(
            LinkCallRequest(
                callId = newCallId(),
                // This device IS the owner: it founds the call, and any link
                // minted later names the same account as its host.
                ownerAccountId = accountId,
                ownerSignatureKey = ownSignatureKey,
                mediaVideo = video,
                isOwner = true,
                slug = null,
                accountIdentitySeed = accountIdentitySeed,
                sessionToken = sessionToken,
                accountId = accountId,
            ),
        )
    }

    /** Logout / forget device: end every call locally and start clean. */
    public fun reset() {
        IncomingCallRinger.stopAll()
        instance.reset()
        group.reset()
        link.reset()
        endedForEveryone.reset()
    }
}

/** The production [OneToOneCallPort]: real sessions, the MLS exporter, the DS journal. */
public class ChatRepositoryCallPort(
    private val applicationContext: Context,
    private val repository: ChatRepository,
    private val sessionTokenProvider: () -> String? = { null },
) : OneToOneCallPort<CallSession> {

    override fun createSession(
        groupIdHex: String,
        callId: ByteArray,
        outbound: (ByteArray, CallSignalingWake) -> Unit,
    ): CallSession = CallSession(
        applicationContext = applicationContext,
        callId = callId,
        onOutboundSignaling = outbound,
        // ADR-0083 — the share fold's own key: this device's leaf key.
        ownSignatureKey = repository.ownGroupCallMemberKey()?.let { b64 ->
            runCatching { java.util.Base64.getDecoder().decode(b64) }.getOrNull()
        },
    ).also { session ->
        // Without this the session resolves STUN-only ICE, and two peers that
        // cannot reach each other directly never connect. The credential is
        // minted beside a call token for this call's room, paid for by one
        // anonymous `call-join` token (ADR-0078 section 2): no session reaches
        // the issuer. Fail-soft by contract: no token to be had, or no relay,
        // leaves the call on STUN rather than refusing it. Signed out, there
        // is no token pool to draw from, so nothing is attempted.
        if (!sessionTokenProvider().isNullOrEmpty()) {
            session.setTurnClient(TurnCredentialClient.forCall(callId))
        }
    }

    override suspend fun deriveCallKey(groupIdHex: String, callId: ByteArray): OneToOneCallKey? =
        repository.deriveCallKey(groupIdHex = groupIdHex, callId = callId)
            ?.let { OneToOneCallKey(seed = it.seed, localLeaf = it.localLeaf) }

    /**
     * The single outbound sink for every 1x1 signaling frame — offer, answer,
     * ICE candidate, end — drained by [OneToOneCallHost]'s outbound queue.
     *
     * The push wake that makes a closed callee ring (`docs/ux/calls.md` §2.1)
     * is fired inside [ChatRepository.publishApplicationMessage]: the session
     * says which frames ring someone or stop a ring ([OneToOneCallWakes]: the
     * dial offer, the End of a call that never connected), and
     * [shouldWakeForCallSignaling] still refuses a wake for any frame that is
     * not an offer or an end. The bytes alone cannot decide: a renegotiation
     * offer and the End of a connected call carry the same tags, and a wake on
     * either rang the peer's iPhone.
     */
    override suspend fun publish(groupIdHex: String, bytes: ByteArray, wake: CallSignalingWake) {
        repository.publishApplicationMessage(groupIdHex = groupIdHex, plaintextBytes = bytes, wake = wake)
    }

    override fun composeEnd(callId: ByteArray, reason: FfiCallEndReason): ByteArray? =
        try {
            composeCallSignaling(FfiCallSignalingBody.End(callId = callId, reason = reason))
        } catch (error: Throwable) {
            android.util.Log.w("ChatRepositoryCallPort", "compose End failed", error)
            null
        }

    /**
     * The caller's name for the ring, resolved the same way every other screen
     * resolves it (ADR-0048): the 1x1 peer's signature key, then the profile
     * directory. A group with no resolvable peer, or a name this device has
     * never learned, yields `null` and the ring says `Someone`.
     */
    /**
     * `docs/ux/calls.md` section 6 — the finished call's row, written into the
     * conversation it happened in. Local to this device: the repository appends
     * it to that group's timeline and persists it with the transcript.
     */
    override fun recordCall(groupIdHex: String, entry: CallTranscriptEntry, startedAtMs: Long?) {
        try {
            repository.recordCallRow(groupIdHex, entry, startedAtMs)
        } catch (error: Throwable) {
            android.util.Log.w("ChatRepositoryCallPort", "recording the call row failed", error)
        }
    }

    /**
     * `docs/ux/calls.md` section 2.1 — is this conversation muted?
     *
     * The same mute the chat list draws and the DeviceGroup journal syncs
     * between a person's devices, read from the state it is already persisted
     * in ([me.lakko.app.devices.sync.ConversationMute]). On the I/O dispatcher
     * because it opens a file, and this host runs on the main one.
     */
    override suspend fun isMuted(groupIdHex: String): Boolean =
        kotlinx.coroutines.withContext(Dispatchers.IO) {
            me.lakko.app.devices.sync.ConversationMute.isMuted(
                context = applicationContext,
                accountIdB64 = repository.activeAccountIdB64,
                groupIdHex = groupIdHex,
            )
        }

    override fun peerKeyB64(groupIdHex: String): String? = try {
        repository.oneToOnePeerKey(groupIdHex)?.let { java.util.Base64.getEncoder().encodeToString(it) }
    } catch (_: Throwable) {
        null
    }

    override fun peerName(groupIdHex: String): String? = try {
        repository.oneToOnePeerKey(groupIdHex)?.let { key ->
            val resolved = me.lakko.app.profile.ProfileDirectory.get(applicationContext).resolve(key)
            // A key fingerprint is not a name: on a locked screen it says
            // nothing and reads as noise, so the ring falls back to `Someone`.
            resolved.primary.takeIf { !resolved.isPlaceholder }
        }
    } catch (error: Throwable) {
        android.util.Log.w("ChatRepositoryCallPort", "resolving the caller's name failed", error)
        null
    }
}

/** The production [GroupCallPort]: the chat repository, the SFU, [SfuCallUi]. */
internal class ChatRepositoryGroupCallPort(
    private val applicationContext: Context,
    private val repository: ChatRepository,
    private val scope: CoroutineScope,
    private val sessionTokenProvider: () -> String?,
) : GroupCallPort {

    override fun sfuBaseUrl(): String? = SfuConfig.baseUrl

    override fun sessionToken(): String? = sessionTokenProvider()

    override fun isGroupActive(groupIdHex: String): Boolean? = repository.isGroupActive(groupIdHex)

    override fun recordCall(groupIdHex: String, entry: CallTranscriptEntry, startedAtMs: Long?) {
        try {
            repository.recordCallRow(groupIdHex, entry, startedAtMs)
        } catch (error: Throwable) {
            android.util.Log.w("ChatRepositoryGroupCallPort", "recording the call row failed", error)
        }
    }

    override suspend fun syncGroup(groupIdHex: String) {
        // Best effort: when a long poll of this group is already in flight this
        // returns at once, and that poll is what keeps the epoch current anyway.
        repository.pollGroup(groupIdHex)
    }

    override fun isCallLive(groupIdHex: String, callIdHex: String, nowMs: Long): Boolean =
        repository.isGroupCallLive(groupIdHex, callIdHex, nowMs)

    override fun recordOtherFreshMembers(groupIdHex: String, callIdHex: String, nowMs: Long): Set<String>? =
        repository.groupCallOtherFreshMembers(groupIdHex, callIdHex, nowMs)

    override fun startedByThisAccount(groupIdHex: String, callIdHex: String): Boolean =
        repository.groupCallStartedByThisAccount(groupIdHex, callIdHex)

    override fun personName(personKeyB64: String): String? =
        callNameOfPerson(me.lakko.app.profile.ProfileDirectory.get(applicationContext), personKeyB64)

    override suspend fun readSettledGroup(groupIdHex: String, callId: ByteArray): SettledGroupRead =
        repository.readSettledCallKey(groupIdHex, callId)

    override suspend fun mintCallJwt(callId: ByteArray): MintCallJwtResult =
        CallJwtClient().use { it.mint(callId) }

    override fun sfuRoom(callId: ByteArray): String? =
        try {
            sfuRoomForCall(callId)
        } catch (_: Throwable) {
            null
        }

    override suspend fun publishSignal(groupIdHex: String, body: FfiCallSignalingBody): Boolean =
        repository.publishGroupCallSignal(groupIdHex, body)

    override suspend fun fetchSfuCallState(url: String): String? {
        val client = io.ktor.client.HttpClient(
            me.lakko.app.transport.AntiCensorshipEngine(me.lakko.app.transport.BaseHttpEngine.create()),
        )
        return try {
            // No token, no credentials (ADR-0078 section 2).
            val response = client.get(url)
            if (response.status.value == 200) response.bodyAsText() else null
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            null
        } finally {
            client.close()
        }
    }

    /** The name on the user's own tile (`docs/ux/calls.md` section 4.4): the
     *  profile name, empty when none is set (the tile then draws
     *  `call.participant.you`, ADR-0061). Read at call start. */
    private fun ownTileName(): String =
        repository.ownDisplayName()?.trim()?.takeIf { it.isNotEmpty() }.orEmpty()

    override fun createSession(request: GroupCallSessionRequest, ui: CallUiState): GroupCallSessionHandle {
        val profileNames = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        val names = callPeerNames(profileNames)
        val callId = hexToBytesOrNull(request.callIdHex)
        val sfu = SfuCallUi(
            applicationContext = applicationContext,
            // Section 4.4: the user's own tile carries their own name from the
            // first second; `You` only when no profile name is set.
            localDisplayName = ownTileName(),
            config = SfuCallSession.Config(
                sfuBaseUrl = request.sfuBaseUrl,
                jwt = request.jwt,
                seed = request.seed,
                localLeaf = request.localLeaf,
                audio = true,
                video = request.video,
                // ADR-0078 section 2: the SFU names nobody; each peer
                // announces itself, verified against this group's roster.
                peerAnnouncer = callId?.let { repository.callPeerAnnouncer(request.groupIdHex, it) },
            ),
            scope = scope,
            ui = ui,
            peerNames = names,
        )
        // The SFU has a public address, so most clients reach it on a
        // server-reflexive candidate - but one behind a symmetric NAT has
        // none, and for it the relay is the only way into the room. The
        // credential came with the call token (ADR-0078 section 2).
        sfu.session.setTurnClient(TurnCredentialClient.preMinted(request.turn))
        // A profile that arrives during the call (the newcomer's introduce-
        // yourself request is answered within about half a minute) bumps the
        // directory's version; rename the tiles already on screen rather than
        // leave them unnamed until the next roster event. Cancelled on
        // hang-up, which every end path reaches through the host's release.
        val renames = scope.launch {
            profileNames.version.drop(1).collect {
                when (sfu.session.state) {
                    SfuCallSession.State.FAILED, SfuCallSession.State.CLOSED -> cancel()
                    else -> names.reapply(ui, SfuCallUi::participantId)
                }
            }
        }
        return object : GroupCallSessionHandle {
            override val failed: Boolean
                get() = sfu.session.state == SfuCallSession.State.FAILED
            override val lastInboundAtMs: Long
                get() = sfu.session.lastInboundAtMs()
            override val callEndedByServer: Boolean
                get() = sfu.session.endedByServer

            override suspend fun connect() = sfu.connect()

            override fun hangUp() {
                renames.cancel()
                sfu.hangUp()
            }

            override fun rekey(seed: ByteArray) = sfu.rekey(seed)

            override suspend fun publishVideo() = sfu.publishVideo()

            override fun setCameraEnabled(on: Boolean) = sfu.setCameraEnabled(on)

            override fun selectCamera(cameraName: String?) = sfu.selectCamera(cameraName)

            // The store already holds the new value; drive the track only.
            override fun setMicrophoneEnabled(on: Boolean) = sfu.session.setMicrophoneEnabled(on)
        }
    }
}

/**
 * Names a call's peers, for both call kinds (`CallPeerNames.kt`): a peer is
 * the person its verified announcement names (ADR-0078 section 2), named by
 * the one resolver (rename, profile name, verified `@username`), then the
 * username their stored profile claims as plain text. A peer nobody here can
 * name has an empty name, which the call screen draws as its unnamed state;
 * never a piece of an id (`CallTileName.kt`).
 */
internal fun callPeerNames(profileNames: me.lakko.app.profile.ProfileDirectory): CallPeerNames =
    CallPeerNames(
        personName = { keyB64 -> personNameOrNull(profileNames.resolve(keyB64)) },
        storedUsername = { keyB64 -> profileNames.claimedUsernameOf(keyB64) },
        warn = { message, error -> android.util.Log.w("AppCallHost", message, error) },
    )

/** 32 lower-case hex characters -> 16 bytes, or `null`. */
private fun hexToBytesOrNull(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/** The production [LinkCallPort]: [MultiJoinerCall] over the account identity. */
internal class MultiJoinerLinkCallPort(
    private val applicationContext: Context,
    private val scope: CoroutineScope,
) : LinkCallPort {

    override fun sfuBaseUrl(): String? = SfuConfig.baseUrl

    override fun callingAvailable(): Boolean = me.lakko.sframe.SFrameAvailability.isCallingAvailable

    override suspend fun start(
        request: LinkCallRequest,
        sfuBaseUrl: String,
        ui: CallUiState,
        events: LinkCallEvents,
    ): LinkCallHandle {
        // The canonical identity for the call's lifetime; freed on hang-up.
        val identity = try {
            me.lakko.app.auth.SessionSecretWipe.identity(request.accountIdentitySeed)
        } catch (error: Throwable) {
            throw IllegalStateException("Re-authenticate to join this call (no live identity).", error)
        }
        val ownProfile = me.lakko.app.profile.OwnProfileStore.load(applicationContext, request.accountId)
        val profileNames = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        val options = MultiJoinerCall.Options(
            applicationContext = applicationContext,
            identity = identity,
            callId = request.callId,
            ownerAccountId = request.ownerAccountId,
            ownerSignatureKey = request.ownerSignatureKey,
            sfuBaseUrl = sfuBaseUrl,
            audio = true,
            video = request.mediaVideo,
            ui = ui,
            scope = scope,
            // Section 4.4: the own tile carries the profile name from the
            // first second, and the same profile is announced to every other
            // member inside the call group (never through the SFU in clear).
            localDisplayName = ownProfile?.displayName?.trim()?.takeIf { it.isNotEmpty() }.orEmpty(),
            // Composed whenever a profile is stored, a display name or a
            // username alone included; with the username goes its certificate
            // (ADR-0060 section 5), which is what lets a receiver verify it.
            ownProfileEnvelope = {
                ownProfile?.let { profile ->
                    try {
                        uniffi.lakko_ffi_uniffi.composeProfileEnvelope(
                            profile.toFfi(
                                accountId = request.accountId,
                                usernameCertificate = ownUsernameCertificate(),
                            ),
                        )
                    } catch (e: Throwable) {
                        android.util.Log.w("AppCallHost", "own profile refused by the composer", e)
                        null
                    }
                }
            },
            // Tiles are named by the one rule (`CallTileName.kt`): a peer is
            // the person its verified announcement names (ADR-0078 section 2),
            // and a member's sealed profile is filed like a profile received
            // in a chat, its account claim kept only when bound
            // (`boundProfileClaim`).
            names = callPeerNames(profileNames),
            onMemberProfile = { personKeyB64, profile ->
                try {
                    profileNames.onProfileReceived(personKeyB64, peerProfileOf(profile))
                } catch (e: Throwable) {
                    android.util.Log.w("AppCallHost", "a call member's profile was not stored", e)
                }
            },
            onJoined = events.onJoined,
            onEnded = events.onEnded,
            onNoCommitter = events.onNoCommitter,
            onError = events.onError,
            onEndedForEveryone = events.onEndedForEveryone,
        )
        val call = try {
            if (request.isOwner) MultiJoinerCall.host(options) else MultiJoinerCall.join(options)
        } catch (error: Throwable) {
            identity.close()
            throw error
        }
        // A member's username verifying, or a contact renamed, while the call
        // is up re-names the tiles already on screen, as in a group call.
        // Cancelled on hang-up, which every end path reaches.
        val renames = scope.launch {
            profileNames.version.drop(1).collect { call.refreshNames() }
        }
        return MultiJoinerLinkCallHandle(call, identity, renames)
    }

    /** This account's username certificate, or `null` (a guest, not fetched
     *  yet, or no reach session). */
    private fun ownUsernameCertificate(): ByteArray? =
        try {
            me.lakko.app.reach.ReachManager.peek()?.usernameCertificate()
        } catch (e: Throwable) {
            null
        }

    /**
     * `docs/ux/call-links.md` section 3 — the link to the call already running.
     *
     * The capability is signed under the account's own Ed25519 key and carries
     * the LIVE `call_id`, so whoever follows it arrives in this call rather than
     * founding a new one. The discovery upload is a network round trip, which
     * is why this runs on the first `Invite` and not at the call's start.
     */
    override suspend fun mintInviteLink(callId: ByteArray, identity: CallInviteIdentity): String =
        me.lakko.app.auth.SessionSecretWipe.identity(identity.accountIdentitySeed).use { own ->
            val discovery = me.lakko.app.discovery.DiscoveryClient()
            discovery.setSessionToken(identity.sessionToken)
            discovery.use { client ->
                var mintedLinkId: ByteArray? = null
                val composed = try {
                    CallInviteComposer(client).composeAndUpload(
                    ownerSigningSeed = own.ed25519Seed(),
                    ownerAccountId = identity.accountId,
                    identity = own,
                    callId = callId,
                    // ADR-0060 §6 — a cold-start joiner writes into this
                    // account's closed mailbox with the link's key.
                    linkGrantFor = { linkId, expiryMs ->
                        val reach = me.lakko.app.reach.ReachManager.peek()
                            ?: throw IllegalStateException("no session")
                        mintedLinkId = linkId
                        reach.mintLink(linkId, expiryMs.takeIf { it != 0L })
                    },
                    )
                } catch (e: Throwable) {
                    // No link was minted (a failed upload mints nothing, the
                    // inline form is gone): the key registered for it goes.
                    mintedLinkId?.let { me.lakko.app.reach.ReachManager.peek()?.retireLink(it) }
                    throw e
                }
                // ADR-0074 section 3.5a rule 5 - the ledger id is kept with
                // the link's key, so a rotation of the account keys can
                // revoke the link at the ledger.
                val linkId = mintedLinkId
                val blobId = composed.blobId
                if (linkId != null) {
                    me.lakko.app.reach.ReachManager.peek()?.noteLinkBlob(linkId, blobId)
                }
                // ADR-0078 section 10: the link's revocation seed is kept on
                // this device; the ledger names no owner.
                run {
                    me.lakko.app.contacts.OwnLinkStores.current.put(
                        me.lakko.app.auth.base64Encode(identity.accountId),
                        me.lakko.app.contacts.OwnLinks.record(
                            kind = me.lakko.app.contacts.OwnLink.KIND_CALL,
                            blobId = blobId,
                            seed = composed.revocationSeed,
                            expiryMs = composed.expiryMs,
                            maxUses = callInviteLinkMaxUses().toLong(),
                        ),
                    )
                }
                composed.url
            }
        }
}

private class MultiJoinerLinkCallHandle(
    private val call: MultiJoinerCall,
    private val identity: Identity,
    private val renames: kotlinx.coroutines.Job,
) : LinkCallHandle {
    private val closed = AtomicBoolean(false)

    override fun hangUp() {
        renames.cancel()
        call.hangUp()
        if (closed.compareAndSet(false, true)) identity.close()
    }

    override fun endForEveryone() {
        // Sealed with the call's identity, so not after it was closed.
        if (!closed.get()) call.endForEveryone()
    }

    override suspend fun publishVideo() = call.publishVideo()

    override fun setCameraEnabled(on: Boolean) = call.setCameraEnabled(on)

    override fun setMicrophoneEnabled(on: Boolean) = call.setMicrophoneEnabled(on)

    override fun selectCamera(cameraName: String?) = call.selectCamera(cameraName)
}
