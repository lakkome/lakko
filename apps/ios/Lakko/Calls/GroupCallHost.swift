// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// A live group (SFU) call session as ``GroupCallHost`` drives it. `SfuCallUi`
/// in production; a fake in tests, so the host's lifecycle is testable without
/// WebRTC.
@MainActor
protocol GroupCallHandle: AnyObject {
    func connect() async throws
    /// Tear the session down. Idempotent.
    func hangUp()
    /// Rotate the SFrame key (G-18 / ADR-0029). No-op for an unchanged seed.
    func rekey(newSeed: Data)
    /// Gate the local microphone track. Touches no UI state.
    func setMicrophoneTrackEnabled(_ on: Bool)
    func setCameraEnabled(_ on: Bool)
    func publishVideo() async throws
    var sessionState: SfuCallSession.State { get }
}

extension SfuCallUi: GroupCallHandle {
    func setMicrophoneTrackEnabled(_ on: Bool) {
        session.setMicrophoneEnabled(on)
    }

    var sessionState: SfuCallSession.State { session.state }
}

/// The app-level owner of the group (SFU) call — the group-call slot beside the
/// 1x1 slot of ``CallHost`` and the link slot of ``LinkCallHost``.
///
/// Created once per process next to ``ChatRepository`` and presented from the
/// signed-in root (`CallHostOverlay`), so a group call outlives the chat it was
/// started from, every SwiftUI rebuild of that chat's view model, and any deep
/// link that replaces the screen. See ``GroupCallHostRules`` for the defects
/// this closes and for every decision it takes.
///
/// ## Mid-call rekey
///
/// ``attach(to:)`` observes the repository's per-group MLS chain draining
/// (``ChatRepository/observeGroupMlsIdle(_:)``). Every region that can move an
/// epoch runs in that chain — the open chat's poll, the foreground receive for
/// chats that are not open (which gives the call's group a dedicated long poll
/// while the call is live), membership commits and the ADR-0045 sweep — so the
/// key follows the epoch whatever screen is showing, and is never read between
/// two queued regions.
@MainActor
final class GroupCallHost: ObservableObject {

    /// Everything the host needs from outside, injectable for tests.
    struct Ports {
        /// The per-call SFrame seed + this device's leaf from the group's MLS
        /// exporter. `nil` fails closed (invariant #4).
        var deriveCallKey: @MainActor (_ groupIdHex: String, _ callId: Data) -> (seed: Data, localLeaf: UInt32)?
        var epoch: @MainActor (_ groupIdHex: String) -> UInt64?
        var hasGroup: @MainActor (_ groupIdHex: String) -> Bool
        /// `Group.isActive`, `nil` when it cannot be read.
        var isActive: @MainActor (_ groupIdHex: String) -> Bool?
        var membershipEnded: @MainActor (_ groupIdHex: String) -> Bool
        /// The chat's group-call record.
        var record: @MainActor (_ groupIdHex: String) -> OngoingGroupCall?
        /// Fold this device's own signal into the record (never echoed back).
        var applySignal: @MainActor (_ groupIdHex: String, _ signal: GroupCallSignal, _ atMs: Int64) -> Void
        /// Publish call-signaling plaintext through the serialized send path.
        var publish: @MainActor (_ groupIdHex: String, _ bytes: Data) async -> Bool
        /// Poll the group once (no long poll), so the epoch and the record are
        /// current.
        var syncGroup: @MainActor (_ groupIdHex: String) async -> Void
        var sessionToken: @MainActor () -> String?
        var sfuBaseUrl: @MainActor () -> String?
        var microphoneGranted: @MainActor () async -> Bool
        var cameraGranted: @MainActor (_ video: Bool) async -> Bool
        var mintJwt: @MainActor (_ sessionToken: String, _ callIdHex: String) async throws -> String
        var makeHandle: @MainActor (
            _ config: SfuCallSession.Config,
            _ ui: CallUiState,
            _ callIdHex: String
        ) -> GroupCallHandle
        var safetyNumber: (_ seed: Data) -> String?
        var mintCallId: () -> Data
        var nowMs: () -> Int64
    }

