// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import UIKit

/// A running call-link call as ``LinkCallHost`` drives it. ``MultiJoinerCall``
/// in production; a fake in tests.
@MainActor
protocol LinkCallHandle: AnyObject {
    /// Tear the call down. Idempotent.
    func hangUp()
    /// Gate the local microphone track. Touches no UI state.
    func setMicrophoneTrackEnabled(_ on: Bool)
    func setCameraEnabled(_ on: Bool)
    func publishVideo() async throws
    var sessionState: SfuCallSession.State { get }
    /// Tell every member the call is over (`docs/ux/calls.md` section 4.1.2).
    /// Acts only for the link's owner; best-effort.
    func endForEveryone()
}

extension MultiJoinerCall: LinkCallHandle {}

/// The app-level owner of the ADR-0032 call-link call — the link slot beside
/// ``CallHost`` (1x1) and ``GroupCallHost``.
///
/// Created once per process and presented from the signed-in root, so the call
/// outlives `JoinCallLandingScreen` (which only decodes the link, runs the guest
/// sign-up, asks this host for the call and shows its state). See
/// ``LinkCallHostRules`` for the defects this closes and for every decision.
@MainActor
final class LinkCallHost: ObservableObject {

    /// What starting the call needs.
    struct Request: Equatable {
        /// The `/c/` slug this call was opened from, or `nil` for a call this
        /// device started itself (`docs/ux/call-links.md` section 2): `New
        /// call` founds a call before any link exists, and one is minted later
        /// only if somebody asks for it.
        let slug: String?
        let callId: Data
        let ownerAccountId: Data
        let ownerSignatureKey: Data
        let mediaVideo: Bool
        let isOwner: Bool
    }

    /// Everything the host needs from outside, injectable for tests.
    struct Ports {
        /// The live session's identity seed + token, `nil` without a session.
        var session: @MainActor () -> (identitySeed: Data, sessionToken: String)?
        var sfuBaseUrl: @MainActor () -> String?
        var microphoneGranted: @MainActor () async -> Bool
        var cameraGranted: @MainActor (_ video: Bool) async -> Bool
        /// Found (owner) or join (joiner) the call. Callbacks may fire before
        /// this returns (the owner reports `onJoined` from inside `host`).
        var startCall: @MainActor (
            _ request: Request,
            _ session: (identitySeed: Data, sessionToken: String),
            _ sfuBaseUrl: String,
            _ video: Bool,
            _ ui: CallUiState,
            _ callbacks: MultiJoinerCall.Callbacks
        ) async throws -> LinkCallHandle
        /// This account as the owner of a call it founds: the 16-byte account
        /// id and the Ed25519 signature key. `nil` without a session.
        var ownIdentity: @MainActor () -> (accountId: Data, signatureKey: Data)? = { nil }
        /// Make the shareable link for a call this device founded
        /// (`docs/ux/call-links.md` section 3). Not called until somebody
        /// presses `Invite`, because it costs a round trip and an anti-spam
        /// solve that starting a call must not pay.
        var mintInviteLink: @MainActor (_ callId: Data) async throws -> String = { _ in
            throw LinkCallHostError.signedOut
        }
        /// Put the call's link on the clipboard (`Copy link`). `nil` where
        /// there is no clipboard to reach; the link is then shown instead.
        var copyToClipboard: ((String) throws -> Void)? = nil
        /// The session is a guest account: its `Participants` menu offers the
        /// link alone, never `Invite contacts`.
        var isGuest: @MainActor () -> Bool = { false }
        /// Sleep for `ms`; `false` when the sleep was cancelled.
        var sleepMs: @MainActor (_ ms: Int64) async -> Bool = { ms in
            do {
                try await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
                return true
            } catch {
                return false
            }
        }
    }

    /// The link call in the slot.
    struct Slot: Equatable {
        let callIdHex: String
        /// `nil` for a call started here rather than opened from a link.
        let slug: String?
        let isOwner: Bool
        var phase: LinkCallHostRules.Phase
        /// Kept as a message so an error on screen follows a language switch.
        var errorMessage: UiText?
    }

