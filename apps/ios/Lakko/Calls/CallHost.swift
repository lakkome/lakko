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
        /// The other person's resolved name for a 1x1 chat, for the banner.
        var peerName: @MainActor (_ groupIdHex: String) -> String?
        var mintCallId: () -> Data
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
    /// `true` when the ringing offer asked for video — the banner says so.
    @Published private(set) var incomingVideo: Bool = false
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

    /// Group (SFU) calls in progress, owned by chat view models. Weak, so a
    /// view model that SwiftUI discarded mid-call cannot leave this device
    /// permanently "busy".
    private var groupCallOwners: [WeakOwner] = []

    private final class WeakOwner {
        weak var value: AnyObject?
        init(_ value: AnyObject) { self.value = value }
    }

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

    // MARK: - Group-call busy

    /// A chat view model's group (SFU) call started or ended. While any is live
    /// a 1x1 offer is answered busy.
    func noteGroupCall(owner: AnyObject, active: Bool) {
        groupCallOwners.removeAll { $0.value == nil || $0.value === owner }
        if active { groupCallOwners.append(WeakOwner(owner)) }
    }

    var groupCallBusy: Bool { groupCallOwners.contains { $0.value != nil } }

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
            groupCallBusy: groupCallBusy
        )
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
        case .ring:
            guard case .offer(let callId, _, _, let media) = row.body else { return }
            let session = install(callId: callId, groupIdHex: row.groupIdHex)
            incomingVideo = media.video
            session.applyIncomingSignaling(row.body)
        }
    }

    // MARK: - Outbound + user actions

    /// Tap-to-call. Settles the microphone permission, then builds the session
    /// (deriving + arming the SFrame key) WITHOUT dialing: it sits in `.idle`,
    /// which the call screen shows as the prejoin; Join dials.
    func startOutgoingCall(groupIdHex: String) {
        guard !hasLiveCall, !callStarting, !groupCallBusy else { return }
        callStarting = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.callStarting = false }
            guard await self.ports.microphoneGranted() else {
                self.callError = MicrophonePermission.deniedMessage
                return
            }
            guard !self.hasLiveCall, !self.groupCallBusy else { return }
            _ = self.install(callId: self.ports.mintCallId(), groupIdHex: groupIdHex)
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

    func declineIncomingCall() {
        activeCall?.declineCall()
    }

    func hangUp() {
        activeCall?.hangUp(reason: .hungUp)
    }

    /// Clear the slot once the "Call ended" overlay is dismissed (or a prejoin
    /// is cancelled).
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
        groupCallOwners.removeAll()
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

    private func install(callId: Data, groupIdHex: String) -> CallSession {
        // A `.done` session still on screen is replaced by the new call.
        clearSlot()
        let session = CallSession(callId: callId) { [weak self] bytes in
            self?.enqueuePublish(groupIdHex: groupIdHex, bytes: bytes)
        }
        if let key = ports.deriveCallKey(groupIdHex, callId) {
            session.setCallKey(seed: key.seed, localLeaf: key.localLeaf)
        } else {
            NSLog("CallHost: call-key derivation failed; the call will refuse per invariant #4")
        }
        activeCall = session
        activeGroupIdHex = groupIdHex
        sessionWasPresented = false
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
        sessionWasPresented = false
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
                // declined ring, a cancelled prejoin) is not a reason to show it.
                activeCallState = next
            }
        }
        switch next {
        case .idle, .ending:
            break
        case .ringing:
            ringDeadlineMs = ports.nowMs() + timing.ringTimeoutMs
            armTimer(.ring, for: session)
        case .outgoing:
            sessionWasPresented = true
            armTimer(.establish, for: session)
        case .connecting:
            sessionWasPresented = true
            if timerPhase != .establish { armTimer(.establish, for: session) }
        case .connected:
            sessionWasPresented = true
            cancelTimer()
        case .done:
            cancelTimer()
            endedCallIds.insert(CallHostRouting.hex(session.callId))
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
    static func live(repository: ChatRepository) -> CallHost {
        CallHost(ports: Ports(
            publish: { [weak repository] groupIdHex, bytes in
                guard let repository else { return false }
                do {
                    try await repository.publishApplicationMessage(
                        groupIdHex: groupIdHex, plaintextBytes: bytes)
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
                repository?.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
                    .map { ProfileDirectory.shared.resolve(signatureKey: $0).name }
            },
            mintCallId: { CallSession.mintCallId() }
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
