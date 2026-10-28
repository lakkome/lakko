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
/// The VoIP token is sealed into every group subscription beside the alert
/// token, under its own provider (``PushTokenKind/voip``): they are different
/// tokens for different push types (ADR-0078 section 3, ``PushDeviceTokens``).
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

    /// The bound account (ADR-0042, amended 2026-09-30, with ADR-0078) every
    /// wake is checked against.
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
        tokens: VoipTokenRegistration? = nil,
        bindingStore: PushAccountBindingStore = .live
    ) {
        self.provider = provider
        self.registry = registry ?? .shared
        self.bindingStore = bindingStore
        self.tokens = tokens ?? VoipTokenRegistration()
        super.init()
    }

    /// Begin receiving VoIP pushes. Idempotent — the registry is created once
    /// and iOS delivers the current token to the delegate once per launch.
    ///
    /// Called at launch rather than at sign-in: a VoIP token is device state,
    /// not account state, and the app has to be able to receive a push before
    /// it knows whether anyone will send one. What is account-scoped is the
    /// subscriptions the token is sealed into (``PushDeviceTokens``).
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

    /// Store the token PushKit already holds, without waiting for its delegate
    /// callback (``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``
    /// runs this just before it sends the subscriptions).
    ///
    /// PushKit delivers the token once per launch, not once per sign-in, so
    /// without this a sign-in in a running app (the first one after install,
    /// or one after Log out) sealed no VoIP token until the next launch.
    func noteCurrentToken() {
        if let token = pushRegistry?.pushToken(for: .voIP) { tokens.noteToken(token) }
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
    /// ADR-0042 (amended 2026-09-30, with ADR-0078): a wake is acted on only
    /// for the bound account, and only when the wake's `s` is a subscription
    /// that account holds for the wake's `g`. Apple's rule does not bend for a wake that is not
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
    /// First the account gate (ADR-0042, amended 2026-09-30, with ADR-0078):
    /// a binding must exist, the wake must carry a well-formed `s`
    /// (``PushAccountBinding/boundAccount(wakeSubscriptionIdHex:binding:)``),
    /// and the bound account's route row for the wake's `g` must hold that
    /// same subscription id, else ``VoipWakeDecision/refuse``. Every later read
    /// is for that bound account and no other, and the mute flag is the bound
    /// account's.
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
    /// A wake that passed the gate resolved its conversation, so the mute flag
    /// can be read; a wake with no `g` cannot pass it. `defaults` is the App
    /// Group suite the mute mirror lives in, a parameter only so a test can
    /// supply its own.
    static func decide(
        _ wake: VoipWakePayload,
        binding: PushAccountBinding?,
        routes: PushRoutingStore = PushRoutingStore(),
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> VoipWakeDecision {
        guard let accountIdB64 = PushAccountBinding.boundAccount(
            wakeSubscriptionIdHex: wake.subscriptionIdHex, binding: binding),
              let wakeHex = wake.groupHex?.lowercased(),
              let groupIdHex = routes.lookupRoute(
                accountIdB64: accountIdB64,
                pushGidHex: wakeHex,
                subscriptionIdHex: wake.subscriptionIdHex) else {
            return .refuse
        }
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

// MARK: - Token storage

/// The VoIP token's half of ``PushDeviceTokens``, kept apart from PushKit and
/// CallKit so it can be tested.
///
/// The token is device state and outlives a sign-out. Since ADR-0078 there is
/// no registration of its own: the token is sealed into every group
/// subscription of the signed-in account, so a token that CHANGED while an
/// account is bound sends every subscription again (``onChange``). Log out
/// clears the binding first, so from then on a delivered token is only
/// stored.
@MainActor
final class VoipTokenRegistration {

    private static let log = Logger(subsystem: "me.lakko", category: "voip-push")

    private let store: PushDeviceTokens
    private let bindingStore: PushAccountBindingStore

    /// Sends every subscription of the bound account again. Defaults to
    /// ``PushSubscriber/tokensChanged()``.
    private let onChange: @MainActor () async -> Void

    init(
        store: PushDeviceTokens = .shared,
        bindingStore: PushAccountBindingStore = .live,
        onChange: (@MainActor () async -> Void)? = nil
    ) {
        self.store = store
        self.bindingStore = bindingStore
        self.onChange = onChange ?? { await PushSubscriber.shared.tokensChanged() }
    }

    /// Store a token without sending anything (a sign-in sends next).
    func noteToken(_ token: Data) {
        guard !token.isEmpty else { return }
        store.setVoip(token)
    }

    /// PushKit delivered a token (at launch, or because it changed). Stored;
    /// when it changed and an account is bound, every subscription is sent
    /// again. Returns the task doing that, or `nil`.
    @discardableResult
    func tokenDelivered(_ token: Data) -> Task<Void, Never>? {
        guard !token.isEmpty, store.setVoip(token) else { return nil }
        guard bindingStore.current() != nil else {
            Self.log.info("voip token held: no signed-in account")
            return nil
        }
        let onChange = onChange
        return Task { @MainActor in await onChange() }
    }

    /// PushKit invalidated the token: it is dropped, and the subscriptions
    /// of a bound account are sent again without it.
    @discardableResult
    func tokenInvalidated() -> Task<Void, Never>? {
        guard store.setVoip(nil), bindingStore.current() != nil else { return nil }
        let onChange = onChange
        return Task { @MainActor in await onChange() }
    }
}

// MARK: - Wake decision

/// Where a VoIP wake points, once it is known to be for the signed-in account.
struct VoipWakeTarget: Equatable {
    /// The account the wake is bound to (``PushAccountBinding``).
    let accountIdB64: String
    /// The real group id (reverse-mapped from the wake's `push_gid`).
    let groupIdHex: String
}

/// What ``VoipPushRegistrar`` does with a VoIP wake. Every case reports a call
/// to CallKit first — Apple's rule — and differs in what follows.
enum VoipWakeDecision: Equatable {
    /// Not for the signed-in account: no binding, no `s`, or an `s` the bound
    /// account does not hold for the wake's `g`. The nameless placeholder is
    /// ended at once and nothing is fetched.
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
            _ = tokens.tokenInvalidated()
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