    enum StartOutcome: Equatable {
        case started
        case alreadyInThisCall
        /// The messages are kept as ``UiText`` so a landing showing one follows
        /// a language switch (ADR-0061).
        case busy(message: UiText)
        case failed(message: UiText)

        /// Two outcomes are equal when they would show the same words now: a
        /// message is compared by its text, not by its identity.
        static func == (lhs: StartOutcome, rhs: StartOutcome) -> Bool {
            switch (lhs, rhs) {
            case (.started, .started), (.alreadyInThisCall, .alreadyInThisCall): return true
            case let (.busy(a), .busy(b)), let (.failed(a), .failed(b)): return a.text == b.text
            default: return false
            }
        }
    }

    // MARK: - Published state

    /// The call in the link slot — including an outcome (ended / dead / error)
    /// kept for the user until dismissed.
    @Published private(set) var slot: Slot?
    /// The in-call state the root presents full-screen once the call is live,
    /// kept until the call screen is dismissed.
    @Published private(set) var presentation: CallUiState?
    @Published private(set) var presentationId: String?
    /// The link slug of the presented call, so dismissing the screen can take
    /// the user back from that link's landing.
    private(set) var presentationSlug: String?
    /// A landing screen is on screen (it shows the call itself, so no notice).
    @Published private(set) var landingShowing: Bool = false
    /// The link to the call in the slot, for the call screen's `Invite`.
    ///
    /// Only ever set for a call this device FOUNDED. A capability names its
    /// signer as the call's owner and ADR-0032 elects the committer from that
    /// owner, so a link minted by a joiner would send arrivals to a call
    /// nobody is founding — the offer is therefore absent rather than failing
    /// later, and the control is absent with it.
    @Published private(set) var invite: CallInviteOffer?
    /// "{name} ended the call for everyone", for a few seconds after the owner
    /// of the link call this device was in ended it
    /// (`docs/ux/calls.md` section 4.1.2). Never an alert.
    @Published private(set) var endedForEveryoneNotice: UiText?

    /// The contacts an invitation can be sent to (`docs/ux/call-links.md`
    /// section 3). Empty for someone with no contacts; `Invite contacts` is
    /// offered all the same and its list says so. Wired once at app start
    /// (``wireInvitations(chat:contacts:)``).
    var inviteTargets: @MainActor () -> [InviteContactRow] = { [] }
    /// Send a plain text message to a contact (``InviteContactRow/id``): into
    /// the one-to-one with them, founded first when there is none.
    var sendInvitation: @MainActor (_ contactId: String, _ text: String) async throws -> Void = { _, _ in
        throw CallInviteOfferError.noSendPath
    }

    /// The call occupying the app's one call slot, across all three kinds.
    var slotOccupant: @MainActor () -> CallKind? = { nil }
    /// End whatever call occupies the slot (the busy refusal's affordance).
    var endCurrentCall: @MainActor () -> Void = {}

    // MARK: - Internals

    private let ports: Ports
    private var handle: LinkCallHandle?
    private var ui: CallUiState?
    /// The attempt the slot belongs to. A report from an older attempt — one
    /// the user already left — never touches the slot.
    private var attempt: UUID?
    private var controls: Set<AnyCancellable> = []
    /// The landings currently on screen. A set of tokens rather than a count,
    /// so a repeated `onAppear` cannot leave the notice hidden for good.
    private var visibleLandings: Set<UUID> = []
    private var endedNoticeTask: Task<Void, Never>?
    /// The session of a call its owner ended for everyone, out of the slot
    /// but kept up for ``EndForEveryone/hangUpDelayMs`` so the sealed end
    /// leaves the socket before the hang-up closes it.
    private var lingering: LinkCallHandle?
    private var lingerTask: Task<Void, Never>?

    init(ports: Ports) {
        self.ports = ports
    }

    /// The link call is starting, waiting for admission or live.
    var occupiesSlot: Bool {
        slot.map { LinkCallHostRules.occupiesSlot($0.phase) } ?? false
    }

