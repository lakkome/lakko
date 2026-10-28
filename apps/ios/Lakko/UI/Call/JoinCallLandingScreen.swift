// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import SwiftUI

/// `/c/<slug>` multi-joiner call-invite landing (ADR-0032, G-21/G-22) — the iOS
/// mirror of `apps/web/src/routes/JoinCall.tsx`.
///
/// Whoever opens a `https://lakko.me/c/<slug>` call link lands here. The screen
/// decodes + verifies the `CallInviteCapability` (owner's Ed25519 signature),
/// then asks the app-level ``LinkCallHost`` for the call and shows the host's
/// state for it. The host owns the ``MultiJoinerCall`` — so the call survives
/// this screen being replaced — and runs the ADR-0032 owner-founds flow:
///
///   * If the visitor IS the owner (its identity signature key matches the
///     capability's owner key), it FOUNDS + hosts the call
///     (``MultiJoinerCall/host(options:ui:callbacks:)``).
///   * Otherwise it JOINS: mint a fresh KeyPackage, connect to the SFU room,
///     relay the KeyPackage to the acting committer, and wait for the Welcome
///     (``MultiJoinerCall/join(options:ui:callbacks:)``). Until the Welcome
///     lands the screen shows "waiting for the owner to admit you" (invariant #4
///     — no media before the group is joined). If no committer responds within
///     the admission window the call is dead.
///
/// ## Guest join (no login required, ADR-0032, docs/ux/call-links.md section 9)
///
/// A visitor with no session joins without an account of their own making: on
/// `Join` a guest account is created silently
/// (``AuthRepository/signupGuest(onCommitting:onAccountReady:)``, the audited
/// PoW-gated signup without a username) and the call starts as soon as its
/// session lands. No recovery phrase is shown first: a person who was sent a
/// link wants the call. The session landing replaces the anonymous root with
/// ``GuestRoot``, which re-mounts this landing; because the call slug stays
/// parked in ``InviteIntake`` with the auto-resume flag armed, the re-mounted
/// landing continues the join automatically.
///
/// Once the guest is not in the call (it ended, it is dead, the join failed,
/// the app was reopened) this screen asks whether to keep the account
/// (``GuestOfferQuestion``): `Create account` takes a username and a password
/// and then lands on "Protect your account" (ADR-0075 D2: the recovery phrase
/// is not shown); `No, thanks` deletes the account at once. An account left
/// unanswered is deleted by the servers after a day.
@MainActor
public final class JoinCallLandingViewModel: ObservableObject {

    /// What the landing shows. `waiting` is the ADR-0032 joiner-side
    /// pre-admission phase; `dead` is the no-live-committer timeout; `busy` is
    /// a refusal because another call holds the one call slot.
    public enum Phase: Equatable {
        case idle
        case starting
        case waiting
        case live
        case ended
        case dead
        case busy(UiText)
        /// Kept as ``UiText`` so a message on screen follows a live language
        /// switch (ADR-0061). A message from the call host is passed as is.
        case error(UiText)
    }

    /// Decoded + verified capability fields the flow needs.
    public struct Decoded: Equatable {
        let callId: Data
        let ownerAccountId: Data
        let ownerSignatureKey: Data
        let ownerFingerprint: String
        let mediaVideo: Bool
        let valid: Bool
        /// `true` when the local identity IS the capability owner. Always false
        /// for an anonymous visitor.
        let isOwner: Bool

        var callIdHex: String { callId.map { String(format: "%02x", $0) }.joined() }
    }

