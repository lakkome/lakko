// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import UIKit

/// State + dispatcher for the per-thread ``ChatScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatViewModel.kt` (1.15f4b)
///
/// Combines the ``ChatRepository``-side thread (looked up by
/// `groupIdHex`) with VM-local composer state (the current draft
/// input + the send-busy / send-error flags) into a single set of
/// `@Published` slots SwiftUI observes.
///
/// Continuous foreground polling lives on the screen via
/// `.task { while !Task.isCancelled { … } }`, same pattern as
/// 1.14e5's contacts inbox loop.
@MainActor
public final class ChatViewModel: ObservableObject {

    // MARK: - Constructor inputs

    public let groupIdHex: String

    // MARK: - Form fields

    @Published public var draft: String = ""

    /// Phase 1.19c3 — attachments staged in the composer. The
    /// user picks photo / file; each call to
    /// ``onAttachmentPicked(bytes:mime:name:)`` encrypts +
    /// uploads to storage immediately so the ref is ready by
    /// the time Send is tapped. Cleared on successful Send, on
    /// per-chip remove, and on screen dismount.
    @Published public private(set) var pendingAttachments: [AttachmentRef] = []

    // MARK: - Ephemeral status

    @Published public private(set) var sending: Bool = false
    @Published public private(set) var sendError: String?
    @Published public private(set) var refreshing: Bool = false
    /// `true` while one or more picked items are being
    /// encrypted + uploaded. The composer disables Send and the
    /// picker buttons until it flips back.
    @Published public private(set) var uploading: Bool = false

    /// Polish 1.x — per-pick upload-progress detail. `nil` when
    /// no upload is in flight; otherwise carries the file name
    /// + which stage of the encrypt / upload pipeline the
    /// in-flight pick is at, so the composer tray can render a
    /// specific "Uploading hello.png…" caption instead of a
    /// generic "Uploading…". The user picks one file at a time
    /// (Apple's photo picker UX is single-image-per-pick by
    /// default in the ChatScreen), so no file-index ornament —
    /// the index is always 1.
    @Published public private(set) var uploadStage: UploadStage?

    /// Stage of a single pick's encrypt+upload pipeline. Mirror
    /// of the web client's `uploadProgress.stage` discriminator.
    public enum UploadStage: Equatable {
        case encrypting(fileName: String)
        case uploading(fileName: String)
    }

    // MARK: - Mirror of the repository's per-thread state

    /// The messages list for this group's thread. Reactive — every
    /// repository `state` update lands here via Combine
    /// subscription.
    @Published public private(set) var messages: [ChatMessage] = []
    /// `false` when the repository has no thread for this groupIdHex
    /// — e.g. immediately after process restart before snapshot
    /// rehydration has rebuilt the per-group handles. The UI still
    /// renders the composer (sendMessage surfaces a clear error if
    /// the handle is missing).
    @Published public private(set) var threadKnown: Bool = false

    // MARK: - Group membership (A4)

    /// Live MLS roster for this group at the current epoch — refreshed by
    /// ``refreshMembers()`` and after every add / remove.
    @Published public private(set) var members: [GroupMember] = []
    /// `true` while the members sheet is presented.
    @Published public var membersPanelOpen: Bool = false
    /// `@username` typed into the add-member field.
    @Published public var addMemberInput: String = ""
    /// `true` while an add or remove commit is in flight (disables the form).
    @Published public private(set) var membershipBusy: Bool = false
    /// Last add/remove error surfaced to the sheet, else `nil`.
    @Published public private(set) var membershipError: String?
    /// ADR-0027 / UX §7.1 — an external-join request arrived while the
    /// quarantine was full, so the core did not keep it. It is in no list and
    /// can be approved by nobody; the only honest thing left is to say it
    /// happened.
    ///
    /// Session-scoped and cleared when the admin acts on the queue (approve or
    /// reject), because acting is precisely what may have made room. There is
    /// no count: `MAX_PENDING_EXTERNAL_JOINS` is module-private in the core,
    /// and a client-side literal is the stale-drift class §7.1 flags.
    @Published public private(set) var unkeptJoinRequest: Bool = false
    /// ADR-0022 — `true` when the local member may add / remove (owner, admin,
    /// or an open/legacy group with no role table); `false` for a plain member.
    /// The sheet hides the add field + per-member Remove when this is `false`.
    @Published public private(set) var canManageMembers: Bool = true
    /// ADR-0022 — `true` when the local member is the owner (only the owner sees
    /// the per-member promote / demote toggle).
    @Published public private(set) var isOwner: Bool = false

    // MARK: - ADR-0045 UX §6.2 — the end of your own membership

    /// Is this device still a member of this group, and — if not — did it ask?
    ///
    /// The one signal there is. `processIncomingVerifyingRoles` returns the same
    /// content-less `Commit` for a commit that removes our own leaf as for any
    /// other, so nothing in the receive path announces it; `Group.isActive()` is
    /// what OpenMLS itself consults before refusing to encrypt, export or
    /// decrypt, and reading it is how this UI's gate becomes the same predicate
    /// as the core's refusal rather than a re-derivation of it.
    ///
    /// Refreshed by ``refreshMembership()``, which every path a removing commit
    /// can arrive on already calls: the repository-state sink (every poll) and
    /// ``refreshMembers()`` (every membership action and every panel open).
    /// The state is TERMINAL and survives a restart, so this answers the same at
    /// launch as it did in the batch that carried the removal — there is no
    /// transient window to debounce and nothing to re-check later.
    @Published public private(set) var membership: MembershipState = .active

    /// The standing gate the background disciplines consult (§6.2.8), and what
    /// the screen reads to withdraw the composer, the call buttons and the
    /// membership affordances.
    public var membershipHasEnded: Bool { membershipEnded(membership) }

    /// §6.2.5 — the two lines of the terminal timeline row, or `nil` while the
    /// membership holds.
    public var terminalRowCopy: TerminalRowCopy? { terminalRow(membership) }

    /// §6.2.5 — what occupies the composer's slot. The draft is read live so a
    /// commit landing mid-sentence preserves what was actually typed.
    public var composerSlot: ComposerState { composerState(membership, draft: draft) }

    /// ADR-0045 UX §6.2.5 — whether a message still carries Edit / Delete /
    /// React. The composer's own question, answered by the same predicate: all
    /// three ops compose an application message, so all three die with Send.
    ///
    /// This replaces a weaker shape rather than adding to it. `react`,
    /// `beginEdit` and `deleteMessage` each already guarded on
    /// `membershipHasEnded` and returned — so the menu opened, offered three
    /// actions and silently answered none of them, which is a worse lie than
    /// failing loudly: the user cannot tell a swallowed action from a slow one.
    /// Those guards stay as the backstop; this is what stops the offer.
    public var messageActionsAvailable: Bool {
        messageActionsState(membership) == .available
    }

    /// §6.2.8 — which one-shot teardown steps have already been claimed. A
    /// `Set` on the `@MainActor` view model rather than a lock, because
    /// ``claimTeardownSteps(_:alreadyRun:)`` takes what it hands out in the same
    /// synchronous call and the whole claim completes before the first `await`.
    private var teardownClaimed: Set<EndedTeardownStep> = []

    /// ADR-0045 — does this group route membership through proposals (i.e. does
    /// it carry an ADR-0022 role table)? Gates `Leave group`, which has no route
    /// through the core in a table-less group.
    @Published public private(set) var usesProposalPath: Bool = false

    /// ADR-0045 — feedback for a membership request that was accepted locally
    /// and published. Distinct from ``membershipError`` because a request is not
    /// an outcome: the change has NOT happened yet, and the copy must not
    /// suggest it has.
    @Published public private(set) var membershipNotice: String?
    /// The group's current role table (owner + admins), or `nil` for an open
    /// group — drives the per-member admin labelling.
    @Published public private(set) var roleTable: RoleTable?
    /// ADR-0027 (G-3) — pending external-join requests. Each entry is a joiner
    /// leaf signature key (the `proposalRef` for approve / reject). The members
    /// sheet renders an owner/admin-only section from this; empty hides it. The
    /// MLS-core retained set (`group.pendingExternalJoins()`) is the source of
    /// truth — re-read on `refreshMembers()` + on each poll while the sheet is
    /// open.
    @Published public private(set) var pendingJoins: [Data] = []

    /// ADR-0045 S4a — the membership requests waiting for a committer to sweep
    /// them, already filtered by the UX §10 visibility gate. A pending REMOVAL
    /// leaves its member in ``members`` and annotates the row; a pending ADD is
    /// NOT in ``members`` and gets its own section — neither is an optimistic
    /// update, because neither has taken effect yet.
    @Published public private(set) var pendingRequests: [PendingRequest] = []
    /// ADR-0045 S7 residual 1 — own `add` requests that left the pending set
    /// without reaching the roster, and were therefore never carried. Emptied
    /// with the rest of the pending surface once the membership has ended.
    @Published public private(set) var unappliedAddRows: [UnappliedAdd] = []

    /// ADR-0045 S4a — the §5.1 notice, or `nil` when nothing is past the quiet
    /// window.
    public var pendingNoticeText: PendingNotice? { pendingNotice(pendingRequests) }
    /// ADR-0028 OQ2 (G-3) — advisory attribution for a pending request: which
    /// member minted the `/g/` link it arrived on, and whether that issuer is in
    /// the current roster.
    public struct JoinAttribution: Equatable {
        public let issuerFingerprint: String
        public let issuerInRoster: Bool
    }
    /// ADR-0028 OQ2 (G-3) — keyed by joiner signature key. Refreshed alongside
    /// `pendingJoins`; a missing entry means the joiner posted no attribution.
    @Published public private(set) var joinAttributions: [Data: JoinAttribution] = [:]
    /// ADR-0028 (G-3) — the minted group invite-link (the owner/admin "create
    /// invite link" affordance), or `nil` before it is generated.
    @Published public private(set) var inviteLink: String?

    // MARK: - ADR-0045 UX §6.1 ownership transfer

    /// `true` while the transfer step is pushed onto the members sheet's
    /// navigation stack. §14 pins the container: a PUSHED destination inside the
    /// existing `NavigationStack`, never a sheet over a sheet.
    @Published public var transferStepOpen: Bool = false
    /// The picked successor's signature key, base64 — never a leaf index,
    /// because OpenMLS hands a vacated leaf to the next Add. `nil` until the
    /// owner picks, which is what keeps the action disabled (§6.1.5).
    @Published public var transferPick: String?
    /// The outgoing owner's own role afterwards. Pre-selected, never defaulted:
    /// the core withholds a default because either silent choice is wrong in one
    /// direction, and this only ever carries a value the user can see selected.
    @Published public var transferOutgoing: OutgoingOwner = preselectedOutgoingOwner
    /// ADR-0045 UX §6.1.9 — the new-owner notice, once, until dismissed. Held in
    /// a stored property rather than recomputed: the very next refresh would see
    /// the remembered owner equal the current one and correctly return `nil`, so
    /// a recomputed notice would vanish on the next poll.
    @Published public private(set) var newOwnerBanner: NewOwnerNotice?