    /// The copy a refusal would show right now, or `nil` when a call may start.
    func busyMessage(verb: CallSlot.Verb) -> UiText? {
        slotOccupant().map { CallSlot.busyText($0, verb: verb) }
    }

    /// The slot, when it belongs to `callIdHex`.
    func slot(forCallIdHex callIdHex: String) -> Slot? {
        slot?.callIdHex == callIdHex ? slot : nil
    }

    /// The landing identified by `token` appeared / disappeared.
    func noteLanding(_ token: UUID, visible: Bool) {
        if visible {
            visibleLandings.insert(token)
        } else {
            visibleLandings.remove(token)
        }
        let showing = !visibleLandings.isEmpty
        if showing != landingShowing { landingShowing = showing }
    }

    /// What to show about the call away from its landing.
    var notice: LinkCallHostRules.Notice? {
        LinkCallHostRules.notice(
            phase: slot?.phase,
            landingShowing: landingShowing,
            errorMessage: slot?.errorMessage
        )
    }

    // MARK: - Start

    /// `New call` (`docs/ux/call-links.md` section 2): found a group video
    /// call here and now, and land in it. Nothing is asked first — no media
    /// choice, no participants, no name, and no link.
    func startNewCall() async -> StartOutcome {
        guard let own = ports.ownIdentity() else {
            return .failed(message: .key("call.error.signed_out_start"))
        }
        return await start(Request(
            slug: nil,
            callId: CallInviteValues.freshCallId(),
            ownerAccountId: own.accountId,
            ownerSignatureKey: own.signatureKey,
            // One kind of call. A voice call is this with the camera off,
            // which the in-call controls already do.
            mediaVideo: CallInviteValues.video,
            isOwner: true
        ))
    }

    func start(_ request: Request) async -> StartOutcome {
        let callIdHex = request.callId.map { String(format: "%02x", $0) }.joined()
        // No suspension between this decision and the slot write below, so a
        // double tap finds the slot taken (`alreadyInThisCall`) rather than
        // starting twice.
        let decision = LinkCallHostRules.decideStart(
            occupant: slotOccupant() ?? (occupiesSlot ? .link : nil),
            activeLinkCallIdHex: occupiesSlot ? slot?.callIdHex : nil,
            requestedCallIdHex: callIdHex,
            verb: request.isOwner ? .start : .join
        )
        switch decision {
        case .alreadyInThisCall:
            return .alreadyInThisCall
        case .busy(let message):
            return .busy(message: message)
        case .start:
            break
        }
        // A call ended for everyone a moment ago may still hold its session:
        // close it before a new one exists.
        closeLingeringSession()
        guard let session = ports.session() else {
            return .failed(message: .key("common.error.session_ended"))
        }
        guard let base = ports.sfuBaseUrl() else {
            NSLog("link call: SFU URL not configured (LakkoSfuUrl); refusing")
            return .failed(message: .key("call.error.unavailable"))
        }

        let attempt = UUID()
        self.attempt = attempt
        controls.removeAll()
        handle = nil
        // The screen of a call that is over (its "Call ended" overlay, not
        // dismissed yet) belongs to that call. Kept, it would be shown with
        // this call's `Invite`, and this call's own screen would never be
        // presented.
        presentation = nil
        presentationId = nil
        presentationSlug = nil
        // A new call starts with no link: the first `Invite` makes one, and
        // every later one in the same call re-offers it.
        invite = request.isOwner ? CallInviteOffer(
            mint: { [weak self] in
                guard let self else { throw LinkCallHostError.identityUnavailable }
                return try await self.ports.mintInviteLink(request.callId)
            },
            copy: ports.copyToClipboard,
            contacts: { [weak self] in self?.inviteTargets() ?? [] },
            send: { [weak self] contactId, text in
                guard let self else { throw CallInviteOfferError.noSendPath }
                try await self.sendInvitation(contactId, text)
            },
            isGuest: ports.isGuest
        ) : nil
        let ui = CallUiState()
        self.ui = ui
        slot = Slot(
            callIdHex: callIdHex,
            slug: request.slug,
            isOwner: request.isOwner,
            phase: .starting,
            errorMessage: nil
        )

        // Settle the microphone permission before the SFU session exists, so a
        // denial is an explained refusal instead of a call nobody can hear.
        guard await ports.microphoneGranted() else {
            apply(attempt, .error, errorMessage: MicrophonePermission.deniedText)
            return .failed(message: MicrophonePermission.deniedText)
        }
        // A video call link starts the camera at connect. The link's media is
        // the creator's choice, not this user's, so a denied camera joins with
        // audio only and the call log says why.
        let cameraAllowed = await ports.cameraGranted(request.mediaVideo)
        guard self.attempt == attempt, occupiesSlot else { return .started }
        let joinWithVideo = request.mediaVideo && cameraAllowed

        let callbacks = MultiJoinerCall.Callbacks(
            onJoined: { [weak self] in self?.apply(attempt, .joined) },
            onLive: nil,
            onEnded: { [weak self] in self?.apply(attempt, .ended) },
            onNoCommitter: { [weak self] in self?.apply(attempt, .noCommitter) },
            onError: { [weak self] error in
                NSLog("multi-joiner call: failed: %@", String(describing: error))
                self?.apply(attempt, .error, errorMessage: CallErrorText.reasonText(for: error))
            },
            onEndedForEveryone: { [weak self] peerId in
                self?.endedForEveryone(attempt, peerId: peerId)
            }
        )
        let started: LinkCallHandle
        do {
            started = try await ports.startCall(request, session, base, joinWithVideo, ui, callbacks)
        } catch {
            NSLog("multi-joiner call: start failed: %@", String(describing: error))
            // Left or replaced while the start was in flight: whatever it
            // threw afterwards is not this person's failed call.
            guard self.attempt == attempt else { return .started }
            let reason = CallErrorText.reasonText(for: error)
            apply(attempt, .error, errorMessage: reason)
            return .failed(message: reason ?? .key("call.link.error.generic"))
        }
        guard self.attempt == attempt, occupiesSlot else {
            // Left, replaced, or already over while the start was in flight.
            started.hangUp()
            return .started
        }
        handle = started
        bindControls(ui: ui, handle: started)
        apply(attempt, .signalingOpen(owner: request.isOwner))
        if request.mediaVideo && !cameraAllowed {
            ui.showNotice(CameraPermission.deniedText)
        }
        return .started
    }