    /// What this screen itself is doing, before the call is the host's — the
    /// guest sign-up, a refusal, a failure before the host held the call.
    @Published private var localPhase: Phase = .idle
    @Published public private(set) var decoded: Decoded?
    @Published public private(set) var loading = true
    /// The ledger answered `429` on the link fetch: the link may be fine, so
    /// the screen says "try again later" rather than "invalid".
    @Published public private(set) var rateLimited = false
    /// ADR-0028 amendment 2026-09-28 — the link's fragment secret is missing or
    /// malformed, or the blob does not open under it.
    @Published public private(set) var damaged = false
    /// The ledger could not be asked for the link (the network, a server
    /// error): the link may be fine, the screen offers it again.
    @Published public private(set) var openFailed = false
    /// ADR-0048 / UX spec §4 — a guest (no session) is asked for a name here,
    /// prefilled with "<adjective> <animal>" from the shared rule
    /// (`lakko_ui_rules::guest_name`). It becomes the new account's own profile.
    @Published public var guestName: String = LakkoCore.guestName(random: UInt64.random(in: .min ... .max))
    /// ADR-0054 S4 — the guest account is being created (the one heavy proof
    /// of work): `Getting you into the call...` with the signup line below.
    @Published public private(set) var creatingGuest = false
    /// The guest sign-up in flight; cancelled on Back / Cancel until the
    /// account is being committed.
    private var guestTask: Task<Void, Error>?
    private var guestCommitting = false

    /// Creates the guest account. ``AuthRepository/signupGuest(onCommitting:onAccountReady:)``
    /// in the app; a test replaces it to drive the latches below.
    typealias GuestSignup = @MainActor (
        _ onCommitting: @escaping @MainActor () -> Void,
        _ onAccountReady: @escaping @MainActor (_ accountIdB64: String) -> Void
    ) async throws -> Void

    private let slug: String
    private let authRepository: AuthRepository
    private let guestSignup: GuestSignup
    private let intake: InviteIntake
    /// The call itself lives here, app-wide, so it survives this screen.
    let linkCallHost: LinkCallHost
    private var didDecode = false
    private let landingToken = UUID()
    private var hostSubscription: AnyCancellable?
    // Synchronous double-tap latches (set BEFORE any await), so a rapid second
    // tap can't mint a SECOND guest account or start a second call (Hardening 3).
    private var mintingGuest = false
    private var beginning = false

    init(
        slug: String,
        authRepository: AuthRepository,
        intake: InviteIntake,
        linkCallHost: LinkCallHost,
        guestSignup: GuestSignup? = nil
    ) {
        self.slug = slug
        self.authRepository = authRepository
        self.guestSignup = guestSignup ?? { [authRepository] onCommitting, onAccountReady in
            try await authRepository.signupGuest(
                onCommitting: onCommitting, onAccountReady: onAccountReady)
        }
        self.intake = intake
        self.linkCallHost = linkCallHost
        // The host's slot is what this screen shows once the call is started.
        hostSubscription = linkCallHost.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
    }

    /// The phase shown: the host's, once the host holds this link's call (or
    /// its outcome); this screen's own before that.
    public var phase: Phase {
        guard let decoded, let slot = linkCallHost.slot(forCallIdHex: decoded.callIdHex) else {
            return localPhase
        }
        switch slot.phase {
        case .starting: return .starting
        case .waiting: return .waiting
        case .live: return .live
        case .ended: return .ended
        case .dead: return .dead
        case .error:
            if let message = slot.errorMessage {
                // The failure, then its reason (batch 5 convention).
                return .error(UiText {
                    Localization.text(
                        "common.sentence_pair",
                        texts: ["first": Localization.text("call.link.error.generic"), "second": message.text])
                })
            }
            return .error(.key("call.link.error.generic"))
        }
    }

    // MARK: - Lifecycle

    /// Decode once, then auto-resume if a guest-signup round-trip parked us here.
    /// The `.task` may re-fire across body rebuilds; `didDecode` keeps decode to
    /// one run.
    public func onAppear() async {
        if !didDecode {
            didDecode = true
            await decode()
        }
        // Guest-join resume (web `pendingJoin` effect): once `signupGuest` has
        // landed a session and the guest root re-mounted this landing, continue
        // the parked join automatically. The short-circuit means the one-shot
        // flag is only consumed when we actually have a valid, live-session
        // decode.
        if let decoded, decoded.valid,
            authRepository.identitySecrets != nil,
            intake.consumeCallJoinAutoResume() {
            await beginJoin(decoded)
        }
    }

    /// The landing appeared / disappeared — while it shows the call's state, no
    /// status line for it is shown anywhere else.
    func setVisible(_ visible: Bool) {
        // Leaving the screen while the guest account is being worked on
        // cancels that work (`docs/ux/anti-abuse.md` section 1).
        if !visible { cancelGuestSignup() }
        linkCallHost.noteLanding(landingToken, visible: visible)
    }

