// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CallKit
import Foundation
import PushKit
import os

/// The PushKit half of "the call must arrive when the app is closed"
/// (`docs/ux/calls.md` section 2.1).
///
/// A VoIP push is the only push iOS will launch a terminated app for and let it
/// ring the phone. In exchange the platform imposes one hard rule: **every VoIP
/// push must report a call to CallKit before its handler returns**, or iOS
/// kills the app and, after a few offences, stops delivering VoIP pushes to it
/// at all. Everything in the incoming path below is shaped by that rule:
///
///   * The report happens FIRST, with a placeholder
///     (``IncomingCallReport/placeholder()``), because the payload carries no
///     caller and no call kind — it carries an opaque routing id and a journal
///     pointer, and nothing else (invariants #2 and #5).
///   * Only then does the app go and decrypt: the wake kicks the receive path,
///     the offer arrives as an ordinary signaling row, and ``CallKitBridge``
///     corrects the system screen with the real name and kind through a call
///     update.
///   * A wake that never produces a call is ENDED after the ring window rather
///     than left ringing — an abandoned reported call is the other way an app
///     loses its VoIP privileges.
///
/// The VoIP token is registered with `services/push-gw` under its own provider
/// (``PushTokenKind/voip``), never overwriting the alert token: they are
/// different tokens for different push types.
@MainActor
final class VoipPushRegistrar: NSObject {

    static let shared = VoipPushRegistrar()

    private static let log = Logger(subsystem: "me.lakko", category: "voip-push")

    /// How long a placeholder call may ring without the offer arriving. The
    /// ring window of `docs/ux/calls.md` section 2.1, so a wake whose signaling
    /// row never lands stops ringing at the same moment the caller gives up.
    private static let placeholderRingWindow: TimeInterval = 45

    /// Kick the receive path for a wake, so the offer behind it is fetched and
    /// decrypted. Set by the composition root; a registrar with no wake hook
    /// still rings (the placeholder), it just never learns who is calling.
    var onWake: ((VoipWakePayload) -> Void)?

    private let provider: CallKitProvider
    private let registry: CallKitCallRegistry
    private let client: PushClient
    private var pushRegistry: PKPushRegistry?
    private var lastRegisteredToken: Data?
    private var placeholderTimeout: Task<Void, Never>?

    /// `registry` is `nil`-defaulted rather than `= .shared` because a default
    /// argument is evaluated in a NON-isolated context, and the registry is
    /// main-actor isolated (a warning today, an error under Swift 6).
    init(
        provider: CallKitProvider = .shared,
        registry: CallKitCallRegistry? = nil,
        client: PushClient = PushClient()
    ) {
        self.provider = provider
        self.registry = registry ?? .shared
        self.client = client
        super.init()
    }

    /// Begin receiving VoIP pushes. Idempotent — the registry is created once
    /// and iOS re-delivers the current token to the delegate on every launch.
    ///
    /// Called at launch rather than at sign-in: a VoIP token is device state,
    /// not account state, and the app has to be able to receive a push before
    /// it knows whether anyone will send one. The REGISTRATION with the gateway
    /// is what is account-scoped, and it is refused harmlessly when there is no
    /// device pseudonym yet.
    func start() {
        guard pushRegistry == nil else { return }
        // The main queue, so the delegate callbacks below can reach main-actor
        // state without an async hop — a hop would put the CallKit report on
        // the far side of a turn, after the handler has already returned.
        let registry = PKPushRegistry(queue: .main)
        registry.delegate = self
        registry.desiredPushTypes = [.voIP]
        pushRegistry = registry
    }

    /// Forget the VoIP registration (logout / forget device). Best-effort: a
    /// gateway that keeps the row only sends pushes this device will refuse to
    /// turn into a call.
    func unregister() async {
        lastRegisteredToken = nil
        guard let pseudonym = (try? DevicePushId.peek()) ?? nil else { return }
        do {
            _ = try await client.unregister(devicePseudonym: pseudonym, provider: .apnsVoip)
        } catch {
            Self.log.warning("voip unregister failed: \(error.localizedDescription)")
        }
    }

    // MARK: - Token

