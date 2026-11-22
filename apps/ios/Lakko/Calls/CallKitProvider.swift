// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import CallKit
import Foundation
import WebRTC
import os
import UIKit

/// The app's one `CXProvider`: the object that makes an incoming Lakko call a
/// call as far as the phone is concerned (`docs/ux/calls.md` section 2.1).
///
/// Reporting a call here is what buys every line of that table: the full-screen
/// system call UI over the lock screen, the ringtone and vibration, answering
/// without unlocking, the phone's own in-call controls, and a row in the
/// Phone app's call history. None of it is reachable any other way — a
/// notification, however urgent, is not a call.
///
/// ## One provider per process, and why it is a singleton
///
/// CallKit is a process-wide facility: `CXProvider` owns the system call UI and
/// two of them would fight over it. The two places that report calls reach this
/// object from opposite ends of the app — ``VoipPushRegistrar``, which is owned
/// by the `UIApplicationDelegate` adaptor and can run before any SwiftUI scene
/// exists, and ``CallKitBridge``, built next to ``CallHost`` in `LakkoApp.init`
/// — so the instance is shared rather than injected down two unrelated trees.
/// Everything ABOVE it is injected: the bridge takes the provider as a
/// parameter, so a test drives a provider of its own.
///
/// ## What it does not decide
///
/// Nothing. Which surface shows, what the screen says, and which UUID a call
/// has are all ``IncomingCallPresentation`` / ``IncomingCallReport`` — pure
/// functions with tests. This class only talks to CallKit and forwards the
/// system's answer / end / mute actions to the closures the bridge sets.
/// `@unchecked Sendable` is a claim about this class, and the claim is: every
/// mutable field it has (`onAnswer`, `onEnd`, `onSetMuted`, `onReset`,
/// `liveCalls`) is `@MainActor`-isolated, and the two immutable ones
/// (`CXProvider`, `CXCallController`) are documented as safe to message from
/// any queue. What the compiler cannot see is that the completion handlers
/// CallKit calls on a queue of its own choosing only ever hop BACK to the main
/// actor before touching anything of ours.
final class CallKitProvider: NSObject, @unchecked Sendable {

    /// The process-wide provider. See the type docs for why it is shared.
    static let shared = CallKitProvider()

    private static let log = Logger(subsystem: "me.lakko", category: "callkit")

    /// The system answered a call. The bridge accepts it on ``CallHost``.
    @MainActor var onAnswer: ((UUID) -> Void)?
    /// The system ended a call (Decline, End, or the system tearing it down).
    ///
    /// Async: the action is fulfilled only once this returns (or after
    /// ``SystemCallEnd/fulfilBound(timeoutDate:now:)``), and a background task
    /// holds the process until it returns. That is what gets the `End` frame of
    /// a call declined on the lock screen to the caller while this app sits in
    /// the background, instead of at its next launch.
    @MainActor var onEnd: ((UUID) async -> Void)?
    /// The system's mute switch moved.
    @MainActor var onSetMuted: ((UUID, Bool) -> Void)?
    /// CallKit reset — every call is gone and the app must drop its own.
    @MainActor var onReset: (() -> Void)?

    private let provider: CXProvider
    private let callController = CXCallController()
    /// Keeps the process running while the `End` of a call the system ended is
    /// published. A parameter only so a test can watch it.
    private let backgroundTasks: BackgroundTaskRunning

    /// The calls this provider has reported and not yet ended. It is what makes
    /// "already reported" answerable, so a call that arrived as a VoIP push and
    /// then again as a signaling row rings exactly once.
    @MainActor private var liveCalls: Set<UUID> = []

    init(backgroundTasks: BackgroundTaskRunning = UIApplicationBackgroundTasks()) {
        self.backgroundTasks = backgroundTasks
        let configuration = CXProviderConfiguration()
        configuration.supportsVideo = true
        // One call at a time, matching ``CallSlot`` — the app has one call
        // screen, one microphone and one camera, so telling the system
        // otherwise would let it hand back a state the app cannot render.
        configuration.maximumCallGroups = 1
        configuration.maximumCallsPerCallGroup = 1
        // Lakko has no phone numbers and no email addresses on the call path;
        // a handle is a name or an opaque conversation id, which is exactly
        // what `.generic` is for.
        configuration.supportedHandleTypes = [.generic]
        // `docs/ux/calls.md` section 6 — finished calls belong in the phone's
        // call history, which is where "tapping the row calls back" comes from
        // for a call the user never opened the app for.
        configuration.includesCallsInRecents = true
        provider = CXProvider(configuration: configuration)
        super.init()
        // A `nil` queue means the delegate is called on the main queue, which
        // is what lets the delegate methods below hop onto the main actor with
        // `assumeIsolated` instead of an async dispatch that would let an
        // action outlive its `fulfill()`.
        provider.setDelegate(self, queue: nil)
    }