    /// The group call this device is in.
    struct ActiveCall: Equatable {
        let groupIdHex: String
        let callId: Data
        let ownLeaf: UInt32
        /// This device announced the call.
        let initiator: Bool
        /// This device's `GroupCallJoin` was queued for the journal.
        var joinPublished: Bool
        /// The MLS epoch the session's current seed was derived at.
        var keyedEpoch: UInt64?

        var callIdHex: String { callId.map { String(format: "%02x", $0) }.joined() }
    }

    // MARK: - Published state

    @Published private(set) var activeCall: ActiveCall?
    /// A start / join is past its tap but not yet holding a session. Holds the
    /// slot, so nothing else can start meanwhile.
    @Published private(set) var startingGroupIdHex: String?
    /// The in-call state the root presents full-screen, from the moment the
    /// session exists until the call screen is dismissed — which can be after
    /// the call itself ended, so the "Call ended" overlay stays readable.
    @Published private(set) var presentation: CallUiState?
    /// Identifies ``presentation`` (a fresh screen per call).
    @Published private(set) var presentationId: String?
    /// An entry point could not proceed. The root shows it as an alert.
    @Published private(set) var callError: String?
    /// A join found that the call had already ended.
    @Published private(set) var callNotice: String?

    /// The call occupying the app's one call slot, across all three kinds.
    /// Wired by the composition root (``CallSlotWiring``).
    var slotOccupant: @MainActor () -> CallKind? = { nil }

    // MARK: - Internals

    private let ports: Ports
    private var handle: GroupCallHandle?
    private var ui: CallUiState?
    private var controls: Set<AnyCancellable> = []
    /// The start attempt in flight; a later reset or a newer attempt makes an
    /// older one stand down after each suspension.
    private var attempt: UUID?
    private var idleToken: UUID?
    private weak var repository: ChatRepository?
    /// Tail of the group-control publish chain: a Leave never overtakes its own
    /// Join, and an End never overtakes the Leave.
    private var controlTail: Task<Bool, Never>?

    init(ports: Ports) {
        self.ports = ports
    }

    /// Observe the repository's MLS chain drains. Done once, by the composition
    /// root.
    func attach(to repository: ChatRepository) {
        if let token = idleToken { self.repository?.removeGroupMlsIdleObserver(token) }
        self.repository = repository
        idleToken = repository.observeGroupMlsIdle { [weak self] groupIdHex in
            self?.groupMlsSettled(groupIdHex)
        }
    }

    // MARK: - Slot

    /// A group call is starting or live.
    var occupiesSlot: Bool { startingGroupIdHex != nil || activeCall != nil }

    /// The group of the live group call, for the dedicated receive lane.
    var liveGroupIdHex: String? { activeCall?.groupIdHex ?? startingGroupIdHex }

    /// The call this device is in on `groupIdHex`, if any.
    func activeCallId(groupIdHex: String) -> Data? {
        activeCall?.groupIdHex == groupIdHex ? activeCall?.callId : nil
    }

    // MARK: - Start / join

    /// Start (`joinCallId == nil`) or join a group call over the SFU.
    ///
    /// Claims the slot synchronously, before any suspension, so a double tap
    /// cannot open two sessions. Fail-closed on every missing prerequisite —
    /// microphone, camera for a video call, SFU endpoint, session token, call
    /// key — with an explanation and no call.
    func start(groupIdHex: String, video: Bool, joinCallId: Data?) {
        let verb: CallSlot.Verb = joinCallId == nil ? .start : .join
        if let occupant = slotOccupant() ?? (occupiesSlot ? .group : nil) {
            callError = CallSlot.busyMessage(occupant, verb: verb)
            return
        }
        guard !ports.membershipEnded(groupIdHex) else { return }
        let attempt = UUID()
        self.attempt = attempt
        startingGroupIdHex = groupIdHex
        callNotice = nil
        Task { @MainActor [weak self] in
            await self?.run(attempt: attempt, groupIdHex: groupIdHex, video: video, joinCallId: joinCallId)
        }
    }

    private func isCurrent(_ attempt: UUID) -> Bool { self.attempt == attempt }

