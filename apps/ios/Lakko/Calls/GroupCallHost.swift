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
    /// The last time the SFU proved it was there; 0 before the link came up.
    var lastInboundAtMs: Int64 { get }
    /// The SFU refused this device because the call had already ended.
    var callEndedByServer: Bool { get }
}

extension SfuCallUi: GroupCallHandle {
    func setMicrophoneTrackEnabled(_ on: Bool) {
        session.setMicrophoneEnabled(on)
    }

    var sessionState: SfuCallSession.State { session.state }
    var lastInboundAtMs: Int64 { session.lastInboundAtMs }
    var callEndedByServer: Bool { session.endedByServer }
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
        /// ADR-0091 — this device is behind in the chat: a call is not set up
        /// there (the key would be derived at an epoch the others left). The
        /// buttons stay; a start or join fails with the ordinary text.
        var behindHeld: @MainActor (_ groupIdHex: String) -> Bool = { _ in false }
        /// The chat's group-call record.
        var record: @MainActor (_ groupIdHex: String) -> OngoingGroupCall?
        /// Fold this device's own signal into the record (never echoed back),
        /// under this device's own device and person keys.
        var applySignal: @MainActor (_ groupIdHex: String, _ signal: GroupCallSignal, _ atMs: Int64) -> Void
        /// This device's own leaf signature key in the group — the key its own
        /// joins are recorded under — or `nil` when it cannot be resolved.
        var ownDeviceKey: @MainActor (_ groupIdHex: String) -> MemberIdentity?
        /// Sleep for `ms`; `false` when the sleep was cancelled.
        var sleepMs: @MainActor (_ ms: Int64) async -> Bool
        /// Publish call-signaling plaintext through the serialized send path.
        var publish: @MainActor (_ groupIdHex: String, _ bytes: Data) async -> Bool
        /// Poll the group once (no long poll), so the epoch and the record are
        /// current.
        var syncGroup: @MainActor (_ groupIdHex: String) async -> Void
        var sessionToken: @MainActor () -> String?
        var sfuBaseUrl: @MainActor () -> String?
        var microphoneGranted: @MainActor () async -> Bool
        var cameraGranted: @MainActor (_ video: Bool) async -> Bool
        /// The call token (and the TURN credential beside it) for the room of
        /// `callId`, paid for by one anonymous `call-join` token (ADR-0078
        /// section 2): no session reaches the issuer.
        var mintJwt: @MainActor (_ callId: Data) async throws -> MintCallJwtResult
        var makeHandle: @MainActor (
            _ config: SfuCallSession.Config,
            _ ui: CallUiState,
            _ turn: TurnCredential?
        ) -> GroupCallHandle
        /// The group's own name for the call screen's title — the ADR-0051
        /// shared fact every member has — or `nil` when the group has none,
        /// and the screen says `Group call`. Not the chat title built from the
        /// members' names: Android and the web title a nameless group's call
        /// `Group call`, and the three clients in one call agree (aligned
        /// 2026-10-23).
        var groupName: @MainActor (_ groupIdHex: String) -> String?
        var safetyNumber: (_ seed: Data) -> String?
        var mintCallId: () -> Data
        /// `docs/ux/calls.md` §6 — file the finished call's row in the
        /// conversation it happened in. Defaulted so hosts that never record
        /// (tests of other paths) keep their shorter initialiser.
        /// `docs/ux/navigation.md` section 6.2 — a finished call, with the
        /// parts the flat Calls index needs beside the row the conversation
        /// shows.
        var recordCall: @MainActor (_ call: FinishedCall) -> Void = { _ in }
        var nowMs: () -> Int64
        /// Whether a call is still there, as the SFU sees it (`SfuCallState.swift`).
        /// Injectable so the probe is tested without a network.
        /// The room is `sfuRoomForCall(callId)` and the request carries no
        /// token (ADR-0078 section 2).
        var sfuCallState: @MainActor (_ sfuBaseUrl: String, _ roomHex: String) async -> SfuCallState? = {
            base, roomHex in
            await SfuCallStateProbe.fetch(sfuBaseUrl: base, roomHex: roomHex)
        }
        /// Composes this device's peer announcement for a call in the group and
        /// verifies everyone else's against the group's roster (ADR-0078
        /// section 2). `nil`: no tile is named from an announcement.
        var peerAnnouncer: @MainActor (_ groupIdHex: String, _ callId: Data) -> CallPeerAnnouncer? = { _, _ in nil }
        /// This account's own person key in the group, resolved the way a
        /// received row's sender is (C-68): the key a call this device started
        /// records as its starter. `nil` when it cannot be resolved, which
        /// never offers `End call for everyone`.
        var ownPersonKey: @MainActor (_ groupIdHex: String) -> MemberIdentity? = { _ in nil }
        /// What the call tile-name rule calls a person (``CallTileName``), or
        /// `nil` when the device cannot name them.
        var personName: @MainActor (_ personKey: MemberIdentity) -> String? = { _ in nil }
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
        /// Started or joined with the camera on, so calling back from the
        /// Calls list re-dials in the same mode.
        let video: Bool
        /// When this device started or joined the call. Distinct from
        /// ``connectedAtMs``, which is when its own media first flowed.
        let startedAtMs: Int64
        /// When this device's own media first flowed, or `nil` if it never did
        /// (``GroupCallHostRules/transcriptRow(initiator:connectedAtMs:endedAtMs:)``).
        var connectedAtMs: Int64?

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
    @Published private(set) var callError: UiText?
    /// A join found that the call had already ended.
    @Published private(set) var callNotice: UiText?
    /// "{name} ended the call for everyone", for a few seconds after the
    /// starter of the call this device was in ended it
    /// (`docs/ux/calls.md` section 4.1.2). Never an alert.
    @Published private(set) var endedForEveryoneNotice: UiText?

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
    /// The participant heartbeat of the live call (see ``runHeartbeat(callId:)``).
    /// Cancelled with the session, so it never outlives the call.
    private var heartbeatTask: Task<Void, Never>?
    /// The live call has had at least one other participant
    /// (``GroupCallHostRules/aloneAction(remoteCount:sawRemote:armed:)``).
    private var sawRemote = false
    /// The alone-end timer: everyone else left, the call ends when it fires
    /// unless someone comes back first. Cancelled with the session.
    private var aloneEndTask: Task<Void, Never>?
    /// The call whose terminating `CallEnd` is already on the journal (its
    /// starter ended it for everyone), so leaving it publishes no second one.
    private var endedForEveryoneCallId: Data?
    private var endedNoticeTask: Task<Void, Never>?
    /// When this device's group call in each conversation last ended, for
    /// ``suppressesCallWake(groupIdHex:)``.
    private var wakeSuppression = CallWakeSuppression()

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
        repository.groupCallEndSink = { [weak self] end in self?.receivedEnd(end) }
        repository.onMembershipEndRecorded = { [weak self] groupIdHex, left in
            self?.endForMembership(groupIdHex: groupIdHex, left: left)
        }
    }

    /// UX 9.1 — this account is out of the group of the live call. Nothing can
    /// be published any more, so the call ends here, with the line; the notice
    /// of the group follows as anywhere else. A person who left asked for it.
    func endForMembership(groupIdHex: String, left: Bool) {
        guard liveGroupIdHex == groupIdHex else { return }
        if !left { callNotice = .key("call.ended.no_longer_in_group") }
        releaseSession()
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
            callError = CallSlot.busyText(occupant, verb: verb)
            return
        }
        guard !ports.membershipEnded(groupIdHex) else { return }
        // ADR-0091 (maintainer, 2026-11-20) — behind in this chat: the
        // ordinary could-not-start (could-not-join) text, before anything is
        // captured, announced or connected. Nothing says why.
        if ports.behindHeld(groupIdHex) {
            callError = .key(joinCallId == nil ? "call.error.start_group" : "call.error.join_group")
            return
        }
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
            if isCurrent(attempt) { callError = MicrophonePermission.deniedText }
            return
        }
        // A video start or join captures the camera at connect; settle that
        // permission before anything is announced or connected.
        guard await ports.cameraGranted(video) else {
            if isCurrent(attempt) { callError = CameraPermission.deniedText }
            return
        }
        guard isCurrent(attempt) else { return }
        guard let base = ports.sfuBaseUrl() else {
            NSLog("group call: SFU URL not configured (LakkoSfuUrl); refusing")
            callError = .key("chat.call.unavailable")
            return
        }
        // The anonymous `call-join` token that pays for the call token is drawn
        // with the session, so a call needs one.
        guard ports.sessionToken() != nil else {
            NSLog("group call: no session token; refusing")
            callError = .key("call.error.signed_out_start")
            return
        }
        if let joinCallId {
            // Bring the epoch and the record current before deriving the seed,
            // so the joiner does not arm a key from an epoch the room has left
            // and does not join a call already ended on the journal.
            await ports.syncGroup(groupIdHex)
            guard isCurrent(attempt) else { return }
            let record = ports.record(groupIdHex)
            // This device is not in the call yet: its own entry (left by a
            // relaunch mid-call) does not make the call joinable.
            guard let record, record.callId == joinCallId,
                  GroupCallReducer.isLive(
                      record, nowMs: ports.nowMs(),
                      ignoringDevice: ports.ownDeviceKey(groupIdHex)) else {
                callNotice = .key("chat.call.group_ended")
                return
            }
        }
        guard !ports.membershipEnded(groupIdHex) else { return }
        let callId = joinCallId ?? ports.mintCallId()
        let callIdHex = callId.map { String(format: "%02x", $0) }.joined()
        let keyedEpoch = ports.epoch(groupIdHex)
        guard let key = ports.deriveCallKey(groupIdHex, callId) else {
            NSLog("group call: call-key derivation failed; refusing per invariant #4")
            callError = .key("call.error.encryption_setup")
            return
        }
        let minted: MintCallJwtResult
        do {
            minted = try await ports.mintJwt(callId)
        } catch {
            NSLog("group call: JWT mint failed: %@", error.localizedDescription)
            if isCurrent(attempt) {
                callError = CallErrorText.messageText(
                    for: error,
                    fallbackKey: joinCallId == nil ? "call.error.start_group" : "call.error.join_group")
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
            guard isCurrent(attempt) else {
                // Left while the Start was being published. The group has
                // been told of a call nobody will be in: take it back, or
                // every member is offered it until its record goes stale.
                if announced {
                    await endRecordReportedBySfu(groupIdHex: groupIdHex, callId: callId)
                }
                return
            }
            guard announced else {
                // Not announced: nobody could join, so the call is refused
                // rather than connected as an invisible room.
                callError = .key("call.error.start_group")
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
            keyedEpoch: keyedEpoch,
            video: video,
            startedAtMs: ports.nowMs()
        )
        let ui = CallUiState()
        // Title the screen by the GROUP, not by the literal words "Group call"
        // the screen falls back to. A group call's identity is the group's, and
        // since ADR-0051 that name is a shared fact every member holds, so all
        // of them title the same call the same way. This is deliberately NOT
        // the other participant's name: participants are named on their own
        // tiles, and in a call that can grow from two people to five, naming
        // the screen after whoever happens to be the only other person would
        // rename the call when a third joined.
        ui.setPeerName(ports.groupName(groupIdHex))
        let config = SfuCallSession.Config(
            sfuBaseUrl: base,
            jwt: minted.jwt,
            seed: key.seed,
            localLeaf: key.localLeaf,
            audio: true,
            video: video,
            // ADR-0078 section 2: the SFU names nobody; each peer announces
            // itself, verified against this group's roster.
            peerAnnouncer: ports.peerAnnouncer(groupIdHex, callId),
            callIdHex: callIdHex
        )
        let handle = ports.makeHandle(config, ui, minted.turn)
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
            if handle.callEndedByServer {
                // The SFU says the call ended before this device arrived
                // (`docs/ux/calls.md` section 4.3): close the chat's record
                // for everyone rather than report a failure.
                NSLog("group call: the SFU says call %@ ended before we joined", callIdHex)
                leave(hungUp: false)
                dismissPresentation()
                callError = .key("chat.call.group_ended")
                await endRecordReportedBySfu(groupIdHex: groupIdHex, callId: callId)
                return
            }
            NSLog("group call connect failed: %@", error.localizedDescription)
            callError = CallErrorText.messageText(
                for: error, fallbackKey: initiator ? "call.error.start_group" : "call.error.join_group")
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
        startHeartbeat(callId: call.callId)
    }

    // MARK: - Heartbeat

    /// Re-publish this device's `GroupCallJoin` every
    /// ``GroupCallReducer/heartbeatMs`` while it is in the call.
    ///
    /// A participant whose app is killed or reinstalled mid-call never
    /// publishes its Leave. Without a heartbeat every other member kept counting
    /// it, and showed "Group call in progress" for a call nobody was in until
    /// the four-hour cap. With it, a member counts only while its newest join is
    /// fresh (``GroupCallReducer/memberFreshMs``). Same body as the first join;
    /// no wire change.
    ///
    /// Runs exactly as long as the session: ``releaseSession()`` cancels it, so
    /// it never runs for a call that is not running (in the background or not).
    private func startHeartbeat(callId: Data) {
        heartbeatTask?.cancel()
        heartbeatTask = Task { @MainActor [weak self] in
            await self?.runHeartbeat(callId: callId)
        }
    }

    private func runHeartbeat(callId: Data) async {
        let sleepMs = ports.sleepMs
        while !Task.isCancelled {
            guard await sleepMs(GroupCallReducer.heartbeatMs), !Task.isCancelled else { return }
            guard let call = activeCall, call.callId == callId, call.joinPublished,
                  let handle else { return }
            switch handle.sessionState {
            case .failed, .closed:
                // The phase observer turns this into a leave; nothing to renew.
                return
            case .idle, .connecting, .connected:
                break
            }
            let nowMs = ports.nowMs()
            guard let bytes = try? composeCallSignaling(
                body: .groupJoin(
                    callId: call.callId,
                    leafIndex: call.ownLeaf,
                    joinedAtMs: UInt64(max(0, nowMs))
                )
            ) else { continue }
            ports.applySignal(call.groupIdHex, .join(callId: call.callId, leafIndex: call.ownLeaf), nowMs)
            _ = enqueueControl(groupIdHex: call.groupIdHex, bytes: bytes)
        }
    }

    // MARK: - Leaving

    /// Leave the group call this device is in: tear the SFU session down and
    /// tell the group what ``GroupCallHostRules/planLeave(hungUp:joinPublished:initiator:callId:ownDeviceKey:record:nowMs:)``
    /// decides. Idempotent. The presentation stays until the call screen is
    /// dismissed.
    func leave(hungUp: Bool = true) {
        // Read before the session is released: the call ended when the link
        // was last known alive, not when this device noticed.
        let lastInbound = handle?.lastInboundAtMs ?? 0
        guard let call = releaseSession() else { return }
        let endedAtMs = SfuKeepalive.callEndedAtMs(nowMs: ports.nowMs(), lastInboundMs: lastInbound)
        if let row = GroupCallHostRules.transcriptRow(
            initiator: call.initiator, connectedAtMs: call.connectedAtMs, endedAtMs: endedAtMs,
            video: call.video) {
            ports.recordCall(FinishedCall(
                groupIdHex: call.groupIdHex,
                row: row,
                video: call.video,
                startedAtMs: call.startedAtMs))
        }
        // A device that neither joined nor announced the call has nothing to
        // say about it (see `planLeave`), so it need not read the journal.
        guard call.joinPublished || call.initiator else { return }
        Task { @MainActor [weak self] in
            await self?.announceDeparture(call: call, hungUp: hungUp)
        }
    }

    /// Ask the SFU whether a call this device is offering to join is still
    /// there, and close its record when it is not (`SfuCallState.swift`).
    /// Never throws; a probe that cannot be answered changes nothing. The
    /// call this device is in needs no probe: its own socket is the proof.
    func probeCallState(groupIdHex: String, callIdHex: String) async {
        if activeCallId(groupIdHex: groupIdHex).map(Self.hex) == callIdHex { return }
        // ADR-0078 section 2: the probe names the room (a one-way function of
        // the call id) and carries no token, so no admission token is minted
        // to ask and the issuer never sees who probes.
        guard let base = ports.sfuBaseUrl(),
              let probedCallId = Self.unhex(callIdHex), probedCallId.count == 16,
              let room = try? sfuRoomForCall(callId: probedCallId)
        else { return }
        guard await ports.sfuCallState(base, room) == .ended else { return }
        // Still not in it after the awaits?
        if activeCallId(groupIdHex: groupIdHex).map(Self.hex) == callIdHex { return }
        guard let callId = Self.unhex(callIdHex), callId.count == 16 else { return }
        await endRecordReportedBySfu(groupIdHex: groupIdHex, callId: callId)
    }

    private static func hex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    private static func unhex(_ hex: String) -> Data? {
        let chars = Array(hex.utf8)
        guard chars.count % 2 == 0 else { return nil }
        var out = Data(capacity: chars.count / 2)
        var i = 0
        while i < chars.count {
            guard let byte = UInt8(String(decoding: chars[i..<i + 2], as: UTF8.self), radix: 16) else {
                return nil
            }
            out.append(byte)
            i += 2
        }
        return out
    }

    /// Close a group call's record on this device and for the chat: the SFU
    /// said the call ended. Any member may say so — the End signal carries
    /// only the call id, and a record that is over on the server is over for
    /// everyone.
    func endRecordReportedBySfu(groupIdHex: String, callId: Data) async {
        ports.applySignal(groupIdHex, .end(callId: callId), ports.nowMs())
        guard let bytes = try? composeCallSignaling(body: .end(callId: callId, reason: .hungUp)) else {
            return
        }
        _ = await enqueueControl(groupIdHex: groupIdHex, bytes: bytes).value
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
            ownDeviceKey: ports.ownDeviceKey(call.groupIdHex),
            record: ports.record(call.groupIdHex),
            nowMs: ports.nowMs()
        )
        if plan.publishLeave,
           let bytes = try? composeCallSignaling(
               body: .groupLeave(callId: call.callId, leafIndex: call.ownLeaf, reason: plan.reason)
           ) {
            ports.applySignal(
                call.groupIdHex, .leave(callId: call.callId, leafIndex: call.ownLeaf), ports.nowMs())
            _ = enqueueControl(groupIdHex: call.groupIdHex, bytes: bytes)
        }
        // The starter's end for everyone is the call's terminating `CallEnd`:
        // a second one would only be an ordinary end notice behind it.
        let endAlreadyPublished = endedForEveryoneCallId == call.callId
        if endAlreadyPublished { endedForEveryoneCallId = nil }
        if plan.publishEnd, !endAlreadyPublished,
           let bytes = try? composeCallSignaling(body: .end(callId: call.callId, reason: plan.reason)) {
            ports.applySignal(call.groupIdHex, .end(callId: call.callId), ports.nowMs())
            _ = await enqueueControl(groupIdHex: call.groupIdHex, bytes: bytes).value
        }
    }

    // MARK: - Ending for everyone (`docs/ux/calls.md` section 4.1.2)

    /// This account started the group call this device is in, so `End` offers
    /// to end it for everyone. Account-level: any device of the starter that
    /// is in the call. A call whose starter this device does not know (a
    /// record adopted from a Join, or stored before the starter was kept) is
    /// nobody's to end.
    var startedCall: Bool {
        guard let call = activeCall,
              let record = ports.record(call.groupIdHex), record.callId == call.callId,
              let starter = record.initiatorPersonKey, !starter.isEmpty
        else { return false }
        return starter == ports.ownPersonKey(call.groupIdHex)
    }

    /// End the group call for every person in it. Only acts for the call's
    /// starter.
    ///
    /// The end is a `CallEnd` with the reason `endedForAll`, queued into the
    /// group like any call signal BEFORE this device leaves, so it is on its
    /// way while the session goes down and this device's own Leave lands
    /// behind it. Every device in the call checks that it comes from the
    /// call's starter before it leaves. If it cannot be published the starter
    /// has still left, and nothing is shown.
    func endForEveryone() {
        guard let call = activeCall, startedCall else { return }
        endedForEveryoneCallId = call.callId
        ports.applySignal(call.groupIdHex, .end(callId: call.callId), ports.nowMs())
        if let bytes = try? composeCallSignaling(body: .end(callId: call.callId, reason: .endedForAll)) {
            enqueueControl(groupIdHex: call.groupIdHex, bytes: bytes)
        }
        ui?.endCall()
        leave(hungUp: true)
    }

    /// A `CallEnd` arrived on a group's journal. The repository has already
    /// closed the record; this decides whether the device leaves the call it
    /// is in: only for that call, and only when its starter ended it for
    /// everyone (``EndForEveryone/endsTheCallForMe(_:)``). The same body from
    /// anyone else takes nobody out of a call.
    func receivedEnd(_ received: ReceivedGroupCallEnd) {
        guard let call = activeCall,
              call.groupIdHex == received.groupIdHex, call.callId == received.callId,
              EndForEveryone.endsTheCallForMe(received.end)
        else { return }
        // The terminating `CallEnd` is the starter's: leaving adds no second.
        endedForEveryoneCallId = call.callId
        showEndedForEveryone(
            starterName: received.end.starterPersonKey.flatMap { ports.personName($0) })
        ui?.endCall()
        leave(hungUp: true)
    }

    private func showEndedForEveryone(starterName: String?) {
        endedForEveryoneNotice = EndForEveryone.endedText(starterName: starterName)
        endedNoticeTask?.cancel()
        let sleepMs = ports.sleepMs
        endedNoticeTask = Task { @MainActor [weak self] in
            guard await sleepMs(EndForEveryone.noticeMs), !Task.isCancelled else { return }
            self?.endedForEveryoneNotice = nil
            self?.endedNoticeTask = nil
        }
    }

    /// Take the call out of the slot and tear the session down, publishing
    /// nothing. Synchronous, so the several paths that can end a call act once.
    @discardableResult
    private func releaseSession() -> ActiveCall? {
        let call = activeCall
        if let call {
            wakeSuppression.noteEnded(groupIdHex: call.groupIdHex, atMs: ports.nowMs())
        }
        activeCall = nil
        attempt = nil
        startingGroupIdHex = nil
        controls.removeAll()
        heartbeatTask?.cancel()
        heartbeatTask = nil
        aloneEndTask?.cancel()
        aloneEndTask = nil
        sawRemote = false
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
        endedForEveryoneCallId = nil
        endedNoticeTask?.cancel()
        endedNoticeTask = nil
        endedForEveryoneNotice = nil
        wakeSuppression.reset()
    }

    /// Does a call push wake for `groupIdHex` ring nothing here? This device
    /// is starting or in the group call of that conversation, or left it
    /// moments ago (``CallWakeSuppression``). Covers senders that still wake
    /// for group-call frames. Read synchronously by ``VoipPushRegistrar``.
    func suppressesCallWake(groupIdHex: String) -> Bool {
        let group = groupIdHex.lowercased()
        if activeCall?.groupIdHex.lowercased() == group
            || startingGroupIdHex?.lowercased() == group {
            return true
        }
        return wakeSuppression.suppresses(
            groupIdHex: groupIdHex, activeGroupIdHex: nil, activeState: nil,
            nowMs: ports.nowMs())
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
        // Every send and every poll of every group drains a chain; without a
        // session on this group there is nothing to decide, so skip the reads.
        guard handle != nil, activeCall?.groupIdHex == groupIdHex else { return }
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
        // ADR-0089: the camera background stopped working. Nothing has been
        // sent since; the camera is turned off here, where the camera button
        // lands, so it happens with the call collapsed into the bar too.
        CameraBackground.shared.$failures
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak ui] _ in
                MainActor.assumeIsolated {
                    guard let ui, ui.participants.first(where: { $0.isLocal })?.camOn == true
                    else { return }
                    ui.setCamera(false)
                }
            }
            .store(in: &controls)
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
                    guard let self, let handle, self.handle === handle else { return }
                    switch phase {
                    case .connected:
                        // The row's clock (`docs/ux/calls.md` §6) starts when
                        // media flows, not when the screen opened.
                        if self.activeCall?.connectedAtMs == nil { self.activeCall?.connectedAtMs = self.ports.nowMs() }
                    case .ended:
                        // The End button, or the session closing on its own.
                        self.leave(hungUp: handle.sessionState != .failed)
                    default:
                        break
                    }
                }
            }
            .store(in: &controls)
        // Everyone else left: end the call after the grace unless someone
        // comes back (`docs/ux/calls.md` §4.3).
        ui.$participants
            .map { $0.filter { !$0.isLocal }.count }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle, weak ui] remoteCount in
                MainActor.assumeIsolated {
                    guard let self, let handle, let ui, self.handle === handle else { return }
                    self.applyAloneRule(remoteCount: remoteCount, handle: handle, ui: ui)
                }
            }
            .store(in: &controls)
    }

    private func applyAloneRule(remoteCount: Int, handle: GroupCallHandle, ui: CallUiState) {
        if remoteCount > 0 { sawRemote = true }
        let armed = aloneEndTask.map { !$0.isCancelled } ?? false
        switch GroupCallHostRules.aloneAction(remoteCount: remoteCount, sawRemote: sawRemote, armed: armed) {
        case .armEnd:
            NSLog("group call: everyone else left; ending in %d s", Int(GroupCallHostRules.aloneEndMs / 1000))
            let sleepMs = ports.sleepMs
            aloneEndTask = Task { @MainActor [weak self, weak handle, weak ui] in
                guard await sleepMs(GroupCallHostRules.aloneEndMs), !Task.isCancelled else { return }
                guard let self, let handle, let ui, self.handle === handle else { return }
                self.aloneEndTask = nil
                guard !ui.participants.contains(where: { !$0.isLocal }) else { return }
                NSLog("group call: ended because everyone else left")
                self.leave(hungUp: true)
            }
        case .disarm:
            aloneEndTask?.cancel()
            aloneEndTask = nil
        case .none:
            break
        }
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
                ui.showNotice(CameraPermission.deniedText)
                return
            }
            do {
                try await handle.publishVideo()
            } catch {
                // Fail-safe (invariant #4): revert the toggle and say why. The
                // call stays up audio-only.
                ui.setCamera(false)
                ui.showNotice(CallErrorText.cameraFailureText(error))
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
    /// The production publish port: the serialized application send, with NO
    /// push wake for any group-call frame, its `end` included. A group call
    /// never rings a closed device, and on iOS a call wake is a VoIP push the
    /// receiver must report to CallKit as an incoming call, so a wake here
    /// rang a phantom one-to-one call on every member's iPhone when a group
    /// call ended. ``CallSignalingWake`` cannot tell a group `end` from a
    /// one-to-one `end` by its bytes, so the port says it outright.
    static func livePublish(
        repository: ChatRepository
    ) -> @MainActor (_ groupIdHex: String, _ bytes: Data) async -> Bool {
        { [weak repository] groupIdHex, bytes in
            guard let repository else { return false }
            do {
                try await repository.publishApplicationMessage(
                    groupIdHex: groupIdHex, plaintextBytes: bytes, wake: nil)
                return true
            } catch {
                NSLog("group call publish failed: %@", error.localizedDescription)
                return false
            }
        }
    }

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
            behindHeld: { [weak repository] in repository?.reentryStopsActing(groupIdHex: $0) ?? false },
            record: { [weak repository] in repository?.groupCalls[$0] },
            applySignal: { [weak repository] groupIdHex, signal, atMs in
                repository?.applyOwnGroupCallSignal(groupIdHex: groupIdHex, signal: signal, atMs: atMs)
            },
            ownDeviceKey: { [weak repository] in repository?.ownDeviceKeyB64(groupIdHex: $0) },
            sleepMs: { ms in
                do {
                    try await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
                    return true
                } catch {
                    return false
                }
            },
            publish: livePublish(repository: repository),
            syncGroup: { [weak repository] groupIdHex in
                try? await repository?.pollGroup(groupIdHex: groupIdHex, waitSecs: 0)
            },
            sessionToken: sessionTokenProvider,
            sfuBaseUrl: { SfuConfig.baseUrl },
            microphoneGranted: { await MicrophonePermission.ensureGranted() },
            cameraGranted: { video in await CameraPermission.ensureGranted(forVideo: video) },
            mintJwt: { callId in
                try await CallJwtClient().mint(callId: callId, role: .participant)
            },
            makeHandle: { config, ui, turn in
                let handle = SfuCallUi(
                    config: config,
                    ui: ui,
                    // Section 4.4: the user's own tile carries their own name
                    // from the first second; `You` only when no name is set.
                    localDisplayName: GroupCallHost.ownTileName(),
                    peerNames: GroupCallPeerNames.live(ui: ui)
                )
                // The SFU has a public address, so most clients reach it on a
                // server-reflexive candidate - but one behind a symmetric NAT
                // has none, and for it the relay is the only way into the room.
                // The credential came with the call token (ADR-0078 section 2).
                handle.session.setTurnClient(TurnCredentialClient(preMinted: turn))
                return handle
            },
            groupName: { [weak repository] in repository?.groupName(groupIdHex: $0) },
            safetyNumber: { try? callSafetyNumber(seed: $0) },
            mintCallId: { CallSession.mintCallId() },
            recordCall: { [weak repository] call in
                repository?.recordFinishedCall(call)
            },
            nowMs: { ChatRepository.nowMs() },
            peerAnnouncer: { [weak repository] groupIdHex, callId in
                repository?.callPeerAnnouncer(groupIdHex: groupIdHex, callId: callId)
            },
            // The same resolution, with the same fallback, as the participant
            // this device's own signals are folded under.
            ownPersonKey: { [weak repository] groupIdHex in
                repository?.ownSignatureKeyB64(groupIdHex: groupIdHex)
                    ?? repository?.ownDeviceKeyB64(groupIdHex: groupIdHex)
            },
            personName: { personKey in
                CallTileName.name(
                    resolved: ProfileDirectory.shared.resolve(signatureKeyB64: personKey)
                        .flatMap(CallTileName.personName),
                    announcedName: nil,
                    claimedUsername: ProfileDirectory.shared
                        .storedProfile(signatureKeyB64: personKey)?.username)
            }
        ))
    }

    /// The name on the user's own tile (`docs/ux/calls.md` section 4.4): the
    /// profile name, `You` when none is set. Read at call start.
    @MainActor
    static func ownTileName() -> String {
        let name = ProfileDirectory.shared.ownProfile?.displayName?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        // Empty renders as the localized "You" at the tile
        // (``CallPeerName/tileLabel(displayName:isLocal:)``).
        return name
    }
}