    /// Tap Join: use the live session if there is one, else create a guest
    /// account first (username-less full account) and go on into the call.
    public func onStart(_ decoded: Decoded) async {
        if let mine = linkCallHost.slot(forCallIdHex: decoded.callIdHex),
           LinkCallHostRules.occupiesSlot(mine.phase) {
            return
        }
        // Refuse before minting a guest account for a call that could not start.
        if let busy = linkCallHost.busyMessage(verb: decoded.isOwner ? .start : .join) {
            localPhase = .busy(busy)
            return
        }
        if authRepository.identitySecrets != nil {
            await beginJoin(decoded)
            return
        }
        // No session — mint a guest account. Its session lands at once and the
        // guest root re-mounts this landing; the resume step in `onAppear`
        // continues the join there. The synchronous latch makes a rapid
        // double-tap idempotent — never two guest accounts.
        if mintingGuest { return }
        if guestNameError != nil {
            localPhase = .error(UiText { [weak self] in self?.guestNameError ?? "" })
            return
        }
        let nameSnapshot = guestName
        mintingGuest = true
        localPhase = .starting
        // Arm auto-resume BEFORE the await so the re-mounted landing continues.
        intake.armCallJoinAutoResume()
        creatingGuest = true
        guestCommitting = false
        let task = Task { @MainActor [guestSignup] in
            try await guestSignup(
                { self.guestCommitting = true },
                { accountIdB64 in
                    // The guest account exists and its session is about to
                    // land: keep the name as its own profile and make it the
                    // active one, so the call the re-mounted landing starts
                    // shows it on the own tile and announces it to the others.
                    do {
                        try ProfileDirectory.shared.setOwnDisplayName(
                            nameSnapshot, accountIdB64: accountIdB64)
                    } catch {
                        NSLog("ADR-0048 guest name not saved: \(error)")
                    }
                    ProfileDirectory.shared.activate(accountIdB64: accountIdB64, username: nil)
                })
        }
        guestTask = task
        defer {
            guestTask = nil
            creatingGuest = false
            // The latch covers one attempt, not the life of the screen: once
            // it is over a tap takes the signed-in branch above; and if that
            // guest account is later deleted while this screen is still up
            // (`No, thanks`), Join has to work again (found live on web
            // 2026-10-02: it did nothing).
            mintingGuest = false
        }
        do {
            try await task.value
            // The session landed; the guest root's landing takes over from
            // here. This one shows the Join button again if it outlives the
            // guest account.
            if localPhase == .starting { localPhase = .idle }
        } catch {
            // Nothing landed, so nothing is resumed. The slug stays parked:
            // the landing stays up for the retry.
            _ = intake.consumeCallJoinAutoResume()
            if task.isCancelled || error is CancellationError {
                // Back / Cancel while the account was being created.
                localPhase = .idle
                return
            }
            NSLog("join-call: guest account not created: \(error)")
            localPhase = .error(UiText {
                AuthErrorCopy.shared(error) ?? Localization.text("auth.signup.error.generic")
            })
        }
    }

    /// The inline reason under the guest name field, or `nil`.
    public var guestNameError: String? {
        do {
            _ = try checkDisplayName(guestName)
            return nil
        } catch {
            return (error as? LocalizedError)?.errorDescription
                ?? Localization.text("display_name.error.unknown")
        }
    }

    /// `true` when this device has no session, so joining creates a guest.
    public var joinsAsGuest: Bool { authRepository.identitySecrets == nil }

    /// A guest account was just deleted on this device (`No, thanks`); the
    /// landing says so above the Join button.
    public var guestAccountDeleted: Bool { authRepository.guestAccountDeleted }

    /// Leave this screen. A call still starting or waiting for admission keeps
    /// going (its status shows on every other screen); an outcome already shown
    /// here is forgotten.
    public func exit() {
        cancelGuestSignup()
        if let decoded, let slot = linkCallHost.slot(forCallIdHex: decoded.callIdHex),
           !LinkCallHostRules.occupiesSlot(slot.phase) {
            linkCallHost.dismissOutcome()
        }
        intake.clearCallJoin()
    }