    // MARK: - Reporting calls

    /// Has this call already been reported to the system and not yet ended?
    @MainActor func isReported(_ uuid: UUID) -> Bool {
        liveCalls.contains(uuid)
    }

    /// Report an incoming call, which is what makes the phone ring.
    ///
    /// `completion` fires when CallKit has accepted (or refused) the report.
    /// The PushKit handler MUST NOT return before it does — see
    /// ``VoipPushRegistrar``.
    ///
    /// Reporting the same UUID twice is refused by CallKit, so a second report
    /// for a live call is turned into an ``update(_:with:)`` instead: that is
    /// the ordinary case where the VoIP push rang the phone with a placeholder
    /// and the decrypted offer now knows the caller's name.
    @MainActor func reportIncomingCall(
        uuid: UUID,
        update: CallKitCallUpdate,
        completion: @escaping (Error?) -> Void
    ) {
        if liveCalls.contains(uuid) {
            self.update(uuid, with: update)
            completion(nil)
            return
        }
        liveCalls.insert(uuid)
        provider.reportNewIncomingCall(with: uuid, update: Self.callUpdate(update)) { error in
            if let error {
                // The report was refused (Do Not Disturb with the app not
                // allowed, a blocked caller, a malformed report). CallKit has
                // no call under this UUID, so neither may we.
                Self.log.error("reportNewIncomingCall refused: \(error.localizedDescription)")
                // A hop rather than `assumeIsolated`: this completion's queue
                // is CallKit's to choose, and bookkeeping cleanup can wait a
                // turn. Only the REPORT itself is timing-critical, and it has
                // already happened.
                Task { @MainActor in self.liveCalls.remove(uuid) }
            }
            completion(error)
        }
    }

    /// Correct what the system call screen says about a live call — the caller's
    /// real name and the real call kind, once the offer has been decrypted.
    @MainActor func update(_ uuid: UUID, with update: CallKitCallUpdate) {
        guard liveCalls.contains(uuid) else { return }
        provider.reportCall(with: uuid, updated: Self.callUpdate(update))
    }

    /// Tell the system an OUTGOING call is starting, so it knows a call is
    /// active (routing, the green status bar, other apps' interruptions) and
    /// files it in the call history.
    ///
    /// Requested as a `CXStartCallAction` rather than reported directly:
    /// CallKit only accepts `reportOutgoingCall(with:startedConnectingAt:)` for
    /// a call it already knows, and a start action is the only way to introduce
    /// one. The app has already begun dialing by the time this runs — the
    /// transaction is a notification to the system, not permission to proceed.
    @MainActor func startOutgoingCall(uuid: UUID, update: CallKitCallUpdate) {
        // The Simulator has no system in-call UI to host a CallKit call, so
        // `callservicesd` fulfils the start action and then immediately
        // disconnects the call itself, logging "Disconnecting call because
        // there wont be a UI to host the call" after failing to open
        // `facetime://?showInCallUI=1`. The app sees a `CXEndCallAction` about
        // 60 ms after its own start action and, having no way to tell it from
        // the user pressing End, hangs the call up while it is still dialing —
        // so no outgoing 1x1 call can survive on a Simulator. A device has the
        // UI and does not do this (Apple developer forums, thread 749657), so
        // the call is reported there exactly as before; this only keeps a
        // two-Simulator rig usable for testing calls.
        #if targetEnvironment(simulator)
        return
        #else
        guard !liveCalls.contains(uuid) else { return }
        liveCalls.insert(uuid)
        let handle = CXHandle(type: .generic, value: update.localizedCallerName)
        let action = CXStartCallAction(call: uuid, handle: handle)
        action.isVideo = update.hasVideo
        action.contactIdentifier = update.localizedCallerName
        callController.request(CXTransaction(action: action)) { error in
            // A hop rather than `assumeIsolated`: the transaction's completion
            // queue belongs to CallKit, not to us.
            guard let error else {
                // "Connecting" is reported only once the system has actually
                // taken the start action; reporting it against a call CallKit
                // does not know yet is ignored.
                Task { @MainActor in
                    guard self.liveCalls.contains(uuid) else { return }
                    self.provider.reportOutgoingCall(with: uuid, startedConnectingAt: nil)
                }
                return
            }
            Self.log.error("CXStartCallAction failed: \(error.localizedDescription)")
            Task { @MainActor in self.liveCalls.remove(uuid) }
        }
        #endif
    }

