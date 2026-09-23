// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// A cancellable one-shot timer handed out by ``CallHost/Ports/schedule``.
protocol CallHostTimer {
    func cancel()
}

/// The app-level owner of the 1x1 P2P call.
///
/// ## Why this exists
///
/// The 1x1 ``CallSession`` used to live on `ChatViewModel`, and the repository
/// had ONE `callSignalingSink` slot that whichever chat screen was built last
/// claimed and filtered to its own group. So an incoming call rang only while
/// the caller's chat was the chat on screen; an offer arriving while the user
/// was on Home, the chat list, Contacts or another chat was dropped at that
/// filter, and leaving the chat dropped the view model and hung the call up with
/// it. SwiftUI also rebuilds the chat view model on unrelated re-renders, which
/// took any call it held along.
///
/// The host is created once per process next to ``ChatRepository``, claims the
/// signaling slot itself (``attach(to:)``), and is presented from the root view
/// (`CallHostOverlay`), so the call outlives every screen.
///
/// ## Delivery contract
///
/// ``receive(_:)`` is called synchronously from the repository's receive path,
/// in journal order, for every 1x1-eligible row of EVERY group — the open chat's
/// long poll, the foreground background receive, and the ADR-0045 sweep's
/// verdict read all funnel through `ChatRepository.routeCallSignaling`.
///
///   * **Exactly once.** A row is keyed on (group, DS message id) and a key seen
///     before is refused, however it came back (a held cursor re-fetch, two
///     receive paths overlapping).
///   * **In order, one at a time.** Handling is synchronous and never suspends —
///     it runs inside the repository's non-suspending apply region — so the next
///     row cannot start before this one finished. Everything that needs to wait
///     (publishing, permissions, WebRTC) is spawned, never awaited here.
///
/// The routing rules themselves (stale offers, busy, C-11, widened C-12) are the
/// pure ``CallHostRouting``.
@MainActor
final class CallHost: ObservableObject {

    /// Everything the host needs from outside, injectable for tests.
    struct Ports {
        /// Publish call-signaling plaintext to a group's journal through the
        /// serialized application-send path. `false` on failure.
        var publish: @MainActor (_ groupIdHex: String, _ bytes: Data) async -> Bool
        /// The per-call SFrame seed + this device's leaf, from the group's MLS
        /// exporter. `nil` fails the call closed (invariant #4).
        var deriveCallKey: @MainActor (_ groupIdHex: String, _ callId: Data) -> (seed: Data, localLeaf: UInt32)?
        var nowMs: () -> Int64
        /// Run `fire` once after `afterMs`, unless cancelled.
        var schedule: @MainActor (_ afterMs: Int64, _ fire: @escaping @MainActor () -> Void) -> CallHostTimer
        var microphoneGranted: @MainActor () async -> Bool
        var cameraGranted: @MainActor (_ video: Bool) async -> Bool
        /// The other person's resolved name for a 1x1 chat, for the banner,
        /// the call screen's status line and the encryption sheet. Resolved
        /// through ``CallPeerName``, so it is a name or nothing — never a
        /// placeholder standing in for one.
        var peerName: @MainActor (_ groupIdHex: String) -> String?
        var mintCallId: () -> Data
        /// `docs/ux/calls.md` section 6 — write the finished call's row into
        /// the conversation. Local to this device; no wire format.
        /// `docs/ux/navigation.md` section 6.2 — a finished call, with the
        /// parts the flat Calls index needs beside the row the conversation
        /// shows. One value, because the whole point is that neither surface
        /// stores the finished sentence.
        var recordCall: @MainActor (_ call: FinishedCall) -> Void = { _ in }
        /// `docs/ux/calls.md` section 2.1 — does this conversation carry the
        /// mute flag? A muted conversation never rings; it is still recorded.
        ///
        /// Read from the App Group mirror rather than from
        /// ``DeviceGroupSyncRepository`` directly, so that all three of this
        /// device's ring paths — here, the CallKit report, and the
        /// notification-service extension — answer the question from ONE
        /// value. The extension is a separate process and can reach nothing
        /// else; having the app consult a different source would be the
        /// beginning of the two disagreeing. Absent → not muted → rings.
        var isMuted: @MainActor (_ groupIdHex: String) -> Bool = {
            ConversationMuteMirror.isMuted(groupIdHex: $0)
        }
        /// Does this device have a camera at all? Decides whether the video
        /// button opens the camera check or places a voice call
        /// (`docs/ux/calls.md` section 3.2).
        var hasCamera: @MainActor () -> Bool = { LocalCameraCapture.hasUsableCamera() }
        /// Put the call on the wire. A port rather than a direct
        /// `session.dial(…)` because dialing builds a real peer connection and
        /// opens the microphone, which a unit test must be able to observe
        /// without doing.
        var placeCall: @MainActor (_ session: CallSession, _ audio: Bool, _ video: Bool) -> Void = {
            session, audio, video in session.dial(audio: audio, video: video)
        }
        /// The TURN-credential client handed to each new session, or `nil` for
        /// STUN-only ICE.
        ///
        /// A port rather than a stored client because the credential is minted
        /// against the current session token, and a test must be able to watch
        /// a call be placed without reaching the auth service.
        var turnClient: @MainActor () -> TurnCredentialClient? = { nil }
    }