    private func run(attempt: UUID, groupIdHex: String, video: Bool, joinCallId: Data?) async {
        defer {
            if isCurrent(attempt) { startingGroupIdHex = nil }
        }
        guard await ports.microphoneGranted() else {
            if isCurrent(attempt) { callError = MicrophonePermission.deniedMessage }
            return
        }
        // A video start or join captures the camera at connect; settle that
        // permission before anything is announced or connected.
        guard await ports.cameraGranted(video) else {
            if isCurrent(attempt) { callError = CameraPermission.deniedMessage }
            return
        }
        guard isCurrent(attempt) else { return }
        guard let base = ports.sfuBaseUrl() else {
            NSLog("group call: SFU URL not configured (LakkoSfuUrl); refusing")
            callError = "Group calls are not available: this build has no SFU endpoint."
            return
        }
        guard let token = ports.sessionToken() else {
            NSLog("group call: no session token; refusing")
            callError = "Sign in again to start or join a group call."
            return
        }
        if let joinCallId {
            // Bring the epoch and the record current before deriving the seed,
            // so the joiner does not arm a key from an epoch the room has left
            // and does not join a call already ended on the journal.
            await ports.syncGroup(groupIdHex)
            guard isCurrent(attempt) else { return }
            let record = ports.record(groupIdHex)
            guard let record, record.callId == joinCallId,
                  GroupCallReducer.isLive(record, nowMs: ports.nowMs()) else {
                callNotice = "That group call has ended."
                return
            }
        }
        guard !ports.membershipEnded(groupIdHex) else { return }
        let callId = joinCallId ?? ports.mintCallId()
        let callIdHex = callId.map { String(format: "%02x", $0) }.joined()
        let keyedEpoch = ports.epoch(groupIdHex)
        guard let key = ports.deriveCallKey(groupIdHex, callId) else {
            NSLog("group call: call-key derivation failed; refusing per invariant #4")
            callError = "Could not derive the call key for this chat."
            return
        }
        let jwt: String
        do {
            jwt = try await ports.mintJwt(token, callIdHex)
        } catch {
            NSLog("group call: JWT mint failed: %@", error.localizedDescription)
            if isCurrent(attempt) {
                callError = "Could not get admission to the call: \(error.localizedDescription)"
            }
            return
        }
        guard isCurrent(attempt) else { return }
        let initiator = joinCallId == nil
        if initiator {
            // Announce the call before connecting, so a member that opens the
            // chat while this device negotiates already sees a joinable call.
            let startedAtMs = ports.nowMs()
            let bytes = try? composeCallSignaling(
                body: .groupStart(
                    callId: callId,
                    initiatorLeaf: key.localLeaf,
                    createdAtMs: UInt64(max(0, startedAtMs)),
                    media: FfiCallMediaFlags(audio: true, video: video)
                )
            )
            let announced: Bool
            if let bytes {
                announced = await enqueueControl(groupIdHex: groupIdHex, bytes: bytes).value
            } else {
                announced = false
            }
            guard isCurrent(attempt) else { return }
            guard announced else {
                // Not announced: nobody could join, so the call is refused
                // rather than connected as an invisible room.
                callError = "Could not announce the call to this chat. Try again."
                return
            }
            ports.applySignal(
                groupIdHex,
                .start(
                    callId: callId,
                    initiatorLeaf: key.localLeaf,
                    createdAtMs: UInt64(max(0, startedAtMs)),
                    audio: true,
                    video: video
                ),
                startedAtMs
            )
        }
        // Set before any further await, so a failure from here on reaches
        // `leave` — which is what publishes the End an initiator owes.
        activeCall = ActiveCall(
            groupIdHex: groupIdHex,
            callId: callId,
            ownLeaf: key.localLeaf,
            initiator: initiator,
            joinPublished: false,
            keyedEpoch: keyedEpoch
        )
        let ui = CallUiState()
        let config = SfuCallSession.Config(
            sfuBaseUrl: base,
            jwt: jwt,
            seed: key.seed,
            localLeaf: key.localLeaf,
            audio: true,
            video: video
        )
        let handle = ports.makeHandle(config, ui, callIdHex)
        self.handle = handle
        self.ui = ui
        presentation = ui
        presentationId = callIdHex
        startingGroupIdHex = nil
        bindControls(ui: ui, handle: handle)
        // The awaits above (the JWT mint, the Start publish, which itself runs in
        // the group's MLS chain) can straddle an epoch advance whose drain found
        // no session to rekey. Catch up now, before any media is sealed.
        groupMlsSettled(groupIdHex)
        do {
            try await handle.connect()
        } catch {
            guard self.handle === handle else { return }
            NSLog("group call connect failed: %@", error.localizedDescription)
            callError = "Could not connect to the call: \(error.localizedDescription)"
            leave(hungUp: false)
            // Take the call screen down at once, so the explanation is not
            // hidden behind a "Call ended" overlay.
            dismissPresentation()
            return
        }
        guard self.handle === handle, var call = activeCall else { return }
        // A mute tapped while the session was still connecting found no track
        // to gate; apply the screen's current choice to the track that exists
        // now.
        if let micOn = ui.participants.first(where: { $0.isLocal })?.micOn {
            handle.setMicrophoneTrackEnabled(micOn)
        }
        guard let joinBytes = try? composeCallSignaling(
            body: .groupJoin(
                callId: call.callId,
                leafIndex: call.ownLeaf,
                joinedAtMs: UInt64(max(0, ports.nowMs()))
            )
        ) else { return }
        call.joinPublished = true
        activeCall = call
        ports.applySignal(groupIdHex, .join(callId: call.callId, leafIndex: call.ownLeaf), ports.nowMs())
        _ = enqueueControl(groupIdHex: groupIdHex, bytes: joinBytes)
    }