    /// Media is flowing — the system call screen starts its timer.
    @MainActor func reportConnected(uuid: UUID) {
        guard liveCalls.contains(uuid) else { return }
        provider.reportOutgoingCall(with: uuid, connectedAt: nil)
    }

    /// The call is over. Idempotent: ending a call the provider does not know
    /// is a no-op, so the several places a call can finish (the session, the
    /// host's timers, a remote end) may all report it.
    @MainActor func reportEnded(uuid: UUID, reason: CXCallEndedReason) {
        guard liveCalls.remove(uuid) != nil else { return }
        provider.reportCall(with: uuid, endedAt: nil, reason: reason)
    }

    /// End every reported call — logout, account switch, a reset.
    @MainActor func endAll(reason: CXCallEndedReason = .remoteEnded) {
        for uuid in liveCalls {
            provider.reportCall(with: uuid, endedAt: nil, reason: reason)
        }
        liveCalls.removeAll()
    }

    private static func callUpdate(_ update: CallKitCallUpdate) -> CXCallUpdate {
        let cxUpdate = CXCallUpdate()
        cxUpdate.localizedCallerName = update.localizedCallerName
        cxUpdate.remoteHandle = CXHandle(type: .generic, value: update.localizedCallerName)
        cxUpdate.hasVideo = update.hasVideo
        // Lakko has no hold, no conferencing on the 1x1 path and no DTMF; a
        // control the app cannot honour must not appear on the system screen.
        cxUpdate.supportsHolding = false
        cxUpdate.supportsGrouping = false
        cxUpdate.supportsUngrouping = false
        cxUpdate.supportsDTMF = false
        return cxUpdate
    }
}

// MARK: - CXProviderDelegate

extension CallKitProvider: CXProviderDelegate {

    /// Every delegate method arrives on the main queue (see `setDelegate`), so
    /// the main-actor state below is reached with `assumeIsolated` rather than
    /// an async hop — an action must be fulfilled or failed before it times
    /// out, and a dispatch would put that on the far side of a turn.
    func providerDidReset(_ provider: CXProvider) {
        NSLog("CallKitProvider: providerDidReset")
        MainActor.assumeIsolated {
            liveCalls.removeAll()
            onReset?()
        }
    }

    func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
        // The app has already started dialing; the action exists so the system
        // knows a call is active and files it in Recents.
        action.fulfill()
    }

    func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        MainActor.assumeIsolated {
            onAnswer?(action.callUUID)
        }
        action.fulfill()
    }

    /// Decline on the incoming-call screen, End on the in-call screen, or the
    /// system ending the call itself.
    ///
    /// The action is NOT fulfilled at once. The app's half of an end is a
    /// frame to the other person (`End{declined}` / `End{hung_up}`), and with
    /// the app in the background — the ordinary case for a call that rang on
    /// the lock screen — iOS suspends the process moments after the last call
    /// is over. Fulfilling first left that publish in flight, so the caller
    /// kept ringing for its whole window and filed the call as unanswered,
    /// while the frame went out only when the app next ran (seen live
    /// 2026-11-22: decline at 15:54:40, the caller saw the end at 15:55:03).
    /// So the handler runs under a background task, and the action is
    /// fulfilled once it has returned or after a bounded wait, whichever is
    /// first (``SystemCallEnd``).
    func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        let uuid = action.callUUID
        let bound = SystemCallEnd.fulfilBound(timeoutDate: action.timeoutDate, now: Date())
        NSLog("CallKitProvider: system performed CXEndCallAction (fulfil within %.1f s)", bound)
        MainActor.assumeIsolated {
            liveCalls.remove(uuid)
            let handler = onEnd
            SystemCallEnd.run(
                name: "lakko.call.end",
                background: backgroundTasks,
                fulfilWithin: bound,
                work: { await handler?(uuid) },
                fulfil: { action.fulfill() })
        }
    }

    func provider(_ provider: CXProvider, timedOutPerforming action: CXAction) {
        NSLog("CallKitProvider: a CallKit action timed out before it was fulfilled")
    }

    func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        MainActor.assumeIsolated {
            onSetMuted?(action.callUUID, action.isMuted)
        }
        action.fulfill()
    }

    /// CallKit has activated the shared audio session for the call.
    ///
    /// WebRTC has to be told, because the activation happened outside its own
    /// `RTCAudioSession` (the whole point of
    /// `RTCAudioSessionActivationDelegate`): without this the engine's view of
    /// the session diverges from the system's and the call can come up with no
    /// audio path.
    ///
    /// `useManualAudio` is deliberately left OFF. Turning it on is the usual
    /// CallKit recipe, but it makes WebRTC wait for `isAudioEnabled` before it
    /// starts the audio unit — and Lakko has three call paths (1x1 accepted
    /// in-app, group/SFU and call-link) that never see a CallKit activation, so
    /// every one of them would go silent. Forwarding the activation without
    /// claiming manual control keeps the CallKit path correct and the other
    /// three byte-for-byte as they were.
    func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        RTCAudioSession.sharedInstance().audioSessionDidActivate(audioSession)
    }

    func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        RTCAudioSession.sharedInstance().audioSessionDidDeactivate(audioSession)
    }
}