    // MARK: - Published state

    /// The call in the slot, or `nil`. A session that reached `.done` stays
    /// here only while the "Call ended" overlay is up.
    @Published private(set) var activeCall: CallSession?
    /// The group ``activeCall`` lives in.
    @Published private(set) var activeGroupIdHex: String?
    /// ``activeCall``'s state, re-published here so the root view observes one
    /// object. `nil` when there is nothing to show — including a call that
    /// ended while it was still ringing, which never had a screen to dismiss.
    @Published private(set) var activeCallState: CallSession.WebCallState?
    /// `true` when the ringing offer asked for video — the incoming screen
    /// says so, and it decides which accept is the primary one.
    @Published private(set) var incomingVideo: Bool = false
    /// `true` when this ringing call was reported to CallKit, so the phone's own
    /// call screen owns it and Lakko must not draw its own over the top.
    ///
    /// Written by ``CallKitBridge``, which is the only thing that knows which
    /// surface was chosen. It lives here rather than on the bridge because the
    /// view already observes this object, and one observed object is one fewer
    /// way for the two surfaces to disagree.
    @Published private(set) var ringingOnSystemUi: Bool = false
    /// `docs/ux/calls.md` section 3 — which button started the outgoing call.
    /// `nil` while no outgoing call is being prepared.
    @Published private(set) var outgoingButton: CallButtonKind?
    /// What that button opened: a call already on the wire, or the camera check
    /// the video button shows first. `nil` for an incoming call, which has no
    /// button of ours and never shows a preparation screen (section 3.3).
    @Published private(set) var outgoingSurface: CallStartSurface?
    /// The one line a video call downgraded to voice carries, or `nil`. Shown
    /// under the status line while the call rings out.
    @Published private(set) var outgoingNotice: String?
    /// An entry point could not proceed (permission denied, key derivation).
    @Published private(set) var callError: String?
    /// A start is past its tap but not yet holding a session.
    @Published private(set) var callStarting: Bool = false

    // MARK: - Internals

    private let ports: Ports
    let timing: CallHostTiming

    /// Exactly-once keys of every row already handled. Signaling rows are few;
    /// the set lives for the session and is cleared by ``reset()``.
    private var seenRows: Set<String> = []
    /// Hex call ids of 1x1 calls that ended here. Such a call never rings again.
    private(set) var endedCallIds: Set<String> = []

    /// The group (SFU) or call-link call occupying the app's one call slot, if
    /// any. Wired by the composition root (``CallSlotWiring``) to
    /// ``GroupCallHost`` and ``LinkCallHost``, which own those calls app-wide.
    var otherCallOccupant: @MainActor () -> CallKind? = { nil }

    private var stateSubscription: AnyCancellable?
    private var phaseTimer: CallHostTimer?
    private enum TimerPhase { case ring, establish }
    private var timerPhase: TimerPhase?
    /// Wall-clock deadline of the ring timer, so a refused accept can resume
    /// the ring for what is left of it.
    private var ringDeadlineMs: Int64?
    /// An accept is past its tap (permission prompts may be open).
    private var accepting = false
    /// The session has shown the full-screen call (outgoing / connecting /
    /// connected), so its `.done` keeps the "Call ended" overlay up.
    private var sessionWasPresented = false