    // MARK: - Leaving

    /// Leave the group call this device is in: tear the SFU session down and
    /// tell the group what ``GroupCallHostRules/planLeave(hungUp:joinPublished:initiator:callId:ownLeaf:record:)``
    /// decides. Idempotent. The presentation stays until the call screen is
    /// dismissed.
    func leave(hungUp: Bool = true) {
        guard let call = releaseSession() else { return }
        let plan0 = GroupCallHostRules.planLeave(
            hungUp: hungUp,
            joinPublished: call.joinPublished,
            initiator: call.initiator,
            callId: call.callId,
            ownLeaf: call.ownLeaf,
            record: nil
        )
        // Nothing this device could have to say.
        guard plan0.publishLeave || plan0.publishEnd else { return }
        Task { @MainActor [weak self] in
            await self?.announceDeparture(call: call, hungUp: hungUp)
        }
    }

    private func announceDeparture(call: ActiveCall, hungUp: Bool) async {
        // Read the journal once so the "anyone else still in?" decision sees
        // joins that landed while this device was in the call.
        await ports.syncGroup(call.groupIdHex)
        let plan = GroupCallHostRules.planLeave(
            hungUp: hungUp,
            joinPublished: call.joinPublished,
            initiator: call.initiator,
            callId: call.callId,
            ownLeaf: call.ownLeaf,
            record: ports.record(call.groupIdHex)
        )
        if plan.publishLeave,
           let bytes = try? composeCallSignaling(
               body: .groupLeave(callId: call.callId, leafIndex: call.ownLeaf, reason: plan.reason)
           ) {
            ports.applySignal(
                call.groupIdHex, .leave(callId: call.callId, leafIndex: call.ownLeaf), ports.nowMs())
            _ = enqueueControl(groupIdHex: call.groupIdHex, bytes: bytes)
        }
        if plan.publishEnd,
           let bytes = try? composeCallSignaling(body: .end(callId: call.callId, reason: plan.reason)) {
            ports.applySignal(call.groupIdHex, .end(callId: call.callId), ports.nowMs())
            _ = await enqueueControl(groupIdHex: call.groupIdHex, bytes: bytes).value
        }
    }

    /// Take the call out of the slot and tear the session down, publishing
    /// nothing. Synchronous, so the several paths that can end a call act once.
    @discardableResult
    private func releaseSession() -> ActiveCall? {
        let call = activeCall
        activeCall = nil
        attempt = nil
        startingGroupIdHex = nil
        controls.removeAll()
        let h = handle
        // Cleared BEFORE `hangUp`: the session's `closed` transition moves the
        // screen to `ended`, whose observer must find no handle left.
        handle = nil
        h?.hangUp()
        return call
    }

    /// The call screen was dismissed (its "Call ended" overlay timed out, or it
    /// was closed). Leaves the call first if it is somehow still up.
    func dismissPresentation() {
        if activeCall != nil { leave(hungUp: true) }
        presentation = nil
        presentationId = nil
        ui = nil
    }

    func dismissCallError() { callError = nil }
    func dismissCallNotice() { callNotice = nil }