    /// Cancel this link's call and leave the screen.
    public func cancelCall() {
        cancelGuestSignup()
        if let decoded, linkCallHost.slot(forCallIdHex: decoded.callIdHex) != nil {
            linkCallHost.leave()
        }
        intake.clearCallJoin()
    }

    /// Stop the guest sign-up while it is still only working (before the
    /// account is committed); nothing on the server is left behind.
    private func cancelGuestSignup() {
        guard let guestTask, !guestCommitting else { return }
        guestTask.cancel()
    }

    /// End the call that holds the slot, after a busy refusal, and offer this
    /// link again.
    public func endCurrentCall() {
        linkCallHost.endCurrentCall()
        localPhase = .idle
    }

    /// Back to the start button after a failure.
    public func tryAgain() {
        if let decoded, linkCallHost.slot(forCallIdHex: decoded.callIdHex) != nil {
            linkCallHost.dismissOutcome()
        }
        beginning = false
        localPhase = .idle
    }

    // MARK: - Decode

    /// Decode again after a rate-limit refusal or a failed fetch.
    public func retryDecode() async {
        guard rateLimited || openFailed else { return }
        rateLimited = false
        openFailed = false
        loading = true
        await decode()
    }

    private func decode() async {
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        // `slug` is the link token `slug#fragment`.
        let parts = InviteLinkSecret.split(slug)
        guard let linkSecret = InviteLinkSecret.read(parts.fragment) else {
            decoded = nil
            damaged = true
            loading = false
            return
        }
        do {
            let parsed = try CallInviteUrl.parseCallInviteSlug(parts.base)
            let capabilityBytes: Data
            switch parsed {
            case .pointer(let blobId):
                let fetched: Data?
                do {
                    fetched = try await DiscoveryClient().fetchCallInvite(blobId: blobId)
                } catch {
                    if Cancellation.endedByCancellation(error) {
                        didDecode = false
                        return
                    }
                    // A failure to ask the ledger says nothing about the link:
                    // a 429 is "try again later", anything else is offered
                    // again too, never called invalid.
                    NSLog("join-call: the call link could not be fetched: %@", String(describing: error))
                    decoded = nil
                    if DiscoveryClientError.isRateLimited(error) {
                        rateLimited = true
                    } else {
                        openFailed = true
                    }
                    loading = false
                    return
                }
                guard let fetched else {
                    decoded = nil
                    loading = false
                    return
                }
                capabilityBytes = fetched
            }
            let cap: CallInviteCapability
            do {
                cap = try CallInviteCapability.openLink(sealed: capabilityBytes, linkSecret: linkSecret)
            } catch {
                decoded = nil
                damaged = true
                loading = false
                return
            }
            // ADR-0078 — a capability whose link grant does not verify is
            // damaged here too, as the acceptor refuses it.
            guard CallInviteAcceptor.linkGrantVerifies(cap) else {
                decoded = nil
                damaged = true
                loading = false
                return
            }
            var isOwner = false
            if let seed = authRepository.identitySecrets?.accountIdentitySeed {
                let mine = try Identity.fromSeed(seed: seed).signaturePublicKey()
                isOwner = hexEncode(mine) == hexEncode(cap.ownerSignaturePubkey())
            }
            decoded = Decoded(
                callId: cap.callId(),
                ownerAccountId: cap.ownerAccountId(),
                ownerSignatureKey: cap.ownerSignaturePubkey(),
                ownerFingerprint: cap.ownerFingerprint(),
                mediaVideo: cap.media().video,
                valid: cap.isValidNow(nowMs: nowMs),
                isOwner: isOwner
            )
            loading = false
        } catch {
            // The screen was left while the link was being fetched. Nothing
            // failed: keep the loading state and let the next appearance
            // fetch it again, rather than show a link that "does not work".
            if Cancellation.endedByCancellation(error) {
                didDecode = false
                return
            }
            NSLog("join-call: decode/verify failed: %@", String(describing: error))
            decoded = nil
            rateLimited = DiscoveryClientError.isRateLimited(error)
            loading = false
        }
    }

    // MARK: - Begin