    // MARK: - Wiring

    private let repository: ChatRepository
    /// Supplies the current auth session token for minting the SFU
    /// call-admission JWT. Defaults to `{ nil }` (group calls then
    /// refuse, fail-closed); the composition root passes
    /// `{ authRepository.identitySecrets?.sessionToken }`.
    private let sessionTokenProvider: () -> String?
    /// Resolves `@username -> (accountId, signaturePubkey)` through the
    /// G-4-verified contacts path for the add-member flow. Defaults to a
    /// fail-closed stub (add-member then refuses); the composition root
    /// passes `{ try await contactsClient.resolveVerifiedRecipient(...) }`.
    private let resolveRecipient: (String) async throws -> (accountId: Data, signaturePubkey: Data)
    private var cancellables: Set<AnyCancellable> = []
    /// Phase 2a3e2b-iOS — cross-device sync. Attached via
    /// [`attachSync`] from the screen so the existing init
    /// signature (1.14f2) stays binary-compatible. `nil` until
    /// the screen lifecycle wires it in.
    private weak var deviceGroupSync: DeviceGroupSyncRepository?
    /// Raw group_id bytes for the active thread — cached so
    /// the broadcast / seed paths don't re-fetch on every
    /// keystroke.
    private var groupIdBytes: Data?
    /// Last text the broadcast path published. Used to dedup
    /// echoes that loop back through the poll loop.
    private var lastBroadcastDraft: String?
    /// Debounce timer for outgoing Draft payloads.
    private var draftBroadcastWork: DispatchWorkItem?
    private var didSeedDraft: Bool = false

