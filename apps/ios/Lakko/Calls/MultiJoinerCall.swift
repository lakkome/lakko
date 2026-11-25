// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import WebRTC

/// MLS exporter label for the per-call SFrame seed (audit-pack C-1). Must match
/// every other client.
private let callSeedLabel = "lakko/call/v1"
private let callSeedLen: UInt32 = 32
private let peerPrefix = "peer-"
/// How long a joiner keeps relaying its KeyPackage before giving up. In the
/// owner-founds model a call with no live committer is simply dead — the owner
/// shared the link then left before anyone joined, or the call ended. After this
/// window the joiner stops rebroadcasting and surfaces "this call has ended or
/// isn't active" rather than waiting forever.
private let joinerAdmissionTimeoutNs: UInt64 = 18_000_000_000
/// A pending joiner re-sends its KeyPackage this often until admitted, so an
/// admission is never lost to ordering: a committer that founds the group
/// after the joiner's first broadcast, or that arrives later, still gets it.
let keyPackageRebroadcastNs: UInt64 = 3_000_000_000
/// The owner, finding other peers already in the room, asks to be admitted for
/// this long before concluding nobody there holds a group and founding one.
let ownerRejoinProbeNs: UInt64 = 5_000_000_000

/// ADR-0032 multi-joiner call-link orchestrator (iOS) — the native mirror of
/// `apps/web/src/calls/multi-joiner-call.ts`.
///
/// Drives the "owner-founds + deterministic committer election" protocol on top
/// of the EXISTING SFU signal-relay — no server change.
///
/// ## Roles
///
///   * **Owner** (``host(options:ui:callbacks:)``) — the sole founder. Calls
///     `Group.create(identity)`, derives the SFrame seed, joins the SFU room,
///     and admits joiners (it is the acting committer while present).
///   * **Joiner** (``join(options:ui:callbacks:)``) — never founds a group.
///     Mints a fresh KeyPackage, connects to the SFU room signaling-only, relays
///     `wrapCallKeyPackage(kp)` to the room, waits for the committer's
///     `wrapCallWelcome(welcome)`, then `Group.joinFromWelcome` + derives the
///     same seed. Media publishes ONLY after the group is joined (invariant #4 —
///     no plaintext / half-open media).
///
/// ## Committer election (survives the owner leaving)
///
/// On every SFU roster change the acting committer is recomputed as a pure
/// function of public state (`electCommitter`, via ``electActingCommitterLeaf``):
/// the owner if present, else the lowest-leaf present member. Only the member
/// that IS the acting committer processes an inbound KeyPackage. Failover +
/// reclaim fall out with no election protocol.
///
/// ## Three hardenings (lakko-security)
///
///   1. **Post-join owner-membership check** — after `joinFromWelcome` the
///      joined group MUST contain the OWNER whose signature the SIGNED
///      capability carried, else a Welcome that landed us in an impostor's
///      isolated group is refused fail-closed (no media, teardown, error).
///   2. **G-22 dedup** — one SFU-admitted peer is admitted at most once per
///      call (keyed on the relay `from` peer id + the exact KP bytes); a retry
///      re-serves the stored Welcome.
///   3. **Double-tap latch** — the factories synchronously guard against a
///      second start (via the caller's landing-screen latch + the internal
///      `joined` / `timedOut` / dedup gates).
@MainActor
final class MultiJoinerCall {

    // MARK: - Public API types

    /// Inputs shared by both roles.
    struct Options {
        /// The canonical device `Identity` (rebuilt from the phrase seed).
        let identity: Identity
        /// 16-byte `call_id` from the capability.
        let callId: Data
        /// Owner's 16-byte `account_id` from the capability. A profile the
        /// owner seals into the call keeps its account id (and with it a
        /// username certificate) only when it claims this one
        /// (``CallTileName/boundProfileClaim(_:fromOwner:ownerAccountId:)``).
        let ownerAccountId: Data
        /// Owner's 32-byte Ed25519 signature key from the capability (identifies
        /// the owner's MLS leaf + is the election's `is_owner` anchor).
        let ownerSignatureKey: Data
        /// SFU WebSocket origin (`SfuConfig.baseUrl`).
        let sfuBaseUrl: String
        /// Mic request.
        let audio: Bool
        /// Camera request.
        let video: Bool
        /// What the remote tiles are named through (``LinkCallTileNames``):
        /// the people this device already knows, the one display resolver and
        /// the profile store a member's sealed profile is filed in. The
        /// default knows nobody and files nothing.
        var naming = LinkCallTileNames.Ports()
        /// `services/auth` base URL for the JWT mint (default `CallJwtClient`
        /// default).
        var authBaseUrl: String?
        /// The name on the local tile (`docs/ux/calls.md` section 4.4).
        var localDisplayName: String = ""
        /// The name this person typed for this call (`docs/ux/calls.md`
        /// section 4.1): a guest's name from the link page. `nil`: the name
        /// typed for this call earlier in this app run, else none.
        var initialCallName: String?
        /// The user's own ADR-0048 profile envelope, or `nil` without an
        /// account. Sealed into the call's MLS group and relayed, so every
        /// other member — a guest with no contact list included — names this
        /// tile. It carries the display name or, when there is none, the
        /// username, so there is always something to name the tile by. The
        /// SFU relays ciphertext only.
        var ownProfileEnvelope: (() -> Data?)?
    }

    /// Landing-screen callbacks.
    struct Callbacks {
        /// Fired once this client has an MLS group + derived seed (owner: at
        /// founding; joiner: on the Welcome). The landing switches from "waiting
        /// for owner" to the in-call view.
        var onJoined: (() -> Void)?
        /// Fired on the first `connected` SFU transition (media flowing).
        var onLive: (() -> Void)?
        /// Fired on a terminal (`closed` / `failed`) transition.
        var onEnded: (() -> Void)?
        /// Fired instead of ``onEnded`` when the session failed because the
        /// SFU refused this device: the call had already ended
        /// (`ErrorCode::CallEnded`, sent in place of the Welcome). `nil` falls
        /// back to ``onEnded``.
        var onCallAlreadyEnded: (() -> Void)? = nil
        /// Joiner only: no committer admitted this client within the admission
        /// window — the call has no live committer.
        var onNoCommitter: (() -> Void)?
        /// Fired on a fatal setup error (incl. the owner-membership refusal).
        var onError: ((Error) -> Void)?
        /// The link's owner ended the call for everyone
        /// (`docs/ux/calls.md` section 4.1.2). `peerId` is the SFU peer that
        /// relayed it, for the name on the notice. The host ends the call.
        var onEndedForEveryone: ((_ peerId: String) -> Void)?
    }

