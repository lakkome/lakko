// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CallKit
import Foundation
import LakkoCore
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

    /// How long a placeholder call may ring without the offer arriving.
    ///
    /// The ring window of `docs/ux/calls.md` section 2.1, read from
    /// ``CallHostRouting/ringWindowMs`` — the one place the number lives — so a
    /// wake whose signaling row never lands stops ringing at the same moment
    /// the caller gives up. It used to be a second literal `45` here, which is
    /// a number this device and the CALLER'S have to agree on.
    private static var placeholderRingWindow: TimeInterval {
        TimeInterval(CallHostRouting.ringWindowMs) / 1000
    }

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
    ///
    /// ## Why a muted call is reported and then ended, rather than not reported
    ///
    /// The obvious implementation of "a muted chat never rings" — look the
    /// conversation up and return without reporting — is the one thing this
    /// path may not do. iOS terminates an app that takes a VoIP push without
    /// reporting a call, and after a few offences stops delivering VoIP pushes
    /// to it at all, which would break ringing for every conversation including
    /// the unmuted ones. Nor is there a report that does not ring: a
    /// `CXProviderConfiguration` is set up once for the whole provider, so
    /// there is no per-call silent variant to choose, and the system call
    /// screen takes over the phone regardless — which is itself one of the
    /// interruptions section 2.1 withholds.
    ///
    /// So the call is reported, exactly as the platform demands, and ended in
    /// the SAME completion handler before the push handler returns. That is
    /// Apple's own shape for a call the app decides not to present. The system
    /// gets its report; the person gets no call screen and no ringtone; and the
    /// call is still recorded, because the app's own receive path is running
    /// and ``CallHost`` files the history row when the offer lands.
    private func handleIncoming(payload: [AnyHashable: Any], completion: @escaping () -> Void) {
        let wake = VoipWakePayload.parse(payload)
        let silent = Self.wakeIsMuted(wake)
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
                // The report was refused. CallKit has no call under this UUID,
                // so neither may we — and nothing needs ending.
                Self.log.error("voip call report failed: \(error.localizedDescription)")
            }
            if error != nil || silent {
                // A hop rather than `assumeIsolated`: this completion runs on
                // CallKit's own queue, and everything inside touches
                // main-actor state. Only `completion()` below is
                // timing-critical, and it does not wait for this.
                Task { @MainActor in
                    guard let self else { return }
                    if error == nil {
                        // A muted conversation. The report has landed, so the
                        // platform rule is satisfied; take the call straight
                        // back down. `.unanswered` rather than
                        // `.declinedElsewhere` — nobody refused this call, it
                        // simply was not put in front of anyone here.
                        self.provider.reportEnded(uuid: uuid, reason: .unanswered)
                    }
                    self.registry.release(uuid: uuid)
                    self.placeholderTimeout?.cancel()
                }
            }
            completion()
        }
        // The fetch is started AFTER the report, never before: nothing here may
        // delay the report, and this work continues perfectly well once the
        // handler has returned. It runs for a muted conversation too — that is
        // what records the call.
        onWake?(wake)
    }

    /// Is the conversation this wake points at muted?
    ///
    /// `docs/ux/calls.md` section 2.1, through the one rule
    /// (`LakkoCore.ringDecision`). The age argument is `0` and that is not a
    /// placeholder: a VoIP wake carries an opaque routing id and a journal
    /// pointer and NO timestamp (invariants #2 and #5), so the only thing
    /// knowable at report time is the mute flag. Whether the offer behind it is
    /// already past the window is answered twice over by the code that does
    /// know — ``CallHostRouting/decide(row:nowMs:active:endedCallIds:otherCallBusy:muted:)``
    /// on the real row, and ``armPlaceholderTimeout(uuid:)`` if the row never
    /// arrives.
    ///
    /// Every failure to resolve the conversation answers "not muted", so the
    /// call rings: an unknown wake must ring rather than be swallowed.
    /// `defaults` is the App Group suite the mute mirror lives in, a parameter
    /// only so a test can supply its own. The mirror is per account: the push
    /// route names the account; a wake that misses the route (a raw group id)
    /// is read for `signedInAccountIdB64`, the account signed in here.
    static func wakeIsMuted(
        _ wake: VoipWakePayload,
        routes: PushRoutingStore = PushRoutingStore(),
        signedInAccountIdB64: String? = AcceptedContactsStore.boundAccountIdB64,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> Bool {
        guard let wakeHex = wake.groupHex else { return false }
        // ADR-0042 — `g` is an opaque `push_gid`; the reverse index turns it
        // back into the real group. A miss falls back to reading it as a raw
        // group id, which is what the pre-Sprint-3 wake carried.
        let route = routes.lookupRoute(pushGidHex: wakeHex)
        let groupIdHex = route?.groupIdHex ?? wakeHex
        let muted = ConversationMuteMirror.isMuted(
            accountIdB64: route?.accountIdB64 ?? signedInAccountIdB64,
            groupIdHex: groupIdHex, defaults: defaults)
        return LakkoCore.ringDecision(muted: muted, offerAgeMs: 0) == .silent
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
