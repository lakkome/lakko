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
/// ## Guest join (no login required, ADR-0032 UX)
///
/// An anonymous visitor (no session) joins WITHOUT logging in: on tapping Join
/// we auto-create a FULL, username-less account via ``AuthRepository/signupGuest()``
/// (the audited PoW-gated signup). That flips auth to `.saveFreshPhrase`, so
/// `AppRoot` renders the existing `SaveFreshPhraseScreen`; once the user
/// acknowledges, `confirmFreshPhraseSaved` lands the session and — because the
/// call slug stays parked in ``InviteIntake`` with the auto-resume flag armed —
/// the re-mounted landing continues the join automatically.
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
        case busy(String)
        case error(String)
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
    /// ADR-0048 / UX spec §4 — a guest (no session) is asked for a name here,
    /// prefilled `Guest`. It becomes the new account's own profile.
    @Published public var guestName: String = "Guest"

    private let slug: String
    private let authRepository: AuthRepository
    private let intake: InviteIntake
    /// The call itself lives here, app-wide, so it survives this screen.
    let linkCallHost: LinkCallHost
    private var didDecode = false
    private var hostSubscription: AnyCancellable?
    // Synchronous double-tap latches (set BEFORE any await), so a rapid second
    // tap can't mint a SECOND guest account or start a second call (Hardening 3).
    private var mintingGuest = false
    private var beginning = false

    init(slug: String, authRepository: AuthRepository, intake: InviteIntake, linkCallHost: LinkCallHost) {
        self.slug = slug
        self.authRepository = authRepository
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
        case .error: return .error(slot.errorMessage ?? "The call failed.")
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
        // Guest-join resume (web `pendingJoin` effect): once `signupGuest` +
        // the SaveFreshPhrase acknowledgement land a session, continue the parked
        // join automatically. The short-circuit means the one-shot flag is only
        // consumed when we actually have a valid, live-session decode.
        if let decoded, decoded.valid,
            authRepository.identitySecrets != nil,
            intake.consumeCallJoinAutoResume() {
            await beginJoin(decoded)
        }
    }

    /// The landing appeared / disappeared — while it shows the call's state, no
    /// status line for it is shown anywhere else.
    func setVisible(_ visible: Bool) {
        linkCallHost.noteLanding(visible: visible)
    }

    /// Tap Join: use the live session if there is one, else create a guest
    /// account first (username-less full account) and resume after the phrase is
    /// saved.
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
        // No session — mint a guest account. This flips auth to `.saveFreshPhrase`
        // (AppRoot renders SaveFreshPhraseScreen); the resume effect above
        // continues the join once the session lands. The synchronous latch makes
        // a rapid double-tap idempotent — never two guest accounts.
        if mintingGuest { return }
        if let nameError = guestNameError {
            localPhase = .error(nameError)
            return
        }
        let nameSnapshot = guestName
        mintingGuest = true
        localPhase = .starting
        // Arm auto-resume BEFORE the await so the re-mounted landing continues.
        intake.armCallJoinAutoResume()
        do {
            _ = try await authRepository.signupGuest()
            // The guest account exists from here (its session lands once the
            // phrase is acknowledged), so its id is known: keep the name as its
            // own profile, published into every group it later joins.
            if case .saveFreshPhrase(let account) = authRepository.state {
                do {
                    try ProfileDirectory.shared.setOwnDisplayName(
                        nameSnapshot, accountIdB64: account.accountIdB64)
                } catch {
                    NSLog("ADR-0048 guest name not saved: \(error)")
                }
            }
        } catch {
            mintingGuest = false // allow a retry after a failed mint
            intake.clearCallJoin() // clears the auto-resume flag too
            // Re-park the slug so the landing stays reachable for the retry.
            localPhase = .error(error.localizedDescription)
        }
    }

    /// The inline reason under the guest name field, or `nil`.
    public var guestNameError: String? {
        do {
            _ = try checkDisplayName(guestName)
            return nil
        } catch {
            return (error as? LocalizedError)?.errorDescription ?? "That name cannot be used."
        }
    }

    /// `true` when this device has no session, so joining creates a guest.
    public var joinsAsGuest: Bool { authRepository.identitySecrets == nil }

    /// Leave this screen. A call still starting or waiting for admission keeps
    /// going (its status shows on every other screen); an outcome already shown
    /// here is forgotten.
    public func exit() {
        if let decoded, let slot = linkCallHost.slot(forCallIdHex: decoded.callIdHex),
           !LinkCallHostRules.occupiesSlot(slot.phase) {
            linkCallHost.dismissOutcome()
        }
        intake.clearCallJoin()
    }

    /// Cancel this link's call and leave the screen.
    public func cancelCall() {
        if let decoded, linkCallHost.slot(forCallIdHex: decoded.callIdHex) != nil {
            linkCallHost.leave()
        }
        intake.clearCallJoin()
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

    private func decode() async {
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        do {
            let parsed = try CallInviteUrl.parseCallInviteSlug(slug)
            let capabilityBytes: Data
            switch parsed {
            case .pointer(let blobId):
                guard let fetched = try await DiscoveryClient().fetchCallInvite(blobId: blobId) else {
                    decoded = nil
                    loading = false
                    return
                }
                capabilityBytes = fetched
            case .inline(let bytes):
                capabilityBytes = bytes
            }
            let cap = try CallInviteCapability.decodeAndVerify(wire: capabilityBytes)
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
            NSLog("join-call: decode/verify failed: %@", String(describing: error))
            decoded = nil
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
        let outcome = await linkCallHost.start(
            LinkCallHost.Request(
                slug: slug,
                callId: decoded.callId,
                ownerAccountId: decoded.ownerAccountId,
                ownerSignatureKey: decoded.ownerSignatureKey,
                mediaVideo: decoded.mediaVideo,
                isOwner: decoded.isOwner
            )
        )
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
    private let onDone: () -> Void

    public init(viewModel: JoinCallLandingViewModel, onDone: @escaping () -> Void) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
    }

    public var body: some View {
        panel
            .task { await viewModel.onAppear() }
            .onAppear { viewModel.setVisible(true) }
            .onDisappear { viewModel.setVisible(false) }
    }

    @ViewBuilder
    private var panel: some View {
        VStack(spacing: 16) {
            Text("Join call")
                .font(.title2)
                .bold()

            if viewModel.loading {
                ProgressView("Loading call link…")
            } else if viewModel.decoded == nil || viewModel.decoded?.valid == false {
                Text("This call link is invalid or has expired.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                closeButton
            } else if let decoded = viewModel.decoded {
                Text("Host: \(decoded.ownerFingerprint)")
                    .font(.footnote.monospaced())
                    .foregroundStyle(.secondary)
                content(decoded)
            }
        }
        .padding()
    }

    @ViewBuilder
    private func content(_ decoded: JoinCallLandingViewModel.Decoded) -> some View {
        switch viewModel.phase {
        case .idle:
            if viewModel.joinsAsGuest {
                // ADR-0048 — the name the people in this call's chats see.
                VStack(alignment: .leading, spacing: 4) {
                    TextField("Your name", text: $viewModel.guestName)
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.words)
                    if let error = viewModel.guestNameError {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(.red)
                    }
                }
            }
            Button(decoded.isOwner ? "Start call" : "Join call") {
                Task { await viewModel.onStart(decoded) }
            }
            .buttonStyle(.borderedProminent)
            .disabled(viewModel.joinsAsGuest && viewModel.guestNameError != nil)
            // No login required: a joiner with no session gets a full,
            // username-less guest account (signupGuest) on tap.
            closeButton
        case .starting:
            ProgressView("Connecting…")
            runningButtons
        case .waiting:
            ProgressView("Waiting for the owner to admit you…")
            Text("You can leave this screen; you will join when you are admitted.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            runningButtons
        case .live:
            Text("You are in this call.")
            Button("Back") {
                viewModel.exit()
                onDone()
            }
        case .dead:
            Text("This call has ended or isn't active.")
                .multilineTextAlignment(.center)
            closeButton
        case .ended:
            Text("The call has ended.")
                .multilineTextAlignment(.center)
            closeButton
        case .busy(let message):
            Text(message)
                .multilineTextAlignment(.center)
            Button("End current call") { viewModel.endCurrentCall() }
                .buttonStyle(.borderedProminent)
                .tint(.red)
            closeButton
        case .error(let message):
            Text("Failed: \(message)")
                .foregroundStyle(.red)
                .multilineTextAlignment(.center)
            Button("Try again") { viewModel.tryAgain() }
                .buttonStyle(.bordered)
            closeButton
        }
    }

    /// A call starting or waiting for admission: leave the screen with the call
    /// still going, or cancel it.
    @ViewBuilder
    private var runningButtons: some View {
        HStack(spacing: 12) {
            Button("Back") {
                viewModel.exit()
                onDone()
            }
            .buttonStyle(.bordered)
            Button("Cancel", role: .destructive) {
                viewModel.cancelCall()
                onDone()
            }
            .buttonStyle(.bordered)
        }
    }

    private var closeButton: some View {
        Button("Close") {
            viewModel.exit()
            onDone()
        }
    }
}