    enum Role { case owner, joiner }

    // MARK: - Wiring

    private let opts: Options
    private let cbs: Callbacks
    private let ui: CallUiState
    private let role: Role
    private let ownerKeyHex: String
    private let presence: CallPresence
    /// Implicitly-unwrapped so the callbacks can capture a fully-initialised
    /// `self` before the session is assigned (assigned at the end of `init`).
    private var session: SfuCallSession!

    // MARK: - Mutable state

    /// The call MLS group (owner: founded; joiner: after the Welcome).
    private var group: Group?
    /// This client's MLS leaf once it has a group.
    private var localLeaf: UInt32?
    private var joined = false
    /// Who is behind each remote tile and what it is called
    /// (``CallTileName``): what each member announced over the call group, and
    /// the person the profile store knows them as.
    private let names: LinkCallTileNames
    /// The name this device typed for this call (`""` = none).
    private var ownCallName = ""
    /// Broadcasts that arrived before this client had a group to read them.
    private var earlyRelays = EarlyRelays<(from: String, bytes: Data)>()

    /// G-22 dedup: relay `from` SFU peer id → wrapped Welcome, so ONE
    /// SFU-admitted peer is admitted at most once per call — even if it re-mints
    /// FRESH KeyPackages — and a retry re-serves the stored Welcome. Plus an
    /// exact-KP-bytes guard for an identical re-send.
    private var admittedByPeer: [String: Data] = [:]
    private var admittedByKp: Set<String> = []
    /// The exact-KeyPackage guard entry each admitted peer holds, so a
    /// departure can forget it along with the rest of the admission.
    private var kpKeyByPeer: [String: String] = [:]
    /// Periodic KeyPackage re-send while `pendingKeyPackage` is set.
    private var rebroadcastTask: Task<Void, Never>?
    /// The owner's rejoin probe (see `ownerRejoinProbeNs`).
    private var ownerProbeTask: Task<Void, Never>?
    /// G-22: PRIMARY admission dedup — the joiner's stable MLS signature (leaf)
    /// key, so a peer re-minting FRESH KeyPackages under one identity is admitted
    /// at most once. Maps signature-key hex → the stored wrapped Welcome.
    private var admittedBySigKey: [String: Data] = [:]
    /// The joiner's wrapped KeyPackage, (re)broadcast until it has joined.
    private var pendingKeyPackage: Data?
    /// Joiner admission-timeout handle; `nil` once cleared (joined / dead).
    private var admissionTask: Task<Void, Never>?
    /// `true` once the admission timeout fired — stops any further rebroadcast.
    private var timedOut = false
    /// Canonical safety number of the CURRENT epoch.
    private var safetyNumber: String?
    private var firedLive = false
    /// Sticky last-known remote camera state, keyed by the roster tile id
    /// (`"peer-<peerId>"`), so a video-track ARRIVAL for a peer we already
    /// know is camera-muted is never wrongly flipped back on (the join-time
    /// track-vs-media-state reorder race — a newcomer subscribing to an
    /// already-muted peer gets the still-present disabled track, whose add
    /// can land after the muted re-announce). Media-state is authoritative;
    /// a track arrival only defers to it. `@MainActor`-confined like the
    /// rest of this class — no new concurrency.
    private var lastKnownCameraOn: [String: Bool] = [:]
    /// The microphone each remote peer last announced, keyed like
    /// ``lastKnownCameraOn``. A state that arrives before the peer's tile
    /// exists is not lost: the tile is built from it.
    private var lastKnownMicOn: [String: Bool] = [:]
    /// The sharers' screen tracks and their first-frame probes (ADR-0083).
    private let screenTracks = ScreenShareTracks()
    /// Who is speaking, read on this device from the call's own audio
    /// (`docs/ux/group-call-grid.md` section 7.3).
    private let speaking: GroupSpeakingMonitor

    // MARK: - Init