    /// Logout / account switch: nothing can be published any more, so tear
    /// down locally and forget everything.
    func reset() {
        releaseSession()
        presentation = nil
        presentationId = nil
        ui = nil
        callError = nil
        callNotice = nil
        controlTail = nil
    }

    /// Resolves once every group-control publish queued so far was attempted.
    /// Tests.
    func whenControlPublishesSettle() async {
        _ = await controlTail?.value
    }

    // MARK: - Epoch changes (G-18 / ADR-0029)

    /// The group's MLS chain drained: the epoch may have moved. Rekey the live
    /// call if it did — the decryptors keep the previous key for the overlap
    /// window so in-flight frames still open, then drop it. A re-derivation
    /// failure keeps the current seed (invariant #4); a group this device is no
    /// longer in ends the call locally.
    func groupMlsSettled(_ groupIdHex: String) {
        let call = activeCall
        let currentEpoch = ports.epoch(groupIdHex)
        let action = GroupCallHostRules.decideEpochChange(
            callGroupIdHex: handle != nil ? call?.groupIdHex : nil,
            settledGroupIdHex: groupIdHex,
            groupJoined: ports.hasGroup(groupIdHex),
            isActive: ports.isActive(groupIdHex),
            keyedEpoch: call?.keyedEpoch,
            currentEpoch: currentEpoch
        )
        switch action {
        case .none:
            return
        case .endLocally:
            // Nothing can be published to a group this device has left.
            NSLog("group call: the call's group is no longer joined; ending the call locally")
            releaseSession()
        case .rekey:
            guard var call, let handle,
                  let key = ports.deriveCallKey(groupIdHex, call.callId) else { return }
            handle.rekey(newSeed: key.seed)
            call.keyedEpoch = currentEpoch
            activeCall = call
            // The safety number follows the key: peers that derive it at the new
            // epoch must see the same digits.
            if let ui, ui.e2ee.active {
                ui.setE2ee(CallUiState.E2eeState(active: true, shortFingerprint: ports.safetyNumber(key.seed)))
            }
        }
    }

    // MARK: - Controls

    /// The in-call screen only drives the state it is given; these bridge the
    /// local participant's mic and camera choices onto the session, and the
    /// screen's `ended` phase onto a proper leave. Delivered on the next main
    /// run-loop turn, so a handler that writes the same state never re-enters
    /// the publisher that is notifying it.
    private func bindControls(ui: CallUiState, handle: GroupCallHandle) {
        controls.removeAll()
        ui.$participants
            .compactMap { $0.first(where: { $0.isLocal })?.micOn }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle] micOn in
                MainActor.assumeIsolated {
                    guard let self, let handle, self.handle === handle else { return }
                    handle.setMicrophoneTrackEnabled(micOn)
                }
            }
            .store(in: &controls)
        ui.$participants
            .compactMap { $0.first(where: { $0.isLocal })?.camOn }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle] camOn in
                MainActor.assumeIsolated {
                    guard let self, let handle, self.handle === handle else { return }
                    self.applyCamera(camOn, handle: handle, ui: ui)
                }
            }
            .store(in: &controls)
        ui.$phase
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle] phase in
                MainActor.assumeIsolated {
                    guard case .ended = phase,
                          let self, let handle, self.handle === handle else { return }
                    // The End button, or the session closing on its own.
                    self.leave(hungUp: handle.sessionState != .failed)
                }
            }
            .store(in: &controls)
    }

    private func applyCamera(_ camOn: Bool, handle: GroupCallHandle, ui: CallUiState) {
        guard camOn else {
            handle.setCameraEnabled(false)
            return
        }
        Task { @MainActor [weak self] in
            // The in-call control already settled the permission; this is the
            // layered half, so a camOn flip from anywhere else can never start a
            // capture the user has not allowed.
            guard await self?.ports.cameraGranted(true) ?? false else {
                ui.setCamera(false)
                ui.addSystemEvent(CameraPermission.deniedMessage)
                return
            }
            do {
                try await handle.publishVideo()
            } catch {
                // Fail-safe (invariant #4): revert the toggle and say why. The
                // call stays up audio-only.
                ui.setCamera(false)
                ui.addSystemEvent("Could not start the camera: \(error.localizedDescription)")
                NSLog("SFU group call: publishVideo failed: %@", String(describing: error))
            }
        }
    }

    // MARK: - Publishing

    @discardableResult
    private func enqueueControl(groupIdHex: String, bytes: Data) -> Task<Bool, Never> {
        let previous = controlTail
        let publish = ports.publish
        let task = Task { @MainActor () -> Bool in
            _ = await previous?.value
            let ok = await publish(groupIdHex, bytes)
            if !ok { NSLog("group call: control publish failed") }
            return ok
        }
        controlTail = task
        return task
    }
}