    /// `docs/ux/calls.md` section 6 — what the transcript row needs, gathered
    /// while the call runs because none of it is readable once it is over: the
    /// side that placed it, and the moment media started flowing (`nil` for a
    /// call that never connected, which is what separates a call from an
    /// attempt).
    private var activeCallDirection: CallTranscriptRow.Direction = .outgoing
    private var connectedAtMs: Int64?
    /// When the call started: the moment it began ringing here or dialling
    /// out, NOT the moment media connected. The Calls list files a missed call
    /// at the time it happened, and a missed call has no connect time.
    private var callStartedAtMs: Int64?
    /// The call actually happened: it rang here, or it was dialed. A pre-join
    /// the user cancelled was never a call and leaves no row.
    private var callWasPlaced = false
    /// The row for the session in the slot has been written. A session reaches
    /// `.done` once, but the slot is cleared from several places; this keeps a
    /// second pass from filing a duplicate row.
    private var callRecorded = false

    /// Tail of the outbound publish chain: a call's offer, ICE candidates and
    /// end go out in the order the session produced them.
    private var publishTail: Task<Void, Never>?

    init(ports: Ports, timing: CallHostTiming = CallHostTiming()) {
        self.ports = ports
        self.timing = timing
    }

    /// Claim the repository's call-signaling slot. Done once, by the
    /// composition root; nothing else writes the slot.
    func attach(to repository: ChatRepository) {
        repository.callSignalingSink = { [weak self] row in
            self?.receive(row)
        }
    }

    // MARK: - One call slot

    /// A group or call-link call holds the slot. While it does, a 1x1 offer is
    /// answered busy and a 1x1 start is refused.
    var otherCallBusy: Bool { otherCallOccupant() != nil }

    /// A 1x1 call holds the slot: being prepared, ringing, dialing, live or
    /// ending, or a start past its tap.
    var occupiesSlot: Bool { hasLiveCall || callStarting }

    /// A 1x1 call occupies the slot (anything short of `.done`).
    var hasLiveCall: Bool {
        guard let call = activeCall else { return false }
        return call.state != .done
    }

    // MARK: - Inbound

    /// Handle one row. See the type docs for the delivery contract.
    func receive(_ row: InboundCallRow) {
        guard seenRows.insert(row.dedupKey).inserted else { return }
        let active: CallHostRouting.ActiveCall? = {
            guard let call = activeCall, call.state != .done, let group = activeGroupIdHex else {
                return nil
            }
            return .init(callIdHex: CallHostRouting.hex(call.callId), groupIdHex: group)
        }()
        let action = CallHostRouting.decide(
            row: row,
            nowMs: ports.nowMs(),
            active: active,
            endedCallIds: endedCallIds,
            otherCallBusy: otherCallBusy,
            muted: ports.isMuted(row.groupIdHex)
        )
        // Every incoming OFFER is accounted for, whatever the verdict. A call
        // that does not ring is silent on both ends until the caller's own ring
        // window writes `Missed call`, and the quiet verdicts are
        // indistinguishable from the outside: `drop` (past the window, or an
        // unreadable row time), `silent` (a muted conversation), `busy` (a slot
        // this device never released) and `apply` (a row for a call already in
        // the slot). Nine calls in a row were lost to this on 2026-09-22, with
        // only `drop` instrumented.
        if case .offer = row.body {
            let age = ChatRepository.parseDsTimestampMs(row.createdAt)
                .map { String(ports.nowMs() - $0) } ?? "unreadable"
            NSLog(
                "CallHost: incoming offer -> %@, row age %@ ms (createdAt %@, slot %@)",
                String(describing: action), age, row.createdAt,
                active.map { "busy:" + $0.callIdHex } ?? "free")
        }
        switch action {
        case .drop:
            return
        case .apply:
            activeCall?.applyIncomingSignaling(row.body)
        case .busy:
            // Not recorded as ended: the caller is still ringing this device,
            // and a later offer row for the same call is answered busy again.
            let callId = CallSession.callId(of: row.body)
            if let bytes = try? composeCallSignaling(body: .end(callId: callId, reason: .busy)) {
                enqueuePublish(groupIdHex: row.groupIdHex, bytes: bytes)
            }
        case .silent:
            // `docs/ux/calls.md` section 2.1. No session, no screen, no sound —
            // and a history row all the same, so the person can see who called
            // while the conversation was silenced. Filed as `missed` because
            // that is what it is and what an unanswered unmuted call becomes,
            // so the two are indistinguishable afterwards, which is the point.
            //
            // Recorded once: the call id joins `endedCallIds`, so a re-delivered
            // offer row for the same call does not file a second row.
            guard case .offer(let callId, _, _, let media) = row.body else { return }
            endedCallIds.insert(CallHostRouting.hex(callId))
            ports.recordCall(FinishedCall(
                groupIdHex: row.groupIdHex,
                row: CallTranscriptRow(direction: .incoming, outcome: .missed),
                video: media.video,
                // When it happened, by the server's stamp — not now, which is
                // whenever this device got round to reading the row.
                startedAtMs: ChatRepository.parseDsTimestampMs(row.createdAt) ?? ports.nowMs()))
        case .ring:
            guard case .offer(let callId, _, _, let media) = row.body else { return }
            let session = install(callId: callId, groupIdHex: row.groupIdHex, direction: .incoming)
            incomingVideo = media.video
            session.applyIncomingSignaling(row.body)
        }
    }