    /// Fold one orchestrator report into the slot.
    private func apply(_ attempt: UUID, _ event: LinkCallHostRules.Event, errorMessage: UiText? = nil) {
        guard self.attempt == attempt, var current = slot else { return }
        let phase = LinkCallHostRules.reduce(current.phase, event)
        guard phase != current.phase else { return }
        current.phase = phase
        if phase == .error { current.errorMessage = errorMessage ?? current.errorMessage }
        slot = current
        if phase == .live, presentation == nil, let ui {
            presentation = ui
            presentationId = current.callIdHex
            presentationSlug = current.slug
        }
        if !LinkCallHostRules.occupiesSlot(phase) {
            // Over on its own: drop the session and keep the outcome for the
            // user to see.
            controls.removeAll()
            let h = handle
            handle = nil
            h?.hangUp()
        }
    }

    // MARK: - Ending for everyone (`docs/ux/calls.md` section 4.1.2)

    /// This device made the link of the call it is in, so `End` offers to end
    /// the call for everyone.
    var startedCall: Bool { occupiesSlot && slot?.isOwner == true }

    /// End the link call for every person in it. Only the link's owner. If
    /// the end could not be sent the owner has still left, and nothing is
    /// shown.
    ///
    /// The slot is released at once, as after `End`, and nothing more of this
    /// person is sent; the session itself is hung up
    /// ``EndForEveryone/hangUpDelayMs`` later, because the end rides the
    /// socket the hang-up closes. A link call started inside that window
    /// closes the old session first (``start(_:)``).
    func endForEveryone() {
        guard let handle, startedCall else { return }
        handle.endForEveryone()
        // The person has left: the microphone and the camera stop now, not
        // when the session closes.
        handle.setMicrophoneTrackEnabled(false)
        handle.setCameraEnabled(false)
        attempt = nil
        controls.removeAll()
        self.handle = nil
        slot = nil
        invite = nil
        ui?.endCall()
        closeLingeringSession()
        lingering = handle
        let sleepMs = ports.sleepMs
        lingerTask = Task { @MainActor [weak self] in
            guard await sleepMs(EndForEveryone.hangUpDelayMs), !Task.isCancelled else { return }
            guard let self else {
                handle.hangUp()
                return
            }
            if self.lingering === handle { self.closeLingeringSession() }
        }
    }

