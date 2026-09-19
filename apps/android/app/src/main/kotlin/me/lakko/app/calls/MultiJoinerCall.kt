// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.SfuPeerSnapshot
import uniffi.lakko_ffi_uniffi.callSafetyNumber
import uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope
import uniffi.lakko_ffi_uniffi.keyPackageSignatureKey
import uniffi.lakko_ffi_uniffi.sfuPeerTag
import uniffi.lakko_ffi_uniffi.wrapCallKeyPackage
import uniffi.lakko_ffi_uniffi.wrapCallWelcome

/**
 * ADR-0032 multi-joiner call-link orchestrator (Android) — the Kotlin twin
 * of `apps/web/src/calls/multi-joiner-call.ts`.
 *
 * Drives the "owner-founds + deterministic committer election" protocol on
 * top of the EXISTING SFU signal-relay — no server change.
 *
 * ## Roles
 *
 *   * **Owner** ([host]) — the sole founder. Calls `Group.create(identity)`,
 *     derives the SFrame seed, joins the SFU room, and admits joiners (it is
 *     the acting committer while present).
 *   * **Joiner** ([join]) — never founds a group. Mints a fresh KeyPackage,
 *     connects to the SFU room signaling-only, relays
 *     `wrapCallKeyPackage(kp)` to the room, waits for the committer's
 *     `wrapCallWelcome(welcome)`, then `Group.joinFromWelcome` + derives the
 *     same seed. Media publishes ONLY after the group is joined (invariant #4
 *     — no plaintext / half-open media).
 *
 * ## Committer election (survives the owner leaving)
 *
 * On every SFU roster change the acting committer is recomputed as a pure
 * function of public state ([electActingCommitterLeaf]): the owner if
 * present, else the lowest-leaf present member. Only the member that IS the
 * acting committer processes an inbound KeyPackage — everyone else drops it.
 * Failover + reclaim fall out with no election protocol.
 *
 * ## Transport (reuse, no new SFU frame)
 *
 * The KeyPackage / Welcome / commit ride the existing opaque SFU
 * `signal-relay` ([SfuCallSession.sendSignalRelayToCall] / `...ToPeer`). The
 * committer classifies each inbound payload with `dispatchIncomingEnvelope`
 * (CallKeyPackage / CallWelcome / IntroMessage). An MLS `Commit` that must
 * reach existing members is broadcast raw; a receiver's dispatch classifies
 * it as `IntroMessage` (fallback = verbatim bytes) and feeds it to
 * `Group.processIncoming` inside a try/catch.
 *
 * ## Per-admission epoch advance + rekey
 *
 * Adding a member advances the MLS epoch, so the per-call SFrame seed
 * changes. The committer (after `addMemberFromWire`) and every existing
 * member (after applying the broadcast commit) re-derive the seed and
 * [SfuCallSession.rekey] their SFrame (G-18 / ADR-0029 overlap keyer); the
 * new joiner starts at the post-add epoch.
 *
 * Admission is AUTO for the reference (ADR-0027 approve-toggle UI is a
 * follow-up); non-owner presence binding is best-effort (the ADR-0032
 * low-severity residual); the cold-start welcome-ferry is not wired here.
 *
 * Instead of the web `callActions` global store, this drives a [CallUiState]
 * so the shared `CallScreen` renders the roster with no per-call-type
 * branching. The caller ([Options.ui]) owns that state; this is pure glue.
 */