    private func register(token: Data) {
        guard PushTokenRegistration.shouldRegister(
            token: token,
            lastRegistered: lastRegisteredToken
        ) else {
            return
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            guard let pseudonym = (try? DevicePushId.peek()) ?? nil else {
                // No device pseudonym yet — the account has not finished
                // signing in. The next `start()` after sign-in re-delivers the
                // same token and this runs again.
                Self.log.info("voip token held: no device pseudonym yet")
                return
            }
            do {
                try await self.client.register(
                    devicePseudonym: pseudonym,
                    provider: PushTokenKind.voip.provider,
                    tokenBytes: token
                )
                self.lastRegisteredToken = token
                Self.log.info("voip token registered")
            } catch {
                // Expected until the gateway learns the provider — see the
                // `apnsVoip` case in `PushClient.Provider`. Never fatal: the
                // alert-push path keeps working, and the NSE keeps raising the
                // call alert that stands in for a VoIP wake.
                Self.log.warning("voip token register failed: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - Incoming

    /// Report the call, then let the app go find out what it is.
    ///
    /// `completion` is the PushKit handler's; it is called from inside the
    /// CallKit report's own completion so the platform rule holds by
    /// construction rather than by ordering luck.
    private func handleIncoming(payload: [AnyHashable: Any], completion: @escaping () -> Void) {
        let wake = VoipWakePayload.parse(payload)
        let uuid = UUID()
        if let superseded = registry.placeholderReported(uuid) {
            // A previous wake's call never materialised. Ending it here keeps
            // the system from showing two calls for a device that can hold one.
            provider.reportEnded(uuid: superseded, reason: .unanswered)
        }
        armPlaceholderTimeout(uuid: uuid)
        provider.reportIncomingCall(uuid: uuid, update: IncomingCallReport.placeholder()) {
            [weak self] error in
            if let error {
                Self.log.error("voip call report failed: \(error.localizedDescription)")
                // A hop rather than `assumeIsolated`: this completion's queue
                // is CallKit's to choose. Only `completion()` below is
                // timing-critical, and it does not wait for this.
                Task { @MainActor in
                    self?.registry.release(uuid: uuid)
                    self?.placeholderTimeout?.cancel()
                }
            }
            completion()
        }
        // The fetch is started AFTER the report, never before: nothing here may
        // delay the report, and this work continues perfectly well once the
        // handler has returned.
        onWake?(wake)
    }

    private func armPlaceholderTimeout(uuid: UUID) {
        placeholderTimeout?.cancel()
        placeholderTimeout = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.placeholderRingWindow * 1_000_000_000))
            guard !Task.isCancelled, let self else { return }
            // Still a placeholder means no offer ever arrived: the call was
            // cancelled before this device could fetch it, the wake was stray,
            // or the session could not be rebuilt. Either way it is over.
            guard self.registry.pendingPlaceholder == uuid else { return }
            self.registry.releasePlaceholder()
            self.provider.reportEnded(uuid: uuid, reason: .unanswered)
        }
    }
}

// MARK: - PKPushRegistryDelegate

extension VoipPushRegistrar: PKPushRegistryDelegate {

    /// Delegate callbacks arrive on the main queue (see ``start()``), so the
    /// main-actor state is reached with `assumeIsolated` rather than a hop.
    nonisolated func pushRegistry(
        _ registry: PKPushRegistry,
        didUpdate pushCredentials: PKPushCredentials,
        for type: PKPushType
    ) {
        guard type == .voIP else { return }
        MainActor.assumeIsolated {
            register(token: pushCredentials.token)
        }
    }

    nonisolated func pushRegistry(
        _ registry: PKPushRegistry,
        didInvalidatePushTokenFor type: PKPushType
    ) {
        guard type == .voIP else { return }
        MainActor.assumeIsolated {
            lastRegisteredToken = nil
            Self.log.warning("voip push token invalidated")
        }
    }

    nonisolated func pushRegistry(
        _ registry: PKPushRegistry,
        didReceiveIncomingPushWith payload: PKPushPayload,
        for type: PKPushType,
        completion: @escaping () -> Void
    ) {
        guard type == .voIP else {
            completion()
            return
        }
        MainActor.assumeIsolated {
            handleIncoming(payload: payload.dictionaryPayload, completion: completion)
        }
    }
}