    /// Hang up the session ``endForEveryone()`` left up, if any. Idempotent.
    private func closeLingeringSession() {
        lingerTask?.cancel()
        lingerTask = nil
        let h = lingering
        lingering = nil
        h?.hangUp()
    }

    /// The link's owner ended the call for everyone: say who, then end it
    /// here the way a call that ended on its own ends. The orchestrator has
    /// already checked the sender against the owner key of the signed
    /// capability; `peerId` only names the tile the notice reads its name off.
    private func endedForEveryone(_ attempt: UUID, peerId: String) {
        guard self.attempt == attempt, occupiesSlot else { return }
        let name = ui?.participants
            .first { $0.id == SfuCallUi.participantId(forPeer: peerId) }?.displayName
        showEndedForEveryone(starterName: name)
        ui?.endCall()
        apply(attempt, .ended)
    }

    private func showEndedForEveryone(starterName: String?) {
        endedForEveryoneNotice = EndForEveryone.endedText(starterName: starterName)
        endedNoticeTask?.cancel()
        endedNoticeTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(EndForEveryone.noticeMs) * 1_000_000)
            guard !Task.isCancelled else { return }
            self?.endedForEveryoneNotice = nil
            self?.endedNoticeTask = nil
        }
    }

    // MARK: - Leaving

    /// Leave the link call: no outcome to show, the user chose it.
    func leave() {
        attempt = nil
        controls.removeAll()
        let h = handle
        handle = nil
        slot = nil
        invite = nil
        h?.hangUp()
    }

    /// Forget an outcome (ended / dead / error) the user has seen.
    func dismissOutcome() {
        guard let slot, !LinkCallHostRules.occupiesSlot(slot.phase) else { return }
        self.slot = nil
    }

    /// The call screen was dismissed. Leaves the call if it is somehow still
    /// up, and forgets an outcome the screen already showed.
    func dismissPresentation() {
        if occupiesSlot { leave() } else { dismissOutcome() }
        invite = nil
        presentation = nil
        presentationId = nil
        presentationSlug = nil
    }

    /// Logout / account switch.
    func reset() {
        leave()
        closeLingeringSession()
        endedNoticeTask?.cancel()
        endedNoticeTask = nil
        endedForEveryoneNotice = nil
        invite = nil
        presentation = nil
        presentationId = nil
        presentationSlug = nil
        ui = nil
    }

    // MARK: - Controls

    /// Mic, camera and the screen's `ended` phase. The controls act only once
    /// the call is live: a joiner still waiting for its Welcome has published
    /// nothing (invariant #4) and must not start announcing camera state to a
    /// room it has not been admitted to.
    private func bindControls(ui: CallUiState, handle: LinkCallHandle) {
        controls.removeAll()
        ui.$participants
            .compactMap { $0.first(where: { $0.isLocal })?.micOn }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle] micOn in
                MainActor.assumeIsolated {
                    guard let self, let handle, self.isLive(handle) else { return }
                    handle.setMicrophoneTrackEnabled(micOn)
                }
            }
            .store(in: &controls)
        ui.$participants
            .compactMap { $0.first(where: { $0.isLocal })?.camOn }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle] camOn in
                MainActor.assumeIsolated {
                    guard let self, let handle, self.isLive(handle) else { return }
                    self.applyCamera(camOn, handle: handle, ui: ui)
                }
            }
            .store(in: &controls)
        ui.$phase
            .receive(on: DispatchQueue.main)
            .sink { [weak self, weak handle] phase in
                MainActor.assumeIsolated {
                    guard case .ended = phase,
                          let self, let handle, self.handle === handle else { return }
                    // A session that is still up means the user is ending it; one
                    // already closed ended on its own, and `onEnded` kept that
                    // outcome for the user to see.
                    if handle.sessionState != .closed && handle.sessionState != .failed {
                        self.leave()
                    }
                }
            }
            .store(in: &controls)
    }

    private func isLive(_ handle: LinkCallHandle) -> Bool {
        self.handle === handle && slot?.phase == .live
    }

    private func applyCamera(_ camOn: Bool, handle: LinkCallHandle, ui: CallUiState) {
        guard camOn else {
            handle.setCameraEnabled(false)
            return
        }
        Task { @MainActor [weak self] in
            // The in-call control already settled the permission; this is the
            // layered half.
            guard await self?.ports.cameraGranted(true) ?? false else {
                ui.setCamera(false)
                ui.showNotice(CameraPermission.deniedText)
                return
            }
            do {
                try await handle.publishVideo()
            } catch {
                // Fail-safe (invariant #4): revert the toggle and say why; the
                // call stays up audio-only.
                ui.setCamera(false)
                ui.showNotice(CallErrorText.cameraFailureText(error))
                NSLog("multi-joiner call: publishVideo failed: %@", String(describing: error))
            }
        }
    }
}