    private init(
        opts: Options,
        cbs: Callbacks,
        ui: CallUiState,
        role: Role,
        jwt: String,
        turn: TurnCredential?,
        ownerKeyHex: String,
        initialSeed: Data?,
        initialLeaf: UInt32?
    ) {
        self.opts = opts
        self.cbs = cbs
        self.ui = ui
        self.role = role
        self.ownerKeyHex = ownerKeyHex
        self.localLeaf = initialLeaf
        self.speaking = GroupSpeakingMonitor(ui: ui)
        self.names = LinkCallTileNames(ports: opts.naming)
        self.presence = CallPresence(
            selfKeyHex: hexEncode(opts.identity.signaturePublicKey()),
            ownerKeyHex: ownerKeyHex
        )

        let config = SfuCallSession.Config(
            sfuBaseUrl: opts.sfuBaseUrl,
            jwt: jwt,
            // The joiner defers media until it holds a Welcome + seed; the owner
            // has its seed at founding and publishes immediately.
            seed: initialSeed,
            localLeaf: initialLeaf,
            audio: opts.audio,
            video: opts.video,
            // Media waits for a seed on both sides: the joiner for its Welcome,
            // the owner for the SFU roster, which decides whether it founds the
            // group or comes back into one a successor is holding.
            deferPublish: true,
            // ADR-0078 section 2: who each peer is comes from its own sealed,
            // signed announcement, verified against the call group's roster
            // (none before this client holds the group).
            peerAnnouncer: CallPeerAnnouncer.forGroup(
                identity: opts.identity,
                callId: opts.callId,
                group: { [weak self] in self?.group }
            )
        )
        // All stored properties except `session` are now set, so `self` is
        // fully initialised and the callbacks may capture it (weakly). The
        // closures inherit `@MainActor` from this init, so they may call the
        // isolated instance methods directly.
        let callbacks = SfuCallSession.Callbacks(
            onStateChange: { [weak self] state in self?.onSessionState(state) },
            onWelcome: { [weak self] selfPeerId, roster in
                self?.onSfuWelcome(selfPeerId: selfPeerId, roster: roster)
            },
            onPeerJoined: { [weak self] peerId in self?.onSfuPeerJoined(peerId: peerId) },
            onPeerIdentified: { [weak self] peerId, identity in
                self?.onPeerIdentified(peerId: peerId, identity: identity)
            },
            onPeerLeft: { [weak self] peerId in self?.onSfuPeerLeft(peerId: peerId) },
            onRemoteVideoTrack: { [weak self] track, streamId in
                // N-way SFU remote video — the SFU pins the downstream MSID
                // stream-id to the source peer id, and the roster participant
                // id is `"peer-<peerId>"`, so route the track to that tile.
                // A nil stream-id (a camera-off `didRemove rtpReceiver` fires
                // `onRemoteVideoTrack(nil, nil)` with no MSID) can't target one
                // map entry, so drop it here rather than mis-clear a tile; the
                // frozen last frame clears on peer-left. Mirrors `SfuCallUi`.
                guard let self, let streamId else { return }
                let id = "peer-\(streamId)"
                self.ui.setRemoteVideoTrack(track, forParticipant: id)
                // Flip the tile's `camOn` so its video surface (gated on
                // `participant.camOn` in `CallScreen`) actually paints on track
                // arrival — and stops on a nil track. Mirrors web
                // `setParticipantCamera`.
                //
                // Join-race guard: a track ARRIVAL for a peer we already know
                // is camera-muted (via a prior `onRemoteMediaState`) must NOT
                // flip them back on — the disabled track is still present on
                // the SFU and its add can land after the muted re-announce.
                // Defer to the sticky last-known state; an unknown peer defaults
                // on (they published video); a nil track is always off.
                let on = MultiJoinerCall.joinRaceCameraOn(
                    trackPresent: track != nil,
                    lastKnown: self.lastKnownCameraOn[id]
                )
                self.ui.setParticipantCamera(id, on)
            },
            onLocalVideoTrack: { [weak self] track in self?.ui.setLocalVideoTrack(track) },
            onServerError: { _, _ in },
            onSignalRelay: { [weak self] from, payload in self?.onRelay(from: from, payload: payload) },
            onRemoteMediaState: { [weak self] from, cameraOn in
                // Camera on/off cleanup: a remote peer broadcast its camera
                // state. Flip that participant's roster `camOn` gate (keyed on
                // `"peer-<peerId>"`, the same tile id `onRemoteVideoTrack` uses)
                // — the only signal that clears a remote camera-OFF mute (which
                // fires no track event) and re-shows the tile on camera-back-on.
                // The session intercepts media-state upstream, so it never
                // reaches `onRelay`'s (now-dead) `.mediaState` branch. UI-only;
                // SFrame/seed/MLS untouched.
                guard let self else { return }
                let id = "peer-\(from)"
                // Media-state is authoritative — record it so a later (racy)
                // track arrival for a muted peer defers to it (join-race guard).
                self.lastKnownCameraOn[id] = cameraOn
                self.ui.setParticipantCamera(id, cameraOn)
            },
            onRemoteMicrophoneState: { [weak self] from, microphoneOn in
                // The person's own word on their microphone: the mute mark on
                // their tile and in the participant list (group-call-grid 7.2).
                guard let self else { return }
                let id = "peer-\(from)"
                self.lastKnownMicOn[id] = microphoneOn
                self.ui.setParticipantMic(id, microphoneOn)
            },
            onRemoteLightVideoTrack: { [weak self] track, peerId in
                // The light companion of that peer's camera (``LightStream``):
                // kept beside the main track for the peer's tile to draw, and
                // nothing else. No participant, no camera flag.
                self?.ui.setRemoteLightVideoTrack(track, forParticipant: "peer-\(peerId)")
            },
            onRemoteMiddleVideoTrack: { [weak self] track, peerId in
                // That peer's middle track (ADR-0070 step 5): the heavy
                // picture a tile watches when the shared rule says `middle`.
                self?.ui.setRemoteMiddleVideoTrack(track, forParticipant: "peer-\(peerId)")
            },
            onVideoGiven: { [weak self] given in
                // What the SFU gives of each person (ADR-0070 step 6).
                self?.ui.setRemoteVideoGiven(byPeerId: given)
            },
            onScreenShareChanged: { [weak self] current in
                // The fold's current share (ADR-0083, C-87). Mirrors `SfuCallUi`.
                guard let self else { return }
                let prefix = peerPrefix
                self.ui.setScreenShare(
                    ScreenShareShown.of(current, sharerParticipantId: { prefix + $0 }))
            },
            onRemoteScreenVideoTrack: { [weak self] track, peerId in
                guard let self else { return }
                self.screenTracks.attach(track, sharer: peerPrefix + peerId, ui: self.ui)
            },
            onRemoteCallName: { [weak self] peerId, name in
                // The name that peer typed for this call outranks every other
                // source (``CallTileName``).
                guard let self else { return }
                self.names.setCallName(peerId, name)
                self.names.relabel(self.ui)
            },
            onCallChat: { [weak self] peerId, seq, text in
                // A verified message in the call's chat (section 4.1.4).
                self?.ui.chat.receive(peerId: peerId, seq: seq, text: text)
            },
            onCallChatPicture: { [weak self] peerId, seq, picture in
                // A verified picture in the call's chat (section 4.1.4.1).
                self?.ui.chat.receivePicture(peerId: peerId, seq: seq, picture: picture)
            }
        )
        self.session = SfuCallSession(config: config, callbacks: callbacks)
        // The name this device typed for the call (`docs/ux/calls.md` section
        // 4.1): the guest's name from the link page, else one typed for this
        // call earlier in this app run. Sent with every announcement, shown on
        // the own tile, changed from the call's Settings sheet.
        let callIdHex = hexEncode(opts.callId)
        let initialName = opts.initialCallName ?? CallNameMemory.remembered(callIdHex: callIdHex)
        self.ownCallName = initialName
        CallNameMemory.remember(callIdHex: callIdHex, name: initialName)
        self.session.setOwnCallName(initialName)
        ui.setCallNameControl(
            ownName: initialName,
            defaultName: opts.localDisplayName,
            save: { [weak self] name in
                guard let self else { return }
                self.ownCallName = name
                CallNameMemory.remember(callIdHex: callIdHex, name: name)
                self.session.setOwnCallName(name)
                self.ui.setParticipantName("local", self.localTileName())
            })
        // The call's chat (`docs/ux/calls.md` section 4.1.4), for the
        // owner, the members and guests alike.
        ui.chat.setActions(
            send: { [weak self] text in self?.session.sendCallChat(text) ?? (seq: 0, sent: false) },
            resend: { [weak self] seq in self?.session.resendCallChat(seq: seq) ?? false })
        // Pictures in the call's chat (section 4.1.4.1).
        ui.chat.setPictureActions(
            reserveSeq: { [weak self] in self?.session.reserveCallChatSeq() ?? 0 },
            send: { [weak self] content, seq in self?.session.sendCallChat(content, seq: seq) ?? false })
        self.speaking.source = { [weak self] in await self?.session.sampleAudioLevels() }
        // What a tile shows of a person, for the SFU (``VideoWants``).
        ui.tileVideoQualitySink = { [weak self] tile, peerId, quality in
            self?.session.reportTileVideoQuality(tile: tile, peerId: peerId, quality: quality)
        }
        // A username verified or a contact renamed re-names the tiles on screen.
        self.names.follow(ui: ui)
        // A joiner behind a symmetric NAT has no server-reflexive candidate and
        // reaches the room only through the relay. The credential came with
        // the call token (ADR-0078 section 2). Fail-soft: none leaves the call
        // on STUN.
        self.session.setTurnClient(TurnCredentialClient(preMinted: turn))
    }