    public init(
        repository: ChatRepository,
        groupIdHex: String,
        sessionTokenProvider: @escaping () -> String? = { nil },
        resolveRecipient: @escaping (String) async throws
            -> (accountId: Data, signaturePubkey: Data) = { _ in
                throw ChatError.argument(
                    "no @username resolver wired — add-member unavailable"
                )
            }
    ) {
        self.repository = repository
        self.groupIdHex = groupIdHex
        self.sessionTokenProvider = sessionTokenProvider
        self.resolveRecipient = resolveRecipient
        sync(state: repository.state)
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] newState in
                self?.sync(state: newState)
            }
            .store(in: &cancellables)
        // Phase 3a9 — attach a call-signaling sink to the
        // repository. The closure runs on the main actor
        // because the repository's poll loop already
        // dispatches results on @MainActor.
        repository.callSignalingSink = { [weak self] hex, payloadBytes in
            guard hex == groupIdHex else { return }
            Task { @MainActor in
                self?.handleIncomingCallSignaling(payloadBytes: payloadBytes)
            }
        }
        // ADR-0024 — attach the chat-status sink: inbound typing +
        // read-receipt signals for THIS thread fold into ephemeral
        // typing state + a monotone read watermark (no bubble). The
        // poll loop already dispatches on @MainActor.
        repository.chatStatusSink = { [weak self] hex, status in
            guard hex == groupIdHex else { return }
            Task { @MainActor in
                self?.handleChatStatus(status)
            }
        }
        // ADR-0024 — drive the OUTGOING typing signal off composer
        // edits (throttled to one `true` per window; a single `false`
        // on empty / blur / send). The `attachSync` path also observes
        // `$draft` for the cross-device draft broadcast — independent
        // sinks, no interference.
        $draft
            .receive(on: RunLoop.main)
            .sink { [weak self] text in
                self?.onDraftChanged(text)
            }
            .store(in: &cancellables)
        // ADR-0024 — publish a read receipt when the timeline advances
        // while the screen is open. Gated on `screenVisible` so a
        // background poll-driven append doesn't mark messages read
        // before the user has actually looked at the thread.
        $messages
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in
                guard let self = self, self.screenVisible else { return }
                self.maybePublishReadReceipt()
            }
            .store(in: &cancellables)
    }

    /// ADR-0024 — `true` while ``ChatScreen`` is on-screen. The screen
    /// sets it in `.onAppear` / `.onDisappear`. Read receipts fire only
    /// while visible (the user is actually reading); blur also clears
    /// the outgoing typing state.
    @Published var screenVisible: Bool = false {
        didSet {
            if screenVisible {
                // Newly visible — publish a receipt for whatever's
                // already loaded.
                maybePublishReadReceipt()
                // ADR-0045 S4b — start announcing that this device can sweep.
                startBeaconTicker()
            } else {
                // Left the thread — stop advertising that we're typing.
                stopTyping()
                // ADR-0045 S4b — and stop announcing that we can sweep: a sweep
                // runs only at the tail of a poll driven by a visible screen, so
                // a beacon from here on would be a claim this device cannot back
                // up, and every other candidate would defer to it.
                stopBeaconTicker()
            }
        }
    }

    // MARK: - ADR-0045 S4b — the committer liveness beacon

    /// Wakes the emitter. **Not the beacon interval** — that is the core's
    /// `committerBeaconIntervalMs`, enforced by `shouldEmitBeacon` and never
    /// hand-copied here. This only bounds how long after the screen becomes
    /// visible the first beacon waits.
    private var beaconTicker: Timer?

    /// The iOS reading of "foregrounded", and it is load-bearing rather than
    /// cosmetic — see `ChatRepository.emitCommitterBeaconIfDue`.
    ///
    /// BOTH conditions are required and neither implies the other: an app can be
    /// `.active` with this thread off-screen (its poll loop is not running, so it
    /// cannot sweep), and this screen can be the top view controller with the app
    /// `.inactive` or `.background` (same). The ADR-0041 NotificationService
    /// extension is a different process entirely and never reaches this code.
    private var isForeground: Bool {
        screenVisible && UIApplication.shared.applicationState == .active
    }

    private func startBeaconTicker() {
        guard beaconTicker == nil else { return }
        // ADR-0045 UX §6.2.8 item 2 — and never re-arm it for a group this
        // device has been evicted from. Without this the one-shot teardown is
        // undone by the very next `.onAppear`: it invalidates the ticker, then
        // `screenVisible = true` starts a fresh one, and because the step is
        // already claimed nothing invalidates it again. The emit path is gated
        // independently so no beacon would actually go out — but a timer that
        // wakes forever to be told no is exactly the shape §6.2.8 item 1
        // rejects for the poll.
        guard !membershipHasEnded else { return }
        // The core makes the FIRST beacon of a session immediate, so fire once
        // now rather than waiting a whole tick to claim a duty this device is
        // ready to take.
        Task { @MainActor [weak self] in await self?.emitBeaconTick() }
        let ticker = Timer.scheduledTimer(withTimeInterval: beaconTickSeconds, repeats: true) {
            [weak self] _ in
            Task { @MainActor [weak self] in await self?.emitBeaconTick() }
        }
        RunLoop.main.add(ticker, forMode: .common)
        beaconTicker = ticker
    }

    private func stopBeaconTicker() {
        beaconTicker?.invalidate()
        beaconTicker = nil
    }

    private func emitBeaconTick() async {
        await repository.emitCommitterBeaconIfDue(
            groupIdHex: groupIdHex, foreground: isForeground)
        // The quiet window matures with the clock, so re-fold on the same tick
        // rather than only when the user does something.
        if membersPanelOpen { refreshPendingRequests() }
    }

    deinit {
        typingExpiryTimer?.invalidate()
        typingSendThrottle?.cancel()
        beaconTicker?.invalidate()
    }

    /// ADR-0042 — provision this thread's opaque push routing on appear
    /// (the equivalent of the web `Chat.tsx` onMount `provisionPushRouting`).
    /// Owner + no secret yet → mint the `push_routing_secret` + broadcast it
    /// over the E2EE journal + subscribe under the derived `push_gid`; a
    /// member holding a secret → subscribe under `push_gid`; anyone else →
    /// defer (never subscribes under the leaky DS `group_id`, invariant #5).
    /// Idempotent + race-safe (a shared in-flight guard coalesces this with
    /// the launch-time legacy-sweep). Fire-and-forget.
    public func ensurePushRouting() {
        repository.ensurePushRouting(groupIdHex: groupIdHex)
    }

    // MARK: - Phase 3a9 — call surface helpers

    /// Tap-to-call from the chat header. Mints a fresh
    /// `call_id` and builds the session (deriving + arming the
    /// SFrame key) but does NOT dial yet — the session sits in
    /// `.idle`, which `driveFrom` maps to the prejoin phase.
    /// The user picks mic / camera on the prejoin screen and the
    /// Join button calls `CallSession.dial` (G3-1-prejoin-join).
    public func startCall(audio: Bool = true, video: Bool = false) {
        guard activeCall == nil else { return }
        // ADR-0045 UX §6.2.8 item 4 / §6.2.11 — every exporter-backed
        // affordance GOES once the membership has ended, and failing closed is
        // not enough on its own. The per-call SFrame seed comes from
        // `exportSecret("lakko/call/v1", …)`, which is gated on `is_active()`
        // and now raises — so the call would refuse, correctly, and a control
        // that always errors is exactly the shape §6.1.1 rejects. The stronger
        // reason is §6.2.11: a call control asserts a live E2EE session, and
        // there is not one. The toolbar hides the buttons; this is the layered
        // half.
        guard !membershipHasEnded else { return }
        _ = (audio, video)
        let callId = CallSession.mintCallId()
        let session = makeSession(callId: callId)
        activeCall = session
    }

    /// Accept the in-flight inbound call.
    public func acceptIncomingCall() {
        activeCall?.acceptCall()
    }

    /// Decline the in-flight inbound call.
    public func declineIncomingCall() {
        activeCall?.declineCall()
    }

    /// Hang up the active call.
    public func hangUpCall() {
        activeCall?.hangUp(reason: .hungUp)
    }

    /// Clear the overlay after a `done` transition. Called
    /// from the SwiftUI overlay's dismiss action.
    public func clearFinishedCall() {
        if activeCall?.state == .done {
            activeCall = nil
        }
    }

    private func makeSession(callId: Data) -> CallSession {
        // Capture the repository + groupIdHex for the
        // outbound publish callback. We can't depend on
        // ContextsRepository here (the chat client is
        // owned by `ChatRepository`); the publish goes
        // straight through the repository's `publishGroup`
        // path via a thin helper.
        let session = CallSession(callId: callId) { [weak self] bytes in
            guard let self = self else { return }
            Task { @MainActor in
                await self.publishCallSignaling(bytes)
            }
        }
        // G3-1-call-key — derive the per-call SFrame seed from
        // the chat group's MLS exporter and arm the session
        // BEFORE it dials / accepts. Both directions go through
        // `makeSession`, so this single call covers the outgoing
        // (`startCall`) and the incoming (`handleIncomingCallSignaling`)
        // paths. Without the seed, `attachSFrame` refuses the
        // call per security invariant #4 (no non-E2EE fallback);
        // a failed derivation leaves the seed unset, which is the
        // correct fail-closed outcome (the call simply refuses).
        if let key = repository.deriveCallKey(groupIdHex: groupIdHex, callId: callId) {
            session.setCallKey(seed: key.seed, localLeaf: key.localLeaf)
        } else {
            NSLog(
                "makeSession: call-key derivation failed — " +
                "call will refuse per security invariant #4"
            )
        }
        return session
    }

    @MainActor
    private func publishCallSignaling(_ bytes: Data) async {
        do {
            try await repository.publishApplicationMessage(
                groupIdHex: groupIdHex,
                plaintextBytes: bytes
            )
        } catch {
            NSLog("publishCallSignaling failed: %@", error.localizedDescription)
        }
    }

    private func handleIncomingCallSignaling(payloadBytes: Data) {
        do {
            let parsed = try parseCallSignaling(plaintext: payloadBytes)
            let bodyCallId = CallSession.callId(of: parsed)
            // Spin up an answerer session for a first-seen
            // inbound offer; otherwise route into the active
            // session if call_ids match.
            if activeCall == nil {
                if case .offer = parsed {
                    let session = makeSession(callId: bodyCallId)
                    activeCall = session
                    session.applyIncomingSignaling(parsed)
                }
                return
            }
            guard let session = activeCall, session.callId == bodyCallId else {
                // Stale signaling for a different call_id —
                // drop silently.
                return
            }
            session.applyIncomingSignaling(parsed)
        } catch {
            NSLog("parseCallSignaling failed: %@", error.localizedDescription)
        }
    }

    // MARK: - Phase 3a9 — call surface

    /// Active 1×1 call for this thread. `nil` when no call.
    /// Published so the SwiftUI overlay re-renders on every
    /// state transition.
    @Published public private(set) var activeCall: CallSession?

    /// Active multi-party SFU group call for this thread (`nil` when
    /// none). A group chat (>2 members) routes through this; a 1×1 chat
    /// stays on `activeCall`. Published so the UI presents the call
    /// overlay driven by `sfuCallUiState`.
    @Published private(set) var activeSfuCall: SfuCallUi?

    /// The `CallUiState` the active SFU call drives (roster / phase /
    /// E2EE). The SwiftUI overlay observes this while `activeSfuCall`
    /// is non-nil. Reused across calls. `internal` — `SfuCallUi` /
    /// `CallUiState` are app-module types consumed by `ChatScreen`.
    let sfuCallUiState = CallUiState()

    /// G-18 / ADR-0029 — the `call_id` of the active SFU group call,
    /// retained so a mid-call MLS epoch change can re-derive the per-call
    /// seed with the SAME call_id (the exporter context) at the new
    /// epoch. Set in `startGroupCall`, cleared in `hangUpGroupCall`.
    private var activeGroupCallId: Data?

    /// Piece B — observes the SFU call's local `camOn` flag on
    /// `sfuCallUiState` and drives the mid-call camera-on onto the session
    /// (the SFU `CallScreen` is presented UI-only, so the camera button
    /// only flips the store; this bridges that to `publishVideo` /
    /// `setCameraEnabled`). Set up in `startGroupCall`, cancelled in
    /// `hangUpGroupCall`. Mirror of the web `Chat.tsx` camera effect.
    private var sfuCameraCancellable: AnyCancellable?

    /// `true` when this chat has >2 MLS members — the call button then
    /// routes through the SFU group-call path; a 2-member chat stays on
    /// the 1×1 P2P `CallSession`. `nil` member count (no handle) reads
    /// as a 1×1.
    var isGroupChat: Bool {
        (repository.memberCount(groupIdHex: groupIdHex) ?? 0) > 2
    }

    /// Start a multi-party SFU group call for this (group) chat. Mints
    /// the call-admission JWT, derives the per-call SFrame seed + leaf
    /// from the MLS exporter, and connects through `SfuCallUi`. Fails
    /// closed (NSLog + no call) on any missing prerequisite — SFU URL,
    /// session token, or call-key derivation.
    func startGroupCall(video: Bool = false) {
        // ADR-0045 UX §6.2.8 item 4 — see `startCall(audio:video:)`.
        guard !membershipHasEnded else { return }
        guard activeSfuCall == nil, activeCall == nil else { return }
        guard let base = SfuConfig.baseUrl else {
            NSLog("startGroupCall: SFU URL not configured (LakkoSfuUrl) — refusing")
            return
        }
        guard let token = sessionTokenProvider() else {
            NSLog("startGroupCall: no session token — re-authenticate to start a group call")
            return
        }
        let callIdBytes = CallSession.mintCallId()
        let callIdHex = callIdBytes.map { String(format: "%02x", $0) }.joined()
        guard let key = repository.deriveCallKey(groupIdHex: groupIdHex, callId: callIdBytes) else {
            NSLog("startGroupCall: call-key derivation failed — refusing per invariant #4")
            return
        }
        // G-18 / ADR-0029 — retain the call_id so a mid-call epoch change
        // re-derives the seed with the same exporter context.
        activeGroupCallId = callIdBytes
        let jwt = CallJwtClient()
        jwt.setSessionToken(token)
        Task { @MainActor in
            do {
                let minted = try await jwt.mint(callId: callIdHex, role: .participant)
                let config = SfuCallSession.Config(
                    sfuBaseUrl: base,
                    jwt: minted.jwt,
                    seed: key.seed,
                    localLeaf: key.localLeaf,
                    audio: true,
                    video: video
                )
                // ADR-0021 — precompute the per-call peer_tag -> contact
                // label map once, so the SFU roster resolves to the
                // identifier the rest of the app shows for that contact.
                // The tag is re-derived locally (the SFU never sends a
                // name); a tag collision falls back to the pseudonym.
                var tagToLabel: [String: String] = [:]
                var collidedTags = Set<String>()
                for c in AcceptedContactsStore.load() {
                    guard let accountId = c.request.fromAccountId else { continue }
                    let accountIdHex = accountId
                        .map { String(format: "%02x", $0) }
                        .joined()
                    let tag = sfuPeerTag(callId: callIdHex, accountId: accountIdHex)
                    if tagToLabel[tag] != nil {
                        collidedTags.insert(tag)
                    } else {
                        // Prefer the user-chosen display name; fall back to
                        // the short pubkey fingerprint (ADR-0021 follow-up).
                        tagToLabel[tag] = c.displayLabel
                    }
                }
                let resolveDisplayName: (String, String, String) -> String = {
                    _, pseudonym, peerTag in
                    if !peerTag.isEmpty, !collidedTags.contains(peerTag),
                        let label = tagToLabel[peerTag] {
                        return label
                    }
                    return pseudonym.isEmpty ? "Peer" : String(pseudonym.prefix(8))
                }
                let handle = SfuCallUi(
                    config: config,
                    ui: sfuCallUiState,
                    resolveDisplayName: resolveDisplayName
                )
                activeSfuCall = handle
                observeSfuCameraToggle()
                try await handle.connect()
            } catch {
                NSLog("startGroupCall failed: %@", error.localizedDescription)
                sfuCameraCancellable?.cancel()
                sfuCameraCancellable = nil
                activeSfuCall?.hangUp()
                activeSfuCall = nil
                activeGroupCallId = nil
            }
        }
    }

    /// Hang up the active SFU group call + clear it.
    func hangUpGroupCall() {
        sfuCameraCancellable?.cancel()
        sfuCameraCancellable = nil
        activeSfuCall?.hangUp()
        activeSfuCall = nil
        activeGroupCallId = nil
    }

    /// Piece B — bridge the SFU call's local `camOn` (flipped by the in-call
    /// camera button, which only drives `sfuCallUiState` because the SFU
    /// `CallScreen` is presented UI-only) onto the live session. Mirrors the
    /// web `Chat.tsx` camera effect: `camOn == true` -> `publishVideo()`
    /// (acquire + SFrame-seal + renegotiate, idempotent + in-flight-guarded);
    /// `false` -> `setCameraEnabled(false)` (local track gate). `removeDuplicates`
    /// so an unrelated `participants` re-emission doesn't re-fire.
    private func observeSfuCameraToggle() {
        sfuCameraCancellable = sfuCallUiState.$participants
            .map { $0.first(where: { $0.isLocal })?.camOn ?? false }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] camOn in
                MainActor.assumeIsolated {
                    self?.applySfuCameraToggle(camOn)
                }
            }
    }

    private func applySfuCameraToggle(_ camOn: Bool) {
        guard let handle = activeSfuCall else { return }
        if camOn {
            Task { @MainActor [weak self] in
                do {
                    try await handle.publishVideo()
                } catch {
                    guard let self else { return }
                    // Fail-safe (invariant #4): the camera could not start —
                    // revert the toggle so the button snaps back, and surface
                    // the reason in the in-call log. The call stays up
                    // audio-only.
                    self.sfuCallUiState.setCamera(false)
                    self.sfuCallUiState.addSystemEvent(
                        "Could not start the camera: \(error.localizedDescription)"
                    )
                    NSLog("SFU group call: publishVideo failed: %@", String(describing: error))
                }
            }
        } else {
            handle.setCameraEnabled(false)
        }
    }

    /// G-18 / ADR-0029 — mid-call call-key rotation. When the group's MLS
    /// epoch advances during a live SFU group call (a member added /
    /// removed, or an Update commit), the per-call SFrame seed changes;
    /// re-derive it at the NEW epoch (same call_id) and rekey every
    /// attached SFrame transform. The decryptor keeps the previous key
    /// for the overlap window so in-flight frames sealed under the old
    /// epoch still open, then drops it (forward secrecy). The rekey
    /// itself is a no-op when the seed is unchanged (`SfuCallSession`
    /// guards on it), so this is cheap to call after every poll / roster
    /// refresh. Fail-closed: a derivation failure keeps the current seed
    /// — never a downgrade to plaintext (invariant #4). Mirrors the web
    /// `membersVersion`-driven rekey effect in `routes/Chat.tsx`.
    private func maybeRekeyActiveGroupCall() {
        guard let handle = activeSfuCall, let callId = activeGroupCallId else {
            return
        }
        guard let key = repository.deriveCallKey(groupIdHex: groupIdHex, callId: callId) else {
            // Best-effort — keep the current seed on a re-derivation
            // failure (fail-closed, invariant #4).
            return
        }
        handle.rekey(newSeed: key.seed)
    }

    // MARK: - Group membership actions (A4)

    /// Open / close the members sheet. Refreshes the roster on open.
    func toggleMembersPanel() {
        membersPanelOpen.toggle()
        if membersPanelOpen {
            membershipError = nil
            // A notice describes one action's outcome, so it must not outlive
            // the panel that produced it: the pending row annotation is the
            // durable signal, not this.
            membershipNotice = nil
            refreshMembers()
        }
    }

    /// Re-read the live MLS roster + the local member's role from the repository.
    /// ADR-0045 UX §6 / §12 item 6 — the one funnel every membership handler
    /// reports a failure through.
    ///
    /// Before this, all nine handlers were `membershipError =
    /// error.localizedDescription`, so a member who tapped `Add` was shown
    /// `proposeAddMemberFromWire failed: Group(message: "add proposal refused:
    /// proposer is not owner or admin")` — a `ChatError.wrapped` around the
    /// core's sentence for a developer reading a log.
    ///
    /// The **log keeps that sentence**, because that is who it is for. The
    /// screen gets the copy the reason selects, and the reason is a typed
    /// value rather than anything read out of the message.
    ///
    /// A non-refusal keeps its own description unchanged: collapsing a dead
    /// socket into "This change could not be requested." would be a worse lie
    /// than the raw text, and §6 row 7 is about not leaking a *refusal's*
    /// internal reason, not about hiding every error.
    private func reportMembershipFailure(_ error: Error, op: String) {
        membershipError =
            membershipRefusalMessage(
                for: error,
                retryMaySucceed: { membershipRefusalRetryMaySucceed(reason: $0) })
            ?? error.localizedDescription
        NSLog("%@ failed: %@", op, error.localizedDescription)
    }

    func refreshMembers() {
        // ADR-0045 UX §6.2 — FIRST, because every gate below depends on it.
        // A panel open is one of the paths that can be the first thing to
        // observe the terminal state (the poll that carried the commit may
        // have landed while the sheet was closed).
        refreshMembership()
        members = repository.members(groupIdHex: groupIdHex) ?? []
        let role = repository.ownRole(groupIdHex: groupIdHex)
        // ADR-0045 UX §6.2.7 — and NOBODY may manage members once their own
        // membership has ended. Gating here rather than at each of the six
        // render sites is deliberate: `Add member`, the invite block, every
        // `Remove` and every admin toggle are all refused by the core for an
        // inactive group, and one gate cannot be partly applied. Note the
        // difference from role-gating, which hides what SOMEONE ELSE may do —
        // this hides what nobody in this group may do any more.
        canManageMembers = role != .member && !membershipHasEnded
        // The owner cannot reach the terminal state at all (§6.2.9 — the core
        // refuses a `Remove` targeting the owner and refuses an owner's own
        // leave), so this conjunction is unreachable rather than load-bearing.
        // It is written anyway for the reason the line above is: one gate
        // cannot be partly applied, and the per-member admin toggle this flag
        // drives would otherwise be the one membership control in the panel
        // that survived on an argument about a different rule.
        isOwner = role == .owner && !membershipHasEnded
        // ADR-0045 — whether this group routes membership through proposals at
        // all. Read here rather than at each use so the `Leave group` gate and
        // the action behind it see one answer from one roster refresh, instead
        // of two reads that can disagree across a commit.
        usesProposalPath = repository.usesProposalPath(groupIdHex: groupIdHex)
        roleTable = repository.groupRoles(groupIdHex: groupIdHex)
        // ADR-0027 (G-3) — refresh the pending external-join list alongside the
        // roster (the retained set is the source of truth).
        pendingJoins = refreshedPendingJoins()
        // ADR-0027 / §7.1 — the request the quarantine turned away is in no
        // list, so it can only come from the repository's own observation.
        unkeptJoinRequest =
            !membershipHasEnded && repository.sawUnkeptExternalJoin(groupIdHex: groupIdHex)
        refreshPendingRequests()
        refreshJoinAttributions()
        // ADR-0045 UX §6.1.9 — decided AFTER the roster + role table are fresh
        // (it compares the table's owner against what we last saw) and after the
        // pending set, so a transfer that has just been swept is judged on the
        // state it produced rather than the state it replaced.
        refreshNewOwnerNotice()
        // G-18 / ADR-0029 — a roster refresh follows every local
        // membership commit (add / remove / role / approve), any of which
        // advances the epoch. Rekey the active SFU call to the new epoch
        // (a no-op when the seed is unchanged, e.g. a panel-open refresh).
        maybeRekeyActiveGroupCall()
    }

    /// ADR-0045 S4a — re-join the core's retained pending set to this device's
    /// observation store and apply the UX §10 visibility gate.
    ///
    /// Cheap and idempotent, so it runs on every roster refresh AND on the
    /// beacon tick: the quiet window matures with the clock, not with a user
    /// action, so a request that was `pending` when the sheet opened has to be
    /// able to become `waiting` while it stays open.
    func refreshPendingRequests() {
        // ADR-0045 UX §6.2.7 — the pending notice describes changes that will
        // apply to a group that no longer contains us, and the annotations
        // describe a roster that will never move again. Emptied at the SOURCE
        // so the notice, the row annotations, `Waiting to be added` and the
        // exit block's `waitingRoleChange` all fall silent together rather than
        // one render site at a time.
        if membershipHasEnded {
            pendingRequests = []
            unappliedAddRows = []
            return
        }
        pendingRequests = visibleRequests(
            repository.pendingRequests(groupIdHex: groupIdHex),
            canManageMembers: canManageMembers
        )
        // ADR-0045 S7 residual 1 — read AFTER the fold above, which is what
        // detects and records them: a row is created at the exact moment its
        // observation is pruned.
        unappliedAddRows = repository.unappliedAdds(groupIdHex: groupIdHex)
    }

    /// ADR-0045 S7 residual 1 — dismiss one `Not applied` row. It stays
    /// dismissed: the detection re-runs on every fold, so without the tombstone
    /// the notice could not be dismissed at all.
    func onDismissUnapplied(addSignatureKeyB64: String) {
        repository.dismissUnapplied(
            groupIdHex: groupIdHex, addSignatureKeyB64: addSignatureKeyB64)
        unappliedAddRows = repository.unappliedAdds(groupIdHex: groupIdHex)
    }

    /// ADR-0027 (G-3) — the core's retained external-join set, or empty once
    /// this device's own membership has ended (§6.2.7).
    ///
    /// A queue this reader can no longer act on: approving needs `add_member`,
    /// which the core refuses for an inactive group, so the section would be a
    /// list of buttons that all fail. Gated in ONE helper because both call
    /// sites (the panel refresh and the per-poll `sync`) would otherwise have
    /// to remember it independently.
    private func refreshedPendingJoins() -> [Data] {
        membershipHasEnded ? [] : repository.pendingExternalJoins(groupIdHex: groupIdHex)
    }

    /// ADR-0045 S4a — the row annotation for one roster member, or `nil`.
    func pendingAnnotation(_ member: GroupMember) -> String? {
        rowAnnotation(pendingRequests, leafIndex: member.leafIndex, ownLeaf: ownLeafIndex)
    }

    /// ADR-0045 S4a — the `Waiting to be added` rows.
    var waitingToBeAddedRequests: [PendingRequest] { waitingToBeAdded(pendingRequests) }

    /// ADR-0028 OQ2 (G-3) — refresh the advisory join-attribution map for the
    /// current pending requests. Best-effort: failures leave the map empty (the
    /// role-gated approval gate is the boundary). Owner/admin only.
    private func refreshJoinAttributions() {
        guard canManageMembers else {
            joinAttributions = [:]
            return
        }
        let hex = groupIdHex
        Task { [weak self] in
            guard let self else { return }
            let attrs = await self.repository.fetchJoinAttributions(groupIdHex: hex)
            var map: [Data: JoinAttribution] = [:]
            for a in attrs {
                map[a.joinerSignatureKey] = JoinAttribution(
                    issuerFingerprint: a.issuerFingerprint, issuerInRoster: a.issuerInRoster)
            }
            self.joinAttributions = map
        }
    }

    /// `true` when `member` is an admin in the current role table.
    func memberIsAdmin(_ member: GroupMember) -> Bool {
        roleTable?.admins.contains(member.signaturePublicKey) ?? false
    }

    /// `true` when `member` is the group owner (the owner row never shows a
    /// promote / demote toggle).
    func memberIsOwner(_ member: GroupMember) -> Bool {
        roleTable?.owner == member.signaturePublicKey
    }

    /// Our own leaf index — the roster marks it "(you)" and blocks self-removal.
    var ownLeafIndex: UInt32? {
        repository.ownLeafIndex(groupIdHex: groupIdHex)
    }

    /// Human label for a roster member: a matched contact's chosen name (or its
    /// fingerprint), else a short fingerprint of the signature key. "(you)" is
    /// appended for the local leaf. Mirrors the ADR-0021 resolution the call
    /// roster uses, keyed on the member's signature public key.
    func memberLabel(_ member: GroupMember) -> String {
        let isYou = ownLeafIndex == member.leafIndex
        let suffix = isYou ? " (you)" : ""
        for c in AcceptedContactsStore.load()
        where c.request.fromPubkey == member.signaturePublicKey {
            return c.displayLabel + suffix
        }
        let fp = member.signaturePublicKey.prefix(4)
            .map { String(format: "%02x", $0) }
            .joined()
        return "Member " + fp + suffix
    }

    /// Resolve the typed `@username` (G-4-verified) and add it to this group:
    /// publish the add commit to the journal (own-marked) + ferry the Welcome.
    /// Fail-closed — a resolve / verify / publish failure surfaces an error and
    /// leaves the group untouched.
    func onAddMember() {
        let raw = addMemberInput.trimmingCharacters(in: .whitespacesAndNewlines)
        let username = raw.hasPrefix("@") ? String(raw.dropFirst()) : raw
        guard !username.isEmpty else { return }
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                let resolved = try await resolveRecipient(username)
                let outcome = try await repository.addMemberToGroup(
                    groupIdHex: groupIdHex,
                    recipientAccountId: resolved.accountId,
                    recipientSignaturePubkey: resolved.signaturePubkey
                )
                addMemberInput = ""
                // ADR-0045 — `proposed` is not a quieter `committed`: nobody has
                // been added yet. `refreshMembers` re-reads the roster (which is
                // unchanged) AND the pending set (which now holds the request),
                // so the sheet shows a request rather than a member.
                _ = outcome
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onAddMember")
            }
        }
    }

    /// Remove a member by leaf index: publish the removal commit to the journal
    /// (own-marked). Refuses to remove the local leaf. Fail-closed on error.
    func onRemoveMember(leafIndex: UInt32) {
        guard ownLeafIndex != leafIndex else {
            membershipError = "You cannot remove yourself from the group."
            return
        }
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                // ADR-0045 — on the proposal path the member STAYS in the
                // roster, annotated, until a committer sweeps. Dropping the row
                // now would assert they are gone while they are still in the
                // group and still receiving every message.
                _ = try await repository.removeMemberFromGroup(
                    groupIdHex: groupIdHex,
                    leafIndex: leafIndex
                )
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onRemoveMember")
            }
        }
    }

    /// ADR-0045 — whether to offer `Leave group`, and whether it is live.
    ///
    /// One rule, two consumers: the sheet renders from this, and
    /// ``onLeaveGroup()`` re-asks before publishing. A hidden control is not a
    /// guarantee — `isOwner` is only as fresh as the last roster refresh — so
    /// the action must not assume the gate it was drawn behind still holds.
    var leaveAffordance: LeaveGroupAffordance {
        leaveGroupAffordance(
            usesProposalPath: usesProposalPath,
            isOwner: isOwner,
            ownLeaf: ownLeafIndex,
            busy: membershipBusy,
            requests: pendingRequests)
    }

    /// ADR-0045 — ask to leave the group.
    ///
    /// Reachable for the first time because of that ADR: naming this device's
    /// OWN leaf in a Remove proposal needs no role, where ADR-0022 had recorded
    /// self-leave as impossible because OpenMLS `CannotRemoveSelf` stops a
    /// member committing their own departure. A proposal is not a commit, so
    /// there is nothing to commit and nothing to refuse.
    ///
    /// Deliberately NOT routed through ``onRemoveMember(leafIndex:)``, whose
    /// own-leaf guard is still correct for the direct-commit path a table-less
    /// group takes.
    ///
    /// The caller is expected to have confirmed first: a published request
    /// cannot be withdrawn — there is no wire withdraw, and evicting it locally
    /// would leave it live in every peer's store — so the confirmation is the
    /// last exit and this method is past it.
    func onLeaveGroup() {
        // ADR-0045 UX §6.2.7 — `Leave group` names a group we are not in. The
        // exit block already renders `Delete this chat` instead, so this is the
        // layered half: a control that is merely not drawn is not a guarantee,
        // and the core would answer a leave here with `UseAfterEviction` prose
        // about proposals to a user who has already left.
        guard !membershipHasEnded else { return }
        switch leaveAffordance {
        case .hidden(let refusal):
            // Reached only if the gate went stale between draw and tap. Answer
            // in this feature's own vocabulary rather than letting the core's
            // prose surface to a user who never asked about proposals.
            membershipError = refusal.message
            return
        case .disabled:
            // Busy, or we have already asked. Asking twice would put two
            // identical Remove proposals into one sweep.
            return
        case .enabled:
            break
        }
        guard let leaf = ownLeafIndex else { return }
        membershipBusy = true
        membershipError = nil
        membershipNotice = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                // The repository records the own-request observation and
                // suppresses the echo itself, so unlike the web reference there
                // is nothing to do here afterwards but re-read the roster —
                // which must still contain us, annotated.
                _ = try await repository.removeMemberFromGroup(
                    groupIdHex: groupIdHex,
                    leafIndex: leaf
                )
                // ADR-0045 UX §6.2.3 — the ONE thing that later tells
                // `You left this group.` from `You are no longer a member of
                // this group.`, and it has to be its own durable field rather
                // than a read of the own-request observation the repository
                // just wrote. That observation lives under `remove:{ownLeaf}`
                // and is pruned the moment the core stops holding the request —
                // which is exactly what the sweep that removes us does. The
                // poll batch carrying the eviction therefore destroys the only
                // evidence that we asked for it, and the order of those two
                // steps is an implementation detail no test pins.
                //
                // Written on BOTH outcomes, `committed` included: a legacy
                // group with no role table takes the direct path and evicts us
                // in the same call. Fail-safe when absent — the fallback string
                // is true either way.
                repository.noteLeaveRequested(groupIdHex: groupIdHex)
                membershipNotice =
                    "Leave requested. It applies when an owner or an admin applies the change."
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onLeaveGroup")
            }
        }
    }

    // MARK: - ADR-0045 UX §6.1 ownership transfer

    /// The roster's OWN identity treatment, in the roster's own order, as the
    /// picker reads it. Built from ``memberLabel(_:)`` rather than a second
    /// derivation, so the picker and the members list cannot render the same
    /// person differently — that is the one thing the user cross-checks.
    var rosterViews: [RosterEntryView] {
        members.map {
            RosterEntryView(
                leafIndex: $0.leafIndex,
                signatureKeyB64: base64Encode($0.signaturePublicKey),
                label: memberLabel($0))
        }
    }

    /// The role table's owner key, base64. `nil` for an open / legacy group.
    var ownerKeyB64: String? {
        roleTable.map { base64Encode($0.owner) }
    }

    /// The admin set as base64 keys, for the picker's muted `Admin` marker.
    var adminKeysB64: Set<String> {
        Set((roleTable?.admins ?? []).map { base64Encode($0) })
    }

    /// This device's own signature key, base64 — resolved through the roster
    /// rather than stored, so a leaf renumber cannot make it name someone else.
    ///
    /// Read from the LIVE group rather than from the published ``members``
    /// snapshot, because ADR-0023 gave it a second job: it is the identity the
    /// own-reaction highlight and the "read by every other member" exclusion
    /// compare against, and those must not go blank between roster refreshes.
    /// One rule, and it is the same one the receive and send paths use.
    var ownSignatureKeyB64: MemberIdentity? {
        repository.ownSignatureKeyB64(groupIdHex: groupIdHex)
    }

    /// §6.1.3 — every member except the current owner, marked (never filtered)
    /// for admin, in the roster's order.
    var transferPickerRows: [TransferCandidate] {
        transferCandidates(rosterViews, ownerKeyB64: ownerKeyB64, adminKeysB64: adminKeysB64)
    }

    /// §6.1.6 / §6.1.8 — the role change already waiting to apply, if any.
    ///
    /// `.waiting` and not merely pending is the whole point: below the quiet
    /// window the control stays ENABLED and a second tap is refused by the
    /// repository's C-65 pre-check. A request that is not yet rendered must not
    /// silently disable a control with nothing to explain it.
    ///
    /// The successor label is resolved through the LIVE roster HERE, at render
    /// time, never from a stored label: a successor who has since left degrades
    /// to the unnamed variant rather than to a name the roster no longer carries.
    var waitingRoleChangeState: WaitingRoleChange? {
        guard let hit = pendingRequests.first(where: {
            $0.kind == .roleChange && $0.state == .waiting
        }) else { return nil }
        let successor = hit.transferToB64.flatMap { key in
            rosterViews.first { $0.signatureKeyB64 == key }
        }
        return WaitingRoleChange(
            // Our own PROMOTE is not our own transfer — only a record carrying a
            // successor key is a transfer. A promote reporting itself as an
            // ownership transfer would be a plain falsehood.
            ownTransfer: hit.own && hit.transferToB64 != nil,
            successorLabel: successor?.label)
    }

    /// §6.1.1 — the exit block: which of the two role-disjoint controls this
    /// reader gets, and why the other one is disabled.
    var exitBlockState: ExitBlock {
        exitBlock(
            ExitBlockInput(
                usesProposalPath: usesProposalPath,
                isOwner: isOwner,
                ownLeafKnown: ownLeafIndex != nil,
                candidateCount: transferPickerRows.count,
                waitingRoleChange: waitingRoleChangeState,
                ownRemovalPending: ownLeafIndex.map {
                    rowAnnotation(pendingRequests, leafIndex: $0, ownLeaf: $0) != nil
                } ?? false,
                // §6.2.7 — beats every other arm. Distinct from
                // `ownRemovalPending` above in the way that matters: that is a
                // request in flight over a group we are still fully in, this is
                // the terminal state after it landed.
                membershipEnded: membershipHasEnded))
    }

    /// The picked successor, or `nil` while nothing is picked — which is what
    /// keeps the action disabled (§6.1.5).
    var transferSelected: TransferCandidate? {
        guard let pick = transferPick else { return nil }
        return transferPickerRows.first { $0.signatureKeyB64 == pick }
    }

    /// §6.1.5 — the action's label and whether it is live.
    var transferActionState: (label: String, enabled: Bool) {
        transferActionLabel(selected: transferSelected)
    }

    /// Open the transfer step. Resets the picker and re-applies the pre-selected
    /// outgoing role, so a cancelled attempt never leaks its choices into the
    /// next one.
    func openTransferStep() {
        membershipError = nil
        membershipNotice = nil
        transferPick = nil
        transferOutgoing = preselectedOutgoingOwner
        transferStepOpen = true
    }

    /// §6.1.5 — `Cancel` is a PRE-PUBLISH exit, and it is the last one. Nothing
    /// here is a withdraw: once the proposal is published it is in every
    /// member's store and dies only when some commit lands, so no client may
    /// offer a cancel afterwards — including, and especially, for a transfer.
    func cancelTransferStep() {
        transferStepOpen = false
        transferPick = nil
    }

    /// §6.1 — publish the ownership transfer, then sweep it (ADR-0045).
    ///
    /// The shape is ``onToggleAdmin(member:makeAdmin:)``'s, deliberately: a
    /// transfer IS a role change — not a new commit type, just the owner-only
    /// role-table rewrite with the owner slot moved — so anything else here
    /// would be a second, differently-shaped route into membership.
    ///
    /// It sweeps EXPLICITLY rather than waiting for the poll tail, and that is
    /// the one place it goes beyond the toggle. The reason is the outcome copy:
    /// §6.1.7 names the transferor's own new role only when the change actually
    /// landed, and a request that has not landed must not claim it has. The
    /// owner is rank 0, so the sweep is this same device and in practice the
    /// change is immediate.
    func onTransferOwnership() {
        guard let successor = transferSelected else { return }
        guard !membershipBusy else { return }
        let outgoing = transferOutgoing
        membershipBusy = true
        membershipError = nil
        membershipNotice = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            guard let successorKey = base64Decode(successor.signatureKeyB64) else {
                membershipError = "This change could not be requested."
                return
            }
            do {
                _ = try await repository.transferGroupOwnership(
                    groupIdHex: groupIdHex,
                    newOwnerSignaturePubkey: successorKey,
                    outgoing: outgoing)
                transferStepOpen = false
                transferPick = nil
                let swept = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                let landed: Bool
                if case .committed = swept { landed = true } else { landed = false }
                membershipNotice = transferOutcomeNotice(
                    outcome: landed ? .committed : .proposed,
                    successorLabel: successor.label,
                    outgoing: outgoing)
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onTransferOwnership")
            }
        }
    }

    /// §6.1.9 — dismiss the new-owner notice. It is informational and must never
    /// be the only way a user learns their role, so dismissing it loses nothing.
    func dismissNewOwnerNotice() {
        newOwnerBanner = nil
    }

    /// §6.1.9 — decide the new-owner notice, then record what we observed.
    ///
    /// Nothing on the wire announces a transfer: the role table simply changes
    /// and every other device learns of it by re-reading. So this is a
    /// client-side INFERENCE from an observed change, structurally unable to fire
    /// on a device that first observes the table already naming it owner — a
    /// fresh device, a restore, a first sync. Hence fail-open everywhere.
    ///
    /// The order is load-bearing: decide FIRST, remember SECOND, or the
    /// comparison would always be against itself.
    private func refreshNewOwnerNotice() {
        guard let current = ownerKeyB64 else { return }
        let previous = repository.lastObservedOwner(groupIdHex: groupIdHex)
        // Resolved through the live roster; `nil` when the previous owner has
        // left in a later commit (transfer and leave are necessarily separate
        // ones).
        let previousLabel = previous.flatMap { key in
            rosterViews.first { $0.signatureKeyB64 == key }?.label
        }
        if let notice = newOwnerNotice(
            lastObservedOwnerB64: previous,
            currentOwnerB64: current,
            ownSignatureKeyB64: ownSignatureKeyB64,
            previousOwnerLabel: previousLabel
        ) {
            newOwnerBanner = notice
        }
        repository.rememberObservedOwner(groupIdHex: groupIdHex, ownerKeyB64: current)
    }

    /// Promote / demote a member as admin (owner only — the toggle is shown
    /// only when `isOwner`). Publishes the `setGroupRoles` commit. Fail-closed.
    func onToggleAdmin(member: GroupMember, makeAdmin: Bool) {
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                _ = try await repository.setMemberAdmin(
                    groupIdHex: groupIdHex,
                    memberSignaturePubkey: member.signaturePublicKey,
                    makeAdmin: makeAdmin
                )
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onToggleAdmin")
            }
        }
    }

    /// ADR-0027 (G-3), on the ADR-0045 proposal path since S7 — approve a
    /// pending external-join request (owner/admin only). Publishes ONE
    /// artifact, an ordinary admin-authored `Add` proposal; there is no commit,
    /// no epoch advance and no Welcome here. The joiner is admitted when a
    /// committer sweeps the proposal, which is what posts the
    /// `WelcomeRendezvous` they are polling for — and ADR-0045 decision 2 gives
    /// that step no deadline, so the notice must not promise it has happened.
    ///
    /// The sweep is attempted immediately so the common single-admin case still
    /// reads as "tap, done"; when this device is standing down it simply does
    /// not fire, and the copy says so. Fail-closed on error.
    func onApproveJoin(proposalRef: Data) {
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        membershipNotice = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                _ = try await repository.approveExternalJoin(
                    groupIdHex: groupIdHex,
                    proposalRef: proposalRef
                )
                let swept = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                if case .committed = swept {
                    membershipNotice = "Join approved. They have been added to the group."
                } else {
                    // ADR-0045 S7 residual 2 — an approve that did not apply has
                    // grown a queue only a sweep drains, and the core's own
                    // feedback (its cap refusal) lands on whoever proposes NEXT.
                    // Naming the depth here puts it in front of the person who
                    // caused it. Read from the core, not from `pendingRequests`,
                    // which has not been refreshed yet.
                    let depth = repository.pendingProposals(groupIdHex: groupIdHex).count
                    let queue = waitingQueueLine(depth)
                    membershipNotice =
                        "Join approved. They join when an owner or an admin applies the change."
                        + (queue.map { " \($0)" } ?? "")
                }
                // Acting on the queue is what may have made room for the
                // request the quarantine turned away, so the notice about it
                // stops being current.
                repository.clearUnkeptExternalJoin(groupIdHex: groupIdHex)
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onApproveJoin")
            }
        }
    }

    /// ADR-0027 (G-3) — reject a pending external-join request (owner/admin
    /// only). Local-only drop of the retained proposal; no commit / journal
    /// post. Fail-closed on error.
    func onRejectJoin(proposalRef: Data) {
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        defer { membershipBusy = false }
        do {
            try repository.rejectExternalJoin(
                groupIdHex: groupIdHex,
                proposalRef: proposalRef
            )
            // Same reason as in `onApproveJoin`: the queue moved.
            repository.clearUnkeptExternalJoin(groupIdHex: groupIdHex)
            refreshMembers()
        } catch {
            reportMembershipFailure(error, op: "onRejectJoin")
        }
    }

    /// ADR-0028 (G-3) — mint a group invite-link (owner/admin only) and publish
    /// it to `inviteLink` for the members sheet to show + copy. Fail-closed.
    func onCreateInviteLink() {
        membershipError = nil
        Task { @MainActor in
            do {
                inviteLink = try await repository.createGroupInviteLink(groupIdHex: groupIdHex)
            } catch {
                reportMembershipFailure(error, op: "onCreateInviteLink")
            }
        }
    }

    /// ADR-0028 revoke — revoke the currently-shown group invite link (issuer
    /// only). A pointer link carries a ledger blob the server deletes; an inline
    /// link has nothing to revoke (we just clear the field). Fail-closed.
    func onRevokeInviteLink() {
        guard let link = inviteLink else { return }
        membershipError = nil
        Task { @MainActor in
            do {
                _ = try await repository.revokeGroupInviteLink(link: link)
                inviteLink = nil
            } catch {
                reportMembershipFailure(error, op: "onRevokeInviteLink")
            }
        }
    }

    /// Phase 2a3e2c-iOS — read-cursor broadcast tracker.
    /// Highest broadcast messageId; the per-`messages` effect
    /// schedules a fresh `ReadCursor` payload only when the
    /// newest visible row's id exceeds this value.
    private var lastBroadcastReadId: Int64 = -1
    private var readCursorWork: DispatchWorkItem?

    /// Bind to the process-scoped
    /// ``DeviceGroupSyncRepository`` (Phase 2a3e2b). Called
    /// from `ChatScreen.onAppear`. Subscribes to:
    ///
    ///   * incoming `Draft` payloads keyed on this thread —
    ///     seed `draft` once on first mount when local is
    ///     empty (avoids stomping mid-typing on this device).
    ///   * local `draft` changes — debounce 500 ms then
    ///     broadcast.
    ///   * local `messages` mutations — debounce 1.5 s then
    ///     broadcast a `ReadCursor` payload pointing at the
    ///     highest message-id (Phase 2a3e2c).
    public func attachSync(_ sync: DeviceGroupSyncRepository) {
        deviceGroupSync = sync
        // Resolve groupIdBytes lazily — the ChatRepository
        // exposes the per-thread record's groupId; we hex-
        // decode the `groupIdHex` we already carry.
        groupIdBytes = Self.hexToBytes(groupIdHex)
        guard let groupIdBytes = groupIdBytes else { return }

        // Observe the sync repository's drafts map. On every
        // change, seed locally IF the local composer is
        // empty + we haven't seeded yet.
        let key = groupIdHex
        sync.$drafts
            .receive(on: RunLoop.main)
            .sink { [weak self] drafts in
                guard let self = self else { return }
                guard !self.didSeedDraft else { return }
                guard let incoming = drafts[key] else { return }
                if self.draft.isEmpty {
                    self.draft = incoming.text
                }
                self.didSeedDraft = true
            }
            .store(in: &cancellables)

        // Observe own `draft` changes + broadcast debounced.
        $draft
            .receive(on: RunLoop.main)
            .sink { [weak self] text in
                self?.scheduleDraftBroadcast(text: text, groupIdBytes: groupIdBytes)
            }
            .store(in: &cancellables)

        // Phase 2a3e2c-iOS — read-cursor broadcast. Every time
        // the timeline grows past the previously-broadcast
        // value, mint a fresh `ReadCursor` payload pointing at
        // the newest visible message.
        $messages
            .receive(on: RunLoop.main)
            .sink { [weak self] msgs in
                self?.scheduleReadCursorBroadcast(
                    messages: msgs,
                    groupIdBytes: groupIdBytes
                )
            }
            .store(in: &cancellables)
    }

    private func scheduleReadCursorBroadcast(
        messages: [ChatMessage],
        groupIdBytes: Data
    ) {
        guard !messages.isEmpty else { return }
        // Find the highest server-assigned messageId. Outgoing
        // rows with messageId == 0 (pre-ack) get skipped — the
        // ack path will trigger a fresh effect run.
        var highest: Int64 = -1
        for m in messages where m.messageId > 0 {
            if m.messageId > highest { highest = m.messageId }
        }
        if highest <= lastBroadcastReadId { return }
        let target = highest
        readCursorWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self = self,
                  let sync = self.deviceGroupSync else { return }
            self.lastBroadcastReadId = target
            // 8 big-endian bytes — wire shape stable across
            // platforms.
            var idBytes = Data(count: 8)
            var v = UInt64(bitPattern: target)
            for i in (0..<8).reversed() {
                idBytes[i] = UInt8(v & 0xff)
                v >>= 8
            }
            let now = UInt64(Date().timeIntervalSince1970 * 1000)
            Task { @MainActor in
                do {
                    try await sync.setReadCursor(
                        groupId: groupIdBytes,
                        upToMessageId: idBytes,
                        readAtMs: now
                    )
                } catch {
                    NSLog("setReadCursor failed: %@", error.localizedDescription)
                }
            }
        }
        readCursorWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5, execute: work)
    }

    private func scheduleDraftBroadcast(text: String, groupIdBytes: Data) {
        if text == lastBroadcastDraft { return }
        draftBroadcastWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self = self,
                  let sync = self.deviceGroupSync else { return }
            self.lastBroadcastDraft = text
            Task { @MainActor in
                do {
                    try await sync.setDraft(
                        groupId: groupIdBytes,
                        text: text
                    )
                } catch {
                    NSLog("setDraft failed: %@", error.localizedDescription)
                }
            }
        }
        draftBroadcastWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5, execute: work)
    }

    private static func hexToBytes(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var bytes = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let b = UInt8(hex[index..<next], radix: 16) else { return nil }
            bytes.append(b)
            index = next
        }
        return bytes
    }

    private func sync(state: ChatRepositoryState) {
        let thread = state.threads.first { $0.groupIdHex == groupIdHex }
        self.messages = thread?.messages ?? []
        self.threadKnown = thread != nil
        // ADR-0045 UX §6.2 — every poll batch republishes the repository state,
        // and a removing commit arrives on a poll like any other, so this is
        // the one place that sees every path into the terminal state. It is a
        // cheap synchronous read of a boolean the core already maintains.
        refreshMembership()
        // ADR-0027 (G-3) — a poll may have retained a fresh external-join
        // request; refresh the pending list live while the members sheet is
        // open (cheap in-memory read of the group's retained set).
        if membersPanelOpen {
            pendingJoins = refreshedPendingJoins()
            unkeptJoinRequest =
                !membershipHasEnded && repository.sawUnkeptExternalJoin(groupIdHex: groupIdHex)
            refreshJoinAttributions()
        }
    }

    // MARK: - ADR-0045 UX §6.2 — detecting the end, and tearing down after it

    /// Re-read the membership verdict off the live handle, and drive the
    /// one-shot teardown when it has just become terminal.
    ///
    /// Idempotent and cheap, so it runs on every repository-state update and on
    /// every roster refresh. The teardown is what makes it more than a getter,
    /// and the discipline is in ``claimTeardownSteps(_:alreadyRun:)``: a step is
    /// CLAIMED synchronously, before any suspension point, so a second call
    /// arriving while the async half is still in flight gets nothing rather than
    /// firing a concurrent duplicate. A step that FAILS is released so the next
    /// pass retries it.
    ///
    /// Three of §6.2.8's five disciplines are not here, and that is deliberate
    /// rather than an omission — they are standing GATES with nothing to
    /// dispose, each consulting the state where it runs: the poll
    /// (`ChatRepository.pollGroup`), the shrink rotation
    /// (`membershipRotationDecision`'s `selfEvicted`) and the exporter-backed
    /// call buttons (`ChatScreen`'s toolbar).
    private func refreshMembership() {
        membership = repository.membershipStateNow(groupIdHex: groupIdHex)
        let owed = claimTeardownSteps(membership, alreadyRun: &teardownClaimed)
        guard !owed.isEmpty else { return }
        for step in owed {
            switch step {
            case .stopBeaconTicker:
                // Belt and braces beside the gate inside
                // `ChatRepository.emitCommitterBeaconIfDue`: the gate is what
                // makes a surviving timer harmless, this is what stops the
                // wasted wake. Local and infallible, so it never needs
                // releasing.
                stopBeaconTicker()
            case .retirePushSubscription:
                // The half ADR-0043 recorded as unclosable ("nobody else can
                // delete it and no client has 'you were removed' handling").
                // This IS that handling. Bounded and never throwing on the
                // repository side; the claim was taken above, synchronously,
                // BEFORE this `Task` — which is the whole point, since the
                // caller re-runs on every roster change.
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    let retired = await self.repository.retireOwnPushSubscription(
                        groupIdHex: self.groupIdHex)
                    // `false` means there was no readable secret to name an id
                    // from — done rather than failed, so the claim stands and
                    // this does not spin.
                    _ = retired
                }
            }
        }
    }

    /// §6.2.4 — `Delete this chat`. Reachable only from the exit block's
    /// `ended` arm, and the repository re-checks that itself rather than
    /// trusting the control it was drawn behind.
    ///
    /// The caller is expected to have confirmed first: this is the ONE control
    /// in this feature that destroys user data, on one confirmation, with no
    /// undo and no export, so the dialog is the only protection there is and
    /// this method is past it.
    func onDeleteChat() -> Bool {
        membershipError = nil
        let deleted = repository.deleteChat(groupIdHex: groupIdHex)
        if !deleted {
            // Only reachable if the gate went stale between draw and tap.
            membershipError = "This chat could not be deleted."
        }
        return deleted
    }

    // MARK: - Actions

    public func onSendClicked() {
        guard !sending else { return }
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let stagedAttachments = pendingAttachments
        // 1.19c3: attachment-only messages are valid (empty
        // text + ≥1 attachment); pre-1.19 text-only messages
        // still valid (≥1 char text + zero attachments). The
        // only invalid input is the all-empty composer.
        guard !trimmed.isEmpty || !stagedAttachments.isEmpty else { return }

        sending = true
        sendError = nil
        // ADR-0024 — sending clears the outgoing typing state (publishes
        // a single `{ typing: false }` if a `true` is outstanding).
        stopTyping()
        Task {
            do {
                try await repository.sendMessage(
                    groupIdHex: groupIdHex,
                    plaintext: trimmed,
                    attachments: stagedAttachments
                )
                sending = false
                sendError = nil
                draft = ""
                pendingAttachments = []
            } catch {
                sending = false
                // 1.19c3: leave `pendingAttachments` populated
                // on send failure so the user can retry without
                // re-picking the same files.
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Send failed."
            }
        }
    }

    // MARK: - ADR-0023 — edit / delete / reactions

    /// The local member's IDENTITY in this group. Used to highlight the
    /// reaction chips the local user contributed.
    ///
    /// Not the leaf it used to be: a leaf compared against a stored reactor
    /// starts matching whoever was added into our slot after we left it, so the
    /// highlight would light on a stranger's reaction.
    var ownIdentity: MemberIdentity? { ownSignatureKeyB64 }

    /// The message currently open in the edit sheet (`nil` when
    /// closed). Drives the SwiftUI `.sheet` presentation; the
    /// sheet pre-fills `editText` with the message's current
    /// body.
    @Published public var editingMessage: ChatMessage?
    /// Working draft for the edit sheet.
    @Published public var editText: String = ""

    /// Open the edit sheet for `message`, pre-filling the field
    /// with the current text. Only offered for own outgoing
    /// messages that carry a messageId.
    func beginEdit(_ message: ChatMessage) {
        editingMessage = message
        editText = message.text
    }

    /// Cancel the edit sheet without publishing.
    func cancelEdit() {
        editingMessage = nil
        editText = ""
    }

    /// Publish the edit for the message in the sheet. No-op for
    /// an empty / unchanged body or a missing target.
    func commitEdit() {
        guard let message = editingMessage else { return }
        let trimmed = editText.trimmingCharacters(in: .whitespacesAndNewlines)
        let target = message.messageId
        editingMessage = nil
        editText = ""
        guard target > 0, !trimmed.isEmpty, trimmed != message.text else { return }
        Task { @MainActor in
            do {
                try await repository.editMessage(
                    groupIdHex: groupIdHex,
                    target: target,
                    newText: trimmed
                )
            } catch {
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Edit failed."
            }
        }
    }

    /// Delete (tombstone) own message `message`.
    func deleteMessage(_ message: ChatMessage) {
        let target = message.messageId
        guard target > 0 else { return }
        Task { @MainActor in
            do {
                try await repository.deleteMessage(
                    groupIdHex: groupIdHex,
                    target: target
                )
            } catch {
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Delete failed."
            }
        }
    }

    /// React to `message` with `emoji` — toggles on the receive
    /// side + locally. Available on any message with a messageId.
    func react(to message: ChatMessage, emoji: String) {
        let target = message.messageId
        guard target > 0, !emoji.isEmpty else { return }
        Task { @MainActor in
            do {
                try await repository.reactToMessage(
                    groupIdHex: groupIdHex,
                    target: target,
                    emoji: emoji
                )
            } catch {
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Reaction failed."
            }
        }
    }

    // MARK: - ADR-0024 — typing indicators + read receipts

    /// Re-sync replay guard + auto-expiry window. A typing signal whose
    /// DS `created_at` is older than this is ignored on receive (a full
    /// journal re-download would otherwise resurrect stale typing); and
    /// a displayed "typing…" auto-clears this long after the last fresh
    /// `true` from that member. Defined once in ``PresenceOps``.
    private static let typingExpirySecs: TimeInterval = PresenceOps.typingExpiry

    /// Throttle for the OUTGOING `{ typing: true }` — we publish at
    /// most one per window while the user is actively editing. A
    /// 30 s burst is then ~7 tiny journal rows.
    private static let typingSendThrottleSecs: TimeInterval = PresenceOps.typingSendThrottle

    /// Receive-side ephemeral typing state: member identity → the local
    /// receive time of that member's last fresh `typing: true`. A member
    /// is shown "typing…" while its entry is within
    /// ``typingExpirySecs``; an explicit `false` removes it at once.
    /// NEVER persisted — pure in-memory session state.
    ///
    /// Keyed on the signature key, not the leaf: OpenMLS hands a vacated leaf
    /// to the next member added, so a leaf key let a newcomer inherit the
    /// departed member's typing state and let one member's stop clear
    /// another's. Self-healing in ~6 s, unlike the watermark below — but real
    /// while it lasts, and one rule is easier to keep right than two.
    private var typingMembers: PresenceOps.TypingMap = [:]

    /// Receive-side monotone read watermark: member identity → the highest
    /// messageId that member has reported reading. Advance-only (a lower
    /// value never overwrites a higher one), so a re-sync replay of an
    /// old receipt is idempotent. Session state, not persisted.
    ///
    /// Keyed on the signature key for the reason above, sharpened: this map is
    /// monotone and is never reset on a membership change, so a leaf key handed
    /// a departed member's ENTIRE read history to whoever took their slot, and
    /// ``readState(of:)`` then reported messages as read by someone who had
    /// never seen them.
    @Published private(set) var readWatermarks: PresenceOps.ReadWatermarkMap = [:]

    /// The members currently shown as typing, recomputed from
    /// ``typingMembers`` on every change / expiry tick. Drives the
    /// indicator string. Excludes ourselves defensively (we never
    /// publish our own `true` to ourselves, but the poll echo guard is
    /// belt-and-braces).
    @Published private(set) var typingMembersNow: [MemberIdentity] = []

    /// Repeating timer that expires stale typing entries (~1 Hz). Armed
    /// lazily on the first inbound `true`, invalidated on teardown.
    private var typingExpiryTimer: Timer?

    /// Outgoing-typing throttle: the pending "send a fresh `true`"
    /// gate. While set, a keystroke does not re-publish `true`.
    private var typingSendThrottle: DispatchWorkItem?
    /// `true` once we've published a `typing: true` that has not yet
    /// been cleared by a `false` (on blur / send). Guards against
    /// spamming `false` when we never sent a `true`.
    private var typingTrueOutstanding: Bool = false

    /// Our own last-published read watermark. We re-publish a receipt
    /// only when the highest received messageId advances past this.
    private var lastPublishedReadUpTo: Int64 = -1

    /// Handle an inbound typing / read-receipt status event for this
    /// thread (dispatched from the repository's `chatStatusSink`).
    private func handleChatStatus(_ status: ChatStatusEvent) {
        switch status {
        case .typing(let sender, let isTyping, let createdAt):
            // Drop our own echo defensively (the DS doesn't dedupe).
            if let own = ownSignatureKeyB64, sender == own { return }
            let before = typingMembers
            typingMembers = PresenceOps.foldTyping(
                typingMembers,
                sender: sender,
                isTyping: isTyping,
                rowDate: Self.parseDsTimestamp(createdAt),
                now: Date()
            )
            guard typingMembers != before else { return }
            if isTyping { armTypingExpiryTimer() }
            recomputeTypingNow()
        case .readReceipt(let sender, let upTo):
            if let own = ownSignatureKeyB64, sender == own { return }
            // MONOTONE — advance only.
            readWatermarks = PresenceOps.foldReadWatermark(
                readWatermarks, sender: sender, upTo: upTo)
        }
    }

    /// Recompute the live typing list from ``typingMembers``,
    /// dropping any entry past the expiry window, and publish it.
    private func recomputeTypingNow() {
        let now = Date()
        typingMembers = PresenceOps.pruneTyping(typingMembers, now: now)
        let live = PresenceOps.activeTypers(typingMembers, now: now)
        if live != typingMembersNow {
            typingMembersNow = live
        }
        if typingMembers.isEmpty {
            typingExpiryTimer?.invalidate()
            typingExpiryTimer = nil
        }
    }

    /// Arm the ~1 Hz expiry tick if it isn't already running.
    private func armTypingExpiryTimer() {
        guard typingExpiryTimer == nil else { return }
        let timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) {
            [weak self] _ in
            Task { @MainActor in self?.recomputeTypingNow() }
        }
        typingExpiryTimer = timer
    }

    /// The indicator string shown just above the input bar, or `nil`
    /// when nobody is typing. 1×1 → bare "typing…"; one named member →
    /// "<name> is typing…"; multiple → "Several people are typing…".
    var typingIndicatorText: String? {
        let typers = typingMembersNow
        guard !typers.isEmpty else { return nil }
        if typers.count > 1 {
            return "Several people are typing…"
        }
        let who = typers[0]
        // 1×1 chats (2 members) read as a bare "typing…" — naming the
        // sole peer adds nothing.
        if !isGroupChat {
            return "typing…"
        }
        if let name = displayName(for: who) {
            return "\(name) is typing…"
        }
        return "typing…"
    }

    /// Resolve a member identity to the human label the roster uses (a
    /// matched contact's chosen name, else a short key fingerprint).
    /// `nil` when that member isn't in the current roster.
    ///
    /// Keyed on the identity rather than the leaf so a member who left cannot
    /// be relabelled with the name of whoever took their slot.
    private func displayName(for identity: MemberIdentity) -> String? {
        let roster = members.isEmpty
            ? (repository.members(groupIdHex: groupIdHex) ?? [])
            : members
        guard let member = roster.first(
            where: { base64Encode($0.signaturePublicKey) == identity })
        else { return nil }
        for c in AcceptedContactsStore.load()
        where c.request.fromPubkey == member.signaturePublicKey {
            return c.displayLabel
        }
        let fp = member.signaturePublicKey.prefix(4)
            .map { String(format: "%02x", $0) }
            .joined()
        return "Member " + fp
    }

    // MARK: ADR-0024 — send side

    /// Call on every composer keystroke. Publishes a throttled
    /// `{ typing: true }` (one per ``typingSendThrottleSecs``) while the
    /// field is non-empty. An empty field is treated as "stopped" and
    /// clears the typing state once.
    func onDraftChanged(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            // Emptying the field = stopped typing.
            stopTyping()
            return
        }
        // Throttle: if a window is already pending, this keystroke is
        // absorbed (no new `true`).
        if typingSendThrottle != nil { return }
        publishTyping(true)
        typingTrueOutstanding = true
        let work = DispatchWorkItem { [weak self] in
            self?.typingSendThrottle = nil
        }
        typingSendThrottle = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + Self.typingSendThrottleSecs,
            execute: work
        )
    }

    /// Clear the outgoing typing state — publishes a single
    /// `{ typing: false }` if we have an outstanding `true`. Called on
    /// focus-loss, on send, and when the field empties. No `false` spam:
    /// a no-op when nothing is outstanding.
    func stopTyping() {
        typingSendThrottle?.cancel()
        typingSendThrottle = nil
        guard typingTrueOutstanding else { return }
        typingTrueOutstanding = false
        publishTyping(false)
    }

    private func publishTyping(_ isTyping: Bool) {
        Task { @MainActor in
            do {
                try await repository.sendTyping(
                    groupIdHex: groupIdHex,
                    isTyping: isTyping
                )
            } catch {
                NSLog("sendTyping failed: %@", error.localizedDescription)
            }
        }
    }

    /// ADR-0024 — publish a read receipt when the chat is open and the
    /// highest received messageId advances past our own last-published
    /// watermark (monotone). Call when the screen is visible + the
    /// timeline changes. The receipt broadcasts read-state to the group
    /// (a deliberate disclosure per the ADR).
    func maybePublishReadReceipt() {
        // ADR-0045 UX §6.2 — a read receipt is an ordinary MLS application
        // message, so it is refused for an inactive group exactly as a chat
        // send is. §6.2.8's list does not name it because it has nothing to
        // dispose, but it is the same rule reached from the same place: this
        // fires on `.onAppear`, so without the gate every visit to a dead chat
        // logs one `UseAfterEviction`. There is also nothing to report — the
        // watermark it would broadcast can never reach a member who could use
        // it.
        guard !membershipHasEnded else { return }
        var highest: Int64 = -1
        for m in messages where m.messageId > 0 {
            if m.messageId > highest { highest = m.messageId }
        }
        guard highest > lastPublishedReadUpTo else { return }
        lastPublishedReadUpTo = highest
        let target = highest
        Task { @MainActor in
            do {
                try await repository.sendReadReceipt(
                    groupIdHex: groupIdHex,
                    upTo: target
                )
            } catch {
                NSLog("sendReadReceipt failed: %@", error.localizedDescription)
            }
        }
    }

    /// ADR-0024 — read state of one of OUR own messages, for the
    /// "Sent" / "Read" check. `.read` when EVERY other roster member's
    /// watermark is ≥ this message's id; `.sent` otherwise. `nil` for a
    /// message with no server id yet (pre-ack) or a non-own message —
    /// the row then renders no check.
    func readState(of message: ChatMessage) -> OwnMessageReadState? {
        guard message.direction == .outgoing, message.messageId > 0 else {
            return nil
        }
        // Every OTHER member must have read up to this id. The roster
        // includes ourselves; exclude our own identity. An empty "others"
        // set (a solo group) reads as Sent.
        //
        // The roster is the CURRENT membership, so a departed member's stale
        // watermark is never consulted, and a newcomer — who has reported
        // nothing — holds this at Sent until they genuinely catch up. Keyed on
        // the identity: under a leaf key the newcomer arrived already holding
        // the departed member's watermark and this reported Read.
        let roster = members.isEmpty
            ? (repository.members(groupIdHex: groupIdHex) ?? [])
            : members
        let rosterIdentities = roster.map { base64Encode($0.signaturePublicKey) }
        let others = rosterIdentities.filter { $0 != ownSignatureKeyB64 }
        guard !others.isEmpty else { return .sent }
        return PresenceOps.isReadByEveryOther(
            watermarks: readWatermarks,
            messageId: message.messageId,
            roster: rosterIdentities,
            own: ownSignatureKeyB64
        ) ? .read : .sent
    }

    /// Parse a DS ISO-8601 timestamp (with or without fractional
    /// seconds) into a `Date`. `nil` on an unparseable string — the
    /// caller then keeps the signal (fail-open on the freshness check
    /// is safe: the receive-time expiry timer still bounds display).
    private static func parseDsTimestamp(_ s: String) -> Date? {
        let withFrac = ISO8601DateFormatter()
        withFrac.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = withFrac.date(from: s) { return d }
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        return plain.date(from: s)
    }

    /// Drive the encrypt + upload flow for one picked item.
    /// Called by the screen layer after a photo / document
    /// picker resolves to raw bytes + MIME + filename.
    /// Sequential — one picked item at a time, called from the
    /// screen's `Task { … }` per pick — keeps the upload-state
    /// flag honest.
    public func onAttachmentPicked(
        bytes: Data,
        mime: String?,
        name: String?
    ) async {
        uploading = true
        sendError = nil
        // Drive `uploadStage` immediately so the composer tray
        // doesn't show the generic spinner for the brief encrypt
        // window. `AttachmentBridge.encryptAndUpload` flips the
        // stage to `.uploading` once it reaches the PUT path.
        let displayName = (name?.isEmpty ?? true) ? "attachment" : name!
        uploadStage = .encrypting(fileName: displayName)
        defer {
            uploading = false
            uploadStage = nil
        }
        do {
            let ref = try await AttachmentBridge.encryptAndUpload(
                bytes: bytes,
                mime: mime,
                name: name,
                onStage: { [weak self] stage in
                    // Hop back to MainActor for the published
                    // mutation — `onStage` may run on whatever
                    // thread the URLSession completion lands on.
                    Task { @MainActor in
                        switch stage {
                        case .encrypting:
                            self?.uploadStage = .encrypting(fileName: displayName)
                        case .uploading:
                            self?.uploadStage = .uploading(fileName: displayName)
                        }
                    }
                }
            )
            pendingAttachments.append(ref)
        } catch let err as AttachmentBridgeError {
            sendError = err.errorDescription
        } catch {
            sendError = "Attachment failed: \(error)"
        }
    }

    /// Drop a staged attachment from the pending tray. Index
    /// matches the order in ``pendingAttachments``.
    public func onRemovePendingAttachment(at index: Int) {
        guard pendingAttachments.indices.contains(index) else { return }
        pendingAttachments.remove(at: index)
    }

    public func onRefreshClicked() {
        refreshing = true
        Task {
            defer { refreshing = false }
            do {
                try await repository.pollGroup(groupIdHex: groupIdHex)
            } catch {
                // best-effort
            }
        }
    }

    /// One long-poll tick — used by ``ChatScreen``'s
    /// `.task { … }` modifier in a `while !Task.isCancelled` loop.
    /// Silent (does NOT flip `refreshing`) so the spinner doesn't
    /// stay on for the full 30s long-poll window.
    /// ADR-0045 UX §6.2.8 item 1 — should the screen's poll loop keep going?
    ///
    /// The repository's own gate is what stops the fetch, but on iOS that is
    /// not enough on its own and the difference is a real defect rather than
    /// tidiness: this loop is `while !Task.isCancelled { await
    /// pollOnceLongPoll() }`, and `pollGroup` normally parks on a ~30 s long
    /// poll. Once it returns IMMEDIATELY the loop would spin as fast as the
    /// runtime allows, burning the CPU of the one platform whose poll is
    /// continuous rather than event-driven. So the screen stops iterating too.
    public var pollLoopShouldContinue: Bool { !membershipHasEnded }

    public func pollOnceLongPoll(waitSecs: Int = ChatViewModel.defaultLongPollSecs) async {
        do {
            try await repository.pollGroup(
                groupIdHex: groupIdHex,
                waitSecs: waitSecs
            )
        } catch {
            // best-effort
        }
        // G-18 / ADR-0029 — a poll may have applied a REMOTE commit that
        // advanced the epoch during a live SFU call. Re-derive + rekey to
        // the current epoch (a no-op when the seed is unchanged). This is
        // the iOS analog of the web `membersVersion`-driven rekey effect.
        maybeRekeyActiveGroupCall()
        // ADR-0045 — the poll's tail ran a sweep, which may have landed a
        // membership change, and the batch may have carried a peer's proposal.
        // Either way the sheet's roster + pending set are stale.
        if membersPanelOpen { refreshMembers() }
    }

    /// DS long-poll budget — same value as
    /// ``ContactsViewModel/defaultLongPollSecs``.
    public static let defaultLongPollSecs: Int = 30
}

/// ADR-0024 — read state of one of our own outgoing messages, for the
/// composer-side "Sent" / "Read" check.
public enum OwnMessageReadState: Equatable {
    /// Delivered to the DS but not yet read by every other member —
    /// renders a single check.
    case sent
    /// Read by every other roster member (their watermark covers this
    /// message's id) — renders a filled double check.
    case read
}
