// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CallKit
import Combine
import Foundation
import UIKit
import UserNotifications
import os

/// Joins the app's 1x1 call (``CallHost``) to the phone's call machinery
/// (``CallKitProvider``), in both directions.
///
///   * **Out:** a call that starts ringing, dialing, connects or ends is
///     reported to the system, so the phone rings over the lock screen, shows
///     its own in-call UI, and files the call in Recents
///     (`docs/ux/calls.md` sections 2.1 and 6).
///   * **In:** Answer, End and Mute pressed on the system call screen drive
///     ``CallHost/acceptIncomingCall(withVideo:)``,
///     ``CallHost/endCurrentCall()`` and the session's microphone — the same
///     entry points the in-app buttons use. There is no second accept path.
///
/// ## Why a separate object
///
/// ``CallHost`` is the call's owner and knows nothing about CallKit; it must
/// stay that way, because it is the piece with the routing rules and the
/// property tests (`CallHostTests`), and it runs identically in a test with no
/// system call UI anywhere. The bridge is the adapter, holds no rules of its
/// own — which surface shows is ``IncomingCallPresentation``, what the screen
/// says is ``IncomingCallReport``, and how a call's end reads to the system is
/// ``CallKitEndReason`` — and is built once, next to the host, in
/// `LakkoApp.init`.
@MainActor
final class CallKitBridge {

    private static let log = Logger(subsystem: "me.lakko", category: "callkit")

    private let provider: CallKitProvider
    private let registry: CallKitCallRegistry
    private unowned let host: CallHost
    /// Whether the app is frontmost. Injected so a test can drive both rows of
    /// `docs/ux/calls.md` section 2.1 without a running scene.
    private let appIsActive: @MainActor () -> Bool
    private var subscriptions: Set<AnyCancellable> = []
    /// The UUID of the call currently reported for this host, so an end can be
    /// reported after ``CallHost`` has already cleared its slot.
    private var reportedUuid: UUID?

    /// The two shared objects and the app-state probe arrive as `nil`-defaulted
    /// parameters rather than as `= .shared` defaults, because a default
    /// argument is evaluated in a NON-isolated context: reading a main-actor
    /// singleton there is a warning today and an error under the Swift 6
    /// language mode. Resolved inside the (main-actor) body instead.
    init(
        provider: CallKitProvider = .shared,
        registry: CallKitCallRegistry? = nil,
        host: CallHost,
        appIsActive: (@MainActor () -> Bool)? = nil
    ) {
        self.provider = provider
        self.registry = registry ?? .shared
        self.host = host
        self.appIsActive = appIsActive ?? { UIApplication.shared.applicationState == .active }
        wireSystemActions()
        observeHost()
    }

    // MARK: - System -> app

    private func wireSystemActions() {
        provider.onAnswer = { [weak self] uuid in
            guard let self, self.matchesActiveCall(uuid) else { return }
            // The system screen's single Answer button means "take this call as
            // it was offered": a video call answers with video, a voice call
            // does not turn a camera on (`docs/ux/calls.md` section 3).
            self.host.acceptIncomingCall(withVideo: self.host.incomingVideo)
            self.clearCallNotifications(for: uuid)
        }
        provider.onEnd = { [weak self] uuid in
            guard let self, self.matchesActiveCall(uuid) else { return }
            self.host.endCurrentCall()
            self.registry.release(uuid: uuid)
            self.clearCallNotifications(for: uuid)
        }
        provider.onSetMuted = { [weak self] uuid, muted in
            guard let self, self.matchesActiveCall(uuid) else { return }
            self.host.activeCall?.setMicrophoneEnabled(!muted)
        }
        provider.onReset = { [weak self] in
            // CallKit has thrown away every call it knew about; the app must
            // not keep one running that the system will never show again.
            self?.reportedUuid = nil
            self?.registry.reset()
            self?.host.endCurrentCall()
        }
    }

    private func matchesActiveCall(_ uuid: UUID) -> Bool {
        guard let call = host.activeCall,
              let active = registry.uuid(forCallId: call.callId) else {
            return false
        }
        return active == uuid
    }

    // MARK: - App -> system

    private func observeHost() {
        host.$activeCallState
            .sink { [weak self] state in
                // `@Published` fires in `willSet`, so the host's other fields
                // are already current for this transition.
                self?.apply(state)
            }
            .store(in: &subscriptions)
    }