    // MARK: - Factories

    /// Owner: found the call MLS group, derive the seed, connect + publish, and
    /// start admitting joiners.
    static func host(
        options: Options,
        ui: CallUiState,
        callbacks: Callbacks
    ) async throws -> MultiJoinerCall {
        let minted = try await mintJwt(options: options)
        let ownerKeyHex = hexEncode(options.ownerSignatureKey)

        // The SFU roster decides (see `onSfuWelcome`): an empty room means the
        // owner founds the call group (sole founder, no race); a room with
        // people in it means the owner is coming BACK to a call a successor
        // kept alive, so it asks to be admitted like anyone else and the
        // election hands it the committer duty once it is in. Owner presence
        // rides `self` on the owner's client.
        let call = MultiJoinerCall(
            opts: options,
            cbs: callbacks,
            ui: ui,
            role: .owner,
            jwt: minted.jwt,
            turn: minted.turn,
            ownerKeyHex: ownerKeyHex,
            initialSeed: nil,
            initialLeaf: nil
        )
        try await call.start()
        return call
    }

    /// Owner: found the call group and go live (an empty room, or a probe that
    /// nobody answered).
    private func foundGroup() {
        guard group == nil, !joined else { return }
        let founded: Group
        let seed: Data
        let leaf: UInt32
        do {
            founded = try Group.create(identity: opts.identity, kind: .group)
            seed = try deriveSeed(founded)
            leaf = try founded.ownLeafIndex()
        } catch {
            cbs.onError?(error)
            return
        }
        group = founded
        localLeaf = leaf
        joined = true
        pendingKeyPackage = nil
        clearAdmissionTimer()
        clearRebroadcast()
        clearOwnerProbe()
        // Nothing kept while probing can be read in a group founded only now.
        _ = earlyRelays.drain()
        safetyNumber = MultiJoinerCall.safeSafetyNumber(seed)
        cbs.onJoined?()
        broadcastOwnProfile()
        Task { [weak self] in
            guard let self else { return }
            do {
                try await self.session.publish(seed: seed, localLeaf: leaf)
            } catch {
                self.reportPublishFailure(error)
            }
        }
    }

