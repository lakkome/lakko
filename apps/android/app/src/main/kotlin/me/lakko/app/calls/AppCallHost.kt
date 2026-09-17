// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
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
        instance.attach(ChatRepositoryCallPort(applicationContext, repository))
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
        instance.reset()
        group.reset()
        link.reset()
    }
}

/** The production [OneToOneCallPort]: real sessions, the MLS exporter, the DS journal. */
public class ChatRepositoryCallPort(
    private val applicationContext: Context,
    private val repository: ChatRepository,
) : OneToOneCallPort<CallSession> {

    override fun createSession(
        groupIdHex: String,
        callId: ByteArray,
        outbound: (ByteArray) -> Unit,
    ): CallSession = CallSession(
        applicationContext = applicationContext,
        callId = callId,
        onOutboundSignaling = outbound,
    )

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

    override fun createSession(request: GroupCallSessionRequest, ui: CallUiState): GroupCallSessionHandle {
        val sfu = SfuCallUi(
            applicationContext = applicationContext,
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
            resolveDisplayName = peerDisplayNameResolver(request.callIdHex),
        )
        return object : GroupCallSessionHandle {
            override val failed: Boolean
                get() = sfu.session.state == SfuCallSession.State.FAILED

            override suspend fun connect() = sfu.connect()

            override fun hangUp() = sfu.hangUp()

            override fun rekey(seed: ByteArray) = sfu.rekey(seed)

            override suspend fun publishVideo() = sfu.publishVideo()

            override fun setCameraEnabled(on: Boolean) = sfu.setCameraEnabled(on)

            // The store already holds the new value; drive the track only.
            override fun setMicrophoneEnabled(on: Boolean) = sfu.session.setMicrophoneEnabled(on)
        }
    }

    /**
     * ADR-0021 — the per-call peer_tag -> contact label map, computed once over
     * the persisted accepted contacts, so the SFU roster resolves to the
     * identifier the rest of the app shows. The tag is re-derived locally (the
     * SFU never sends a name); a collision falls back to the pseudonym.
     */
    private fun peerDisplayNameResolver(callIdHex: String): (String, String, String) -> String {
        val profileNames = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        val tagToLabel = HashMap<String, String>()
        val collidedTags = HashSet<String>()
        for (contact in AcceptedContactsStore.load(applicationContext)) {
            val accountId = contact.request.fromAccountId ?: continue
            val accountIdHex = accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) }
            val tag = sfuPeerTag(callId = callIdHex, accountId = accountIdHex)
            if (tagToLabel.containsKey(tag)) {
                collidedTags.add(tag)
            } else {
                // ADR-0048 — the one resolver (rename, profile name, verified
                // @username, fingerprint).
                tagToLabel[tag] = profileNames.resolve(contact.request.fromPubkey).primary
            }
        }
        return { _, pseudonym, peerTag ->
            if (peerTag.isNotEmpty() && !collidedTags.contains(peerTag) && tagToLabel.containsKey(peerTag)) {
                tagToLabel.getValue(peerTag)
            } else if (pseudonym.isEmpty()) {
                "Peer"
            } else {
                pseudonym.take(8)
            }
        }
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
            Identity.fromSeed(request.accountIdentitySeed)
        } catch (error: Throwable) {
            throw IllegalStateException("Re-authenticate to join this call (no live identity).", error)
        }
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
