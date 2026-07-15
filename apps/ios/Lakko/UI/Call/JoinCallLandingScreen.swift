// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `/c/<slug>` multi-joiner call-invite landing (ADR-0032, G-21/G-22) — the iOS
/// mirror of `apps/web/src/routes/JoinCall.tsx`.
///
/// Whoever opens a `https://lakko.me/c/<slug>` call link lands here. The screen
/// decodes + verifies the `CallInviteCapability` (owner's Ed25519 signature),
/// then drives the ADR-0032 owner-founds flow via ``MultiJoinerCall``:
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

    /// The landing screen's own state machine. `waiting` is the ADR-0032
    /// joiner-side pre-admission phase; `dead` is the no-live-committer timeout.
    public enum Phase: Equatable {
        case idle
        case starting
        case waiting
        case live
        case ended
        case dead
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
    }

    @Published public private(set) var phase: Phase = .idle
    @Published public private(set) var decoded: Decoded?
    @Published public private(set) var loading = true

    /// The shared in-call state the orchestrator populates + `CallScreen` reads.
    let ui = CallUiState()

    private let slug: String
    private let authRepository: AuthRepository
    private let intake: InviteIntake
    private var call: MultiJoinerCall?
    private var didDecode = false
    // Synchronous double-tap latches (set BEFORE any await), so a rapid second
    // tap can't mint a SECOND guest account or start a second call (Hardening 3).
    private var mintingGuest = false
    private var beginning = false

    public init(slug: String, authRepository: AuthRepository, intake: InviteIntake) {
        self.slug = slug
        self.authRepository = authRepository
        self.intake = intake
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

    /// Tap Join: use the live session if there is one, else create a guest
    /// account first (username-less full account) and resume after the phrase is
    /// saved.
    public func onStart(_ decoded: Decoded) async {
        if authRepository.identitySecrets != nil {
            await beginJoin(decoded)
            return
        }
        // No session — mint a guest account. This flips auth to `.saveFreshPhrase`
        // (AppRoot renders SaveFreshPhraseScreen); the resume effect above
        // continues the join once the session lands. The synchronous latch makes
        // a rapid double-tap idempotent — never two guest accounts.
        if mintingGuest { return }
        mintingGuest = true
        phase = .starting
        // Arm auto-resume BEFORE the await so the re-mounted landing continues.
        intake.armCallJoinAutoResume()
        do {
            _ = try await authRepository.signupGuest()
        } catch {
            mintingGuest = false // allow a retry after a failed mint
            intake.clearCallJoin() // clears the auto-resume flag too
            // Re-park the slug so the landing stays reachable for the retry.
            phase = .error(error.localizedDescription)
        }
    }

    /// Dismiss / exit the landing — tear down the call + drop the parked slug so
    /// AppRoot returns to the prior destination.
    public func exit() {
        call?.hangUp()
        call = nil
        intake.clearCallJoin()
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

    /// Run the actual ADR-0032 host/join now that a live session exists.
    private func beginJoin(_ decoded: Decoded) async {
        // Synchronous latch (Hardening 3): guard against a rapid double-tap
        // starting two SFU sessions before `call` is assigned past the first
        // `await`.
        if call != nil || beginning { return }
        beginning = true
        guard let secrets = authRepository.identitySecrets else {
            beginning = false
            phase = .error("Re-authenticate to join this call (no live session).")
            return
        }
        guard let base = SfuConfig.baseUrl else {
            beginning = false
            phase = .error("This client has no SFU endpoint — set LakkoSfuUrl and rebuild.")
            return
        }
        let identity: Identity
        do {
            identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        } catch {
            beginning = false
            phase = .error("Could not rebuild the identity for this call.")
            return
        }
        phase = .starting

        let options = MultiJoinerCall.Options(
            identity: identity,
            callId: decoded.callId,
            ownerAccountId: decoded.ownerAccountId,
            ownerSignatureKey: decoded.ownerSignatureKey,
            sfuBaseUrl: base,
            sessionToken: secrets.sessionToken,
            audio: true,
            video: decoded.mediaVideo,
            resolveDisplayName: nil,
            authBaseUrl: nil
        )
        let callbacks = MultiJoinerCall.Callbacks(
            onJoined: { [weak self] in self?.phase = .live },
            onLive: nil,
            onEnded: { [weak self] in self?.phase = .ended },
            onNoCommitter: { [weak self] in
                self?.call = nil
                self?.phase = .dead
            },
            onError: { [weak self] error in
                self?.phase = .error(error.localizedDescription)
            }
        )

        do {
            if decoded.isOwner {
                // Owner founds + hosts; `onJoined` fires synchronously inside host().
                call = try await MultiJoinerCall.host(options: options, ui: ui, callbacks: callbacks)
            } else {
                // Joiner connects signaling-only + relays its KeyPackage; sit in
                // "waiting" until the Welcome arrives (onJoined → "live").
                call = try await MultiJoinerCall.join(options: options, ui: ui, callbacks: callbacks)
                if phase == .starting { phase = .waiting }
            }
        } catch {
            beginning = false // start failed (call unset) — allow a retry
            phase = .error(error.localizedDescription)
        }
    }
}

/// The landing view for a tapped `/c/<slug>` multi-joiner call-invite link.
public struct JoinCallLandingScreen: View {
    @StateObject private var viewModel: JoinCallLandingViewModel
    private let onDone: () -> Void

    public init(viewModel: JoinCallLandingViewModel, onDone: @escaping () -> Void) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
    }

    public var body: some View {
        Group {
            if viewModel.phase == .live {
                // Live call: present the shared in-call UI, driven by the same
                // CallUiState the orchestrator populates.
                CallScreen(
                    externalState: viewModel.ui,
                    onExit: {
                        viewModel.exit()
                        onDone()
                    }
                )
            } else {
                panel
            }
        }
        .task { await viewModel.onAppear() }
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
            Button(decoded.isOwner ? "Start call" : "Join call") {
                Task { await viewModel.onStart(decoded) }
            }
            .buttonStyle(.borderedProminent)
            // No login required: a joiner with no session gets a full,
            // username-less guest account (signupGuest) on tap.
        case .starting:
            ProgressView("Connecting…")
        case .waiting:
            ProgressView("Waiting for the owner to admit you…")
        case .dead:
            Text("This call has ended or isn't active.")
                .multilineTextAlignment(.center)
            closeButton
        case .ended:
            Text("The call has ended.")
                .multilineTextAlignment(.center)
            closeButton
        case .error(let message):
            Text("Failed: \(message)")
                .foregroundStyle(.red)
                .multilineTextAlignment(.center)
            closeButton
        case .live:
            EmptyView() // handled above
        }
    }

    private var closeButton: some View {
        Button("Close") {
            viewModel.exit()
            onDone()
        }
    }
}
