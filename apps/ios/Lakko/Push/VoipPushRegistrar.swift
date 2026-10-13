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
    ///
    /// Handed the resolved target — the BOUND account and the real group id —
    /// and only for a wake that passed the account gate
    /// (``decide(_:binding:routes:defaults:)``). A refused wake fetches nothing.
    var onWake: ((VoipWakeTarget) -> Void)?

    /// The `(device pseudonym, account)` binding (ADR-0042, amended
    /// 2026-09-30) every wake is checked against.
    private let bindingStore: PushAccountBindingStore

    private let provider: CallKitProvider
    private let registry: CallKitCallRegistry
    private let tokens: VoipTokenRegistration
    private var pushRegistry: PKPushRegistry?
    private var placeholderTimeout: Task<Void, Never>?

    /// `registry` is `nil`-defaulted rather than `= .shared` because a default
    /// argument is evaluated in a NON-isolated context, and the registry is
    /// main-actor isolated (a warning today, an error under Swift 6).
    init(
        provider: CallKitProvider = .shared,
        registry: CallKitCallRegistry? = nil,
        gateway: VoipPushGateway = .live(),
        bindingStore: PushAccountBindingStore = .live
    ) {
        self.provider = provider
        self.registry = registry ?? .shared
        self.bindingStore = bindingStore
        self.tokens = VoipTokenRegistration(gateway: gateway, bindingStore: bindingStore)
        super.init()
    }

    /// Begin receiving VoIP pushes. Idempotent — the registry is created once
    /// and iOS delivers the current token to the delegate once per launch.
    ///
    /// Called at launch rather than at sign-in: a VoIP token is device state,
    /// not account state, and the app has to be able to receive a push before
    /// it knows whether anyone will send one. The REGISTRATION with the gateway
    /// is what is account-scoped (``VoipTokenRegistration``): it is made only
    /// for a pseudonym bound to a signed-in account, at launch for a restored
    /// session and at each sign-in
    /// (``registerForSignIn(devicePseudonym:)``).
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

    /// Forget the VoIP registration made under `devicePseudonym` (logout /
    /// forget device). See ``VoipTokenRegistration/unregister(devicePseudonym:)``.
    func unregister(devicePseudonym: Data) async {
        await tokens.unregister(devicePseudonym: devicePseudonym)
    }

    /// Register the VoIP token under the pseudonym a sign-in has just been
    /// bound under (``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``).
    ///
    /// PushKit delivers the token once per launch, not once per sign-in, so
    /// without this a sign-in in a running app (the first one after install,
    /// or one after Log out) had no VoIP registration until the next launch.
    func registerForSignIn(devicePseudonym: Data) async {
        if let token = pushRegistry?.pushToken(for: .voIP) { tokens.noteToken(token) }
        await tokens.registerForSignIn(devicePseudonym: devicePseudonym)
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
    ///
    /// ## A wake for no one here is reported and ended the same way
    ///
    /// ADR-0042 (amended 2026-09-30): a wake is acted on only for the account
    /// this device's push pseudonym is bound to, and only when the wake's `d`
    /// is that pseudonym. Apple's rule does not bend for a wake that is not
    /// ours, so it too is reported — as the nameless placeholder — and ended in
    /// the same completion handler. Unlike a muted call nothing is fetched: the
    /// wake belongs to no account signed in here, so there is nothing to
    /// record and no name to show.
    private func handleIncoming(payload: [AnyHashable: Any], completion: @escaping () -> Void) {
        let wake = VoipWakePayload.parse(payload)
        let decision = Self.decide(wake, binding: bindingStore.current())
        let endAtOnce: Bool
        switch decision {
        case .refuse:
            endAtOnce = true
            Self.log.info("voip wake not for the signed-in account: ending the placeholder")
        case .silent:
            endAtOnce = true
        case .ring:
            endAtOnce = false
        }
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
            if error != nil || endAtOnce {
                // A hop rather than `assumeIsolated`: this completion runs on
                // CallKit's own queue, and everything inside touches
                // main-actor state. Only `completion()` below is
                // timing-critical, and it does not wait for this.
                Task { @MainActor in
                    guard let self else { return }
                    if error == nil {
                        // A muted conversation, or a wake for no account
                        // signed in here. The report has landed, so the
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
        // what records the call — and never for a refused wake.
        switch decision {
        case .ring(let target?), .silent(let target):
            onWake?(target)
        case .ring(nil), .refuse:
            break
        }
    }

    /// What to do with a VoIP wake, decided from the wake alone at report time.
    ///
    /// First the account gate (ADR-0042, amended 2026-09-30): the wake's `d`
    /// must equal the device pseudonym bound to the signed-in account
    /// (``PushAccountBinding/boundAccount(wakeDevicePseudonymHex:binding:)``),
    /// else ``VoipWakeDecision/refuse``. Every later read is for that bound
    /// account and no other: the reverse index is looked up for
    /// `(bound account, push_gid)`, a row another account wrote is a miss, and
    /// the mute flag is the bound account's.
    ///
    /// Then the mute rule, `docs/ux/calls.md` section 2.1, through the one rule
    /// (`LakkoCore.ringDecision`). The age argument is `0` and that is not a
    /// placeholder: a VoIP wake carries an opaque routing id and a journal
    /// pointer and NO timestamp (invariants #2 and #5), so the only thing
    /// knowable at report time is the mute flag. Whether the offer behind it is
    /// already past the window is answered twice over by the code that does
    /// know — ``CallHostRouting/decide(row:nowMs:active:endedCallIds:otherCallBusy:muted:)``
    /// on the real row, and ``armPlaceholderTimeout(uuid:)`` if the row never
    /// arrives.
    ///
    /// For a wake that passed the gate, every failure to resolve the
    /// conversation answers "not muted", so the call rings: a wake for the
    /// signed-in account must ring rather than be swallowed. A route miss
    /// falls back to reading `g` as a raw group id (the pre-ADR-0042 wake),
    /// still for the bound account. `defaults` is the App Group suite the mute
    /// mirror lives in, a parameter only so a test can supply its own.
    static func decide(
        _ wake: VoipWakePayload,
        binding: PushAccountBinding?,
        routes: PushRoutingStore = PushRoutingStore(),
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> VoipWakeDecision {
        guard let accountIdB64 = PushAccountBinding.boundAccount(
            wakeDevicePseudonymHex: wake.devicePseudonymHex, binding: binding) else {
            return .refuse
        }
        guard let wakeHex = wake.groupHex?.lowercased() else { return .ring(nil) }
        let groupIdHex = routes.lookupRoute(accountIdB64: accountIdB64, pushGidHex: wakeHex)
            ?? wakeHex
        let target = VoipWakeTarget(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let muted = ConversationMuteMirror.isMuted(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, defaults: defaults)
        return LakkoCore.ringDecision(muted: muted, offerAgeMs: 0) == .silent
            ? .silent(target)
            : .ring(target)
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

// MARK: - Token registration

/// The two `services/push-gw` calls the registration makes, as closures so a
/// test can hold one open and watch the order they arrive in.
struct VoipPushGateway {
    var register: @MainActor (_ devicePseudonym: Data, _ token: Data) async throws -> Void
    var unregister: @MainActor (_ devicePseudonym: Data) async throws -> Void

    static func live(client: PushClient = PushClient()) -> VoipPushGateway {
        VoipPushGateway(
            register: { pseudonym, token in
                try await client.register(
                    devicePseudonym: pseudonym,
                    provider: PushTokenKind.voip.provider,
                    tokenBytes: token
                )
            },
            unregister: { pseudonym in
                _ = try await client.unregister(
                    devicePseudonym: pseudonym, provider: PushTokenKind.voip.provider)
            }
        )
    }
}

/// The VoIP token's registration with `services/push-gw`, kept apart from
/// PushKit and CallKit so its ordering can be tested.
///
/// The token is device state and outlives a sign-out; the registration is
/// made under the device pseudonym, which is minted per sign-in and cleared
/// at Log out (``DevicePushId``). Three rules keep a sign-out and the sign-in
/// after it from undoing each other:
///
///   * Every call names its pseudonym explicitly, read by the caller before
///     it waited on anything. An unregister that is still in flight when the
///     next account signs in names the OLD pseudonym, so it cannot take the
///     new registration off the gateway.
///   * A token is registered only under the pseudonym the push binding names
///     (``VoipPushRegistration/isBound(devicePseudonym:binding:)``), asked
///     again when the call's turn comes. Log out clears that binding first,
///     so from then on nothing is registered for the session that ended.
///   * Gateway calls run one after another in the order they were asked for,
///     so a register and the unregister behind it (same pseudonym) reach the
///     gateway in that order.
@MainActor
final class VoipTokenRegistration {

    private static let log = Logger(subsystem: "me.lakko", category: "voip-push")

    private let gateway: VoipPushGateway
    private let bindingStore: PushAccountBindingStore
    private let devicePseudonym: @MainActor () -> Data?

    /// The newest VoIP token PushKit delivered.
    private var currentToken: Data?

    /// What the gateway holds as far as this process knows: the token, and
    /// the pseudonym it is registered under. A new sign-in mints a new
    /// pseudonym, so the same token is registered again for it.
    private var lastRegistered: (devicePseudonym: Data, token: Data)?

    /// The newest gateway call; each one runs after the one before it.
    private var chain: Task<Void, Never>?

    init(
        gateway: VoipPushGateway,
        bindingStore: PushAccountBindingStore,
        devicePseudonym: @escaping @MainActor () -> Data? = { (try? DevicePushId.peek()) ?? nil }
    ) {
        self.gateway = gateway
        self.bindingStore = bindingStore
        self.devicePseudonym = devicePseudonym
    }

    /// Remember a token without registering it.
    func noteToken(_ token: Data) {
        guard !token.isEmpty else { return }
        currentToken = token
    }

    /// PushKit delivered a token (at launch, or because it changed). It is
    /// registered now when an account is signed in; otherwise it is held for
    /// ``registerForSignIn(devicePseudonym:)``.
    @discardableResult
    func tokenDelivered(_ token: Data) -> Task<Void, Never>? {
        noteToken(token)
        guard let pseudonym = boundPseudonym() else {
            Self.log.info("voip token held: no signed-in account")
            return nil
        }
        return Task { @MainActor [weak self] in
            await self?.register(token: token, devicePseudonym: pseudonym)
        }
    }

    /// PushKit invalidated the token: nothing is held and nothing is known
    /// to be registered.
    func tokenInvalidated() {
        currentToken = nil
        lastRegistered = nil
    }

    /// Register the held token under the pseudonym a sign-in was just bound
    /// under. Nothing to do when PushKit has not delivered a token yet:
    /// ``tokenDelivered(_:)`` registers it when it arrives.
    func registerForSignIn(devicePseudonym pseudonym: Data) async {
        guard let token = currentToken else {
            Self.log.info("voip token not delivered yet")
            return
        }
        await register(token: token, devicePseudonym: pseudonym)
    }

    /// Forget the registration made under `pseudonym`. Best-effort: a
    /// gateway that keeps the row only sends pushes this device will refuse
    /// to turn into a call.
    func unregister(devicePseudonym pseudonym: Data) async {
        if lastRegistered?.devicePseudonym == pseudonym { lastRegistered = nil }
        await enqueue { [gateway] in
            do {
                try await gateway.unregister(pseudonym)
            } catch {
                Self.log.warning("voip unregister failed: \(error.localizedDescription)")
            }
        }
    }

    /// The device pseudonym, when it is the one bound to a signed-in account.
    private func boundPseudonym() -> Data? {
        guard let pseudonym = devicePseudonym() else { return nil }
        return VoipPushRegistration.isBound(
            devicePseudonym: pseudonym, binding: bindingStore.current()) ? pseudonym : nil
    }

    private func register(token: Data, devicePseudonym pseudonym: Data) async {
        await enqueue { [weak self] in
            guard let self else { return }
            // Asked here, after every gateway call queued before this one:
            // the sign-in this was started for may have ended.
            guard self.boundPseudonym() == pseudonym else { return }
            let last = self.lastRegistered
            guard PushTokenRegistration.shouldRegister(
                token: token,
                lastRegistered: last?.devicePseudonym == pseudonym ? last?.token : nil
            ) else {
                return
            }
            do {
                try await self.gateway.register(pseudonym, token)
                // A sign-out during the round trip queued its unregister
                // behind this call; the row is not remembered as held.
                guard self.boundPseudonym() == pseudonym else { return }
                self.lastRegistered = (pseudonym, token)
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

    /// Run `operation` after every gateway call asked for before it.
    private func enqueue(_ operation: @escaping @MainActor () async -> Void) async {
        let previous = chain
        let task = Task { @MainActor in
            await previous?.value
            await operation()
        }
        chain = task
        await task.value
    }
}

/// When a VoIP token may be registered under a device pseudonym.
enum VoipPushRegistration {
    /// `true` when `devicePseudonym` is the one the push binding names, that
    /// is, the pseudonym of a sign-in that has not ended.
    static func isBound(devicePseudonym: Data, binding: PushAccountBinding?) -> Bool {
        guard let binding, !binding.accountIdB64.isEmpty,
              let bound = PushAccountBinding.normalizedPseudonymHex(binding.devicePseudonymHex)
        else {
            return false
        }
        return bound == PushAccountBinding.hex(devicePseudonym)
    }
}

// MARK: - Wake decision

/// Where a VoIP wake points, once it is known to be for the signed-in account.
struct VoipWakeTarget: Equatable {
    /// The account the wake is bound to (``PushAccountBinding``).
    let accountIdB64: String
    /// The real group id (reverse-mapped from the wake's `push_gid`, or the
    /// raw `g` of a pre-ADR-0042 wake).
    let groupIdHex: String
}

/// What ``VoipPushRegistrar`` does with a VoIP wake. Every case reports a call
/// to CallKit first — Apple's rule — and differs in what follows.
enum VoipWakeDecision: Equatable {
    /// Not for the signed-in account: no binding, no `d`, or a `d` that is not
    /// the bound pseudonym. The nameless placeholder is ended at once and
    /// nothing is fetched.
    case refuse
    /// Ring, and fetch the offer. `nil` when the wake names no conversation.
    case ring(VoipWakeTarget?)
    /// The conversation is muted: ended at once, but still fetched, so the
    /// call is recorded.
    case silent(VoipWakeTarget)
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
            _ = tokens.tokenDelivered(pushCredentials.token)
        }
    }

    nonisolated func pushRegistry(
        _ registry: PKPushRegistry,
        didInvalidatePushTokenFor type: PKPushType
    ) {
        guard type == .voIP else { return }
        MainActor.assumeIsolated {
            tokens.tokenInvalidated()
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