// MARK: - Production ports

extension LinkCallHost {
    static func live(authRepository: AuthRepository) -> LinkCallHost {
        LinkCallHost(ports: Ports(
            session: { [weak authRepository] in
                guard let secrets = authRepository?.identitySecrets else { return nil }
                return (secrets.accountIdentitySeed, secrets.sessionToken)
            },
            sfuBaseUrl: { SfuConfig.baseUrl },
            microphoneGranted: { await MicrophonePermission.ensureGranted() },
            cameraGranted: { video in await CameraPermission.ensureGranted(forVideo: video) },
            startCall: { request, session, base, video, ui, callbacks in
                let identity: Identity
                do {
                    identity = try Identity.fromSeed(seed: session.identitySeed)
                } catch {
                    throw LinkCallHostError.identityUnavailable
                }
                let options = MultiJoinerCall.Options(
                    identity: identity,
                    callId: request.callId,
                    ownerAccountId: request.ownerAccountId,
                    ownerSignatureKey: request.ownerSignatureKey,
                    sfuBaseUrl: base,
                    audio: true,
                    video: video,
                    // Tiles are named by the one resolver: a member's verified
                    // peer announcement names the person (ADR-0078 section 2),
                    // and a member's sealed profile is filed like any received
                    // profile.
                    naming: .live(directory: .shared),
                    authBaseUrl: nil,
                    // Section 4.4: the own tile carries the profile name from
                    // the first second, and the same profile is announced to
                    // every other member inside the call group.
                    localDisplayName: GroupCallHost.ownTileName(),
                    ownProfileEnvelope: { ProfileDirectory.shared.ownProfileEnvelopeForCall() }
                )
                if request.isOwner {
                    // Owner founds + hosts; `onJoined` fires inside `host`.
                    return try await MultiJoinerCall.host(options: options, ui: ui, callbacks: callbacks)
                }
                // Joiner connects signaling-only + relays its KeyPackage, then
                // waits for the Welcome (`onJoined`).
                return try await MultiJoinerCall.join(options: options, ui: ui, callbacks: callbacks)
            },
            ownIdentity: { [weak authRepository] in
                guard let secrets = authRepository?.identitySecrets,
                      let identity = try? Identity.fromSeed(seed: secrets.accountIdentitySeed)
                else { return nil }
                return (secrets.accountId, identity.signaturePublicKey())
            },
            mintInviteLink: { [weak authRepository] callId in
                guard let secrets = authRepository?.identitySecrets else {
                    throw LinkCallHostError.signedOut
                }
                let identity: Identity
                do {
                    identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
                } catch {
                    throw LinkCallHostError.identityUnavailable
                }
                var mintedLinkId: Data?
                let composed: CallInviteComposer.Composed
                do {
                    composed = try await CallInviteComposer.composeAndUpload(
                        callId: callId,
                        ownerSigningSeed: identity.ed25519Seed(),
                        ownerAccountId: secrets.accountId,
                        identity: identity,
                        discoveryClient: DiscoveryClient(sessionToken: secrets.sessionToken),
                        linkGrant: { linkId, expiresAtMs in
                            mintedLinkId = linkId
                            return try await ReachCenter.shared.newLinkGrant(
                                linkId: linkId, expiresAtMs: expiresAtMs)
                        }
                    )
                } catch {
                    // No link was minted (a failed upload mints nothing, the
                    // inline form is gone): the key registered for it goes.
                    if let mintedLinkId { ReachCenter.shared.removeLink(linkId: mintedLinkId) }
                    throw error
                }
                let blobId = composed.blobId
                // ADR-0074 section 3.5a rule 5 — the ledger id is kept with
                // the link's reach scope, so a key rotation can revoke the
                // link at the call-invite ledger.
                if let mintedLinkId {
                    ReachCenter.shared.attachBlob(
                        linkId: mintedLinkId, blobId: blobId, kind: LinkReach.callKind)
                    // ADR-0078 section 10: the link's own revocation seed,
                    // the only thing that revokes its ledger row.
                    OwnLinkStore.shared.put(accountIdB64: base64Encode(secrets.accountId), OwnLink(
                        kind: .call, slug: base64UrlEncodeNoPad(blobId),
                        revocationSeed: composed.revocationSeed, label: CallInviteValues.label,
                        expiryMs: composed.expiryMs, maxUses: Int64(CallInviteValues.maxUses),
                        bootstrapReferral: false,
                        mintedAtMs: Int64(Date().timeIntervalSince1970 * 1000)))
                }
                return composed.url
            },
            copyToClipboard: { UIPasteboard.general.string = $0 },
            isGuest: { [weak authRepository] in authRepository?.isGuest ?? false }
        ))
    }