    /// Hand the ADR-0032 host / join to the app-level ``LinkCallHost`` now that
    /// a live session exists.
    private func beginJoin(_ decoded: Decoded) async {
        // Synchronous latch (Hardening 3): a rapid double-tap must not reach the
        // host twice before its slot is written. The host is idempotent for the
        // same call too (`alreadyInThisCall`).
        if beginning { return }
        beginning = true
        localPhase = .starting
        // The call belongs to the app-level host, not to this screen: a call
        // still starting keeps going when the person leaves (``exit()``). The
        // auto-resume reaches here from the screen's `.task`, which is
        // cancelled on leaving, and a cancelled start would be filed on the
        // host's slot as a call that failed. So the start is not this task's
        // to cancel.
        let host = linkCallHost
        let request = LinkCallHost.Request(
            slug: slug,
            callId: decoded.callId,
            ownerAccountId: decoded.ownerAccountId,
            ownerSignatureKey: decoded.ownerSignatureKey,
            mediaVideo: decoded.mediaVideo,
            isOwner: decoded.isOwner
        )
        let outcome = await Cancellation.shielded { await host.start(request) }
        beginning = false
        switch outcome {
        case .started, .alreadyInThisCall:
            // The host's slot is what this screen shows from here on.
            localPhase = .idle
        case .busy(let message):
            localPhase = .busy(message)
        case .failed(let message):
            localPhase = .error(message)
        }
    }
}

/// The landing view for a tapped `/c/<slug>` multi-joiner call-invite link.
///
/// It never shows the live call itself: that is the root's full-screen call
/// (`CallHostOverlay`), on this screen and every other one.
public struct JoinCallLandingScreen: View {
    @StateObject private var viewModel: JoinCallLandingViewModel
    @ObservedObject private var l10n = Localization.shared
    private let onDone: () -> Void
    /// The guest's one question (`docs/ux/call-links.md` section 5), when the
    /// session is a guest account: drawn whenever the guest is not in the
    /// call, in place of `Close` (this landing is the guest's screen; there is
    /// no app behind it to close to). `nil` for everyone else.
    private let guestOffer: GuestAccountOfferModel?