    // MARK: - Outbound + user actions

    /// Tap-to-call. Settles the microphone permission, builds the session
    /// (deriving + arming the SFrame key) — and then does one of two things,
    /// decided by ``CallMediaChoice/startSurface(for:hasCamera:)``:
    ///
    ///   * the **phone button dials there and then** (`docs/ux/calls.md`
    ///     section 3.1), so the call screen opens on `Calling...`. There used to
    ///     be a pre-join between the tap and the call — a card asking the user
    ///     to "check your microphone", with a camera box that was empty and two
    ///     switches — which is a settings page in front of a phone call;
    ///   * the **video button leaves the session in `.idle`** (section 3.2),
    ///     which the screen shows as the camera check; Call dials from there.
    ///
    /// A video call on a device with no camera is a voice call with one line
    /// saying so, never a check of a camera that does not exist.
    func startOutgoingCall(groupIdHex: String, button: CallButtonKind = .voice) {
        guard !hasLiveCall, !callStarting else { return }
        if let other = otherCallOccupant() {
            callError = CallSlot.busyMessage(other, verb: .start)
            return
        }
        callStarting = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.callStarting = false }
            guard await self.ports.microphoneGranted() else {
                self.callError = MicrophonePermission.deniedMessage
                return
            }
            guard !self.hasLiveCall, !self.otherCallBusy else { return }
            let hasCamera = self.ports.hasCamera()
            let surface = CallMediaChoice.startSurface(for: button, hasCamera: hasCamera)
            let session = self.install(
                callId: self.ports.mintCallId(),
                groupIdHex: groupIdHex,
                direction: .outgoing,
                button: button,
                surface: surface)
            self.outgoingNotice = CallMediaChoice.noCameraNotice(
                for: button, hasCamera: hasCamera)
            if case .dial(let video) = surface {
                self.ports.placeCall(session, true, video)
            }
        }
    }

    /// Accept the ringing call — after the microphone permission, and the
    /// camera permission too when accepting with video. A refusal leaves the
    /// call ringing (so Decline, or an audio-only accept, still works).
    func acceptIncomingCall(withVideo: Bool) {
        guard let session = activeCall, session.state == .ringing, !accepting else { return }
        accepting = true
        // The ring timeout no longer applies; the establish timeout does (a
        // permission prompt left open, a transport that never comes up).
        armTimer(.establish, for: session)
        Task { @MainActor [weak self] in
            guard let self else { return }
            let micOk = await self.ports.microphoneGranted()
            var camOk = false
            if micOk {
                camOk = await self.ports.cameraGranted(withVideo)
            }
            guard self.activeCall === session, session.state == .ringing else {
                self.accepting = false
                return
            }
            guard micOk, camOk else {
                self.accepting = false
                self.callError = micOk ? CameraPermission.deniedMessage : MicrophonePermission.deniedMessage
                self.resumeRing(for: session)
                return
            }
            session.acceptCall(withVideo: withVideo)
            self.accepting = false
        }
    }

    /// Record which surface is ringing this call. Called by ``CallKitBridge``.
    func setRingingOnSystemUi(_ onSystem: Bool) {
        guard ringingOnSystemUi != onSystem else { return }
        ringingOnSystemUi = onSystem
    }

    func declineIncomingCall() {
        activeCall?.declineCall()
    }

    func hangUp() {
        activeCall?.hangUp(reason: .hungUp)
    }

    /// End the 1x1 call in the slot, whatever state it is in — the "End
    /// current call" affordance of a refused start elsewhere.
    func endCurrentCall() {
        guard let call = activeCall else { return }
        switch call.state {
        case .ringing:
            call.declineCall()
        case .idle:
            clearFinishedCall()
        case .outgoing, .connecting, .connected:
            call.hangUp(reason: .hungUp)
        case .ending, .done:
            break
        }
    }

    /// Clear the slot once the "Call ended" overlay is dismissed (or a camera
    /// check is cancelled).
    func clearFinishedCall() {
        guard let call = activeCall else { return }
        if call.state == .done || call.state == .idle {
            if call.state == .idle { call.hangUp(reason: .hungUp) }
            clearSlot()
        }
    }

    func dismissCallError() {
        callError = nil
    }

    /// Logout / account switch: end whatever is live and forget the session's
    /// bookkeeping.
    func reset() {
        if hasLiveCall { activeCall?.hangUp(reason: .hungUp) }
        clearSlot()
        seenRows.removeAll()
        endedCallIds.removeAll()
        callError = nil
    }

    /// The display name of the person on the other end of the active call.
    var activePeerName: String? {
        activeGroupIdHex.flatMap { ports.peerName($0) }
    }

    /// Resolves once every publish queued so far has been attempted. Tests.
    func whenPublishesSettle() async {
        await publishTail?.value
    }

    // MARK: - Slot + timers

    private func install(
        callId: Data,
        groupIdHex: String,
        direction: CallTranscriptRow.Direction,
        button: CallButtonKind? = nil,
        surface: CallStartSurface? = nil
    ) -> CallSession {
        // A `.done` session still on screen is replaced by the new call.
        clearSlot()
        activeCallDirection = direction
        outgoingButton = button
        outgoingSurface = surface
        let session = CallSession(callId: callId) { [weak self] bytes in
            self?.enqueuePublish(groupIdHex: groupIdHex, bytes: bytes)
        }
        if let key = ports.deriveCallKey(groupIdHex, callId) {
            session.setCallKey(seed: key.seed, localLeaf: key.localLeaf)
        } else {
            NSLog("CallHost: call-key derivation failed; the call will refuse per invariant #4")
        }
        // Without this the session resolves STUN-only ICE, and two peers that
        // cannot reach each other directly never connect. Fail-soft by
        // contract: a `nil` client, or a credential the auth service declines
        // to mint, leaves the call on STUN rather than refusing it.
        session.setTurnClient(ports.turnClient())
        activeCall = session
        activeGroupIdHex = groupIdHex
        sessionWasPresented = false
        connectedAtMs = nil
        callStartedAtMs = nil
        callWasPlaced = false
        callRecorded = false
        stateSubscription = session.$state.sink { [weak self, weak session] next in
            guard let self, let session, self.activeCall === session else { return }
            self.onStateChange(next, session: session)
        }
        return session
    }

    private func clearSlot() {
        stateSubscription = nil
        cancelTimer()
        ringDeadlineMs = nil
        accepting = false
        activeCall = nil
        activeGroupIdHex = nil
        activeCallState = nil
        incomingVideo = false
        ringingOnSystemUi = false
        outgoingButton = nil
        outgoingSurface = nil
        outgoingNotice = nil
        sessionWasPresented = false
        connectedAtMs = nil
        callStartedAtMs = nil
        callWasPlaced = false
        callRecorded = false
    }

    /// `docs/ux/calls.md` section 6 — file the finished call's row in the
    /// conversation it happened in. Once per call: a pre-join the user
    /// cancelled never rang anywhere and is not a call.
    private func recordFinishedCall(_ session: CallSession) {
        guard callWasPlaced, !callRecorded, let groupIdHex = activeGroupIdHex else { return }
        callRecorded = true
        let row = CallTranscriptRow(
            direction: activeCallDirection,
            outcome: CallTranscriptRow.outcome(
                connectedAtMs: connectedAtMs,
                endedAtMs: ports.nowMs(),
                cause: Self.cause(session.endReason)))
        // The mode the call was STARTED in, which is what calling back should
        // repeat: the button this device pressed for a call it placed, and the
        // media the offer asked for on one it received. Read here rather than
        // off the session, whose camera flag is torn down before this runs.
        let video = activeCallDirection == .outgoing ? outgoingButton == .video : incomingVideo
        ports.recordCall(FinishedCall(
            groupIdHex: groupIdHex,
            row: row,
            video: video,
            startedAtMs: callStartedAtMs ?? ports.nowMs()))
    }

    /// The session's end reason as the transcript's own vocabulary. `error`
    /// becomes `failed`: the row never says why, only that the call did not
    /// happen.
    private static func cause(_ reason: CallSession.WebCallEndReason?) -> CallTranscriptRow.Cause? {
        switch reason {
        case .hungUp: return .hungUp
        case .busy: return .busy
        case .declined: return .declined
        case .timedOut: return .timedOut
        case .error: return .failed
        case nil: return nil
        }
    }

    /// `@Published` emits in `willSet`, so `next` is the new value while
    /// `session.state` still reads the old one.
    private func onStateChange(_ next: CallSession.WebCallState, session: CallSession) {
        defer {
            if sessionWasPresented {
                activeCallState = next
            } else if next == .done {
                activeCallState = nil
            } else if next != .ending {
                // `.ending` of a call that never showed the full screen (a
                // declined ring, a cancelled camera check) is not a reason to show it.
                activeCallState = next
            }
        }
        switch next {
        case .idle, .ending:
            break
        case .ringing:
            callWasPlaced = true
            if callStartedAtMs == nil { callStartedAtMs = ports.nowMs() }
            ringDeadlineMs = ports.nowMs() + timing.ringTimeoutMs
            armTimer(.ring, for: session)
        case .outgoing:
            sessionWasPresented = true
            callWasPlaced = true
            if callStartedAtMs == nil { callStartedAtMs = ports.nowMs() }
            armTimer(.establish, for: session)
        case .connecting:
            sessionWasPresented = true
            if timerPhase != .establish { armTimer(.establish, for: session) }
        case .connected:
            sessionWasPresented = true
            if connectedAtMs == nil { connectedAtMs = ports.nowMs() }
            cancelTimer()
        case .done:
            cancelTimer()
            endedCallIds.insert(CallHostRouting.hex(session.callId))
            recordFinishedCall(session)
            if !sessionWasPresented {
                // Declined, timed out or ended remotely while ringing — nothing
                // on screen needs dismissing, so free the slot now. Deferred a
                // turn: this runs in `willSet`, before the session is `.done`.
                Task { @MainActor [weak self, weak session] in
                    guard let self, let session, self.activeCall === session else { return }
                    self.clearSlot()
                }
            }
        }
    }

    private func armTimer(_ phase: TimerPhase, for session: CallSession) {
        cancelTimer()
        timerPhase = phase
        let ms = phase == .ring ? timing.ringTimeoutMs : timing.establishTimeoutMs
        phaseTimer = ports.schedule(ms) { [weak self, weak session] in
            guard let self, let session, self.activeCall === session else { return }
            self.timerFired(phase, session: session)
        }
    }

    private func resumeRing(for session: CallSession) {
        let remaining = max(0, (ringDeadlineMs ?? ports.nowMs()) - ports.nowMs())
        cancelTimer()
        timerPhase = .ring
        phaseTimer = ports.schedule(remaining) { [weak self, weak session] in
            guard let self, let session, self.activeCall === session else { return }
            self.timerFired(.ring, session: session)
        }
    }

    private func cancelTimer() {
        phaseTimer?.cancel()
        phaseTimer = nil
        timerPhase = nil
    }

    private func timerFired(_ phase: TimerPhase, session: CallSession) {
        phaseTimer = nil
        timerPhase = nil
        switch phase {
        case .ring:
            // A permission prompt being answered is not an unanswered call; the
            // establish timer armed at accept bounds it instead.
            guard session.state == .ringing, !accepting else { return }
            session.hangUp(reason: .timedOut)
        case .establish:
            switch session.state {
            case .outgoing, .connecting, .ringing:
                session.hangUp(reason: .timedOut)
            case .idle, .connected, .ending, .done:
                break
            }
        }
    }

    private func enqueuePublish(groupIdHex: String, bytes: Data) {
        let previous = publishTail
        let publish = ports.publish
        publishTail = Task { @MainActor in
            await previous?.value
            if await !publish(groupIdHex, bytes) {
                NSLog("CallHost: call-signaling publish failed")
            }
        }
    }
}