// MARK: - Finishing a system-ended call in the background

/// A handle on a running background task (``BackgroundTaskRunning``).
struct BackgroundTaskToken: Hashable {
    let raw: Int
}

/// Something that keeps the process running for a while after the app has
/// left the screen. `UIApplication` in the app, a recorder in a test.
protocol BackgroundTaskRunning {
    /// Start a task. `expiration` runs on the main thread when the system is
    /// about to suspend the process anyway; the task must be ended there.
    /// `nil` when the system grants none.
    @MainActor func begin(
        name: String, expiration: @escaping @MainActor () -> Void) -> BackgroundTaskToken?
    @MainActor func end(_ token: BackgroundTaskToken)
}

/// The app's background tasks, through `UIApplication`.
struct UIApplicationBackgroundTasks: BackgroundTaskRunning {
    @MainActor func begin(
        name: String, expiration: @escaping @MainActor () -> Void
    ) -> BackgroundTaskToken? {
        // UIKit calls the expiration handler synchronously on the main thread.
        let id = UIApplication.shared.beginBackgroundTask(withName: name) {
            MainActor.assumeIsolated { expiration() }
        }
        return id == .invalid ? nil : BackgroundTaskToken(raw: id.rawValue)
    }

    @MainActor func end(_ token: BackgroundTaskToken) {
        UIApplication.shared.endBackgroundTask(UIBackgroundTaskIdentifier(rawValue: token.raw))
    }
}

/// How the app finishes a call the SYSTEM ended (Decline on the lock screen,
/// End on the system call screen): the app's own work — publishing the `End`
/// frame that stops the caller ringing — runs under a background task, and the
/// CallKit action is fulfilled when that work is done or after a bounded wait,
/// whichever comes first.
///
/// Two separate clocks on purpose. The action has a deadline of its own
/// (`CXAction.timeoutDate`), after which CallKit fails it; the background task
/// has the system's, announced by its expiration handler. The action is
/// fulfilled well inside the first; the background task is held until the work
/// returns, so a slow network gets the time the system grants rather than the
/// action's few seconds.
@MainActor
enum SystemCallEnd {

    /// The longest the action waits for the app's work. Chosen, not measured:
    /// long enough for one Delivery Service publish on a normal connection,
    /// short against any action deadline seen in documentation.
    nonisolated static let maxFulfilWait: TimeInterval = 5

    /// How far ahead of the action's own deadline it is fulfilled at the latest.
    nonisolated static let actionDeadlineMargin: TimeInterval = 1

    /// The wait before the action is fulfilled even though the work has not
    /// returned: at most ``maxFulfilWait``, at least a margin before the
    /// action's deadline, never negative.
    nonisolated static func fulfilBound(timeoutDate: Date, now: Date) -> TimeInterval {
        let beforeDeadline = timeoutDate.timeIntervalSince(now) - actionDeadlineMargin
        return max(0, min(maxFulfilWait, beforeDeadline))
    }

    private final class State {
        var fulfilled = false
        var token: BackgroundTaskToken?
        var timeout: Task<Void, Never>?
    }

    /// Run `work` under a background task named `name`; call `fulfil` exactly
    /// once, when `work` returns or `fulfilWithin` seconds have passed or the
    /// background task expires, whichever is first; end the background task
    /// when `work` returns or it expires.
    static func run(
        name: String,
        background: BackgroundTaskRunning,
        fulfilWithin: TimeInterval,
        work: @escaping @MainActor () async -> Void,
        fulfil: @escaping @MainActor () -> Void
    ) {
        let state = State()
        let finish: @MainActor () -> Void = {
            guard !state.fulfilled else { return }
            state.fulfilled = true
            state.timeout?.cancel()
            state.timeout = nil
            fulfil()
        }
        let release: @MainActor () -> Void = {
            guard let token = state.token else { return }
            state.token = nil
            background.end(token)
        }
        state.token = background.begin(name: name) {
            finish()
            release()
        }
        let nanos = UInt64(max(0, fulfilWithin) * 1_000_000_000)
        state.timeout = Task { @MainActor in
            try? await Task.sleep(nanoseconds: nanos)
            guard !Task.isCancelled else { return }
            finish()
        }
        Task { @MainActor in
            await work()
            finish()
            release()
        }
    }
}
