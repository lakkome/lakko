// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.chat.MemberIdentity
import me.lakko.app.chat.resolveMemberIdentity
import me.lakko.app.chat.rosterOf
import me.lakko.app.profile.accountIdHexOrNull
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.GroupKind
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.SfuPeerSnapshot
import uniffi.lakko_ffi_uniffi.callSafetyNumber
import uniffi.lakko_ffi_uniffi.composeCallSignaling
import uniffi.lakko_ffi_uniffi.decodeProfileEnvelope
import uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope
import uniffi.lakko_ffi_uniffi.keyPackageSignatureKey
import uniffi.lakko_ffi_uniffi.parseCallSignaling
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
 *   * **Owner** ([host]) — the sole founder. Calls `Group.create(identity, GROUP)`,
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
    turn: TurnCredential?,
    private val ownerKeyHex: String,
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
        /** Owner's 16-byte `account_id` from the capability. A profile the
         *  owner seals into the call keeps its account id (and with it a
         *  username certificate) only when it claims this one
         *  ([boundProfileClaim]). */
        val ownerAccountId: ByteArray,
        /** Owner's 32-byte Ed25519 signature key from the capability
         *  (identifies the owner's MLS leaf + is the election's `is_owner`
         *  anchor + the FINDING 2 redirect-refusal check). */
        val ownerSignatureKey: ByteArray,
        /** SFU WebSocket origin ([SfuConfig.baseUrl]). */
        val sfuBaseUrl: String,
        val audio: Boolean,
        val video: Boolean,
        /** The shared call-UI state the `CallScreen` renders. */
        val ui: CallUiState,
        /** Coroutine scope the session callbacks + admission timer ride. */
        val scope: CoroutineScope,
        /** Names the remote tiles (`CallTileName.kt`, `CallPeerNames.kt`):
         *  the person a peer's verified announcement names (ADR-0078 section
         *  2), then what that member announces inside the call. */
        val names: CallPeerNames,
        /** A member announced its profile inside the call. `personKeyB64` is
         *  the MLS-authenticated sender, resolved to the account key at
         *  receipt (C-68). The profile keeps its claimed account id (and
         *  username certificate) only when bound ([boundProfileClaim]). The
         *  host files it in the profile store. */
        val onMemberProfile: (personKeyB64: MemberIdentity, profile: FfiProfile) -> Unit = { _, _ -> },
        /** The name on the local tile (`docs/ux/calls.md` section 4.4). */
        /** Empty when this device has no own name: the tile draws `call.participant.you`. */
        val localDisplayName: String = "",
        /** The user's own ADR-0048 profile envelope, or `null` when none is
         *  stored. Composed for a display name or a username alone: an
         *  account with no profile name is announced by its username. Sealed
         *  into the call's MLS group and relayed, so every other member — a
         *  guest with no contact list included — names this tile. The SFU
         *  relays ciphertext only. */
        val ownProfileEnvelope: (() -> ByteArray?)? = null,
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
        /** The link's owner ended the call for everyone (`docs/ux/calls.md`
         *  section 4.1.2). `starterName` is the name on the owner's tile, or
         *  `null` while that tile is unnamed. The host ends the call. */
        val onEndedForEveryone: (starterName: String?) -> Unit = {},
    )

    private val ui: CallUiState = opts.ui
    private val presence: CallPresence =
        CallPresence(hexKey(opts.identity.signaturePublicKey()), ownerKeyHex)
    private val session: SfuCallSession

    /** The call MLS group (owner: founded; joiner: after the Welcome). */
    private var group: Group? = null
    /** This client's MLS leaf once it has a group. */
    private var localLeaf: Int? = initialLeaf
    private var joined = false
    /** A broadcast that arrived before this client had a group to read it. */
    private class EarlyRelay(val from: String, val payload: ByteArray)

    private val earlyRelays = EarlyRelays<EarlyRelay>()

    /** G-22 dedup (FINDING 4). Keyed on the relay `from` SFU peer id ->
     *  wrapped Welcome, so ONE SFU-admitted peer is admitted at most once per
     *  call — even if it re-mints FRESH KeyPackages — and a retry re-serves
     *  the stored Welcome. Plus an exact-KP-bytes guard for an identical
     *  re-send. */
    private val admittedByPeer = HashMap<String, ByteArray>()
    private val admittedByKp = HashSet<String>()
    /** The exact-KeyPackage guard entry each admitted peer holds, so a
     *  departure can forget it along with the rest of the admission. */
    private val kpKeyByPeer = HashMap<String, String>()
    /** Periodic KeyPackage re-send while `pendingKeyPackage` is set. */
    private var rebroadcastJob: Job? = null
    /** The owner's rejoin probe (see [OWNER_REJOIN_PROBE_MS]). */
    private var ownerProbeJob: Job? = null

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

    /** Who is speaking (`docs/ux/group-call-grid.md` section 7.3), read on this
     *  device from the session's own connection while the call is connected. */
    private val speaking: SpeakingMonitor =
        SpeakingMonitor(
            scope = opts.scope,
            ui = opts.ui,
            sample = { session.sampleAudioLevels() },
            participantIdOf = { "$PEER_PREFIX$it" },
        )

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
                // Media waits for a seed on both sides: the joiner for its
                // Welcome, the owner for the SFU roster, which decides whether
                // it founds the group or comes back into one a successor holds.
                deferPublish = true,
                // ADR-0078 section 2: who each peer is comes from its own
                // sealed, signed announcement, verified against the call
                // group's roster (none before this client holds the group).
                peerAnnouncer = CallPeerAnnouncer.forGroup(opts.identity, opts.callId) { group },
            ),
            scope = opts.scope,
            callbacks = SfuCallSession.Callbacks(
                onStateChange = { s -> onSessionState(s) },
                onWelcome = { self, roster -> onSfuWelcome(self, roster) },
                onPeerJoined = { peerId -> onSfuPeerJoined(peerId) },
                onPeerIdentified = { peerId, identity -> onPeerIdentified(peerId, identity) },
                onPeerLeft = { peerId -> onSfuPeerLeft(peerId) },
                onRemoteVideoTrack = { track, streamId -> onRemoteVideoTrack(track, streamId) },
                // What the SFU says it gives of each person (`VideoGiven.kt`).
                onVideoGiven = { given -> applyVideoGiven(opts.ui, given) },
                // The call's current screen share (ADR-0083), as SfuCallUi.
                onScreenShare = { share -> opts.ui.setScreenShare(share) },
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
        // A joiner behind a symmetric NAT has no server-reflexive candidate
        // and reaches the room only through the relay. The credential came
        // with the call token (ADR-0078 section 2). Fail-soft: none leaves the
        // call on STUN.
        session.setTurnClient(TurnCredentialClient.preMinted(turn))
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

        /** A pending joiner re-sends its KeyPackage this often until admitted,
         *  so an admission is never lost to ordering: a committer that founds
         *  the group after the joiner's first broadcast, or that arrives
         *  later, still gets it. */
        private const val KEY_PACKAGE_REBROADCAST_MS = 3_000L

        /** The owner, finding other peers already in the room, asks to be
         *  admitted for this long before concluding nobody there holds a
         *  group and founding one. */
        private const val OWNER_REJOIN_PROBE_MS = 5_000L

        /** Owner: found the call MLS group, derive the seed, connect +
         *  publish, and start admitting joiners. */
        suspend fun host(opts: Options): MultiJoinerCall {
            val minted = mintJwt(opts)
            val ownerKeyHex = hexKey(opts.ownerSignatureKey)

            // The SFU roster decides (see `onSfuWelcome`): an empty room means
            // the owner founds the call group (sole founder, no race); a room
            // with people in it means the owner is coming BACK to a call a
            // successor kept alive, so it asks to be admitted like anyone else
            // and the election hands it the committer duty once it is in.
            // Owner presence rides `self` on the owner's client.
            val call = MultiJoinerCall(
                applicationContext = opts.applicationContext,
                opts = opts,
                role = Role.OWNER,
                jwt = minted.jwt,
                turn = minted.turn,
                ownerKeyHex = ownerKeyHex,
                initialSeed = null,
                initialLeaf = null,
            )
            call.start()
            return call
        }

        /** Joiner: mint a fresh KeyPackage, connect signaling-only, relay the
         *  KP, and wait for the committer's Welcome. */
        suspend fun join(opts: Options): MultiJoinerCall {
            val minted = mintJwt(opts)
            val ownerKeyHex = hexKey(opts.ownerSignatureKey)
            // Owner presence is learnt once this client holds the group: the
            // owner's sealed announcement verifies against the capability's
            // owner key (ADR-0078 section 2).
            val call = MultiJoinerCall(
                applicationContext = opts.applicationContext,
                opts = opts,
                role = Role.JOINER,
                jwt = minted.jwt,
                turn = minted.turn,
                ownerKeyHex = ownerKeyHex,
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

        /** The call token and the TURN credential beside it, paid for by
         *  one anonymous `call-join` token for the room of the link's call id
         *  (ADR-0078 section 2). */
        private suspend fun mintJwt(opts: Options): MintCallJwtResult =
            CallJwtClient().use { it.mint(opts.callId) }

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
            me.lakko.app.rethrowIfCancelled(err)
            opts.onError(err)
            throw err
        }
    }

    // -----------------------------------------------------------------
    // Public handle
    // -----------------------------------------------------------------

    /** Tear down the call. Idempotent. */
    fun hangUp() {
        earlyRelays.drain()
        clearAdmissionTimer()
        clearRebroadcast()
        clearOwnerProbe()
        lastKnownCameraOn.clear()
        session.hangUp()
    }

    /** Owner: found the call group and go live (an empty room, or a probe
     *  that nobody answered). */
    private fun foundGroup() {
        if (group != null || joined) return
        val founded: Group
        val seed: ByteArray
        val leaf: Int
        try {
            // ADR-0062 — a call-link group grows (ADR-0032 admits joiners
            // into it), so it follows the group rule.
            founded = Group.create(opts.identity, GroupKind.GROUP)
            seed = deriveSeed(founded)
            leaf = founded.ownLeafIndex().toInt()
        } catch (err: Throwable) {
            opts.onError(err)
            return
        }
        group = founded
        localLeaf = leaf
        joined = true
        pendingKeyPackage = null
        clearAdmissionTimer()
        clearRebroadcast()
        clearOwnerProbe()
        // Nothing kept from before was sealed for a group founded just now.
        earlyRelays.drain()
        safetyNumber = safeSafetyNumber(seed)
        opts.onJoined()
        broadcastOwnProfile()
        opts.scope.launch {
            try {
                session.publish(seed, leaf)
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err)
                opts.onError(err)
            }
        }
    }

    /** Owner, in a room with people: ask to be admitted (the successor holds
     *  the group) and fall back to founding when nobody answers. */
    private fun beginOwnerRejoinProbe() {
        if (group != null || joined || pendingKeyPackage != null) return
        try {
            pendingKeyPackage = wrapCallKeyPackage(opts.identity.createKeyPackage())
        } catch (err: Throwable) {
            opts.onError(err)
            return
        }
        broadcastKeyPackage()
        startRebroadcast()
        ownerProbeJob = opts.scope.launch {
            delay(OWNER_REJOIN_PROBE_MS)
            ownerProbeJob = null
            if (joined) return@launch
            pendingKeyPackage = null
            clearRebroadcast()
            foundGroup()
        }
    }

    private fun startRebroadcast() {
        if (rebroadcastJob != null) return
        rebroadcastJob = opts.scope.launch {
            while (true) {
                delay(KEY_PACKAGE_REBROADCAST_MS)
                if (pendingKeyPackage == null) {
                    rebroadcastJob = null
                    return@launch
                }
                broadcastKeyPackage()
            }
        }
    }

    private fun clearRebroadcast() {
        rebroadcastJob?.cancel()
        rebroadcastJob = null
    }

    private fun clearOwnerProbe() {
        ownerProbeJob?.cancel()
        ownerProbeJob = null
    }

    /** Re-run the naming rule for every remote tile. The host calls it when
     *  the profile store changes: a username verified, a contact renamed. */
    fun refreshNames() {
        opts.names.reapply(ui) { "$PEER_PREFIX$it" }
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

    /** Use the camera the person chose in the call's Settings sheet, or the
     *  default one for `null` ([SfuCallSession.selectCamera]). */
    fun selectCamera(cameraName: String?) {
        session.selectCamera(cameraName)
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
        clearRebroadcast()
        session.hangUp()
        opts.onNoCommitter()
    }

    // -----------------------------------------------------------------
    // SFU session callbacks -> UI state + presence
    // -----------------------------------------------------------------

    private fun onSessionState(state: SfuCallSession.State) {
        when (state) {
            SfuCallSession.State.CONNECTING -> {
                ui.setPhase(CallUiState.CallPhase.Connecting)
                // The tiles of this call say which picture of a person they
                // show (`VideoWants.kt`).
                bindTileVideoQuality(ui) { tile, peerId, quality ->
                    session.reportTileVideoQuality(tile, peerId, quality)
                }
            }
            SfuCallSession.State.CONNECTED -> {
                ui.setPhase(CallUiState.CallPhase.Connected)
                ui.setE2ee(CallUiState.E2eeState(active = true, shortFingerprint = safetyNumber))
                speaking.start()
                if (!firedLive) {
                    firedLive = true
                    opts.onLive()
                }
            }
            SfuCallSession.State.FAILED,
            SfuCallSession.State.CLOSED -> {
                speaking.stop()
                // The call's tracks are gone with its connection.
                ui.clearRemoteCopyVideoTracks()
                ui.setPhase(CallUiState.CallPhase.Ended(System.currentTimeMillis()))
                opts.onEnded()
            }
            SfuCallSession.State.IDLE -> Unit
        }
    }

    private fun onSfuWelcome(selfPeerId: String, roster: List<SfuPeerSnapshot>) {
        presence.setRoster(roster.map { it.peerId })
        val remotes = roster
            .filter { it.peerId != selfPeerId }
            .map { remoteParticipant(opts.names.name(it.peerId), it.peerId) }
        ui.setParticipants(listOf(localParticipant()) + remotes)
        if (role == Role.OWNER && group == null && !joined) {
            if (remotes.isEmpty()) foundGroup() else beginOwnerRejoinProbe()
            return
        }
        broadcastKeyPackage()
        if (pendingKeyPackage != null) startRebroadcast()
    }

    private fun onSfuPeerJoined(peerId: String) {
        presence.addPeer(peerId)
        ui.participantJoined(remoteParticipant(opts.names.name(peerId), peerId))
        // A newly-present peer may be a freshly-elected committer (owner
        // reclaim / a successor); re-broadcast so it sees a pending join.
        broadcastKeyPackage()
    }

    /** ADR-0078 section 2: a peer's sealed announcement verified against a
     *  leaf of the call group. Bind the peer to that member (presence, the
     *  committer election, a departure's removal) and to that person (the
     *  tile's name). */
    private fun onPeerIdentified(peerId: String, identity: VerifiedPeerIdentity) {
        presence.bindPeerToMember(peerId, identity.memberKeyHex)
        if (opts.names.peerIdentified(peerId, identity.personKeyB64, identity.memberKeyHex)) refreshNames()
    }

    private fun onSfuPeerLeft(peerId: String) {
        val memberKeyHex = presence.memberKeyFor(peerId)
        presence.removePeer(peerId)
        // Someone who leaves can come back: forget their admission (else the
        // G-22 dedup re-serves a Welcome a fresh app cannot open and they wait
        // forever) and, if this client is now the acting committer, remove
        // their leaf so the key rotates away from them.
        forgetAdmission(peerId, memberKeyHex)
        if (memberKeyHex != null) removeDepartedMember(memberKeyHex)
        opts.names.forget(peerId)
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
        //
        // A stream id ending in `~mid` or `~light` is the peer's middle track
        // or light companion (`LightStream.kt`): it goes to its own map and
        // creates no participant, no tile and no camera flag.
        applyRemoteVideoTrack(ui, track, streamId, lastKnownCameraOn)
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
            // for us is a broadcast MLS message: a commit an existing member
            // must apply, or a member's sealed profile.
            FfiIncomingEnvelopeKind.INTRO_MESSAGE -> onMaybeCommit(from, env.payload)
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
            // ADR-0048 — a bare (unsealed) profile on the relay binds to
            // nobody: the SFU peer id is not an identity. Names travel sealed
            // inside the call group instead (`broadcastOwnProfile`) and surface
            // through `onMaybeCommit` as an MLS application message. Dropped.
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
            // ADR-0052 decision 3 — a shared pin likewise rides the GROUP
            // JOURNAL, where the chat receive loop checks the sender's standing
            // under the conversation's lock. A relay payload binds the pin to
            // no conversation and no member; dropped, never stored.
            FfiIncomingEnvelopeKind.PIN -> Unit
            // ADR-0060 / ADR-0057 — the scoped-discovery envelopes and the
            // registrar seed ride a conversation journal or the DeviceGroup,
            // never the SFU signal-relay. Off-contract here — dropped.
            FfiIncomingEnvelopeKind.INTRODUCTION_OFFER,
            FfiIncomingEnvelopeKind.INTRODUCTIONS,
            FfiIncomingEnvelopeKind.VISIBILITY,
            FfiIncomingEnvelopeKind.REACH_STATE,
            FfiIncomingEnvelopeKind.REGISTRAR_SEED,
            FfiIncomingEnvelopeKind.COMMITTER_DELEGATION,
            // ADR-0087 — handle, knock and admission seeds, group photo and
            // group reactions likewise ride a conversation journal or the
            // DeviceGroup, never the SFU signal-relay. Dropped.
            FfiIncomingEnvelopeKind.CONVERSATION_HANDLE,
            FfiIncomingEnvelopeKind.KNOCK_SEED,
            FfiIncomingEnvelopeKind.ADMISSION_SEED,
            FfiIncomingEnvelopeKind.GROUP_PHOTO,
            FfiIncomingEnvelopeKind.GROUP_REACTIONS,
            -> Unit
            // ADR-0059 - a shared-channels list rides the GROUP JOURNAL and
            // speaks for its MLS sender, never the SFU signal-relay; dropped.
            FfiIncomingEnvelopeKind.SHARED_CHANNELS -> Unit
            // ADR-0073 phase 2 — a group-closed notice rides the GROUP JOURNAL,
            // where the chat receive loop checks the sender's role. A relay
            // payload closes nothing; dropped.
            FfiIncomingEnvelopeKind.GROUP_CLOSED -> Unit
            // ADR-0074 part 3 - a key-rotation envelope means something ONLY
            // in the account's own DeviceGroup. A relay payload changes no
            // keys; dropped, and never rendered.
            FfiIncomingEnvelopeKind.KEY_ROTATION -> Unit
            // ADR-0080 - a device card means something ONLY in the account's
            // own DeviceGroup. A relay payload names no device; dropped.
            FfiIncomingEnvelopeKind.DEVICE_CARD -> Unit
            // ADR-0079 (reworked 2026-10-17) - a link-request claim means
            // something ONLY in the account's own DeviceGroup; dropped.
            FfiIncomingEnvelopeKind.LINK_CLAIM -> Unit
            // ADR-0079 (order of 2026-10-17) - a link-founded note, the same.
            FfiIncomingEnvelopeKind.LINK_FOUNDED -> Unit
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
        kpKeyByPeer[from] = kpKey
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
        // The newcomer decrypts only from this epoch on: tell it who we are.
        broadcastOwnProfile()
    }

    /** Joiner side: open the committer's Welcome and go live. The returning
     *  owner is a joiner here too (see `host`). */
    private fun onIncomingWelcome(from: String, welcomeEnvelope: ByteArray) {
        if (joined || pendingKeyPackage == null) return
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
        clearRebroadcast()
        clearOwnerProbe()

        // Before the seed is derived: a kept commit advances the epoch, and
        // media must be published under the key of the epoch the group is at.
        replayEarlyRelays()
        val seed = deriveSeed(joinedGroup)
        safetyNumber = safeSafetyNumber(seed)
        // Who the committer's peer is (and the owner's) comes from their
        // sealed announcements, which this client can open from now on.
        opts.onJoined()
        broadcastOwnProfile()
        // Publish media ONLY now — after joining + deriving the seed
        // (invariant #4).
        opts.scope.launch {
            try {
                session.publish(seed, leaf)
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err)
                opts.onError(err)
            }
        }
    }

    /** Process what arrived before the Welcome, oldest first. A payload this
     *  epoch cannot open is dropped inside [onMaybeCommit], as it always was.
     *  Media is not published yet, so there is no key to rotate: the caller
     *  derives the seed once, after this. */
    private fun replayEarlyRelays() {
        for (relay in earlyRelays.drain()) onMaybeCommit(relay.from, relay.payload, beforePublish = true)
    }

    /** Existing member: apply a broadcast MLS message — a commit (epoch
     *  advance -> rekey) or a member's sealed application message (today: the
     *  ADR-0048 profile that names its tile). */
    private fun onMaybeCommit(from: String, bytes: ByteArray, beforePublish: Boolean = false) {
        val g = group
        if (g == null) {
            // Not readable yet: a member's profile can outrun the Welcome.
            // Kept and replayed once the group exists ([replayEarlyRelays]).
            earlyRelays.keep(EarlyRelay(from, bytes))
            return
        }
        val processed = try {
            // The call group has no ADR-0022 role table — plain apply. A
            // wrong-epoch / junk payload throws and is ignored.
            g.processIncoming(opts.identity, bytes)
        } catch (e: Throwable) {
            return
        }
        when (processed) {
            is ProcessedMessage.Application -> {
                // Who the relaying peer is comes from its verified
                // announcement (ADR-0078 section 2), not from the sender of a
                // message it relayed: anyone can relay a sealed message it
                // received.
                val senderLeaf = processed.senderLeaf?.toInt()
                val senderLeafKeyHex = senderLeaf?.let { leaf ->
                    groupMembers().firstOrNull { it.leafIndex == leaf }?.signatureKeyHex
                }
                // The sender is resolved to the PERSON here, as the chat
                // receive path does: this is the one moment the roster is the
                // epoch the message was sealed at (ADR-0050, C-68).
                val senderKey = resolveMemberIdentity(rosterOf(g), processed.senderLeaf)
                // Whether the MLS-authenticated sender is the link's owner: the
                // key the SIGNED capability names, matched the way the join
                // check does.
                val fromOwner = linkCallSenderIsOwner(senderLeafKeyHex, ownerKeyHex)
                onMemberApplication(from, senderKey, senderLeafKeyHex, fromOwner, processed.data)
            }
            is ProcessedMessage.Commit -> {
                // Replayed before the Welcome's own announcement and seed
                // derivation, which cover both of these.
                if (beforePublish) return
                rekeyToCurrentEpoch()
                // Someone was admitted: they decrypt only from this epoch on,
                // so every member re-announces its name for the newcomer.
                broadcastOwnProfile()
            }
            else -> Unit
        }
    }

    /** A member's sealed application message: the owner's end for everyone
     *  (`docs/ux/calls.md` section 4.1.2), or a profile; anything else is
     *  ignored. What a profile announces binds to the relaying SFU
     *  peer — the same binding the peer's media tracks already rest on — and
     *  names the tile at once, a profile with a username and no display name
     *  included. The profile is filed in the profile store, under the person
     *  who sealed it, only when the account it claims is the account the SFU
     *  admitted that peer as; the tile is then named by the one resolver (a
     *  verified `@username`, the viewer's own name for a contact). */
    private fun onMemberApplication(
        from: String,
        senderKey: MemberIdentity?,
        senderLeafKeyHex: String?,
        fromOwner: Boolean,
        plaintext: ByteArray,
    ) {
        if (saysEndForEveryone(plaintext)) {
            // Acted on only from the link's owner; from anyone else it is
            // ignored and takes nobody out of the call.
            if (fromOwner) {
                val name = ui.participants.value
                    .firstOrNull { it.id == "$PEER_PREFIX$from" }
                    ?.displayName
                    ?.takeIf { it.isNotBlank() }
                opts.onEndedForEveryone(name)
            }
            return
        }
        val profile = try {
            decodeProfileEnvelope(plaintext)
        } catch (e: Throwable) {
            return
        }
        // The announced name names `from` only when `from` announced the leaf
        // that sealed this profile; a relayed copy names nobody.
        opts.names.memberAnnounced(
            peerId = from,
            senderLeafKeyHex = senderLeafKeyHex,
            displayName = profile.displayName,
            username = profile.username,
        )
        if (senderKey != null) {
            opts.onMemberProfile(senderKey, boundProfileClaim(profile, fromOwner, opts.ownerAccountId))
        }
        refreshNames()
    }

    /** Whether a sealed application message is "this call has been ended for
     *  everyone". States an intent only; the caller checks who sent it. */
    private fun saysEndForEveryone(plaintext: ByteArray): Boolean {
        val body = try {
            parseCallSignaling(plaintext)
        } catch (e: Throwable) {
            return false
        }
        return isEndForEveryone(body, opts.callId)
    }

    /**
     * End the call for every person in it. Only the link's owner may: every
     * other member checks the sender against the owner key of the signed
     * capability and ignores it from anyone else. Sealed into the call's own
     * group, so the SFU relays ciphertext. Best-effort: the caller hangs up
     * whether or not this went out.
     */
    fun endForEveryone() {
        val g = group ?: return
        if (role != Role.OWNER || !joined) return
        try {
            val payload = composeCallSignaling(
                FfiCallSignalingBody.End(callId = opts.callId, reason = FfiCallEndReason.ENDED_FOR_ALL),
            )
            session.sendSignalRelayToCall(g.createApplicationMessage(opts.identity, payload))
        } catch (e: Throwable) {
            Log.w("MultiJoinerCall", "the end for everyone was not sent", e)
        }
    }

    private fun forgetAdmission(peerId: String, memberKeyHex: String?) {
        admittedByPeer.remove(peerId)
        kpKeyByPeer.remove(peerId)?.let { admittedByKp.remove(it) }
        if (memberKeyHex != null) admittedBySigKey.remove(memberKeyHex)
    }

    /** Acting committer: remove a departed member's leaf and rekey. */
    private fun removeDepartedMember(memberKeyHex: String) {
        val g = group ?: return
        if (!joined) return
        if (memberKeyHex == hexKey(opts.identity.signaturePublicKey())) return
        val member = groupMembers().firstOrNull { it.signatureKeyHex == memberKeyHex } ?: return
        if (!isActingCommitter()) return
        val commit = try {
            g.removeMember(opts.identity, member.leafIndex.toUInt())
        } catch (err: Throwable) {
            Log.w("MultiJoinerCall", "could not remove a departed member", err)
            return
        }
        session.sendSignalRelayToCall(commit)
        rekeyToCurrentEpoch()
    }

    /** Seal the own profile into the call group and relay it to every member.
     *  Best-effort: a missing profile or a failed send only leaves this
     *  device's tile unnamed on the others' screens. */
    private fun broadcastOwnProfile() {
        val g = group ?: return
        if (!joined) return
        val envelope = opts.ownProfileEnvelope?.invoke() ?: return
        try {
            session.sendSignalRelayToCall(g.createApplicationMessage(opts.identity, envelope))
        } catch (e: Throwable) {
            Log.w("MultiJoinerCall", "own profile not announced", e)
        }
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

    private fun localParticipant(): CallUiState.Participant =
        CallUiState.Participant(
            id = "local",
            displayName = opts.localDisplayName,
            isLocal = true,
            micOn = opts.audio,
            camOn = opts.video,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
        )

    private fun remoteParticipant(displayName: String, peerId: String): CallUiState.Participant =
        CallUiState.Participant(
            id = "$PEER_PREFIX$peerId",
            displayName = displayName,
            isLocal = false,
            micOn = true,
            camOn = false,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
        )
}
