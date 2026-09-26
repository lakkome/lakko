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

    /// UX spec section 10 — the preview of the photo or video being uploaded,
    /// shown in the composer's upload chip while the upload runs.
    @Published public private(set) var uploadingThumbnail: Data?

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
    /// `true` while the conversation's info is presented over the chat
    /// (`docs/ux/conversation-info.md` sections 1 and 3).
    @Published public var conversationInfoOpen: Bool = false
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
    /// Drives ``trackGroupCallFreshness``; cancelled when the record goes away
    /// and on teardown.
    private var groupCallFreshnessTask: Task<Void, Never>?
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

    init(
        repository: ChatRepository,
        groupIdHex: String,
        callHost: CallHost? = nil,
        groupCallHost: GroupCallHost? = nil,
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
        // The composition root passes the app-level host. A view model built
        // without one (previews) gets a host of its own that is NOT attached to
        // the repository's signaling feed, so it can never steal it.
        self.callHost = callHost
            ?? CallHost.live(
                repository: repository, sessionTokenProvider: sessionTokenProvider)
        // Same for the group call host: one of its own is not attached to the
        // repository's MLS chain, so it observes nothing it does not own.
        self.groupCallHost = groupCallHost
            ?? GroupCallHost.live(repository: repository, sessionTokenProvider: sessionTokenProvider)
        self.sessionTokenProvider = sessionTokenProvider
        self.resolveRecipient = resolveRecipient
        sync(state: repository.state)
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] newState in
                self?.sync(state: newState)
            }
            .store(in: &cancellables)
        // The 1x1 call lives in the app-level ``CallHost`` (fed by every receive
        // path, presented from the root view). This screen only reads it to
        // disable its own call controls while a call is up, so it re-renders
        // when the host changes.
        self.callHost.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        // Likewise the app-level group call host: this screen reads it for the
        // banner and the call controls.
        self.groupCallHost.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        // Group-call record — kept by the repository (so it is fed whichever
        // chat is open); this screen only mirrors its own chat's entry.
        repository.$groupCalls
            .map { $0[groupIdHex] }
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] record in
                guard let self else { return }
                self.ongoingGroupCall = record
                self.logGroupCallVerdict(record)
                self.trackGroupCallFreshness(record)
            }
            .store(in: &cancellables)
        // ADR-0024 — read watermarks are held durably by the repository (fed
        // at receipt for every group); seed from it and follow this group's
        // entry so a receipt applied while another chat was open, or before
        // a relaunch, still shows as Read.
        readWatermarks = repository.readWatermarks(groupIdHex: groupIdHex)
        repository.$readWatermarksByGroup
            .compactMap { $0[groupIdHex] }
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] map in
                self?.readWatermarks = map
            }
            .store(in: &cancellables)
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
        // ADR-0048 — every name this screen shows is resolved through the
        // profile directory; a profile or a username verdict arriving must
        // re-render them in place.
        ProfileDirectory.shared.$revision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        // ADR-0051 — a shared group name arriving re-titles the header and the
        // members panel in place.
        repository.$groupNameRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
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
                // ADR-0048 §2 trigger 4 — if someone here is still `Member`,
                // ask the group to introduce itself (the core's rule decides,
                // at most once a day per group; nothing is sent otherwise).
                requestProfilesIfDue()
            } else {
                // Left the thread — stop advertising that we're typing.
                stopTyping()
                // ADR-0052 — and drop an armed reply: "a reply is not a draft
                // that follows the person" (section 3.1).
                replyingTo = nil
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

    // MARK: - ADR-0048 §2 trigger 4 — ask the group for the names it still lacks

    /// Fire-and-forget: the repository counts the placeholders off the live
    /// roster, consults the persisted "last asked" stamp, and lets the core's
    /// `shouldRequestProfiles` decide. Nothing here awaits it — the screen
    /// renders whatever names it has, and an answer re-renders through the
    /// profile directory's revision like any other profile.
    private func requestProfilesIfDue() {
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.repository.requestProfilesOnOpen(groupIdHex: self.groupIdHex)
        }
    }

    private func emitBeaconTick() async {
        await repository.emitCommitterBeaconIfDue(
            groupIdHex: groupIdHex, foreground: isForeground)
        // The quiet window matures with the clock, so re-fold on the same tick
        // rather than only when the user does something.
        if conversationInfoOpen { refreshPendingRequests() }
        // ADR-0045 UX §7 — and LOOK at the queue on the same tick, panel open
        // or not.
        //
        // **This is not a retry timer, and the distinction is the whole
        // design.** The tick decides only *when this device looks*; what
        // decides whether anything happens is still the queue having shrunk
        // since the refusal. A tick against an unchanged queue does nothing at
        // all, so a group where no committer is running produces no attempts
        // however long it ticks.
        //
        // It is here because `refreshMembers` is user-driven on this platform:
        // without it, a request refused while the panel was open would wait
        // for the user to touch membership again before anyone noticed the
        // queue had drained. This tick is already paid for and is already
        // foreground-gated, which is the same condition a sweep needs.
        evaluateArmedRetries()
    }

    deinit {
        typingExpiryTimer?.invalidate()
        typingSendThrottle?.cancel()
        beaconTicker?.invalidate()
        groupCallFreshnessTask?.cancel()
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

    // MARK: - Calls

    /// Tap-to-call from the chat header. Hands off to the app-level
    /// ``CallHost``, which settles the microphone permission, mints a fresh
    /// `call_id`, builds the session (deriving + arming the SFrame key) and
    /// then either dials immediately (the phone button) or leaves the session
    /// idle behind the camera check (the video button) —
    /// `docs/ux/calls.md` sections 3.1 and 3.2. The host refuses, with an
    /// explanation, while another call holds the slot.
    public func startCall(audio: Bool = true, video: Bool = false) {
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
        // `docs/ux/calls.md` section 3 — the button the caller tapped decides
        // what happens next: a voice call goes straight onto the wire, a video
        // call shows the camera check first. The microphone is always asked
        // for; nothing else is asked at all.
        _ = audio
        callHost.startOutgoingCall(
            groupIdHex: groupIdHex, button: CallMediaChoice.button(video: video))
    }

    /// ``ChatScreen``'s long-poll `.task` started: this chat is open and polls
    /// itself, so the foreground receive skips it and what it applies is read.
    func chatScreenOpened() {
        repository.chatScreenOpened(groupIdHex: groupIdHex)
    }

    /// The matching close for ``chatScreenOpened()``.
    func chatScreenClosed() {
        repository.chatScreenClosed(groupIdHex: groupIdHex)
    }

    /// The app-level 1x1 call host (shared by every chat screen).
    let callHost: CallHost

    /// The app-level group (SFU) call host. The group call lives there, not on
    /// this view model: the chat is replaced wholesale by navigation, and a
    /// call owned here went with it (see ``GroupCallHostRules``).
    let groupCallHost: GroupCallHost

    /// The app's active 1x1 call, on any chat, or `nil`. Read-only here: the
    /// root view presents and drives it.
    public var activeCall: CallSession? { callHost.activeCall }

    /// `docs/ux/calls.md` section 2.1.1 — a muted conversation's ringing call,
    /// when it is THIS conversation's, or `nil`.
    ///
    /// The one surface the root view cannot own: every other call surface is
    /// presented app-wide from ``CallHostOverlay``, but this one has to sit in
    /// this chat's own layout, above the message list, so that nothing the
    /// person is reading moves or is covered. The screen re-renders on the
    /// host's changes already (see the subscription in `init`), so the panel
    /// appears and — when the ring window runs out and the host clears the
    /// slot — goes away with no timer of its own.
    var quietIncomingCall: QuietIncomingCall? {
        let offer = callHost.quietIncomingCall
        return QuietIncomingCallPresentation.showsInConversation(
            offer, openGroupIdHex: groupIdHex) ? offer : nil
    }

    /// Answer the muted conversation's call from the in-chat panel. The same
    /// entry point the full-screen incoming call and the phone's own Answer
    /// button drive.
    func acceptQuietIncomingCall(withVideo: Bool) {
        callHost.acceptIncomingCall(withVideo: withVideo)
    }

    /// Refuse it, through the same one decline path.
    func declineQuietIncomingCall() {
        callHost.declineIncomingCall()
    }

    /// Any call — 1x1, group or call link — holds the app's one call slot, so
    /// this chat's call controls stand down.
    var anyCallInProgress: Bool {
        callHost.occupiesSlot || callHost.otherCallBusy || groupCallHost.occupiesSlot
    }

    /// `docs/ux/media-messages.md` — a call is HOLDING the microphone and
    /// camera, so the composer's recorder stands aside.
    ///
    /// Not the same question as ``anyCallInProgress``, and that is the whole
    /// point: an offer merely being rung takes the call slot — so the call
    /// buttons rightly stand down — while taking no microphone, so a voice
    /// message must still be recordable. ``CallCapture`` is the seam onto the
    /// shared rule; this only reads the facts.
    ///
    /// Every fact is read from the app-level call hosts, so a call collapsed
    /// into the bar of `docs/ux/calls.md` section 4.5 holds the microphone in
    /// THIS chat too, not only in the one it was placed from. Nothing here is
    /// compared against ``groupIdHex``.
    var callOwnsCapture: Bool {
        CallCapture.ownsCapture(
            callIsLive: activeCall != nil || anyCallInProgress,
            oneToOneState: callHost.activeCallState,
            otherKindInSlot: callHost.otherCallBusy || groupCallHost.occupiesSlot)
    }

    /// A group call start / join on THIS chat is past its tap but not yet
    /// holding a session.
    var groupCallStarting: Bool { groupCallHost.startingGroupIdHex == groupIdHex }

    /// This chat's ongoing group call as the repository observed it on the
    /// journal, or `nil`. Liveness is time-dependent, so the screen evaluates
    /// ``groupCallAffordance(nowMs:)`` rather than reading this directly.
    @Published private(set) var ongoingGroupCall: OngoingGroupCall?

    /// `true` when this chat has >2 MLS members — the call button then
    /// routes through the SFU group-call path; a 2-member chat stays on
    /// the 1×1 P2P `CallSession`. `nil` member count (no handle) reads
    /// as a 1×1.
    var isGroupChat: Bool {
        (repository.memberCount(groupIdHex: groupIdHex) ?? 0) > 2
    }

    /// UX spec section 5.1 — the list this conversation belongs to, from the
    /// persisted kind (``ConversationKindDecision``). This is the
    /// PRESENTATION notion: it decides sender names in bubbles, the "N
    /// members" subtitle and the members-sheet title. Call routing keeps
    /// ``isGroupChat`` (the live people count), because a two-person group
    /// made through the new-group flow is still a 1x1 call.
    var conversationKind: ConversationKind {
        let account = repository.accountIdB64
        let stored = account.flatMap {
            repository.conversationKindStore.load(accountIdB64: $0, groupIdHex: groupIdHex)
        }
        let verdict = ConversationKindDecision.resolve(
            stored: stored, peopleCount: repository.memberCount(groupIdHex: groupIdHex))
        if let persist = verdict.persist, let account {
            repository.conversationKindStore.record(
                accountIdB64: account, groupIdHex: groupIdHex, kind: persist)
        }
        return verdict.kind
    }

    /// `true` when the conversation is presented as a group.
    var presentsAsGroup: Bool { conversationKind == .group }

    /// UX spec section 5.1 — record that this conversation is a group: the
    /// new-group path calls it when the picker lands, and the first add to a
    /// 1x1 calls it. One-way; a group never becomes a chat again.
    func markConversationAsGroup() {
        guard let account = repository.accountIdB64 else { return }
        repository.conversationKindStore.record(
            accountIdB64: account, groupIdHex: groupIdHex, kind: .group)
        objectWillChange.send()
    }

    /// The banner / toolbar decision at `nowMs`: is there a live group call on
    /// this chat that this device is not in?
    /// Re-evaluate the banner while a record exists.
    ///
    /// Liveness is a function of the clock as much as of the record: a call is
    /// live while somebody's newest heartbeat is under 150 s old, and that stops
    /// being true with no new row arriving to say so. Everything here is driven
    /// by `@Published` changes, so without a tick the last verdict computed
    /// stands indefinitely — a call that ended keeps offering a join, and a
    /// record that was stale when it landed never recovers even after the
    /// heartbeats that would have refreshed it.
    ///
    /// The ticker exists only while there is a record to judge, and is torn
    /// down the moment there is not, so an idle chat costs nothing.
    private func trackGroupCallFreshness(_ record: OngoingGroupCall?) {
        groupCallFreshnessTask?.cancel()
        guard record != nil else {
            groupCallFreshnessTask = nil
            return
        }
        groupCallFreshnessTask = Task { @MainActor [weak self] in
            self?.probeJoinableGroupCall()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Self.groupCallFreshnessTickMs) * 1_000_000)
                guard !Task.isCancelled, let self, let current = self.ongoingGroupCall else { return }
                // The record has not changed, so nothing republishes it; tell
                // the view to re-read a verdict whose inputs include the time.
                self.objectWillChange.send()
                self.logGroupCallVerdict(current)
                self.probeJoinableGroupCall()
            }
        }
    }

    /// While the banner offers a call to join, ask the SFU whether it is still
    /// there; the host closes the record when the answer is `ended`
    /// (`SfuCallState.swift`, `docs/ux/calls.md` section 4.3).
    private func probeJoinableGroupCall() {
        guard case .joinable(let callId, _, _) = groupCallAffordance() else { return }
        let callIdHex = callId.map { String(format: "%02x", $0) }.joined()
        let host = groupCallHost
        let groupIdHex = self.groupIdHex
        Task { @MainActor in
            await host.probeCallState(groupIdHex: groupIdHex, callIdHex: callIdHex)
        }
    }

    /// How often the banner re-reads a verdict that no row has changed, and
    /// asks the SFU whether the call is still there: the probe interval of
    /// `SfuCallStateProbe`, so a stale banner lives at most this long past the
    /// SFU's own five-second grace.
    static let groupCallFreshnessTickMs: Int64 = SfuCallStateProbe.intervalMs

    /// The live-or-stale judgement, printed whenever this chat's record changes
    /// — which is the only moment it can change, since nothing re-evaluates it
    /// on a timer.
    ///
    /// It prints the numbers rather than the verdict alone, because "no banner"
    /// has three completely different causes that look identical on screen: the
    /// record never arrived (no line at all), it arrived and was judged stale
    /// (a line with an age past the budget), or it was judged live and the
    /// banner still did not render (a line saying `joinable` with no banner).
    ///
    /// `ageMs` is the one to read first. It is `this device's clock` minus `the
    /// DS server's row stamp`, so it is only a measure of staleness while those
    /// two clocks agree; a wildly large or negative age is skew, not a call that
    /// ended.
    private func logGroupCallVerdict(_ record: OngoingGroupCall?) {
        guard let record else {
            NSLog("group-call verdict: no record for this chat")
            return
        }
        let nowMs = ChatRepository.nowMs()
        let own = repository.ownDeviceKeyB64(groupIdHex: groupIdHex)
        let newest = record.newestLastSeenMs(ignoringDevice: own)
        // `startAgeMs` is the second half of the rule and is easy to forget:
        // liveness is "started within 4 h AND (somebody is fresh OR the start
        // itself is under the 60 s unjoined grace)". So a call can be live with
        // nobody joined for its first minute, and after that minute it is live
        // only if Join rows actually folded — which is exactly the difference
        // between a device that saw the announce and one that saw the whole
        // conversation.
        NSLog(
            "group-call verdict call=%@ joined=%d ageMs=%@ freshBudgetMs=%d startAgeMs=%d graceMs=%d live=%@ affordance=%@",
            GroupCallSignalLog.shortId(record.callId),
            record.joined.count,
            newest.map { "\(nowMs &- $0)" } ?? "none",
            GroupCallReducer.memberFreshMs,
            nowMs &- record.startedAtMs,
            GroupCallReducer.unjoinedGraceMs,
            GroupCallReducer.isLive(record, nowMs: nowMs, ignoringDevice: own) ? "true" : "false",
            "\(groupCallAffordance(nowMs: nowMs))")
    }

    func groupCallAffordance(nowMs: Int64 = ChatRepository.nowMs()) -> GroupCallAffordance {
        GroupCallAffordance.of(
            record: ongoingGroupCall,
            activeGroupCallId: groupCallHost.activeCallId(groupIdHex: groupIdHex),
            ownDeviceKey: repository.ownDeviceKeyB64(groupIdHex: groupIdHex),
            nowMs: nowMs
        )
    }

    /// What the toolbar call buttons do right now.
    var chatCallAction: GroupCallHostRules.ChatCallAction {
        GroupCallHostRules.chatCallAction(
            membershipEnded: membershipHasEnded,
            callInProgress: anyCallInProgress,
            affordance: groupCallAffordance(),
            memberCount: repository.memberCount(groupIdHex: groupIdHex)
        )
    }

    /// The toolbar call buttons. A live group call this device is not in is
    /// JOINED rather than competed with by a second call id; otherwise a group
    /// chat starts an SFU call and a 1x1 chat a P2P one.
    func onCallButton(video: Bool) {
        switch chatCallAction {
        case .unavailable:
            return
        case .joinGroup(let callId):
            groupCallHost.start(groupIdHex: groupIdHex, video: video, joinCallId: callId)
        case .startGroup:
            groupCallHost.start(groupIdHex: groupIdHex, video: video, joinCallId: nil)
        case .startOneToOne:
            startCall(audio: true, video: video)
        }
    }

    /// Start a multi-party SFU group call for this (group) chat through the
    /// app-level ``GroupCallHost``. A live call already on this chat is joined
    /// instead.
    func startGroupCall(video: Bool = false) {
        if case .joinable = groupCallAffordance() {
            joinOngoingGroupCall(video: video)
            return
        }
        guard !membershipHasEnded else { return }
        groupCallHost.start(groupIdHex: groupIdHex, video: video, joinCallId: nil)
    }

    /// Join the live group call the banner shows. The host polls the group once
    /// first so the exporter seed is derived at the epoch the other participants
    /// are at; a later epoch change is absorbed by the host's mid-call rekey.
    func joinOngoingGroupCall(video: Bool = false) {
        guard case .joinable(let callId, _, _) = groupCallAffordance() else { return }
        guard !membershipHasEnded else { return }
        groupCallHost.start(groupIdHex: groupIdHex, video: video, joinCallId: callId)
    }

    // MARK: - Group membership actions (A4)

    /// Open this conversation's info (`docs/ux/conversation-info.md`
    /// section 1). Refreshes the roster when what opens is the group's.
    func openConversationInfo() {
        conversationInfoOpen = true
        membershipError = nil
        // A notice describes one action's outcome, so it must not outlive
        // the panel that produced it: the pending row annotation is the
        // durable signal, not this.
        membershipNotice = nil
        // A conversation between two people opens the other person, which has
        // no roster to read.
        if presentsAsGroup { refreshMembers() }
    }

    /// Close the info layer. The conversation is still behind it (section 3).
    func closeConversationInfo() {
        conversationInfoOpen = false
    }

    /// The person on the other side of a conversation between two people, as
    /// the key their messages are signed with. `nil` in a group, or while the
    /// roster cannot be read.
    var oneToOnePeerSignatureKey: Data? {
        repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
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
    }

    // MARK: - ADR-0045 UX §7 — the automatic re-send of a queue-full refusal
    //
    // Every rule lives in `MembershipRetry.swift` and is tested there; what is
    // here is the wiring that feeds it live group state and re-issues what it
    // decides to fire.

    private let retryStore = MembershipRetryStore()

    /// The `(account, group)` scope every armed retry is filed under, or `nil`
    /// outside a session, where there is nothing to file.
    private func retryScope() -> String? {
        guard let account = repository.accountIdB64 else { return nil }
        return MembershipRetryStore.scopeKey(accountIdB64: account, groupIdHex: groupIdHex)
    }

    /// The group as the retry verdict reads it, captured in ONE pass — the
    /// roster and the pending set must come from the same moment, because the
    /// merge that empties one is the merge that moves the other.
    ///
    /// A `nil` roster is a TORN read, not an empty group, and the verdict
    /// treats it as claiming nothing.
    private func retrySnapshot() -> RetryGroupSnapshot {
        let roster = repository.members(groupIdHex: groupIdHex)?.map {
            RetryRosterMember(
                leafIndex: $0.leafIndex,
                signatureKeyB64: base64Encode($0.signaturePublicKey))
        }
        let table = repository.groupRoles(groupIdHex: groupIdHex).map {
            RetryRoleTable(
                ownerKeyB64: base64Encode($0.owner),
                adminKeysB64: $0.admins.map { base64Encode($0) })
        }
        let pending = repository.pendingProposals(groupIdHex: groupIdHex)
        return RetryGroupSnapshot(
            roster: roster,
            pendingKeys: pending.map { proposalKey($0) },
            pendingCount: pending.count,
            roleTable: table,
            membershipEnded: membershipHasEnded,
            retainedJoinKeys: retainedJoinKeysNow())
    }

    /// ADR-0027 — the joiner key of every request the core still holds in its
    /// retained external-join quarantine, or `nil` on a torn read.
    ///
    /// Deliberately NOT ``refreshedPendingJoins()``: that one folds a failed
    /// read into an empty list (correct for a panel, which then renders
    /// nothing) and gates on `membershipHasEnded`. Here an empty list and a
    /// failed read are different answers — the first says the request is gone,
    /// the second says nothing — and conflating them would drop a live retry.
    private func retainedJoinKeysNow() -> [String]? {
        repository.retainedExternalJoinKeys(groupIdHex: groupIdHex)?
            .map { base64Encode($0) }
    }

    /// Claim the slot for `request` before a MANUAL attempt starts.
    ///
    /// This is half of the exactly-once property and the half that is easy to
    /// miss: the re-issue goes through the repository's own per-group commit
    /// lock, so the driver cannot hold that across its own last check. A user
    /// tapping again therefore takes the record away rather than racing an
    /// in-flight retry for it.
    private func disarmRetry(_ request: RetryableRequest) {
        guard let scope = retryScope() else { return }
        _ = retryStore.claim(scope: scope, key: request.key)
    }

    /// Arm an automatic re-send if — and only if — the CORE says this refusal
    /// resolves on its own.
    ///
    /// Nothing here lists a reason. ``armedRetry(for:request:pendingCountAtRefusal:reason:retryMaySucceed:)``
    /// asks `membershipRefusalRetryMaySucceed`, so a `DoNotRetry` refusal, a
    /// `NotPermitted` one, an unrecognised reason from a newer core and an
    /// ordinary error all take the same path: no record, nothing retried.
    private func armRetryIfQueueFull(
        _ request: RetryableRequest,
        _ error: Error,
        pendingCountAtRefusal: Int
    ) {
        guard let scope = retryScope() else { return }
        guard
            let armed = armedRetry(
                for: error,
                request: request,
                pendingCountAtRefusal: pendingCountAtRefusal,
                retryMaySucceed: { membershipRefusalRetryMaySucceed(reason: $0) })
        else { return }
        retryStore.arm(scope: scope, armed)
    }

    /// Run one membership request, arming a re-send if a full queue refused it.
    ///
    /// Wrapping rather than editing each handler's body keeps the arm and the
    /// disarm adjacent to each other and to the call, which is what stops a new
    /// handler from arming without disarming.
    private func withMembershipRetry<T>(
        _ request: RetryableRequest,
        _ run: () async throws -> T
    ) async throws -> T {
        disarmRetry(request)
        let before = repository.pendingProposals(groupIdHex: groupIdHex).count
        do {
            return try await run()
        } catch {
            armRetryIfQueueFull(request, error, pendingCountAtRefusal: before)
            throw error
        }
    }

    /// The approve-an-external-join twin of ``withMembershipRetry(_:_:)``.
    ///
    /// Separate because the arming gate is: ``armedApproveJoinRetry`` also asks
    /// ``externalJoinAffordance(retained:)`` whether the request is one an
    /// approve could act on at all, and the `retained` answer is read LIVE off
    /// the core's quarantine rather than assumed. A torn read of that set
    /// answers `false`, which is the fail-safe direction — it can only decline
    /// to arm, never arm a retry the core has nothing to look up.
    private func withApproveJoinRetry<T>(
        _ proposalRef: Data,
        _ run: () async throws -> T
    ) async throws -> T {
        let subjectKeyB64 = base64Encode(proposalRef)
        disarmRetry(.approveJoin(subjectKeyB64: subjectKeyB64))
        let before = repository.pendingProposals(groupIdHex: groupIdHex).count
        do {
            return try await run()
        } catch {
            if let scope = retryScope(),
                let armed = armedApproveJoinRetry(
                    for: error,
                    subjectKeyB64: subjectKeyB64,
                    pendingCountAtRefusal: before,
                    retained: retainedJoinKeysNow()?.contains(subjectKeyB64) ?? false,
                    retryMaySucceed: { membershipRefusalRetryMaySucceed(reason: $0) })
            {
                retryStore.arm(scope: scope, armed)
            }
            throw error
        }
    }

    /// Re-issue one armed request, reconstructed from its descriptor.
    ///
    /// **Reconstructed, never a stored closure.** The descriptor is exactly
    /// what the request is; anything a closure would additionally capture (a
    /// roster snapshot, a leaf index, a KeyPackage) is stale by the time the
    /// queue drains. In particular a removal's LEAF is resolved from the live
    /// roster here, because OpenMLS hands a vacated leaf to the next member
    /// added and a stored one would eventually remove the wrong person.
    private func reissueMembershipRequest(_ request: RetryableRequest) async throws {
        switch request {
        case .add(let keyB64, let accountIdB64):
            guard let key = base64Decode(keyB64), let account = base64Decode(accountIdB64)
            else { return }
            _ = try await repository.addMemberToGroup(
                groupIdHex: groupIdHex,
                recipientAccountId: account,
                recipientSignaturePubkey: key)
        case .remove(let keyB64), .leave(let keyB64):
            // The verdict already dropped a request whose subject has left, so
            // an unresolvable leaf here is a torn read between the verdict and
            // now. Refusing to guess is the only safe answer: any fallback leaf
            // names somebody, and for a removal that somebody is a real person.
            guard
                let leaf = repository.members(groupIdHex: groupIdHex)?
                    .first(where: { base64Encode($0.signaturePublicKey) == keyB64 })?
                    .leafIndex
            else { return }
            _ = try await repository.removeMemberFromGroup(
                groupIdHex: groupIdHex, leafIndex: leaf)
        case .setAdmin(let keyB64, let makeAdmin):
            guard let key = base64Decode(keyB64) else { return }
            _ = try await repository.setMemberAdmin(
                groupIdHex: groupIdHex,
                memberSignaturePubkey: key,
                makeAdmin: makeAdmin)
        case .transferOwnership(let keyB64, let outgoing):
            guard let key = base64Decode(keyB64) else { return }
            _ = try await repository.transferGroupOwnership(
                groupIdHex: groupIdHex,
                newOwnerSignaturePubkey: key,
                outgoing: outgoing)
        case .approveJoin(let keyB64):
            // The joiner's signature key IS the `proposal_ref` handle, so the
            // descriptor reconstructs the call whole. The KeyPackage it needs
            // is the one the core retained — which is also why the verdict
            // refuses to fire once that request has left the quarantine.
            guard let proposalRef = base64Decode(keyB64) else { return }
            _ = try await repository.approveExternalJoin(
                groupIdHex: groupIdHex, proposalRef: proposalRef)
            // `unkeptJoinRequest` is deliberately NOT cleared here. The manual
            // handler clears it as feedback for something the user just did; a
            // background re-send is not that, and the notice's action item —
            // ask that person to send their request again — stays true
            // regardless, because a request the quarantine never kept is not
            // recoverable by approving a different one.
        }
    }

    /// The driver: evaluate every armed request against the group as it is now.
    ///
    /// **Driven by observation, never by a timer.** It runs from
    /// ``refreshPendingRequests()``, which is itself driven by every roster
    /// refresh and by the beacon tick — i.e. by exactly the events that can
    /// drain a queue, plus a cadence that is already paid for. A quiet group
    /// produces no attempts at all.
    ///
    /// Free when nothing is armed, which is the overwhelmingly common case.
    private func evaluateArmedRetries() {
        guard let scope = retryScope() else { return }
        // Stand down while a MANUAL membership operation is in flight.
        //
        // The disarm-on-tap covers every handler that knows its descriptor up
        // front, but `onAddMember` cannot: the request only becomes nameable
        // once the typed handle has resolved, so between the tap and the
        // resolution there is a window in which a retry for the same person
        // could fire behind the user's own attempt. Standing down costs at
        // most one refresh — the roster refresh that follows the manual
        // operation, or the beacon tick, re-evaluates.
        if membershipBusy { return }
        let armed = retryStore.armed(scope: scope)
        if armed.isEmpty { return }
        let view = retrySnapshot()
        for entry in armed {
            let verdict = retryVerdict(entry, view)
            if verdict == .wait { continue }
            // Claim first, decide second — the claim is the exactly-once step,
            // and a `drop` claims for the same reason a `fire` does: leaving a
            // dropped record in the store would re-evaluate it on every future
            // refresh.
            guard let claimed = retryStore.claim(scope: scope, key: entry.request.key)
            else { continue }
            if case .drop = verdict { continue }
            Task { @MainActor in
                do {
                    try await reissueMembershipRequest(claimed.request)
                    _ = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                    refreshMembers()
                } catch is CancellationError {
                    // The screen went away mid-retry. NOT a failure of the
                    // request and not something to report: turning a
                    // cancellation into an error message is the same class of
                    // mistake ADR-0044 recorded on the commit path. The record
                    // is already claimed, so this attempt is simply lost —
                    // which is exactly the in-memory contract.
                    return
                } catch {
                    // A second queue-full refusal re-arms against the NEW count
                    // and waits for the next drain — silently, because nothing
                    // has changed for the user since the refusal they were
                    // already shown.
                    //
                    // An approve re-arms through its OWN helper, so the
                    // affordance gate runs on this path too. Defence in depth
                    // rather than a hole being closed — `approve_external_join`
                    // looks the request up BEFORE it proposes, so a queue-full
                    // refusal proves it was still retained — but the re-arm is
                    // part of the retry path, and a second way to arm one that
                    // skipped a gate the first way applies is the shape that
                    // eventually diverges.
                    let count = repository.pendingProposals(groupIdHex: groupIdHex).count
                    let port: (FfiRefusalReason) -> Bool = {
                        membershipRefusalRetryMaySucceed(reason: $0)
                    }
                    let again: ArmedRetry?
                    if case .approveJoin(let joinerKeyB64) = claimed.request {
                        again = armedApproveJoinRetry(
                            for: error,
                            subjectKeyB64: joinerKeyB64,
                            pendingCountAtRefusal: count,
                            retained: retainedJoinKeysNow()?.contains(joinerKeyB64) ?? false,
                            retryMaySucceed: port)
                    } else {
                        again = armedRetry(
                            for: error,
                            request: claimed.request,
                            pendingCountAtRefusal: count,
                            retryMaySucceed: port)
                    }
                    if let rearmed = again {
                        retryStore.arm(scope: scope, rearmed)
                    } else {
                        // Anything else is a real failure of a request the user
                        // made and was not present for, so it replaces the
                        // stale message rather than being logged away.
                        reportMembershipFailure(error, op: "membership retry")
                    }
                }
            }
        }
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
        // ADR-0045 UX §7 — and the last thing, against the state just read: a
        // queue that drained since a refusal is exactly what this pass would
        // have observed.
        evaluateArmedRetries()
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
        // The local leaf is not in the contact directory, so the resolver
        // would fall back to "Member"; the row is simply "You".
        if ownLeafIndex == member.leafIndex { return "You" }
        // ADR-0048 — the member list has room for the verified username under
        // the name, so it shows the list form (`Alice Petrova · @alice`); the
        // resolver already falls back to the fingerprint WITHOUT `@`.
        return ProfileDirectory.shared.resolve(signatureKey: member.signaturePublicKey).listLabel
    }

    /// ADR-0048 — the bare resolved name for a signature key (base64), for
    /// surfaces with no room for a username: bubbles, typing, reactions,
    /// call tiles. `nil` for a key that is not base64.
    func personName(signatureKeyB64: MemberIdentity) -> String? {
        ProfileDirectory.shared.resolve(signatureKeyB64: signatureKeyB64)?.name
    }

    /// ADR-0048 — the resolved name of the other person in a 1x1 chat, or `nil`
    /// for a group or an unreadable roster.
    var oneToOnePeerName: String? {
        repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
            .map { ProfileDirectory.shared.resolve(signatureKey: $0).name }
    }

    /// The header title (UX spec section 5.2): the other person's name in a
    /// 1x1 chat; in a group the shared group name (ADR-0051 §5), else the
    /// members' names. Never the group id.
    var conversationTitle: String {
        // ADR-0051 §5 — a shared name wins for ANY conversation that has one,
        // whatever its kind or member count: a two-person conversation renamed
        // from another client is still keyed as a chat here.
        if let shared = groupName { return shared }
        if !presentsAsGroup, let peer = oneToOnePeerName { return peer }
        return ConversationTitle.groupTitle(
            otherMemberNames: ChatListViewModel.otherMemberNames(
                repository: repository, groupIdHex: groupIdHex))
    }

    // MARK: - ADR-0051 group name

    /// The shared group name this device holds, or `nil` (member-derived
    /// title). Read from the repository's store on every access so an accepted
    /// envelope re-titles in place (``ChatRepository/groupNameRevision`` drives
    /// the re-render).
    var groupName: String? {
        repository.groupName(groupIdHex: groupIdHex)
    }

    /// ADR-0051 §3 — may this device rename the group? The owner and admins
    /// (or any member of a legacy group with no role table), read from the LIVE
    /// group, never a cached role — and nobody once their membership ended. A
    /// plain member sees the name read-only.
    var canRenameGroup: Bool {
        repository.ownRole(groupIdHex: groupIdHex) != .member && !membershipHasEnded
    }

    /// ADR-0051 §3 — send the new name (an empty string clears it). The store
    /// and the title change only after the send succeeded; a failure returns
    /// the error copy the members panel shows and leaves the title as it was.
    func saveGroupName(_ name: String) async -> String? {
        do {
            try await repository.setGroupName(groupIdHex: groupIdHex, name: name)
            objectWillChange.send()
            return nil
        } catch {
            NSLog("ADR-0051 rename failed: \(error)")
            return "Could not save the name. Check your connection and try again."
        }
    }

    /// The new-group flow's hand-off (docs/ux/new-group.md §3): the chat opened
    /// with the members that did land, and says which adds did not. Rendered in
    /// the members panel's ungated notice slot.
    func noteSetupNotice(_ text: String) {
        membershipNotice = text
    }

    /// The header's second line: "@username" in a 1x1 chat when the other
    /// person's username is verified, "N members" in a group, else nothing.
    var conversationSubtitle: String? {
        // ADR-0051 — under a shared name the peer's handle is not the second
        // line (the title is no longer the peer); the member count is.
        if !presentsAsGroup, groupName == nil,
           let peer = repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) {
            let resolved = ProfileDirectory.shared.resolve(signatureKey: peer)
            guard let username = resolved.verifiedUsername, resolved.source != .verifiedUsername
            else { return nil }
            return "@\(username)"
        }
        guard let count = repository.memberCount(groupIdHex: groupIdHex) else { return nil }
        return ConversationTitle.memberCountLabel(count)
    }

    /// Resolve the typed `@username` (G-4-verified) and add it to this group:
    /// publish the add commit to the journal (own-marked) + ferry the Welcome.
    /// Fail-closed — a resolve / verify / publish failure surfaces an error and
    /// leaves the group untouched.
    func onAddMember() {
        // ADR-0048 §4 — no lookup for a name that fails the username rule; the
        // reason is already on screen under the field.
        guard case .valid(let username) = checkUsername(addMemberInput) else {
            if let message = checkUsername(addMemberInput).inlineMessage {
                membershipError = message
            }
            return
        }
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                let resolved = try await resolveRecipient(username)
                // ADR-0045 UX §7 — the descriptor is only knowable once the
                // typed handle has resolved, which is why it is built here and
                // not before: re-resolving a username later could name a
                // different account if the handle has since been
                // re-registered, and adding the wrong person is not a retry.
                let outcome = try await withMembershipRetry(
                    .add(
                        subjectKeyB64: base64Encode(resolved.signaturePubkey),
                        subjectAccountIdB64: base64Encode(resolved.accountId))
                ) {
                    try await repository.addMemberToGroup(
                        groupIdHex: groupIdHex,
                        recipientAccountId: resolved.accountId,
                        recipientSignaturePubkey: resolved.signaturePubkey
                    )
                }
                addMemberInput = ""
                // UX spec section 5.1 — adding someone to a 1x1 makes it a
                // group, even while the add is still a pending request.
                markConversationAsGroup()
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
                // ADR-0045 UX §7 — armed against the member's SIGNATURE key,
                // resolved now and re-resolved to a leaf at fire time. A torn
                // roster read means no retry for this attempt rather than a
                // retry against a guess.
                let targetKeyB64 = repository.members(groupIdHex: groupIdHex)?
                    .first(where: { $0.leafIndex == leafIndex })
                    .map { base64Encode($0.signaturePublicKey) }
                if let targetKeyB64 {
                    _ = try await withMembershipRetry(.remove(subjectKeyB64: targetKeyB64)) {
                        try await repository.removeMemberFromGroup(
                            groupIdHex: groupIdHex, leafIndex: leafIndex)
                    }
                } else {
                    _ = try await repository.removeMemberFromGroup(
                        groupIdHex: groupIdHex, leafIndex: leafIndex)
                }
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
                // ADR-0045 UX §7 — keyed on our own SIGNATURE key, never on
                // the leaf: OpenMLS hands a vacated leaf to the next member
                // added, so a retry armed against a leaf could come to name
                // somebody else entirely.
                if let ownKeyB64 = ownSignatureKeyB64 {
                    _ = try await withMembershipRetry(.leave(subjectKeyB64: ownKeyB64)) {
                        try await repository.removeMemberFromGroup(
                            groupIdHex: groupIdHex, leafIndex: leaf)
                    }
                } else {
                    _ = try await repository.removeMemberFromGroup(
                        groupIdHex: groupIdHex, leafIndex: leaf)
                }
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
                _ = try await withMembershipRetry(
                    .transferOwnership(
                        subjectKeyB64: successor.signatureKeyB64, outgoing: outgoing)
                ) {
                    try await repository.transferGroupOwnership(
                        groupIdHex: groupIdHex,
                        newOwnerSignaturePubkey: successorKey,
                        outgoing: outgoing)
                }
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
                _ = try await withMembershipRetry(
                    .setAdmin(
                        subjectKeyB64: base64Encode(member.signaturePublicKey),
                        makeAdmin: makeAdmin)
                ) {
                    try await repository.setMemberAdmin(
                        groupIdHex: groupIdHex,
                        memberSignaturePubkey: member.signaturePublicKey,
                        makeAdmin: makeAdmin
                    )
                }
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
                // ADR-0045 UX §7 — an approve refused by a full pending set now
                // carries its reason (the core stopped flattening it into prose
                // in `21f6c8c`), so it re-sends itself when a committer drains
                // the queue. The wrapper also disarms first, so tapping Approve
                // again takes the record rather than racing an in-flight
                // re-send for it.
                _ = try await withApproveJoinRetry(proposalRef) {
                    try await repository.approveExternalJoin(
                        groupIdHex: groupIdHex,
                        proposalRef: proposalRef
                    )
                }
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
            // ADR-0045 UX §7 — a reject is the user saying no, so any armed
            // re-send of an approve for the same request goes with it. The
            // verdict would drop it anyway once it saw the emptied quarantine
            // (`joinRequestGone`), but leaving a record that contradicts a
            // decision the user just made is not a state to keep for a poll.
            disarmRetry(.approveJoin(subjectKeyB64: base64Encode(proposalRef)))
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
    /// ``DeviceGroupSyncRepository`` (Phase 2a3e2b). Called once
    /// where the model is made (`SessionRoot`). Subscribes to:
    ///
    ///   * incoming `Draft` payloads keyed on this thread —
    ///     seed `draft` once when local is empty (a non-empty
    ///     local draft is never overwritten).
    ///   * local `draft` changes — debounce 500 ms then
    ///     broadcast.
    ///   * local `messages` mutations — debounce 1.5 s then
    ///     broadcast a `ReadCursor` payload pointing at the
    ///     highest message-id (Phase 2a3e2c).
    public func attachSync(_ sync: DeviceGroupSyncRepository) {
        // Idempotent for the same repository: the subscriptions below must
        // not stack if a caller attaches twice.
        if deviceGroupSync === sync, groupIdBytes != nil { return }
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
        if conversationInfoOpen {
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
        // Cleared HERE rather than on success, because the message is now in
        // the conversation from this moment: the words live in the bubble,
        // which keeps them and offers Retry if the send does not land. They
        // used to be cleared only on success - which was right while a
        // failure left nothing in the conversation, and is wrong now that it
        // would leave the same text in two places.
        draft = ""
        pendingAttachments = []
        // ADR-0052 — the armed reply goes with the message and the strip
        // clears now, for the same reason the draft does: the row in the
        // conversation carries the quote from this moment, Retry included.
        let reply = takeArmedReply()
        Task {
            do {
                try await repository.sendMessage(
                    groupIdHex: groupIdHex,
                    plaintext: trimmed,
                    attachments: stagedAttachments,
                    reply: reply
                )
                sending = false
                sendError = nil
            } catch {
                sending = false
                // One sentence; the action is on the bubble, next to the
                // message it belongs to. Same words as the web client
                // (`chat/send-failure.ts`). A refusal that names itself keeps
                // its own words - "you are no longer a member of this group"
                // is the difference between retrying usefully and retrying
                // forever.
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Could not send."
            }
        }
    }

    /// Send a message that did not get there. Drives the bubble's Retry.
    public func onRetrySend(localId: Int64) {
        sendError = nil
        Task {
            do {
                try await repository.retrySend(groupIdHex: groupIdHex, localId: localId)
            } catch {
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Could not send."
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

    // MARK: - ADR-0052 — reply

    /// A reply armed in the composer (`docs/ux/message-actions.md` section
    /// 3.1): which message, and the snapshot taken when Reply was chosen.
    /// The snapshot is what the strip shows and what the envelope will carry,
    /// so a later edit or deletion of the quoted message changes nothing.
    public struct ReplyArmed: Equatable {
        public let messageId: Int64
        public let snapshot: ReplyRef
    }

    /// The reply currently armed, `nil` when the composer is plain.
    @Published public private(set) var replyingTo: ReplyArmed?

    /// Bumped every time the composer should take the keyboard (choosing
    /// Reply focuses the composer). A counter rather than a flag so two
    /// requests in a row both land; the screen observes changes to it.
    @Published public private(set) var composerFocusRequest: Int = 0

    /// Arm a reply to `message`.
    ///
    /// Section 3.1 — one thing at a time: an open edit ends (its draft is
    /// discarded, as Cancel would), and Reply on another message replaces the
    /// strip. A message without a server id has nothing to point at, so it is
    /// refused here as it is in the menu.
    func beginReply(_ message: ChatMessage) {
        guard message.messageId > 0 else { return }
        if editingMessage != nil { cancelEdit() }
        replyingTo = ReplyArmed(
            messageId: message.messageId,
            snapshot: ReplySnapshot.replySnapshot(
                for: message, authorLabel: replyAuthorLabel(for: message)))
        composerFocusRequest += 1
    }

    /// The strip's close control.
    func cancelReply() {
        replyingTo = nil
    }

    /// The armed reply, taken for a send: the strip clears as the message
    /// goes. Read by the text send and by the media send queue alike.
    func takeArmedReply() -> ReplyRef? {
        defer { replyingTo = nil }
        return replyingTo?.snapshot
    }

    // MARK: - ADR-0052 decision 2 — forward

    /// The message whose "Forward to" picker is open, `nil` when it is not
    /// (`docs/ux/message-actions.md` section 5.1). Drives the sheet.
    @Published public private(set) var forwardingMessage: ChatMessage?

    /// ADR-0052 / section 5.1 "tap to jump" — a message id the chat screen
    /// should scroll to and highlight as soon as it is on screen: set on the
    /// ORIGIN conversation's model when a forward's attribution is tapped
    /// elsewhere, consumed once by the screen. Published so a screen that is
    /// already showing picks it up as well as one that is about to appear.
    @Published public private(set) var pendingJump: Int64?

    /// The list model the picker's rows are built from — the same code the
    /// Chats tab runs, over the same repository, so the picker cannot disagree
    /// with the tab about what a conversation is called. Made on first use:
    /// most chats are never forwarded from.
    private lazy var forwardListModel = ChatListViewModel(repository: repository)

    /// Open the picker for `message`. Refused for a row that cannot be
    /// forwarded (the menu withholds Forward there too): no id, a tombstone,
    /// or a send that has not completed.
    func beginForward(_ message: ChatMessage) {
        guard message.messageId > 0, !message.deleted, message.ack == .sent else { return }
        forwardingMessage = message
    }

    /// Cancel (Back, the sheet swiped away) sends nothing.
    func cancelForward() {
        forwardingMessage = nil
    }

    /// Section 5.1 — the picker's rows: the chats and groups this device is a
    /// LIVE member of, in the Chats tab's own order (pinned first, newest
    /// next). Channels are not conversations a message can be forwarded into,
    /// and an ended membership cannot receive a send.
    func forwardTargets(
        flags: [String: FlagsState],
        readCursors: [String: ReadCursorState]
    ) -> [ConversationListRow] {
        let rows = ChatListScreen.chatRows(
            viewModel: forwardListModel, flags: flags, readCursors: readCursors)
        return ConversationList.sorted(rows.filter { $0.endedLine == nil })
    }

    /// Section 5.1 — what the timeline calls the ORIGINAL author of
    /// `message`, for the "Forwarded from" line a recipient will read. An own
    /// message carries this account's own profile name (or username): a
    /// recipient reading "You" would be reading the wrong person. Anyone
    /// else's carries their resolved name, else the other person's name in a
    /// 1x1. `nil` when nothing is known — the line then reads "Forwarded"
    /// rather than the roster's placeholder word.
    func forwardAuthorLabel(for message: ChatMessage) -> String? {
        switch message.direction {
        case .outgoing:
            let directory = ProfileDirectory.shared
            return directory.ownProfile?.displayName ?? directory.ownUsername
        case .incoming, .system:
            if let key = message.authorKeyB64,
               let resolved = ProfileDirectory.shared.resolve(signatureKeyB64: key),
               !resolved.isPlaceholder
            {
                return resolved.name
            }
            return oneToOnePeerName
        }
    }

    /// Picking a row: send `message` to `targetGroupIdHex` at once and close
    /// the picker. The caller then navigates to that conversation, where the
    /// forwarded message is the newest bubble.
    ///
    /// The attachments are downloaded, re-encrypted under fresh keys and
    /// uploaded again before the send (ADR-0052 decision 2, OQ1), so the
    /// target's row appears once they are — the same order a media send
    /// keeps. A failure before the send leaves no row anywhere; it is said
    /// once, here, in the send-error slot of the chat the person forwarded
    /// from, because the target chat has nothing to attach it to.
    func forward(_ message: ChatMessage, to targetGroupIdHex: String) {
        forwardingMessage = nil
        let payload = ForwardPayload.forwardPayload(
            of: message,
            authorLabel: forwardAuthorLabel(for: message),
            originGroupIdHex: groupIdHex)
        let repository = repository
        Task { @MainActor [weak self] in
            do {
                let refs = try await ForwardAttachments.reencryptAll(message.attachments)
                try await repository.sendMessage(
                    groupIdHex: targetGroupIdHex,
                    plaintext: payload.text,
                    attachments: refs,
                    forward: payload.forward)
            } catch {
                NSLog("ADR-0052 forward failed: \(error)")
                self?.sendError = "Forward failed. Try again."
            }
        }
    }

    /// Section 5.1 "tap to jump" — where a forward's attribution leads, or
    /// `nil` when it leads nowhere on this device: the forward carried no
    /// origin, this device does not hold that conversation, or holds it but
    /// not the message. `nil` means do nothing and say nothing (section 5: an
    /// explanation would confirm the conversation exists).
    func forwardOriginJump(of forward: ForwardRef) -> (groupIdHex: String, messageId: Int64)? {
        guard let origin = forward.originGroupIdHex, let messageId = forward.originMessageId,
              let thread = repository.state.threads.first(where: { $0.groupIdHex == origin }),
              thread.messages.contains(where: {
                  $0.messageId == messageId && !ChatTimeline.isHiddenSystemRow($0)
              })
        else { return nil }
        return (origin, messageId)
    }

    /// Ask this conversation's screen to scroll to `messageId` when it is on
    /// screen (set by the shell on the ORIGIN conversation's model before it
    /// navigates there).
    public func requestJump(to messageId: Int64) {
        pendingJump = messageId
    }

    /// The screen takes the pending jump exactly once.
    func takePendingJump() -> Int64? {
        defer { pendingJump = nil }
        return pendingJump
    }

    /// What the timeline calls the author of `message` (section 3.1): "You"
    /// for an own message; for anyone else the resolved name, falling back
    /// to the other person's name in a 1x1 and to the resolver's own
    /// fallback word in a group.
    func replyAuthorLabel(for message: ChatMessage) -> String {
        switch message.direction {
        case .outgoing:
            return "You"
        case .incoming, .system:
            if let key = message.authorKeyB64, let name = personName(signatureKeyB64: key) {
                return name
            }
            return oneToOnePeerName ?? "Member"
        }
    }

    /// Open the edit sheet for `message`, pre-filling the field
    /// with the current text. Only offered for own outgoing
    /// messages that carry a messageId.
    ///
    /// ADR-0052 — choosing Edit while a reply is armed drops the reply
    /// (section 3.1, "one thing at a time").
    func beginEdit(_ message: ChatMessage) {
        replyingTo = nil
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
    /// old receipt is idempotent. A mirror of the repository's durable,
    /// per-group ``ChatRepository/readWatermarksByGroup`` entry — the
    /// repository folds receipts at receipt for every group, so this screen
    /// never folds them itself.
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
        case .readReceipt:
            // Folded durably by the repository at receipt; this screen
            // follows ``ChatRepository/readWatermarksByGroup``.
            return
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
        // ADR-0048 — the one resolver, name only (no room for a username).
        return ProfileDirectory.shared.resolve(signatureKey: member.signaturePublicKey).name
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

    /// A picked item that could not be prepared for sending: one sentence
    /// in the composer's error line, nothing sent.
    public func reportAttachmentProblem(_ copy: String) {
        sendError = copy
    }

    /// UX spec section 10 — encrypt and upload a picked photo or video with
    /// the metadata the receiver needs before downloading: pixel size and a
    /// small JPEG preview uploaded as its own encrypted blob. The attachment
    /// carries no `kind`; its MIME type makes it a photo or video bubble. A
    /// preview that fails to upload is dropped, never the send.
    public func onMediaPrepared(_ prepared: PhotoVideoPrep.Prepared) async {
        uploading = true
        sendError = nil
        uploadingThumbnail = prepared.thumb
        let displayName = prepared.name
        uploadStage = .encrypting(fileName: displayName)
        defer {
            uploading = false
            uploadStage = nil
            uploadingThumbnail = nil
            if let file = prepared.fileURL {
                try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
            }
        }
        var thumbRef: ThumbRef?
        if let thumb = prepared.thumb {
            do {
                thumbRef = try await AttachmentBridge.encryptAndUploadThumb(
                    bytes: thumb,
                    mime: "image/jpeg",
                    width: prepared.thumbWidth,
                    height: prepared.thumbHeight
                )
            } catch {
                thumbRef = nil
            }
        }
        let meta = MediaAttachmentMeta(
            kind: nil,
            width: prepared.width,
            height: prepared.height,
            thumb: thumbRef
        )
        do {
            let ref: AttachmentRef
            if let bytes = prepared.bytes {
                ref = try await AttachmentBridge.encryptAndUpload(
                    bytes: bytes,
                    mime: prepared.mime,
                    name: prepared.name,
                    media: meta,
                    onStage: { [weak self] stage in
                        Task { @MainActor in
                            switch stage {
                            case .encrypting: self?.uploadStage = .encrypting(fileName: displayName)
                            case .uploading: self?.uploadStage = .uploading(fileName: displayName)
                            }
                        }
                    }
                )
                // Our own bytes: the sent bubble shows them without a download.
                MediaSessionCache.shared.store(bytes, key: MediaSessionCache.key(for: ref))
            } else if let file = prepared.fileURL {
                uploadStage = .uploading(fileName: displayName)
                ref = try await AttachmentBridge.encryptAndUpload(
                    fileURL: file,
                    mime: prepared.mime,
                    name: prepared.name,
                    media: meta
                )
                MediaSessionCache.shared.adoptVideoFile(file, key: MediaSessionCache.key(for: ref))
            } else {
                return
            }
            if let thumbRef, let thumb = prepared.thumb {
                MediaSessionCache.shared.store(thumb, key: MediaSessionCache.key(for: thumbRef))
            }
            pendingAttachments.append(ref)
        } catch let err as AttachmentBridgeError {
            sendError = err.errorDescription
        } catch {
            sendError = "Attachment failed: \(error)"
        }
    }

    /// ADR-0047 — what the per-chat media runtime needs, as closures over the
    /// long-lived repository and this chat's id, never over this view model,
    /// so the ``ChatMediaController`` holds no reference back to it.
    public func mediaPort() -> ChatMediaController.Port {
        let repository = repository
        let groupIdHex = groupIdHex
        return ChatMediaController.Port(
            sendAttachment: { ref, reply in
                try await repository.sendMessage(
                    groupIdHex: groupIdHex,
                    plaintext: "",
                    attachments: [ref],
                    reply: reply
                )
            },
            sessionToken: sessionTokenProvider,
            // ADR-0052 — a take recorded with a reply armed is sent as the
            // reply. Weak: the controller must hold nothing that keeps this
            // view model alive, and a screen that is gone has no strip.
            takeArmedReply: { [weak self] in self?.takeArmedReply() }
        )
    }

    /// ADR-0047 §4 — react with any single emoji grapheme from the picker.
    /// The picker can only produce those; this re-checks before anything is
    /// composed, so a malformed value never reaches the wire.
    func reactFromPicker(to message: ChatMessage, emoji: String) {
        guard EmojiIndex.isSingleEmojiGrapheme(emoji) else { return }
        react(to: message, emoji: emoji)
    }

    /// Drop a staged attachment from the pending tray. Index
    /// matches the order in ``pendingAttachments``.
    public func onRemovePendingAttachment(at index: Int) {
        guard pendingAttachments.indices.contains(index) else { return }
        pendingAttachments.remove(at: index)
    }

    /// One long-poll tick — used by ``ChatScreen``'s
    /// `.task { … }` modifier in a `while !Task.isCancelled` loop.
    /// Silent — no spinner for the full 30s long-poll window.
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
        // G-18 / ADR-0029 — the mid-call rekey no longer runs here: the
        // app-level ``GroupCallHost`` rekeys when the group's MLS chain drains,
        // which covers this poll and every other path an epoch can move on,
        // whether or not this chat is open.
        // ADR-0045 — the poll's tail ran a sweep, which may have landed a
        // membership change, and the batch may have carried a peer's proposal.
        // Either way the sheet's roster + pending set are stale.
        if conversationInfoOpen { refreshMembers() }
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