// MARK: - Production ports

extension GroupCallHost {
    /// The composition root's ports over the live repository.
    static func live(
        repository: ChatRepository,
        sessionTokenProvider: @escaping @MainActor () -> String?
    ) -> GroupCallHost {
        GroupCallHost(ports: Ports(
            deriveCallKey: { [weak repository] groupIdHex, callId in
                repository?.deriveCallKey(groupIdHex: groupIdHex, callId: callId)
            },
            epoch: { [weak repository] in repository?.epoch(groupIdHex: $0) },
            hasGroup: { [weak repository] in repository?.hasGroup(groupIdHex: $0) ?? false },
            isActive: { [weak repository] in repository?.isActive(groupIdHex: $0) },
            membershipEnded: { [weak repository] groupIdHex in
                guard let repository else { return true }
                return membershipEnded(repository.membershipStateNow(groupIdHex: groupIdHex))
            },
            record: { [weak repository] in repository?.groupCalls[$0] },
            applySignal: { [weak repository] groupIdHex, signal, atMs in
                repository?.applyGroupCallSignal(groupIdHex: groupIdHex, signal: signal, atMs: atMs)
            },
            publish: { [weak repository] groupIdHex, bytes in
                guard let repository else { return false }
                do {
                    try await repository.publishApplicationMessage(
                        groupIdHex: groupIdHex, plaintextBytes: bytes)
                    return true
                } catch {
                    NSLog("group call publish failed: %@", error.localizedDescription)
                    return false
                }
            },
            syncGroup: { [weak repository] groupIdHex in
                try? await repository?.pollGroup(groupIdHex: groupIdHex, waitSecs: 0)
            },
            sessionToken: sessionTokenProvider,
            sfuBaseUrl: { SfuConfig.baseUrl },
            microphoneGranted: { await MicrophonePermission.ensureGranted() },
            cameraGranted: { video in await CameraPermission.ensureGranted(forVideo: video) },
            mintJwt: { token, callIdHex in
                let client = CallJwtClient()
                client.setSessionToken(token)
                return try await client.mint(callId: callIdHex, role: .participant).jwt
            },
            makeHandle: { config, ui, callIdHex in
                SfuCallUi(
                    config: config,
                    ui: ui,
                    resolveDisplayName: GroupCallHost.displayNameResolver(callIdHex: callIdHex)
                )
            },
            safetyNumber: { try? callSafetyNumber(seed: $0) },
            mintCallId: { CallSession.mintCallId() },
            nowMs: { ChatRepository.nowMs() }
        ))
    }

    /// ADR-0021 — the per-call peer_tag -> contact label resolver, so the SFU
    /// roster shows the name the rest of the app shows for that contact. The
    /// tag is re-derived locally (the SFU never sends a name); a tag collision
    /// falls back to the pseudonym.
    static func displayNameResolver(callIdHex: String) -> (String, String, String) -> String {
        var tagToLabel: [String: String] = [:]
        var collidedTags = Set<String>()
        let profiles = ProfileDirectory.shared
        for c in AcceptedContactsStore.load() {
            guard let accountId = c.request.fromAccountId else { continue }
            let accountIdHex = accountId.map { String(format: "%02x", $0) }.joined()
            let tag = sfuPeerTag(callId: callIdHex, accountId: accountIdHex)
            if tagToLabel[tag] != nil {
                collidedTags.insert(tag)
            } else {
                // ADR-0048 — the one resolver: rename, profile name, verified
                // username, fingerprint. Call tiles show the name only.
                tagToLabel[tag] = profiles.resolve(signatureKey: c.request.fromPubkey).name
            }
        }
        return { _, pseudonym, peerTag in
            if !peerTag.isEmpty, !collidedTags.contains(peerTag), let label = tagToLabel[peerTag] {
                return label
            }
            return pseudonym.isEmpty ? "Peer" : String(pseudonym.prefix(8))
        }
    }
}