// MARK: - Production ports

extension CallHost {
    /// The composition root's ports over the live repository.
    ///
    /// `sessionTokenProvider` is read at each call rather than captured once,
    /// because the token is rebound on login, recover and pairing, and a TURN
    /// credential minted against a stale one is refused.
    static func live(
        repository: ChatRepository,
        sessionTokenProvider: @escaping @MainActor () -> String? = { nil }
    ) -> CallHost {
        // One client for the process: it holds no per-call state, only the
        // bearer token, which the port refreshes before every mint.
        let turn = TurnCredentialClient()
        return CallHost(ports: Ports(
            publish: { [weak repository] groupIdHex, bytes in
                guard let repository else { return false }
                do {
                    // The wake is decided from the composed bytes rather than
                    // passed in by the session, so there is no second place a
                    // caller could get it wrong. Two of the seven bodies ask
                    // for one; see ``CallSignalingWake``.
                    try await repository.publishApplicationMessage(
                        groupIdHex: groupIdHex,
                        plaintextBytes: bytes,
                        wake: CallSignalingWake.shouldWake(bytes) ? .call : nil)
                    return true
                } catch {
                    NSLog("CallHost publish failed: %@", error.localizedDescription)
                    return false
                }
            },
            deriveCallKey: { [weak repository] groupIdHex, callId in
                repository?.deriveCallKey(groupIdHex: groupIdHex, callId: callId)
            },
            nowMs: { ChatRepository.nowMs() },
            schedule: { afterMs, fire in
                SleepingCallHostTimer(afterMs: afterMs, fire: fire)
            },
            microphoneGranted: { await MicrophonePermission.ensureGranted() },
            cameraGranted: { video in await CameraPermission.ensureGranted(forVideo: video) },
            peerName: { [weak repository] groupIdHex in
                guard let repository else { return nil }
                // The resolver answers "Member" for a person nothing is known
                // about; that is a placeholder, not a name, so it must fall
                // through to the conversation's own title rather than be shown
                // as who is on the call.
                let resolved = repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
                    .map { ProfileDirectory.shared.resolve(signatureKey: $0) }
                return CallPeerName.resolve(
                    profileName: resolved.flatMap { $0.isPlaceholder ? nil : $0.name },
                    chatTitle: repository.conversationTitle(groupIdHex: groupIdHex))
            },
            mintCallId: { CallSession.mintCallId() },
            recordCall: { [weak repository] call in
                repository?.recordFinishedCall(call)
            },
            turnClient: {
                guard let token = sessionTokenProvider(), !token.isEmpty else { return nil }
                turn.setSessionToken(token)
                return turn
            }
        ))
    }
}

/// Production timer: a sleeping task.
private final class SleepingCallHostTimer: CallHostTimer {
    private let task: Task<Void, Never>

    @MainActor
    init(afterMs: Int64, fire: @escaping @MainActor () -> Void) {
        task = Task { @MainActor in
            try? await Task.sleep(nanoseconds: UInt64(max(0, afterMs)) * 1_000_000)
            guard !Task.isCancelled else { return }
            fire()
        }
    }

    func cancel() {
        task.cancel()
    }
}