    /// Owner, in a room with people: ask to be admitted (the successor holds
    /// the group) and fall back to founding when nobody answers.
    private func beginOwnerRejoinProbe() {
        guard group == nil, !joined, pendingKeyPackage == nil else { return }
        do {
            let kp = try opts.identity.createKeyPackage()
            pendingKeyPackage = wrapCallKeyPackage(kpBytes: kp)
        } catch {
            cbs.onError?(error)
            return
        }
        broadcastKeyPackage()
        startRebroadcast()
        ownerProbeTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: ownerRejoinProbeNs)
            guard !Task.isCancelled, let self, !self.joined else { return }
            self.ownerProbeTask = nil
            self.pendingKeyPackage = nil
            self.clearRebroadcast()
            self.foundGroup()
        }
    }

    private func startRebroadcast() {
        guard rebroadcastTask == nil else { return }
        rebroadcastTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: keyPackageRebroadcastNs)
                guard !Task.isCancelled, let self else { return }
                if self.pendingKeyPackage == nil {
                    self.rebroadcastTask = nil
                    return
                }
                self.broadcastKeyPackage()
            }
        }
    }

    private func clearRebroadcast() {
        rebroadcastTask?.cancel()
        rebroadcastTask = nil
    }

    private func clearOwnerProbe() {
        ownerProbeTask?.cancel()
        ownerProbeTask = nil
    }

    /// Joiner: mint a fresh KeyPackage, connect signaling-only, relay the KP,
    /// and wait for the committer's Welcome.
    static func join(
        options: Options,
        ui: CallUiState,
        callbacks: Callbacks
    ) async throws -> MultiJoinerCall {
        let minted = try await mintJwt(options: options)
        let ownerKeyHex = hexEncode(options.ownerSignatureKey)
        // Owner presence is learnt once this client holds the group: the
        // owner's sealed announcement verifies against the capability's owner
        // key (ADR-0078 section 2).
        let call = MultiJoinerCall(
            opts: options,
            cbs: callbacks,
            ui: ui,
            role: .joiner,
            jwt: minted.jwt,
            turn: minted.turn,
            ownerKeyHex: ownerKeyHex,
            initialSeed: nil,
            initialLeaf: nil
        )
        // Mint a fresh KeyPackage whose private keys persist in this identity's
        // provider (so the SAME identity opens the Welcome later).
        let kp = try options.identity.createKeyPackage()
        call.pendingKeyPackage = wrapCallKeyPackage(kpBytes: kp)
        try await call.start()
        // Start the admission clock only once signaling is open + the first
        // broadcast is on its way (a call with no live committer is dead).
        call.startAdmissionTimer()
        return call
    }

    /// A publish that threw. One that stopped because the call was left or
    /// ended while it was awaited is not a failure: the end was already
    /// reported through the session's state (`onEnded`), and "could not
    /// start" is not what happened.
    private func reportPublishFailure(_ error: Error) {
        if case SfuCallSession.SfuCallError.endedWhileConnecting = error { return }
        cbs.onError?(error)
    }

    private func start() async throws {
        do {
            try await session.connect()
        } catch {
            cbs.onError?(error)
            throw error
        }
    }

    // MARK: - Public handle

    /// Tear down the call. Idempotent.
    func hangUp() {
        clearAdmissionTimer()
        clearRebroadcast()
        clearOwnerProbe()
        speaking.stop()
        lastKnownCameraOn.removeAll()
        lastKnownMicOn.removeAll()
        session.hangUp()
    }

    /// End the call for every person in it. Only the link's owner may: every
    /// other member checks the sender against the owner key of the signed
    /// capability and ignores it from anyone else. Sealed into the call's own
    /// group, so the SFU relays ciphertext. Best-effort: the caller hangs up
    /// whether or not this went out.
    func endForEveryone() {
        guard role == .owner, let group else { return }
        do {
            let payload = try composeCallSignaling(
                body: .end(callId: opts.callId, reason: .endedForAll))
            let sealed = try group.createApplicationMessage(identity: opts.identity, plaintext: payload)
            session.sendSignalRelayToCall(payload: sealed)
        } catch {
            NSLog("multi-joiner: the end for everyone was not sent: %@", String(describing: error))
        }
    }

    /// Mute / unmute the local mic.
    func setMicrophoneEnabled(_ on: Bool) {
        session.setMicrophoneEnabled(on)
        ui.setMic(on)
    }

    /// Gate the local microphone track only — for a caller that already
    /// reflects the choice in `ui` (the app-level ``LinkCallHost``).
    func setMicrophoneTrackEnabled(_ on: Bool) {
        session.setMicrophoneEnabled(on)
    }

    /// The SFU session's lifecycle state. Lets a caller tell the user ending
    /// the call (session still up) from the session closing on its own.
    var sessionState: SfuCallSession.State { session.state }

    /// Enable / disable the local camera.
    func setCameraEnabled(_ on: Bool) {
        session.setCameraEnabled(on)
        ui.setCamera(on)
    }

    /// Turn the camera ON mid-call (piece B) — delegates to the SFU session.
    /// A joiner defers publish until it has joined the group, so this is a
    /// no-op until then (the session guards on having a peer connection).
    /// Throws on a capture / attach failure so the landing screen can revert
    /// the toggle (the call stays up audio-only — invariant #4).
    func publishVideo() async throws {
        try await session.publishVideo()
    }

    // MARK: - Admission timer

    private func startAdmissionTimer() {
        admissionTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: joinerAdmissionTimeoutNs)
            guard !Task.isCancelled else { return }
            self?.onAdmissionTimeout()
        }
    }

    private func clearAdmissionTimer() {
        admissionTask?.cancel()
        admissionTask = nil
    }

    /// Joiner admission window elapsed with no Welcome — the call has no live
    /// committer. Stop rebroadcasting, tear down signaling, surface the state.
    private func onAdmissionTimeout() {
        admissionTask = nil
        if joined { return }
        timedOut = true
        pendingKeyPackage = nil
        clearRebroadcast()
        session.hangUp()
        cbs.onNoCommitter?()
    }

    // MARK: - SFU session callbacks → store + presence

    private func onSessionState(_ state: SfuCallSession.State) {
        switch state {
        case .connecting:
            ui.setPhase(.connecting)
        case .connected:
            ui.setPhase(.connected)
            ui.setE2ee(CallUiState.E2eeState(active: true, shortFingerprint: safetyNumber))
            speaking.start()
            if !firedLive {
                firedLive = true
                cbs.onLive?()
            }
        case .failed, .closed:
            speaking.stop()
            // The call's chat ends with the call for this device.
            ui.chat.callEnded()
            ui.clearRemoteVideoCopies()
            screenTracks.detachAll(ui: ui)
            ui.setScreenShare(nil)
            ui.setPhase(.ended(endedAt: Date()))
            if session.endedByServer, let alreadyEnded = cbs.onCallAlreadyEnded {
                alreadyEnded()
            } else {
                cbs.onEnded?()
            }
        case .idle:
            break
        }
    }

    private func onSfuWelcome(selfPeerId: String, roster: [SfuPeerSnapshot]) {
        presence.setRoster(roster.map(\.peerId))
        let remotes = roster
            .filter { $0.peerId != selfPeerId }
            .map { snapshot in
                remoteParticipant(name: names.name(peerId: snapshot.peerId), peerId: snapshot.peerId)
            }
        ui.setParticipants([localParticipant()] + remotes)
        if role == .owner, group == nil, !joined {
            if remotes.isEmpty {
                foundGroup()
            } else {
                beginOwnerRejoinProbe()
            }
            return
        }
        broadcastKeyPackage()
        if pendingKeyPackage != nil { startRebroadcast() }
    }

    private func onSfuPeerJoined(peerId: String) {
        presence.addPeer(peerId: peerId)
        ui.participantJoined(remoteParticipant(name: names.name(peerId: peerId), peerId: peerId))
        // A newly-present peer may be a freshly-elected committer (owner reclaim
        // / a successor); re-broadcast so it sees a still-pending join.
        broadcastKeyPackage()
    }

    /// ADR-0078 section 2: a peer's sealed announcement verified against a
    /// leaf of the call group. Bind the peer to that member (presence, the
    /// committer election, a departure's removal) and to that person (the
    /// tile's name).
    private func onPeerIdentified(peerId: String, identity: VerifiedPeerIdentity) {
        presence.bindPeerToMember(peerId: peerId, memberKeyHex: identity.memberKeyHex)
        names.peerIdentified(peerId, person: identity.personKeyB64, memberKeyHex: identity.memberKeyHex)
        names.relabel(ui)
    }

    private func onSfuPeerLeft(peerId: String) {
        let memberKeyHex = presence.memberKeyFor(peerId: peerId)
        presence.removePeer(peerId: peerId)
        // Someone who leaves can come back: forget their admission (else the
        // G-22 dedup re-serves a Welcome a fresh app cannot open and they wait
        // forever) and, if this client is now the acting committer, remove
        // their leaf so the key rotates away from them.
        forgetAdmission(peerId: peerId, memberKeyHex: memberKeyHex)
        if let memberKeyHex { removeDepartedMember(memberKeyHex: memberKeyHex) }
        let id = "\(peerPrefix)\(peerId)"
        // Forget the departed peer's sticky camera state so a later rejoin
        // under the same id starts from a clean slate (join-race guard).
        lastKnownCameraOn.removeValue(forKey: id)
        lastKnownMicOn.removeValue(forKey: id)
        // Their screen's picture goes with them; the share ends in the fold.
        screenTracks.attach(nil, sharer: id, ui: ui)
        names.peerLeft(peerId)
        ui.participantLeft(id)
        ui.setPersonKey(nil, forParticipant: id)
        // The committer may have left → the next inbound KP is evaluated against
        // the fresh election (this client might now be it). No action here.
    }

    // MARK: - ADR-0032 handshake dispatch

    private func onRelay(from: String, payload: Data) {
        let env: FfiIncomingEnvelope
        do {
            env = try dispatchIncomingEnvelope(plaintext: payload)
        } catch {
            return // malformed relay payload — drop
        }
        switch env.kind {
        case .callKeyPackage:
            onIncomingKeyPackage(from: from, kpEnvelope: env.payload)
        case .callWelcome:
            onIncomingWelcome(from: from, welcomeEnvelope: env.payload)
        case .introMessage:
            // Not a call envelope — the fallback carries verbatim bytes, which
            // for us is a broadcast MLS message: a commit an existing member
            // must apply, or a member's sealed profile.
            onMaybeCommit(from: from, env.payload)
        case .mediaState:
            // Camera-off-cleanup: a camera / microphone signal is intercepted +
            // handled in `SfuCallSession.dispatchSignalRelay` (it fires
            // `onRemoteMediaState`, never `onSignalRelay`), so it never reaches
            // this multi-joiner dispatcher. The case is present only to keep the
            // switch exhaustive. The per-tile `camOn` wiring now lives on the
            // `onRemoteMediaState` callback in the `Callbacks(...)` block above.
            break
        case .pushRoutingSecret:
            // ADR-0042 — the push-routing-secret distribution envelope rides
            // the GROUP JOURNAL (handled in `ChatClient.pollMessages`), never
            // the SFU signal-relay. A relay payload that classifies as one is
            // out of place here — drop it. Case present to keep the switch
            // exhaustive after the Sprint-1 FFI added the variant.
            break
        case .committerBeacon:
            // ADR-0045 — the committer-liveness beacon likewise rides the GROUP
            // JOURNAL as an MLS application message, never the SFU
            // signal-relay, and it says nothing about a CALL committer (the
            // ADR-0032 election here is leaf/presence-based and independent).
            // A relay payload that classifies as one is out of place — drop it.
            // Case present to keep the switch exhaustive after the FFI added
            // the variant.
            break
        case .profile:
            // ADR-0048 — a bare (unsealed) profile on the relay binds to
            // nobody: the SFU peer id is not an identity. Names travel sealed
            // inside the call group instead (`broadcastOwnProfile`) and surface
            // through `onMaybeCommit` as an MLS application message. Dropped.
            break
        case .groupName:
            // ADR-0051 — a group-name envelope likewise rides the GROUP
            // JOURNAL (role-gated in `ChatRepository.receiveGroupName`), never
            // the SFU signal-relay. Out of place here — drop.
            break
        case .profileRequest:
            // ADR-0048 — a profile request rides the GROUP JOURNAL (answered
            // in `ChatRepository.answerProfileRequest`), never the SFU
            // signal-relay. Out of place here — drop.
            break
        case .groupContactRequest:
            // `docs/ux/who-can-find-me.md` 6.4 — a contact request rides a
            // GROUP JOURNAL (opened in `ContactsRepository`), never the SFU
            // signal-relay. Out of place here — drop.
            break
        case .channelReadKey, .channelEpochSeeds, .channelRotateRequest,
             .publisherDeviceRequest, .channelAdmissionMark, .publisherReaderKey:
            // ADR-0087 sections 6, 7 and 15 — a channel read key, a new
            // publisher's earlier epoch seeds and a co-publisher's rotate
            // request ride a PublisherGroup journal, never the SFU
            // signal-relay. Out of place here — drop.
            break
        case .pin:
            // ADR-0052 decision 3 — a shared-pin envelope rides the GROUP
            // JOURNAL (standing-gated in `ChatRepository.receivePin`), never
            // the SFU signal-relay. Out of place here — drop.
            break
        case .introductionOffer, .introductions, .visibility, .reachState,
             .committerDelegation, .registrarSeed:
            // ADR-0060 (and the ADR-0057 registrar seed) — group-journal /
            // DeviceGroup envelopes, never the SFU signal-relay. Out of place
            // here — drop.
            break
        case .sharedChannels:
            // ADR-0059 — a shared-channels list rides the GROUP JOURNAL and
            // speaks for its MLS sender, never the SFU signal-relay. Out of
            // place here — drop.
            break
        case .groupClosed:
            // ADR-0073 phase 2 — a group-closed notice rides the GROUP JOURNAL
            // (judged in `ChatRepository.receiveGroupClosed`), never the
            // SFU signal-relay. Out of place here — drop.
            break
        case .keyRotation:
            // ADR-0074 section 3.3 — the account's key-rotation envelope is
            // meaningful ONLY in the account's own DeviceGroup. Anywhere else
            // it is dropped, never opened and never rendered.
            break
        case .deviceCard:
            // ADR-0080 — a device card is meaningful ONLY in the account's
            // own DeviceGroup. A relay payload names no device; dropped.
            break
        case .linkClaim:
            // ADR-0079 (reworked 2026-10-17) — a link-request claim is
            // meaningful ONLY in the account's own DeviceGroup; dropped.
            break
        case .linkFounded:
            // ADR-0079 (order of 2026-10-17) — a link-founded note, the same.
            break
        case .conversationHandle, .knockSeed, .admissionSeed, .groupPhoto, .groupReactions:
            // ADR-0087 — group and channel settings ride the conversation's
            // journal, never the SFU signal-relay. Out of place here — drop.
            break
        case .openItems, .pollBallots:
            // ADR-0092 — the open-items hand-over and old ballot rows ride the
            // group journal, never the SFU signal-relay; dropped.
            break
        case .conversationReentry, .knockKey, .accountDeleted:
            // ADR-0091 phase 2 — a re-entry request rides the DeviceGroup, a
            // mailbox or a knock queue, and the knock key the group journal;
            // never the SFU signal-relay. ADR-0098's deleted-account notice is
            // not a call envelope either. Dropped.
            break
        }
    }

    /// Committer side: admit a joiner's KeyPackage (auto-admit for the
    /// reference). Only the ACTING committer acts; everyone else drops it.
    private func onIncomingKeyPackage(from: String, kpEnvelope: Data) {
        guard let group, localLeaf != nil else { return }
        guard isActingCommitter() else { return }

        // G-22: dedup PRIMARILY on the joiner's stable MLS signature (leaf) key
        // so a peer re-minting FRESH KeyPackages under one identity is added at
        // most once; the per-`from`-peer + exact-KP-bytes guards stay as fallbacks.
        let sigKeyHex = (try? keyPackageSignatureKey(keyPackage: kpEnvelope)).map { hexEncode($0) }
        let kpKey = hexEncode(kpEnvelope)
        let priorWelcome = sigKeyHex.flatMap { admittedBySigKey[$0] } ?? admittedByPeer[from]
        if priorWelcome != nil || admittedByKp.contains(kpKey) {
            // Idempotent: re-serve the stored Welcome (a peer that lost it).
            if let priorWelcome {
                session.sendSignalRelay(toPeer: from, payload: priorWelcome)
            }
            return
        }

        // The joiner sends `wrapCallKeyPackage(kp)`; dispatch already stripped
        // the domain separator, so `kpEnvelope` is the raw TLS KeyPackage.
        let beforeKeys = Set(groupMembers().map { $0.signatureKeyHex })
        let welcomeWrapped: Data
        let commit: Data
        do {
            let added = try group.addMemberFromWire(identity: opts.identity, keyPackage: kpEnvelope)
            commit = added.commit
            welcomeWrapped = wrapCallWelcome(welcomeBytes: added.welcome)
        } catch {
            // A malformed / already-used KeyPackage — refuse, do not advance.
            NSLog("multi-joiner: addMember failed: %@", String(describing: error))
            return
        }
        if let sigKeyHex { admittedBySigKey[sigKeyHex] = welcomeWrapped }
        admittedByPeer[from] = welcomeWrapped
        admittedByKp.insert(kpKey)
        kpKeyByPeer[from] = kpKey
        // Bind the new joiner's peer → its member key for presence.
        for member in groupMembers() where !beforeKeys.contains(member.signatureKeyHex) {
            presence.bindPeerToMember(peerId: from, memberKeyHex: member.signatureKeyHex)
        }
        // Deliver the Welcome to the joiner + broadcast the commit to the rest.
        session.sendSignalRelay(toPeer: from, payload: welcomeWrapped)
        session.sendSignalRelayToCall(payload: commit)
        // The add advanced our epoch → rekey to the new seed.
        rekeyToCurrentEpoch()
        // The newcomer decrypts only from this epoch on: tell it who we are.
        broadcastOwnProfile()
    }

    /// Joiner side: open the committer's Welcome and go live. The returning
    /// owner is a joiner here too (see `host`).
    private func onIncomingWelcome(from: String, welcomeEnvelope: Data) {
        guard !joined, pendingKeyPackage != nil else { return }
        let joinedGroup: Group
        do {
            joinedGroup = try Group.joinFromWelcome(welcome: welcomeEnvelope, identity: opts.identity)
        } catch {
            NSLog("multi-joiner: joinFromWelcome failed: %@", String(describing: error))
            return
        }

        // Hardening 1 (defence-in-depth vs the successor-redirect residual):
        // confirm the JOINED group actually contains the OWNER whose signature
        // the SIGNED capability carried. The owner key comes from the capability
        // (not from any peer), so a Welcome that lands us in an impostor's
        // isolated group — no owner leaf — is a redirect. Refuse it fail-closed
        // (invariant #4): never publish media, tear down, surface a clear error.
        let joinedKeys: [String]
        do {
            joinedKeys = try joinedGroup.members().map { hexEncode($0.signaturePublicKey) }
        } catch {
            NSLog("multi-joiner: could not read joined-group members: %@", String(describing: error))
            return
        }
        guard joinedKeys.contains(ownerKeyHex) else {
            NSLog("multi-joiner: joined a group without the capability owner — redirect refused")
            timedOut = true
            pendingKeyPackage = nil
            clearAdmissionTimer()
            session.hangUp()
            cbs.onError?(MultiJoinerCallError.ownerNotInGroup)
            return
        }

        let seed: Data
        let leaf: UInt32
        do {
            leaf = try joinedGroup.ownLeafIndex()
            seed = try deriveSeed(joinedGroup)
        } catch {
            NSLog("multi-joiner: post-join seed/leaf derivation failed: %@", String(describing: error))
            cbs.onError?(error)
            return
        }

        group = joinedGroup
        localLeaf = leaf
        joined = true
        pendingKeyPackage = nil
        clearAdmissionTimer()
        clearRebroadcast()
        clearOwnerProbe()

        safetyNumber = MultiJoinerCall.safeSafetyNumber(seed)
        // Who the committer's peer is (and the owner's) comes from their sealed
        // announcements, which this client can open from now on.
        cbs.onJoined?()
        broadcastOwnProfile()
        replayEarlyRelays()
        // A replayed commit moved the group past the Welcome's epoch; media
        // must start under the key of the epoch the group is at now. On a
        // derivation failure the Welcome's seed stands (never plaintext).
        let publishSeed = (try? deriveSeed(joinedGroup)) ?? seed
        // Publish media ONLY now — after joining + deriving the seed (invariant #4).
        Task { [weak self] in
            guard let self else { return }
            do {
                try await self.session.publish(seed: publishSeed, localLeaf: leaf)
            } catch {
                self.reportPublishFailure(error)
            }
        }
    }

    /// Process what arrived before the Welcome, oldest first. A payload this
    /// epoch cannot open is dropped inside `onMaybeCommit`, as it always was.
    private func replayEarlyRelays() {
        for relay in earlyRelays.drain() {
            onMaybeCommit(from: relay.from, relay.bytes)
        }
    }

    /// Existing member: apply a broadcast MLS message — a commit (epoch
    /// advance → rekey) or a member's sealed application message (today: the
    /// ADR-0048 profile that names its tile).
    private func onMaybeCommit(from: String, _ bytes: Data) {
        guard let group else {
            // Not readable yet: a member's profile can outrun the Welcome.
            // Kept and replayed once the group exists (`replayEarlyRelays`).
            earlyRelays.keep((from: from, bytes: bytes))
            return
        }
        let processed: ProcessedMessage
        do {
            // The call group has no ADR-0022 role table — plain apply (the
            // call-path caveat). A wrong-epoch / junk payload throws and is
            // ignored.
            processed = try group.processIncoming(identity: opts.identity, bytes: bytes)
        } catch {
            return
        }
        switch processed {
        case .application(let data, let senderLeaf, _):
            // Who the relaying peer is comes from its verified announcement
            // (ADR-0078 section 2), not from the sender of a message it relayed:
            // anyone can relay a sealed message it received.
            let senderLeafKeyHex = senderLeaf.flatMap { leaf in
                groupMembers().first(where: { $0.leafIndex == leaf })?.signatureKeyHex
            }
            // The same leaf resolved to the PERSON, here at receipt (C-68),
            // the way the chat receive loop resolves a profile's sender.
            let senderKey = senderLeaf.flatMap { AuthorIdentity.resolve(group: group, leafIndex: $0) }
            // Whether the MLS-authenticated sender is the link's owner: the
            // key the SIGNED capability names, matched the way the join check
            // does.
            let fromOwner = EndForEveryone.sentByOwner(
                senderLeaf: senderLeaf, members: groupMembers(), ownerKeyHex: ownerKeyHex)
            onMemberApplication(
                from: from, sender: senderKey, senderLeafKeyHex: senderLeafKeyHex,
                fromOwner: fromOwner, plaintext: data)
        case .commit:
            rekeyToCurrentEpoch()
            // Someone was admitted: they decrypt only from this epoch on, so
            // every member re-announces its name for the newcomer.
            broadcastOwnProfile()
        default:
            break
        }
    }

    /// A member's sealed application message: the owner's end for everyone,
    /// or a profile; anything else is ignored. What a profile announces binds
    /// to the relaying SFU peer — the same binding the peer's media tracks
    /// already rest on — and the tile is named again from it: a person who set
    /// no profile name reads as their username (``LinkCallTileNames``).
    private func onMemberApplication(
        from: String, sender: MemberIdentity?, senderLeafKeyHex: String?, fromOwner: Bool,
        plaintext: Data
    ) {
        if EndForEveryone.isEndForEveryone(plaintext: plaintext, callId: opts.callId) {
            // Acted on only from the link's owner; from anyone else it is
            // ignored.
            if fromOwner { cbs.onEndedForEveryone?(from) }
            return
        }
        guard let profile = ChatClient.decodeProfileIfEnvelope(plaintext) else { return }
        names.memberProfile(
            from: from, sender: sender, senderLeafKeyHex: senderLeafKeyHex, fromOwner: fromOwner,
            ownerAccountId: opts.ownerAccountId, profile: profile)
        names.relabel(ui)
    }

    private func forgetAdmission(peerId: String, memberKeyHex: String?) {
        admittedByPeer.removeValue(forKey: peerId)
        if let kpKey = kpKeyByPeer.removeValue(forKey: peerId) {
            admittedByKp.remove(kpKey)
        }
        if let memberKeyHex { admittedBySigKey.removeValue(forKey: memberKeyHex) }
    }

    /// Acting committer: remove a departed member's leaf and rekey.
    private func removeDepartedMember(memberKeyHex: String) {
        guard let group, joined else { return }
        if memberKeyHex == hexEncode(opts.identity.signaturePublicKey()) { return }
        guard let member = groupMembers().first(where: { $0.signatureKeyHex == memberKeyHex }) else { return }
        guard isActingCommitter() else { return }
        let commit: Data
        do {
            commit = try group.removeMember(identity: opts.identity, leafIndex: member.leafIndex)
        } catch {
            NSLog("multi-joiner: could not remove a departed member: %@", String(describing: error))
            return
        }
        session.sendSignalRelayToCall(payload: commit)
        rekeyToCurrentEpoch()
    }

    /// Seal the own profile into the call group and relay it to every member.
    /// Best-effort: a missing profile or a failed send only leaves the tile on
    /// its fallback label.
    private func broadcastOwnProfile() {
        guard let group, joined, let envelope = opts.ownProfileEnvelope?() else { return }
        do {
            let sealed = try group.createApplicationMessage(identity: opts.identity, plaintext: envelope)
            session.sendSignalRelayToCall(payload: sealed)
        } catch {
            NSLog("multi-joiner: own profile not announced: %@", String(describing: error))
        }
    }

    // MARK: - Election + epoch helpers

    private func isActingCommitter() -> Bool {
        guard group != nil, let localLeaf else { return false }
        let leaf = electActingCommitterLeaf(
            members: groupMembers(),
            ownerKeyHex: ownerKeyHex,
            presentKeys: presence.presentKeySet()
        )
        return leaf == localLeaf
    }

    private func groupMembers() -> [ElectionMember] {
        guard let group else { return [] }
        guard let members = try? group.members() else { return [] }
        return members.map { ElectionMember(leafIndex: $0.leafIndex, signatureKeyHex: hexEncode($0.signaturePublicKey)) }
    }

    private func deriveSeed(_ group: Group) throws -> Data {
        try group.exportSecret(
            identity: opts.identity,
            label: callSeedLabel,
            context: opts.callId,
            length: callSeedLen
        )
    }

    /// Re-derive the seed at the current epoch and rekey SFrame + the pill. On a
    /// derivation failure the current key is kept (fail-closed — never plaintext,
    /// invariant #4).
    private func rekeyToCurrentEpoch() {
        guard let group else { return }
        guard let seed = try? deriveSeed(group) else { return }
        session.rekey(newSeed: seed)
        safetyNumber = MultiJoinerCall.safeSafetyNumber(seed)
        ui.setE2ee(CallUiState.E2eeState(active: true, shortFingerprint: safetyNumber))
    }

    private func broadcastKeyPackage() {
        guard !joined, !timedOut, let pending = pendingKeyPackage else { return }
        session.sendSignalRelayToCall(payload: pending)
    }

    // MARK: - Store-participant helpers

    /// The own tile's name: the name typed for the call, else the profile
    /// name (`""` renders as the localized `You`).
    private func localTileName() -> String {
        SfuCallUi.ownTileName(callName: ownCallName, profileName: opts.localDisplayName)
    }

    private func localParticipant() -> CallUiState.Participant {
        CallUiState.Participant(
            id: "local",
            displayName: localTileName(),
            isLocal: true,
            micOn: opts.audio,
            camOn: opts.video,
            handRaised: false,
            quality: .good,
            speaking: false,
            screenSharing: false
        )
    }

    private func remoteParticipant(name: String, peerId: String) -> CallUiState.Participant {
        CallUiState.Participant(
            id: "\(peerPrefix)\(peerId)",
            displayName: name,
            isLocal: false,
            // What the person last announced; unmuted until their first
            // media state arrives.
            micOn: lastKnownMicOn["\(peerPrefix)\(peerId)"] ?? true,
            camOn: false,
            handRaised: false,
            quality: .good,
            speaking: false,
            screenSharing: false
        )
    }

    // MARK: - Module helpers

    /// The call token and the TURN credential beside it, paid for by one
    /// anonymous `call-join` token for the room of the link's call id
    /// (ADR-0078 section 2).
    private static func mintJwt(options: Options) async throws -> MintCallJwtResult {
        let client = options.authBaseUrl.map { CallJwtClient(baseURL: $0) } ?? CallJwtClient()
        return try await client.mint(callId: options.callId, role: .participant)
    }

    /// Safety number is display-only — a derivation failure must never abort.
    private static func safeSafetyNumber(_ seed: Data) -> String? {
        try? callSafetyNumber(seed: seed)
    }

    /// Join-race camera resolution (see `lastKnownCameraOn`): media-state is
    /// authoritative, so a video-track ARRIVAL defers to the last-known state —
    /// a known-muted peer stays off, an unknown peer defaults on (they
    /// published video), and a nil track (removal) is always off. Pure so it is
    /// unit-testable without a live `SfuCallSession`.
    static func joinRaceCameraOn(trackPresent: Bool, lastKnown: Bool?) -> Bool {
        trackPresent ? (lastKnown ?? true) : false
    }
}

/// Typed errors surfaced by ``MultiJoinerCall``.
enum MultiJoinerCallError: Error, LocalizedError {
    /// Hardening 1: the joined group did not contain the capability's owner —
    /// a redirect. Refused fail-closed.
    case ownerNotInGroup

    var errorDescription: String? {
        switch self {
        case .ownerNotInGroup:
            return "Joined the wrong call — the owner is not in this group."
        }
    }
}
