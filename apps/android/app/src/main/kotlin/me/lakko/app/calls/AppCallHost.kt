// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
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
import me.lakko.app.contacts.AcceptedContactsStore
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.composeCallSignaling
import uniffi.lakko_ffi_uniffi.sfuPeerTag

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
        OneToOneCallHost(scope = scope)
    }

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
        group.attach(ChatRepositoryGroupCallPort(applicationContext, repository, scope, sessionTokenProvider))
        link.attach(MultiJoinerLinkCallPort(applicationContext, scope))
        idleSubscription?.invoke()
        // G-18 / ADR-0029 — the hosted group call rekeys whenever its group's
        // MLS work settles, whichever receive path applied it.
        idleSubscription = repository.addGroupMlsIdleListener { groupIdHex -> group.onGroupSettled(groupIdHex) }
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

    /** Logout / forget device: end every call locally and start clean. */
    public fun reset() {
        IncomingCallRinger.stopAll()
        instance.reset()
        group.reset()
        link.reset()
    }
}

/** The production [OneToOneCallPort]: real sessions, the MLS exporter, the DS journal. */
public class ChatRepositoryCallPort(
    private val applicationContext: Context,
    private val repository: ChatRepository,
    private val sessionTokenProvider: () -> String? = { null },
) : OneToOneCallPort<CallSession> {

    /**
     * One client for the process: it holds no per-call state, only the bearer
     * token, which is rebound before every mint because login, recover and
     * pairing all replace it and a credential minted against a stale one is
     * refused by the relay.
     */
    private val turn = TurnCredentialClient()

    override fun createSession(
        groupIdHex: String,
        callId: ByteArray,
        outbound: (ByteArray) -> Unit,
    ): CallSession = CallSession(
        applicationContext = applicationContext,
        callId = callId,
        onOutboundSignaling = outbound,
    ).also { session ->
        // Without this the session resolves STUN-only ICE, and two peers that
        // cannot reach each other directly never connect. Fail-soft by
        // contract: no token, or a credential the auth service declines to
        // mint, leaves the call on STUN rather than refusing it.
        val token = sessionTokenProvider()
        if (!token.isNullOrEmpty()) {
            turn.setSessionToken(token)
            session.setTurnClient(turn)
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
     * is fired inside [ChatRepository.publishApplicationMessage], decided from
     * these very bytes by [shouldWakeForCallSignaling], not chosen here: this
     * sink sees all four 1x1 frames and only two of them may wake anyone, so
     * asking it to pass a flag would put the rule in a place that has no way to
     * tell them apart without re-reading the bytes anyway.
     */
    override suspend fun publish(groupIdHex: String, bytes: ByteArray) {
        repository.publishApplicationMessage(groupIdHex = groupIdHex, plaintextBytes = bytes)
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
    override fun recordCall(groupIdHex: String, entry: CallTranscriptEntry) {
        try {
            repository.recordCallRow(groupIdHex, entry)
        } catch (error: Throwable) {
            android.util.Log.w("ChatRepositoryCallPort", "recording the call row failed", error)
        }
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

    override fun recordCall(groupIdHex: String, entry: CallTranscriptEntry) {
        try {
            repository.recordCallRow(groupIdHex, entry)
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

    override suspend fun readSettledGroup(groupIdHex: String, callId: ByteArray): SettledGroupRead =
        repository.readSettledCallKey(groupIdHex, callId)

    override suspend fun mintCallJwt(sessionToken: String, callIdHex: String): String {
        val client = CallJwtClient()
        return try {
            client.setSessionToken(sessionToken)
            client.mint(callId = callIdHex).jwt
        } finally {
            client.close()
        }
    }

    override suspend fun publishSignal(groupIdHex: String, body: FfiCallSignalingBody): Boolean =
        repository.publishGroupCallSignal(groupIdHex, body)

    override suspend fun fetchSfuCallState(url: String): String? {
        val client = io.ktor.client.HttpClient(
            me.lakko.app.transport.AntiCensorshipEngine(io.ktor.client.engine.okhttp.OkHttp.create {}),
        )
        return try {
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
     *  profile name, `You` when none is set. Read at call start. */
    private fun ownTileName(): String =
        repository.ownDisplayName()?.trim()?.takeIf { it.isNotEmpty() } ?: "You"

    override fun createSession(request: GroupCallSessionRequest, ui: CallUiState): GroupCallSessionHandle {
        val profileNames = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        val names = peerDisplayNameResolver(request.callIdHex, profileNames)
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
            ),
            scope = scope,
            ui = ui,
            resolveDisplayName = names::resolve,
        )
        // The SFU has a public address, so most clients reach it on a
        // server-reflexive candidate - but one behind a symmetric NAT has
        // none, and for it the relay is the only way into the room.
        sessionTokenProvider()?.takeIf { it.isNotEmpty() }?.let { token ->
            sfu.session.setTurnClient(TurnCredentialClient().apply { setSessionToken(token) })
        }
        // A profile that arrives during the call (the newcomer's introduce-
        // yourself request is answered within about half a minute) bumps the
        // directory's version; rename the tiles already on screen rather than
        // leave them `Participant N` until the next roster event. Cancelled on
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

            // The store already holds the new value; drive the track only.
            override fun setMicrophoneEnabled(on: Boolean) = sfu.session.setMicrophoneEnabled(on)
        }
    }

    /**
     * ADR-0021 — the per-call peer_tag -> label resolver over BOTH sources of
     * an account id: the persisted accepted contacts and every stored profile
     * that carried one (ADR-0048 v2), so a group member who is not a contact
     * still gets a name on their tile. The tag is re-derived locally (the SFU
     * never sends a name); the label is the one resolver's primary (rename,
     * profile name, verified `@username`); two keys behind one tag hide it and
     * the tile reads `Participant N` — see [buildPeerTagLabels].
     *
     * Not frozen at call start: the map is rebuilt whenever the directory's
     * version moves (a profile received mid-call), read on every roster event
     * and by the host's reapply collector.
     */
    private fun peerDisplayNameResolver(
        callIdHex: String,
        profileNames: me.lakko.app.profile.ProfileDirectory,
    ): GroupCallNameResolver = GroupCallNameResolver(
        sourcesVersion = { profileNames.version.value },
        sources = {
            val fromContacts = AcceptedContactsStore.load(applicationContext).mapNotNull { contact ->
                // A contact accepted before the acknowledgement carried an
                // account id has none here; their profile, if one was received,
                // is the other way their tile gets a name.
                val accountId = contact.request.fromAccountId ?: return@mapNotNull null
                PeerTagSource(
                    accountIdHex = accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) },
                    signatureKeyB64 = java.util.Base64.getEncoder().encodeToString(contact.request.fromPubkey),
                )
            }
            val fromProfiles = profileNames.accountIdsByKey().map { (keyB64, accountIdHex) ->
                PeerTagSource(accountIdHex = accountIdHex, signatureKeyB64 = keyB64)
            }
            fromContacts + fromProfiles
        },
        tagOf = { accountIdHex -> sfuPeerTag(callId = callIdHex, accountId = accountIdHex) },
        // ADR-0048 — the one resolver; a call tile shows the primary alone.
        labelOf = { keyB64 -> profileNames.resolve(keyB64).primary },
        warn = { message, error -> android.util.Log.w("AppCallHost", message, error) },
    )
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
            Identity.fromSeed(request.accountIdentitySeed)
        } catch (error: Throwable) {
            throw IllegalStateException("Re-authenticate to join this call (no live identity).", error)
        }
        val ownProfile = me.lakko.app.profile.OwnProfileStore.load(applicationContext, request.accountId)
        val options = MultiJoinerCall.Options(
            applicationContext = applicationContext,
            identity = identity,
            callId = request.callId,
            ownerAccountId = request.ownerAccountId,
            ownerSignatureKey = request.ownerSignatureKey,
            sfuBaseUrl = sfuBaseUrl,
            sessionToken = request.sessionToken,
            audio = true,
            video = request.mediaVideo,
            ui = ui,
            scope = scope,
            // Section 4.4: the own tile carries the profile name from the
            // first second, and the same profile is announced to every other
            // member inside the call group (never through the SFU in clear).
            localDisplayName = ownProfile?.displayName?.trim()?.takeIf { it.isNotEmpty() } ?: "You",
            ownProfileEnvelope = {
                ownProfile?.let { profile ->
                    try {
                        uniffi.lakko_ffi_uniffi.composeProfileEnvelope(profile.toFfi(request.accountId))
                    } catch (e: Throwable) {
                        android.util.Log.w("AppCallHost", "own profile refused by the composer", e)
                        null
                    }
                }
            },
            onJoined = events.onJoined,
            onEnded = events.onEnded,
            onNoCommitter = events.onNoCommitter,
            onError = events.onError,
        )
        val call = try {
            if (request.isOwner) MultiJoinerCall.host(options) else MultiJoinerCall.join(options)
        } catch (error: Throwable) {
            identity.close()
            throw error
        }
        return MultiJoinerLinkCallHandle(call, identity)
    }
}

private class MultiJoinerLinkCallHandle(
    private val call: MultiJoinerCall,
    private val identity: Identity,
) : LinkCallHandle {
    private val closed = AtomicBoolean(false)

    override fun hangUp() {
        call.hangUp()
        if (closed.compareAndSet(false, true)) identity.close()
    }

    override suspend fun publishVideo() = call.publishVideo()

    override fun setCameraEnabled(on: Boolean) = call.setCameraEnabled(on)

    override fun setMicrophoneEnabled(on: Boolean) = call.setMicrophoneEnabled(on)
}