    /// `Invite contacts` (`docs/ux/call-links.md` section 3): the list is the
    /// person's contacts, named and ordered the way the Contacts list does
    /// it, and the invitation goes into the one-to-one with the chosen
    /// contact (``InviteContactSend``): the existing one by the rule Contacts
    /// uses (``ContactConversation``), else one founded through the contact
    /// flows' own creation path, then the conversation's normal send path,
    /// which records the sent row.
    func wireInvitations(chat: ChatRepository, contacts: ContactsRepository) {
        inviteTargets = { [weak contacts] in
            guard let contacts else { return [] }
            return InviteContacts.rows(contacts.state.accepted) { contact in
                let resolved = ProfileDirectory.shared.resolve(signatureKey: contact.request.fromPubkey)
                return InviteContactRow(
                    key: contact.request.fromPubkey,
                    name: resolved.name,
                    username: resolved.verifiedUsername,
                    isPlaceholder: resolved.isPlaceholder,
                    nameIsUsername: resolved.source == .verifiedUsername)
            }
        }
        sendInvitation = { [weak chat, weak contacts] contactId, text in
            guard let chat, let contacts, let key = Data(base64Encoded: contactId) else {
                throw CallInviteOfferError.noSendPath
            }
            try await InviteContactSend.run(
                contactKey: key,
                text: text,
                ports: InviteContactSend.Ports(
                    contact: { key in
                        contacts.state.accepted.first { $0.request.fromPubkey == key }
                    },
                    conversationWith: { contact in
                        chat.oneToOneConversation(
                            contactKey: contact.request.fromPubkey,
                            linkedGroupIdHex: contact.groupIdHex)
                    },
                    found: { contact in try await contacts.foundOneToOne(with: contact) },
                    send: { groupIdHex, text in
                        try await chat.sendMessage(groupIdHex: groupIdHex, plaintext: text)
                    }
                ))
        }
    }
}

enum LinkCallHostError: Error, LocalizedError {
    case identityUnavailable
    case signedOut

    var errorDescription: String? {
        switch self {
        case .identityUnavailable:
            return Localization.text("call.link.error.generic")
        case .signedOut:
            return CallInviteOfferRules.signedOutMessage
        }
    }
}