    public init(viewModel: JoinCallLandingViewModel, onDone: @escaping () -> Void) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
        self.guestOffer = nil
    }

    init(
        viewModel: JoinCallLandingViewModel,
        guestOffer: GuestAccountOfferModel?,
        onDone: @escaping () -> Void
    ) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
        self.guestOffer = guestOffer
    }

    public var body: some View {
        panel
            .task { await viewModel.onAppear() }
            .onAppear { viewModel.setVisible(true) }
            .onDisappear { viewModel.setVisible(false) }
            // A call that ends while this screen is up sends a signed-in person
            // back into the app (`docs/ux/call-links.md` section 5). A guest
            // stays: this screen is where they are asked about an account.
            .onChange(of: viewModel.phase) { _, phase in
                if phase == .ended && guestOffer == nil {
                    viewModel.exit()
                    onDone()
                }
            }
    }

    @ViewBuilder
    private var panel: some View {
        VStack(spacing: 16) {
            Text(l10n.t(viewModel.decoded?.isOwner == true ? "call.link.title_start" : "call.link.title_join"))
                .font(.title2)
                .bold()

            if viewModel.loading {
                ProgressView(l10n.t("call.link.loading"))
            } else if viewModel.rateLimited {
                Text(inviteRateLimitedMessage)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                Button(l10n.t("common.try_again")) {
                    Task { await viewModel.retryDecode() }
                }
                .buttonStyle(.bordered)
                closeButton
            } else if viewModel.damaged {
                Text(inviteLinkDamagedMessage)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                closeButton
            } else if viewModel.openFailed {
                Text(inviteLinkOpenFailedMessage)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                Button(l10n.t("common.try_again")) {
                    Task { await viewModel.retryDecode() }
                }
                .buttonStyle(.bordered)
                closeButton
            } else if viewModel.decoded == nil || viewModel.decoded?.valid == false {
                Text(l10n.t("call.link.invalid"))
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                closeButton
            } else if let decoded = viewModel.decoded {
                if viewModel.guestAccountDeleted {
                    Text(l10n.t("guest.offer.deleted"))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                if let host = hostName(decoded) {
                    // ADR-0084 — the host's photo beside their name.
                    InitialsAvatar(
                        name: host, identity: base64Encode(decoded.ownerSignatureKey), size: 56,
                        placeholderSymbol: "person.fill", photo: .person(decoded.ownerSignatureKey))
                    Text(l10n.t("call.link.host", texts: ["name": host]))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                content(decoded)
            }
        }
        .padding()
    }

    /// The host by name, when this device knows one — a contact or a
    /// received profile. Never a fingerprint: a string of digits names nobody,
    /// and the owner's own page has nothing to say about its host.
    private func hostName(_ decoded: JoinCallLandingViewModel.Decoded) -> String? {
        if decoded.isOwner { return nil }
        let resolved = ProfileDirectory.shared.resolve(signatureKey: decoded.ownerSignatureKey)
        return resolved.source == .placeholder ? nil : resolved.name
    }

    @ViewBuilder
    private func content(_ decoded: JoinCallLandingViewModel.Decoded) -> some View {
        switch viewModel.phase {
        case .idle:
            if viewModel.joinsAsGuest {
                // ADR-0048 — the name the people in this call's chats see.
                VStack(alignment: .leading, spacing: 4) {
                    TextField(l10n.t("settings.profile.name.label"), text: $viewModel.guestName)
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.words)
                    if let error = viewModel.guestNameError {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            Button(l10n.t(decoded.isOwner ? "call.link.start" : "call.link.join")) {
                Task { await viewModel.onStart(decoded) }
            }
            .buttonStyle(.borderedProminent)
            .disabled(viewModel.joinsAsGuest && viewModel.guestNameError != nil)
            // No login required: a visitor with no session joins as a guest,
            // and is told so before pressing.
            if viewModel.joinsAsGuest && !decoded.isOwner {
                Text(l10n.t("call.guest.note"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            afterCall
        case .starting:
            if viewModel.creatingGuest {
                // `docs/ux/anti-abuse.md` section 1 — the guest account's one
                // heavy piece of work.
                ProgressView(l10n.t("call.guest.working.title"))
                Text(SignupViewModel.workingNote)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            } else {
                ProgressView(l10n.t("call.link.connecting"))
            }
            runningButtons
        case .waiting:
            ProgressView(l10n.t("call.link.waiting"))
            Text(l10n.t("call.link.waiting_hint"))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            runningButtons
        case .live:
            Text(l10n.t("call.link.live"))
            Button(l10n.t("common.back")) {
                viewModel.exit()
                onDone()
            }
        case .dead:
            Text(l10n.t("call.link.dead"))
                .multilineTextAlignment(.center)
            afterCall
        case .ended:
            Text(l10n.t("call.link.ended"))
                .multilineTextAlignment(.center)
            afterCall
        case .busy(let message):
            Text(message.text)
                .multilineTextAlignment(.center)
            Button(l10n.t("call.link.end_current")) { viewModel.endCurrentCall() }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoDanger)
            closeButton
        case .error(let message):
            Text(message.text)
                .foregroundStyle(Color.lakkoDanger)
                .multilineTextAlignment(.center)
            Button(l10n.t("common.try_again")) { viewModel.tryAgain() }
                .buttonStyle(.bordered)
            afterCall
        }
    }

    /// What follows when the visitor is not in the call: a guest is asked
    /// whether to keep the account; anyone else gets the way out of the
    /// landing.
    @ViewBuilder
    private var afterCall: some View {
        if let guestOffer {
            GuestOfferQuestion(model: guestOffer)
                .padding(.top, 8)
        } else {
            closeButton
        }
    }

    /// A call starting or waiting for admission: leave the screen with the call
    /// still going, or cancel it.
    @ViewBuilder
    private var runningButtons: some View {
        HStack(spacing: 12) {
            Button(l10n.t("common.back")) {
                viewModel.exit()
                onDone()
            }
            .buttonStyle(.bordered)
            Button(l10n.t("common.cancel"), role: .destructive) {
                viewModel.cancelCall()
                onDone()
            }
            .buttonStyle(.bordered)
        }
    }

    private var closeButton: some View {
        Button(l10n.t("common.close")) {
            viewModel.exit()
            onDone()
        }
    }
}