class MultiJoinerCall private constructor(
    applicationContext: Context,
    private val opts: Options,
    private val role: Role,
    jwt: String,
    private val ownerKeyHex: String,
    ownerPeerTag: String?,
    initialSeed: ByteArray?,
    initialLeaf: Int?,
) {
    enum class Role { OWNER, JOINER }

    /** Inputs shared by both roles. */
    data class Options(
        val applicationContext: Context,
        /** The canonical device [Identity] (built from the account-identity
         *  seed). Reused for the whole call — owned by the caller, which frees
         *  it after [hangUp]. */
        val identity: Identity,
        /** 16-byte `call_id` from the capability. */
        val callId: ByteArray,
        /** Owner's 16-byte `account_id` from the capability (for the owner's
         *  per-call peer_tag -> presence match). */
        val ownerAccountId: ByteArray,
        /** Owner's 32-byte Ed25519 signature key from the capability
         *  (identifies the owner's MLS leaf + is the election's `is_owner`
         *  anchor + the FINDING 2 redirect-refusal check). */
        val ownerSignatureKey: ByteArray,
        /** SFU WebSocket origin ([SfuConfig.baseUrl]). */
        val sfuBaseUrl: String,
        /** Live session token — mints the call-admission JWT. */
        val sessionToken: String,
        val audio: Boolean,
        val video: Boolean,
        /** The shared call-UI state the `CallScreen` renders. */
        val ui: CallUiState,
        /** Coroutine scope the session callbacks + admission timer ride. */
        val scope: CoroutineScope,
        /** Resolve a peer to a contact name (ADR-0021 peer_tag lookup). */
        val resolveDisplayName: ((peerId: String, accountPseudonym: String, peerTag: String) -> String)? = null,
        /** Fired once this client has an MLS group + derived seed (owner: at
         *  founding; joiner: on the Welcome). The landing screen switches from
         *  "waiting for owner" to the in-call view. */
        val onJoined: () -> Unit = {},
        /** Fired on the first `connected` SFU transition (media flowing). */
        val onLive: () -> Unit = {},
        /** Fired on a terminal (`closed` / `failed`) transition. */
        val onEnded: () -> Unit = {},
        /** Joiner only: no committer admitted this client within the
         *  admission window — the call has no live committer. */
        val onNoCommitter: () -> Unit = {},
        /** Fired on a fatal setup error. */
        val onError: (Throwable) -> Unit = {},
    )

    private val ui: CallUiState = opts.ui
    private val presence: CallPresence =
        CallPresence(hexKey(opts.identity.signaturePublicKey()), ownerKeyHex, ownerPeerTag)
    private val session: SfuCallSession

    /** The call MLS group (owner: founded; joiner: after the Welcome). */
    private var group: Group? = null
    /** This client's MLS leaf once it has a group. */
    private var localLeaf: Int? = initialLeaf
    private var joined = false

    /** G-22 dedup (FINDING 4). Keyed on the relay `from` SFU peer id ->
     *  wrapped Welcome, so ONE SFU-admitted peer is admitted at most once per
     *  call — even if it re-mints FRESH KeyPackages — and a retry re-serves
     *  the stored Welcome. Plus an exact-KP-bytes guard for an identical
     *  re-send. */
    private val admittedByPeer = HashMap<String, ByteArray>()
    private val admittedByKp = HashSet<String>()

    /** G-22: PRIMARY admission dedup — the joiner's stable MLS signature (leaf)
     *  key, so a peer re-minting FRESH KeyPackages under one identity is admitted
     *  at most once. Maps signature-key hex -> the stored wrapped Welcome. */
    private val admittedBySigKey = HashMap<String, ByteArray>()

    /** The joiner's wrapped KeyPackage, (re)broadcast until it has joined. */
    private var pendingKeyPackage: ByteArray? = null
    /** Joiner admission-timeout handle; cancelled once cleared (joined /
     *  dead). */
    private var admissionTimer: Job? = null
    /** `true` once the admission timeout fired — stops any rebroadcast. */
    private var timedOut = false
    /** Canonical safety number of the CURRENT epoch. */
    private var safetyNumber: String? = null
    private var firedLive = false

    /** Sticky last-known camera state per remote roster tile (keyed on
     *  "peer-<peerId>"). Closes the join-time reorder race: a newcomer
     *  subscribing to an already camera-muted peer still receives that peer's
     *  present-but-disabled video track, which fires [onRemoteVideoTrack] and
     *  would flip `camOn` back on. [onRemoteMediaState] is authoritative and
     *  records the true state here; the track-arrival path defers to it so a
     *  known-muted peer is never wrongly shown as camera-on. Adapter-local (one
     *  adapter per call); both callbacks are delivered on the same session
     *  scope, so no extra synchronisation is needed. */
    private val lastKnownCameraOn = mutableMapOf<String, Boolean>()

    init {
        localLeaf = initialLeaf
        session = SfuCallSession(
            applicationContext = applicationContext,
            config = SfuCallSession.Config(
                sfuBaseUrl = opts.sfuBaseUrl,
                jwt = jwt,
                // The joiner defers media until it holds a Welcome + seed; the
                // owner has its seed at founding and publishes immediately.
                seed = initialSeed,
                localLeaf = initialLeaf ?: 0,
                audio = opts.audio,
                video = opts.video,
                deferPublish = role == Role.JOINER,
            ),
            scope = opts.scope,
            callbacks = SfuCallSession.Callbacks(
                onStateChange = { s -> onSessionState(s) },
                onWelcome = { self, roster -> onSfuWelcome(self, roster) },
                onPeerJoined = { peerId, pseudo, tag -> onSfuPeerJoined(peerId, pseudo, tag) },
                onPeerLeft = { peerId -> onSfuPeerLeft(peerId) },
                onRemoteVideoTrack = { track, streamId -> onRemoteVideoTrack(track, streamId) },
                onRemoteMediaState = { peerId, cameraOn ->
                    // camera-off-cleanup — a remote peer signalled its camera
                    // on/off over the opaque SFU relay (SfuCallSession
                    // intercepts + fires this instead of onSignalRelay). Flip
                    // that peer's roster camOn so a mute — which fires no track
                    // event — stops its tile freezing on the last frame (and
                    // re-renders on unmute). Same target as the
                    // onRemoteVideoTrack camera flag, driven by an explicit
                    // signal. Mirrors SfuCallUi. Record the authoritative state
                    // so a later track ARRIVAL for this peer defers to it (the
                    // join-race guard in onRemoteVideoTrack).
                    val id = "$PEER_PREFIX$peerId"
                    lastKnownCameraOn[id] = cameraOn
                    ui.setParticipantCamera(id, cameraOn)
                },
                onLocalVideoTrack = { track -> ui.setLocalVideoTrack(track) },
                onSignalRelay = { from, payload -> onRelay(from, payload) },
            ),
        )
    }

    companion object {
        /** MLS exporter label for the per-call SFrame seed (audit-pack C-1).
         *  Must match every other client. */
        private const val CALL_SEED_LABEL = "lakko/call/v1"
        private const val CALL_SEED_LEN = 32u
        private const val PEER_PREFIX = "peer-"

        /**
         * How long a joiner keeps relaying its KeyPackage before giving up.
         * In the owner-founds model a call with no live committer is simply
         * dead — the owner shared the link then left before anyone joined, or
         * the call ended. After this window the joiner stops rebroadcasting
         * and surfaces "this call has ended or isn't active".
         */
        private const val JOINER_ADMISSION_TIMEOUT_MS = 18_000L

        /** Owner: found the call MLS group, derive the seed, connect +
         *  publish, and start admitting joiners. */
        suspend fun host(opts: Options): MultiJoinerCall {
            val callIdHex = hexKey(opts.callId)
            val jwt = mintJwt(opts, callIdHex)
            val ownerKeyHex = hexKey(opts.ownerSignatureKey)

            // Found the group (sole-founder — no race).
            val group = Group.create(opts.identity)
            val seed = group.exportSecret(
                opts.identity,
                CALL_SEED_LABEL,
                opts.callId,
                CALL_SEED_LEN,
            )
            // Owner-presence rides `self` (ownerPeerTag = null on the owner).
            val call = MultiJoinerCall(
                applicationContext = opts.applicationContext,
                opts = opts,
                role = Role.OWNER,
                jwt = jwt,
                ownerKeyHex = ownerKeyHex,
                ownerPeerTag = null,
                initialSeed = seed,
                initialLeaf = group.ownLeafIndex().toInt(),
            )
            call.group = group
            call.safetyNumber = safeSafetyNumber(seed)
            call.joined = true
            opts.onJoined()
            call.start()
            return call
        }

        /** Joiner: mint a fresh KeyPackage, connect signaling-only, relay the
         *  KP, and wait for the committer's Welcome. */
        suspend fun join(opts: Options): MultiJoinerCall {
            val callIdHex = hexKey(opts.callId)
            val jwt = mintJwt(opts, callIdHex)
            val ownerKeyHex = hexKey(opts.ownerSignatureKey)
            // The owner's per-call peer_tag lets a joiner detect owner
            // presence on the SFU roster (ADR-0021 + ADR-0032 election).
            val ownerPeerTag = sfuPeerTag(callIdHex, hexKey(opts.ownerAccountId))

            val call = MultiJoinerCall(
                applicationContext = opts.applicationContext,
                opts = opts,
                role = Role.JOINER,
                jwt = jwt,
                ownerKeyHex = ownerKeyHex,
                ownerPeerTag = ownerPeerTag,
                initialSeed = null,
                initialLeaf = null,
            )
            // Mint a fresh KeyPackage whose private keys persist in this
            // identity's provider (so the SAME identity opens the Welcome).
            val kp = opts.identity.createKeyPackage()
            call.pendingKeyPackage = wrapCallKeyPackage(kp)
            call.start()
            // Start the admission clock only once signaling is open + the
            // first broadcast is on its way.
            call.admissionTimer = opts.scope.launch {
                delay(JOINER_ADMISSION_TIMEOUT_MS)
                call.onAdmissionTimeout()
            }
            return call
        }

        private suspend fun mintJwt(opts: Options, callIdHex: String): String {
            val client = CallJwtClient()
            return try {
                client.setSessionToken(opts.sessionToken)
                client.mint(callId = callIdHex).jwt
            } finally {
                client.close()
            }
        }

        /** Safety number is display-only — a derivation failure must never
         *  abort. */
        private fun safeSafetyNumber(seed: ByteArray): String? =
            try {
                callSafetyNumber(seed)
            } catch (e: Throwable) {
                null
            }
    }

    private suspend fun start() {
        try {
            session.connect()
        } catch (err: Throwable) {
            opts.onError(err)
            throw err
        }
    }

    // -----------------------------------------------------------------
    // Public handle
    // -----------------------------------------------------------------

    /** Tear down the call. Idempotent. */
    fun hangUp() {
        clearAdmissionTimer()
        lastKnownCameraOn.clear()
        session.hangUp()
    }

    /** Mute / unmute the local mic. */
    fun setMicrophoneEnabled(on: Boolean) {
        session.setMicrophoneEnabled(on)
        ui.setMic(on)
    }

    /** Enable / disable the local camera. */
    fun setCameraEnabled(on: Boolean) {
        session.setCameraEnabled(on)
        ui.setCamera(on)
    }

    /**
     * ADR-0032 piece B — turn the camera ON mid-call (a link call that started
     * audio-only). Acquires the camera, SFrame-seals the new sender, and
     * renegotiates over the SFU. Idempotent + in-flight-guarded; throws on a
     * capture / arm failure so the caller reverts the toggle (fail-safe; the
     * call stays up audio-only, invariant #4). Delegates to the underlying
     * [SfuCallSession.publishVideo].
     */
    suspend fun publishVideo() {
        session.publishVideo()
    }

    private fun clearAdmissionTimer() {
        admissionTimer?.cancel()
        admissionTimer = null
    }

    /** Joiner admission window elapsed with no Welcome — the call has no live
     *  committer. Stop rebroadcasting, tear down signaling, surface it. */
    private fun onAdmissionTimeout() {
        admissionTimer = null
        if (joined) return
        timedOut = true
        pendingKeyPackage = null
        session.hangUp()
        opts.onNoCommitter()
    }

    // -----------------------------------------------------------------
    // SFU session callbacks -> UI state + presence
    // -----------------------------------------------------------------

    private fun onSessionState(state: SfuCallSession.State) {
        when (state) {
            SfuCallSession.State.CONNECTING ->
                ui.setPhase(CallUiState.CallPhase.Connecting)
            SfuCallSession.State.CONNECTED -> {
                ui.setPhase(CallUiState.CallPhase.Connected)
                ui.setE2ee(CallUiState.E2eeState(active = true, shortFingerprint = safetyNumber))
                if (!firedLive) {
                    firedLive = true
                    opts.onLive()
                }
            }
            SfuCallSession.State.FAILED,
            SfuCallSession.State.CLOSED -> {
                ui.setPhase(CallUiState.CallPhase.Ended(System.currentTimeMillis()))
                opts.onEnded()
            }
            SfuCallSession.State.IDLE -> Unit
        }
    }

    private fun onSfuWelcome(selfPeerId: String, roster: List<SfuPeerSnapshot>) {
        presence.setRoster(roster.map { it.peerId to it.peerTag })
        val remotes = roster
            .filter { it.peerId != selfPeerId }
            .map { remoteParticipant(nameOf(it.peerId, it.accountPseudonym, it.peerTag), it.peerId) }
        ui.setParticipants(listOf(localParticipant()) + remotes)
        broadcastKeyPackage()
    }

    private fun onSfuPeerJoined(peerId: String, accountPseudonym: String, peerTag: String) {
        presence.addPeer(peerId, peerTag)
        ui.participantJoined(remoteParticipant(nameOf(peerId, accountPseudonym, peerTag), peerId))
        // A newly-present peer may be a freshly-elected committer (owner
        // reclaim / a successor); re-broadcast so it sees a pending join.
        broadcastKeyPackage()
    }

    private fun onSfuPeerLeft(peerId: String) {
        presence.removePeer(peerId)
        ui.participantLeft("$PEER_PREFIX$peerId")
        lastKnownCameraOn.remove("$PEER_PREFIX$peerId")
        // The committer may have left -> the next inbound KP is evaluated
        // against the fresh election (this client might now be it).
    }

    private fun onRemoteVideoTrack(track: VideoTrack?, streamId: String?) {
        // N-way remote video — key the track on the source peer's roster id
        // ("peer-<streamId>"; the MSID stream id == the source peer id per the
        // SFU contract) so each tile renders its own peer, then set that
        // participant's camera flag so its video surface (gated on camOn)
        // renders — the same shape as SfuCallUi.
        //
        // Join-race guard: a newcomer subscribing to an already camera-muted
        // peer still receives that peer's present-but-disabled video track, so
        // this path fires even for a muted peer and would wrongly flip camOn
        // back on. [onRemoteMediaState] is authoritative and records the true
        // state in [lastKnownCameraOn]; here we DEFER to it — a track arrival
        // for a peer we already know is muted keeps it off, an unknown peer
        // defaults on, a null track is off. Guard a null streamId.
        streamId?.let { id ->
            val participantId = "$PEER_PREFIX$id"
            ui.setRemoteVideoTrack(track, participantId)
            val on = if (track != null) lastKnownCameraOn[participantId] ?: true else false
            ui.setParticipantCamera(participantId, on)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0032 handshake dispatch
    // -----------------------------------------------------------------

    private fun onRelay(from: String, payload: ByteArray) {
        val env = try {
            dispatchIncomingEnvelope(payload)
        } catch (e: Throwable) {
            return // malformed relay payload — drop
        }
        when (env.kind) {
            FfiIncomingEnvelopeKind.CALL_KEY_PACKAGE -> onIncomingKeyPackage(from, env.payload)
            FfiIncomingEnvelopeKind.CALL_WELCOME -> onIncomingWelcome(from, env.payload)
            // Not a call envelope — the fallback carries verbatim bytes, which
            // for us is a broadcast MLS commit an existing member must apply.
            FfiIncomingEnvelopeKind.INTRO_MESSAGE -> onMaybeCommit(env.payload)
            // camera-off-cleanup — a camera on/off media-state signal is
            // intercepted UPSTREAM by SfuCallSession's classifier, which fires
            // onRemoteMediaState (now wired above to flip the peer's roster
            // camOn) instead of onSignalRelay, so it never reaches here.
            // Explicit no-op for exhaustiveness + intent.
            FfiIncomingEnvelopeKind.MEDIA_STATE -> Unit
            // ADR-0042 — a push-routing-secret distribution envelope rides the
            // GROUP JOURNAL as an E2EE MLS application message (handled by the
            // chat receive loop), never the SFU signal-relay. A peer that
            // relays one here is off-contract — fail-closed drop.
            FfiIncomingEnvelopeKind.PUSH_ROUTING_SECRET -> Unit
            // ADR-0045 — a committer-liveness beacon likewise rides the GROUP
            // JOURNAL as an MLS application message, never the SFU
            // signal-relay, and it says nothing about a CALL committer (the
            // ADR-0032 election here is leaf/presence-based and independent).
            // A peer that relays one is off-contract — fail-closed drop.
            FfiIncomingEnvelopeKind.COMMITTER_BEACON -> Unit
            // ADR-0048 — a profile envelope rides the GROUP JOURNAL as an MLS
            // application message, where the sender is the MLS-authenticated
            // leaf. A relay payload has no such binding (the SFU peer id is not
            // an identity), so a profile arriving here could be bound to nobody
            // and is dropped, never stored (C-68).
            FfiIncomingEnvelopeKind.PROFILE -> Unit
            // ADR-0051 — a group-name envelope likewise rides the GROUP JOURNAL,
            // where the chat receive loop checks the sender's role. A relay
            // payload binds the name to nobody, so it is dropped, never stored.
            FfiIncomingEnvelopeKind.GROUP_NAME -> Unit
            // ADR-0048 trigger 4 — a profile REQUEST likewise rides the GROUP
            // JOURNAL, where the chat receive loop answers it with a profile
            // sealed into the group. A relay payload names no member to answer
            // and a call peer is not a group; dropped, never answered.
            FfiIncomingEnvelopeKind.PROFILE_REQUEST -> Unit
        }
    }

    /** Committer side: admit a joiner's KeyPackage (auto-admit for the
     *  reference). Only the ACTING committer acts; everyone else drops it. */
    private fun onIncomingKeyPackage(from: String, kpEnvelope: ByteArray) {
        val g = group ?: return
        if (localLeaf == null) return
        if (!isActingCommitter()) return

        // G-22: dedup PRIMARILY on the joiner's stable MLS signature (leaf) key
        // so a peer re-minting FRESH KeyPackages under one identity is added at
        // most once; the per-`from`-peer + exact-KP-bytes guards stay as fallbacks.
        val sigKeyHex = try {
            hexKey(keyPackageSignatureKey(kpEnvelope))
        } catch (e: Throwable) {
            null
        }
        val kpKey = hexKey(kpEnvelope)
        val priorWelcome = (sigKeyHex?.let { admittedBySigKey[it] }) ?: admittedByPeer[from]
        if (priorWelcome != null || admittedByKp.contains(kpKey)) {
            // Idempotent: re-serve the stored Welcome (a peer that lost it).
            if (priorWelcome != null) {
                session.sendSignalRelayToPeer(from, priorWelcome)
            }
            return
        }

        val beforeKeys = groupMembers().map { it.signatureKeyHex }.toHashSet()
        val commit: ByteArray
        val welcomeWrapped: ByteArray
        try {
            // The joiner sends `wrapCallKeyPackage(kp)`; dispatch already
            // stripped the domain separator, so `kpEnvelope` is the raw TLS
            // KeyPackage.
            val added = g.addMemberFromWire(opts.identity, kpEnvelope)
            commit = added.commit
            welcomeWrapped = wrapCallWelcome(added.welcome)
        } catch (err: Throwable) {
            // A malformed / already-used KeyPackage — refuse, do not advance.
            Log.w("MultiJoinerCall", "addMemberFromWire failed", err)
            return
        }
        if (sigKeyHex != null) admittedBySigKey[sigKeyHex] = welcomeWrapped
        admittedByPeer[from] = welcomeWrapped
        admittedByKp.add(kpKey)
        // Bind the new joiner's peer -> its member key for presence.
        for (m in groupMembers()) {
            if (!beforeKeys.contains(m.signatureKeyHex)) {
                presence.bindPeerToMember(from, m.signatureKeyHex)
            }
        }
        // Deliver the Welcome to the joiner + broadcast the commit to the
        // rest.
        session.sendSignalRelayToPeer(from, welcomeWrapped)
        session.sendSignalRelayToCall(commit)
        // The add advanced our epoch -> rekey to the new seed.
        rekeyToCurrentEpoch()
    }

    /** Joiner side: open the committer's Welcome and go live. */
    private fun onIncomingWelcome(from: String, welcomeEnvelope: ByteArray) {
        if (role != Role.JOINER || joined) return
        val joinedGroup: Group
        try {
            joinedGroup = Group.joinFromWelcome(welcomeEnvelope, opts.identity)
        } catch (err: Throwable) {
            Log.w("MultiJoinerCall", "joinFromWelcome failed", err)
            return
        }

        // FINDING 2 (defence-in-depth vs the successor-redirect residual):
        // confirm the JOINED group actually contains the OWNER whose signature
        // the SIGNED capability carried. The owner key comes from the
        // capability (not from any peer), so a Welcome that lands us in an
        // impostor's isolated group — no owner leaf — is a redirect. Refuse it
        // fail-closed (invariant #4): never publish media, tear down, surface
        // a clear error. A guest gets this check too (the owner key is in the
        // signed capability, not peer-asserted).
        val joinedKeys = joinedGroup.members().map { hexKey(it.signaturePublicKey) }
        if (!joinedKeys.contains(ownerKeyHex)) {
            Log.w(
                "MultiJoinerCall",
                "joined a group without the capability owner — redirect refused",
            )
            timedOut = true
            pendingKeyPackage = null
            clearAdmissionTimer()
            session.hangUp()
            opts.onError(
                IllegalStateException("Joined the wrong call — the owner is not in this group."),
            )
            return
        }

        group = joinedGroup
        val leaf = joinedGroup.ownLeafIndex().toInt()
        localLeaf = leaf
        joined = true
        pendingKeyPackage = null
        clearAdmissionTimer()

        val seed = deriveSeed(joinedGroup)
        safetyNumber = safeSafetyNumber(seed)
        // The committer (happy path: the owner) admitted us; bind its peer for
        // presence when we can identify it as the owner.
        if (presence.ownerPresent()) {
            presence.bindPeerToMember(from, ownerKeyHex)
        }
        opts.onJoined()
        // Publish media ONLY now — after joining + deriving the seed
        // (invariant #4).
        opts.scope.launch {
            try {
                session.publish(seed, leaf)
            } catch (err: Throwable) {
                opts.onError(err)
            }
        }
    }

    /** Existing member: apply a broadcast commit (epoch advance) + rekey. */
    private fun onMaybeCommit(bytes: ByteArray) {
        val g = group ?: return
        try {
            // The call group has no ADR-0022 role table — plain apply. A
            // non-commit / wrong-epoch / junk payload throws and is ignored.
            g.processIncoming(opts.identity, bytes)
        } catch (e: Throwable) {
            return
        }
        rekeyToCurrentEpoch()
    }

    // -----------------------------------------------------------------
    // Election + epoch helpers
    // -----------------------------------------------------------------

    private fun isActingCommitter(): Boolean {
        val leaf = localLeaf ?: return false
        if (group == null) return false
        val elected = electActingCommitterLeaf(
            groupMembers(),
            ownerKeyHex,
            presence.presentKeySet(),
        )
        return elected == leaf
    }

    private fun groupMembers(): List<ElectionMember> {
        val g = group ?: return emptyList()
        return g.members().map { ElectionMember(it.leafIndex.toInt(), hexKey(it.signaturePublicKey)) }
    }

    private fun deriveSeed(g: Group): ByteArray =
        g.exportSecret(opts.identity, CALL_SEED_LABEL, opts.callId, CALL_SEED_LEN)

    /** Re-derive the seed at the current epoch and rekey SFrame + the pill. */
    private fun rekeyToCurrentEpoch() {
        val g = group ?: return
        val seed = deriveSeed(g)
        session.rekey(seed)
        safetyNumber = safeSafetyNumber(seed)
        ui.setE2ee(CallUiState.E2eeState(active = true, shortFingerprint = safetyNumber))
    }

    private fun broadcastKeyPackage() {
        val kp = pendingKeyPackage
        if (joined || timedOut || kp == null) return
        session.sendSignalRelayToCall(kp)
    }

    // -----------------------------------------------------------------
    // UI-participant helpers (ported from SfuCallUi)
    // -----------------------------------------------------------------

    private fun nameOf(peerId: String, pseudonym: String, peerTag: String): String =
        opts.resolveDisplayName?.invoke(peerId, pseudonym, peerTag)
            ?: if (pseudonym.isNotEmpty()) pseudonym.take(8) else "Peer"

    private fun localParticipant(): CallUiState.Participant =
        CallUiState.Participant(
            id = "local",
            displayName = "You",
            isLocal = true,
            micOn = opts.audio,
            camOn = opts.video,
            handRaised = false,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
            screenSharing = false,
        )

    private fun remoteParticipant(displayName: String, peerId: String): CallUiState.Participant =
        CallUiState.Participant(
            id = "$PEER_PREFIX$peerId",
            displayName = displayName,
            isLocal = false,
            micOn = true,
            camOn = false,
            handRaised = false,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
            screenSharing = false,
        )
}