    private func apply(_ state: CallSession.WebCallState?) {
        guard let state else {
            // The slot emptied without a `.done` we saw (a ring that ended
            // before it was ever presented); make sure the system is not left
            // showing a call.
            endReported(reason: .remoteEnded)
            return
        }
        guard let call = host.activeCall,
              // The registry is what makes a call the VoIP push already reported
              // keep the UUID it is ringing under, instead of being reported a
              // second time under its call id's own.
              let uuid = registry.uuid(forCallId: call.callId) else {
            return
        }
        switch state {
        case .ringing:
            reportIncoming(uuid: uuid)
        case .outgoing:
            reportOutgoing(uuid: uuid)
        case .connecting:
            break
        case .connected:
            reportedUuid = uuid
            provider.reportConnected(uuid: uuid)
        case .done:
            endReported(reason: CallKitEndReason.map(call.endReason))
            registry.release(callId: call.callId)
            clearCallNotifications(for: uuid)
        case .idle, .ending:
            break
        }
    }

    private func reportIncoming(uuid: UUID) {
        let surface = IncomingCallPresentation.decide(
            source: .signalingRow,
            appIsActive: appIsActive(),
            alreadyReported: provider.isReported(uuid)
        )
        switch surface {
        case .systemCallUi:
            reportedUuid = uuid
            provider.reportIncomingCall(
                uuid: uuid,
                update: IncomingCallReport.resolved(
                    callerName: host.activePeerName,
                    video: host.incomingVideo
                )
            ) { error in
                if let error {
                    Self.log.error("incoming call report failed: \(error.localizedDescription)")
                }
            }
        case .none:
            // Already ringing on the system screen from the VoIP push; the
            // decrypted row only knows better who is calling.
            reportedUuid = uuid
            provider.update(
                uuid,
                with: IncomingCallReport.resolved(
                    callerName: host.activePeerName,
                    video: host.incomingVideo
                )
            )
        case .inApp:
            // `CallHostOverlay` is already showing it; the phone stays quiet.
            break
        }
    }

    private func reportOutgoing(uuid: UUID) {
        reportedUuid = uuid
        provider.startOutgoingCall(
            uuid: uuid,
            update: IncomingCallReport.resolved(
                callerName: host.activePeerName,
                video: host.outgoingButton == .video
            )
        )
    }

    private func endReported(reason: CXCallEndedReason) {
        guard let uuid = reportedUuid else { return }
        reportedUuid = nil
        provider.reportEnded(uuid: uuid, reason: reason)
    }

    /// Take down any lock-screen call alert the notification-service extension
    /// raised for this call.
    ///
    /// The NSE alert (``NsePushOutcome/callOffer(_:)``) is the fallback for a
    /// wake that arrived as an ordinary alert push rather than a VoIP one. Once
    /// the call has been answered or ended HERE, that alert is stale, and
    /// leaving it up is also what would make the NSE read a later "call ended"
    /// row as a missed call. The NSE cannot clear it on its own for the answer
    /// case — it never learns that this device picked up.
    private func clearCallNotifications(for uuid: UUID) {
        let callIdHex = IncomingCallReport.hex(IncomingCallReport.callId(forUuid: uuid))
        Task {
            // `current()` is called inside rather than captured: the centre is
            // not a `Sendable` type, and there is nothing to gain from carrying
            // one across the hop.
            let center = UNUserNotificationCenter.current()
            let ids = await center.deliveredNotifications()
                .filter {
                    $0.request.content
                        .userInfo[LakkoCallNotification.callIdUserInfoKey] as? String == callIdHex
                }
                .map(\.request.identifier)
            guard !ids.isEmpty else { return }
            center.removeDeliveredNotifications(withIdentifiers: ids)
        }
    }
}

/// How a finished Lakko call reads to the phone's call history.
///
/// Pure, and separate from the bridge, because the mapping is a product
/// decision (`docs/ux/calls.md` section 6: `Missed call`, `Declined`) rather
/// than plumbing: it decides what the Phone app's Recents list says happened.
enum CallKitEndReason {
    static func map(_ reason: CallSession.WebCallEndReason?) -> CXCallEndedReason {
        switch reason {
        case .timedOut:
            // Nobody picked up within the ring window — a missed call on both
            // sides (section 2.1's ring rules).
            return .unanswered
        case .declined:
            return .declinedElsewhere
        case .busy:
            // The other device is already in a call. The system has no "busy",
            // and `.unanswered` is the truthful remainder: this call never
            // happened.
            return .unanswered
        case .error:
            return .failed
        case .hungUp, nil:
            return .remoteEnded
        }
    }
}
