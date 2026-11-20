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

    /// The group this screen sends into and polls: the conversation's
    /// CURRENT group (ADR-0073 R.5, the head). It changes, without the screen
    /// being rebuilt, when the conversation continues in a successor group
    /// (``followHead(_:)``).
    @Published public private(set) var groupIdHex: String
    /// ADR-0073 R.5 — the conversation this screen shows: the key of the
    /// draft, the read position and every other person-facing store. Never
    /// changes for the life of the model.
    public let conversationIdHex: String

    // MARK: - Form fields

    @Published public var draft: String = ""

    /// `docs/ux/media-messages.md` section 11 — the composer tray of this
    /// conversation: what is attached to the message being written, each item
    /// uploading as soon as it is added. Kept per conversation for the life
    /// of the app process (``ComposerTrays``), so it outlives this screen.
    public let tray: ComposerTrayStore

    // MARK: - Ephemeral status

    @Published public private(set) var sending: Bool = false
    @Published public private(set) var sendError: UiText?
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
    /// `true` while the group's info screen is on top of the stack
    /// (`docs/ux/conversation-info.md` section 3): the roster and the pending
    /// set are refreshed for it then.
    @Published public private(set) var conversationInfoOpen: Bool = false
    /// `true` while an add or remove commit is in flight (disables the form).
    @Published public private(set) var membershipBusy: Bool = false
    /// Last add/remove error surfaced to the sheet, else `nil`.
    @Published public private(set) var membershipError: UiText?
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
    /// membership holds. Not over a conversation that went from under this
    /// screen: the row says the messages are still here, and they are not.
    public var terminalRowCopy: TerminalRowCopy? {
        endedTitle == nil ? terminalRow(membership) : nil
    }

    /// §6.2.5 — what occupies the composer's slot. The draft is read live so a
    /// commit landing mid-sentence preserves what was actually typed.
    public var composerSlot: ComposerState {
        // ADR-0091 phase 2, `docs/ux/conversation-rejoin.md` 2.2 — while this
        // device waits to be brought back, the waiting note takes the
        // composer's slot (the draft kept, selectable).
        if !membershipHasEnded, let note = reentryNote {
            let worthKeeping = !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            return .withdrawn(
                notice: note.joined(separator: "\n"),
                draft: worthKeeping ? draft : nil,
                draftLabel: worthKeeping ? unsentDraftLabel : nil)
        }
        return composerState(membership, draft: draft)
    }

    /// ADR-0091 phase 2 — the waiting note's lines while this device is out
    /// of date in this chat (UX 2.2), else `nil`. Refreshed with the
    /// membership (every poll).
    @Published public private(set) var reentryNote: [String]?

    /// UX 2.2 — a waiting chat: what it cannot do is absent (no composer, no
    /// call buttons, no member changes, no Edit / Delete / React).
    public var reentryWaiting: Bool { reentryNote != nil && !membershipHasEnded }

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

    // MARK: - ADR-0087 section 9 — the conversation goes

    /// UX 9.1 — the dialog over the open conversation. `draft`: what the
    /// composer held, shown selectable, since it has nowhere else to go.
    public struct EndDialog: Equatable, Identifiable {
        public let name: String
        public let draft: String?
        public var id: String { name }
    }

    /// Set once when the group went from under an open conversation.
    @Published public var endDialog: EndDialog?
    /// The conversation is gone and nothing is left to say: back to Chats.
    @Published public private(set) var conversationGone = false
    /// A thread was seen, so its absence is an end and not a chat not yet
    /// loaded.
    private var sawThread = false
    /// The end of this conversation was answered here.
    private var endAnswered = false
    /// UX 9.1 — the conversation went from under this screen, and this is the
    /// name it had. The group and its stores are wiped while the dialog stands,
    /// so nothing else can say what it was called or that it ended: a group
    /// that is no longer held reads like one still being set up.
    @Published public private(set) var endedTitle: String?
    /// `Delete this chat` is running: the screen goes through its own path.
    private var deletingChat = false
    /// No more draft writes: the conversation's draft register was cleared.
    private var draftClosed = false

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
    @Published public private(set) var membershipNotice: UiText? {
        // Any new notice (or none) is not the leave notice until the leave
        // path says so, right after setting it.
        didSet { leaveNoticeShown = false }
    }
    /// The notice on screen is the leave request's (``MembershipActionNotice``).
    private var leaveNoticeShown = false
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
    /// ADR-0053 — the pending joiner keys the current `joinAttributions` map
    /// was read for; `nil` before the first read. See ``JoinAttributionRefresh``.
    private var attributionsFetchedFor: Set<Data>?
    /// An attribution read is in flight; a second is not started beside it.
    private var attributionsInFlight = false
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

    let repository: ChatRepository

    /// ADR-0059 section 6 — the shared-channels runtime, for the group-info
    /// "Share my channels" switch.
    var sharedChannelsHub: SharedChannelsHub { repository.sharedChannels }
    /// Supplies the current auth session token for minting the SFU
    /// call-admission JWT. Defaults to `{ nil }` (group calls then
    /// refuse, fail-closed); the composition root passes
    /// `{ authRepository.identitySecrets?.sessionToken }`.
    private let sessionTokenProvider: () -> String?
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
        sessionTokenProvider: @escaping () -> String? = { nil }
    ) {
        self.repository = repository
        // The route may name the conversation or ANY of its groups (a link
        // minted before the move still opens the conversation).
        self.conversationIdHex = repository.conversationIdHex(ofGroup: groupIdHex)
        self.groupIdHex = repository.headGroupIdHex(of: groupIdHex)
        self.tray = ComposerTrays.shared.store(for: repository.conversationIdHex(ofGroup: groupIdHex))
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
            .map { [weak self] calls in self.flatMap { calls[$0.groupIdHex] } }
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
        readWatermarks = repository.readWatermarks(groupIdHex: self.groupIdHex)
        repository.$readWatermarksByGroup
            .compactMap { [weak self] maps in self.flatMap { maps[$0.groupIdHex] } }
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
            guard let self, hex == self.groupIdHex else { return }
            Task { @MainActor [weak self] in
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
        // Section 11 — Send's state reads the tray.
        tray.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
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
        // ADR-0052 decision 3 — an accepted pin action (received or taken
        // here) rebuilds the pinned bar in place.
        repository.$pinRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        repository.$unseenReactionsRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        // ADR-0087 — the group's photo or allowed reactions changed.
        repository.$groupSettingsRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        // Reactions-and-emoji spec section 6 — the quick reaction (synced).
        QuickReactionSetting.shared.$chosen
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
                // `docs/ux/feed.md` section 5 — a reply chosen in the feed
                // arms the strip as this screen comes up.
                armPendingReply()
                // Newly visible — publish a receipt for whatever's
                // already loaded.
                maybePublishReadReceipt()
                // ADR-0045 S4b — start announcing that this device can sweep.
                startBeaconTicker()
                // ADR-0048 §2 trigger 4 — if someone here is still `Member`,
                // ask the group to introduce itself (the core's rule decides,
                // at most once a day per group; nothing is sent otherwise).
                requestProfilesIfDue()
                // UX 9.1 — the group went while another screen was on top.
                noteConversationEnd(threadPresent: threadKnown)
            } else {
                // The read position goes out now rather than 1.5 s later: an
                // app closed right after reading came back with the badge.
                flushPendingReadCursor()
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
        chatOpenCounted = true
        repository.chatScreenOpened(groupIdHex: groupIdHex)
        // ADR-0091 section 7 — a device back after a re-entry looks for its
        // gap history when the chat is opened (throttled to once a minute).
        let hex = groupIdHex
        Task { @MainActor [repository] in await repository.lookForReentryGapHistory(groupIdHex: hex) }
    }

    /// Whether this model currently counts as an open chat in the
    /// repository, so ``followHead(_:)`` moves the count with the head.
    private var chatOpenCounted = false

    /// The matching close for ``chatScreenOpened()``.
    func chatScreenClosed() {
        chatOpenCounted = false
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

    /// ADR-0062 — the list this conversation belongs to, read from the
    /// group's own GroupContext (`Group.groupKind()`), the same answer on
    /// every member's device and fixed at creation. It decides everything
    /// that distinguishes a one-to-one from a group: sender names in bubbles,
    /// the "N members" subtitle, the members-sheet title, the typing wording
    /// and call routing. A group whose kind cannot be read counts as a group.
    var conversationKind: ConversationKind {
        repository.conversationKind(groupIdHex: groupIdHex)
    }

    /// `true` when the conversation is presented as a group.
    var presentsAsGroup: Bool { conversationKind == .group }

    /// `true` for a group of any size — the call button then routes through
    /// the SFU group-call path; a one-to-one places a 1x1 P2P `CallSession`.
    /// Same read as ``conversationKind``: a two-person group calls over the
    /// SFU.
    var isGroupChat: Bool { presentsAsGroup }

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

    /// Whether anyone else is here to call (`docs/ux/group-and-channel-privacy.md`
    /// section 10). Without it the toolbar has no call buttons.
    var hasSomeoneToCall: Bool {
        GroupCallHostRules.hasSomeoneToCall(
            isGroup: isGroupChat, people: repository.memberCount(groupIdHex: groupIdHex))
    }

    /// What the toolbar call buttons do right now.
    var chatCallAction: GroupCallHostRules.ChatCallAction {
        GroupCallHostRules.chatCallAction(
            // ADR-0091 phase 2 UX 2.6 — a waiting chat places no call.
            membershipEnded: membershipHasEnded || reentryWaiting,
            callInProgress: anyCallInProgress,
            affordance: groupCallAffordance(),
            isGroup: isGroupChat,
            someoneToCall: hasSomeoneToCall
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

    /// The group's info is being opened from the conversation
    /// (`docs/ux/conversation-info.md` sections 1 and 3). A notice or an error
    /// describes one action's outcome, so it does not outlive the visit that
    /// produced it: the pending row annotation is the durable signal.
    func openConversationInfo() {
        membershipError = nil
        membershipNotice = nil
    }

    /// The info screen is on top of the stack (pushed, or uncovered by a
    /// back from a screen above it).
    func groupInfoAppeared() {
        conversationInfoOpen = true
        refreshMembers()
    }

    /// The info screen is covered or gone.
    func groupInfoDisappeared() {
        conversationInfoOpen = false
    }

    // MARK: - Add members (`docs/ux/conversation-info.md` section 2.4)

    /// The people in the group, by person (account) key.
    var memberPersonKeys: Set<Data> { Set(members.map(\.signaturePublicKey)) }

    /// ADR-0060 section 6 — keeps a grant a search read for the person with
    /// the given account key. Injectable for tests.
    var foundGrantKeeper: @MainActor (_ grant: FfiReachGrant, _ accountKey: Data) -> Void = { grant, key in
        ReachCenter.shared.keepFoundGrant(grant, accountKey: key)
    }

    /// The people with an add waiting for the sweep: not listed again.
    var pendingAddPersonKeys: Set<Data> {
        repository.pendingAddPersonKeys(groupIdHex: groupIdHex)
    }

    /// Add the people picked on the Add members screen: each through the
    /// conversation's add path (the same call New group makes for its later
    /// picks), one after another, then one sweep. No notice on success; the
    /// people who could not be added are named once, in place, in the info.
    func onAddMembers(_ picks: [AddMembersPick]) {
        guard !picks.isEmpty, !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        membershipNotice = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            let failed = await AddMembersRun.run(
                picks: picks,
                add: { [self] pick in
                    // ADR-0060 section 6 — a person the search found through
                    // a grant they handed out: the grant is kept first, so
                    // the committer delegations are signed with it and the
                    // sweep's Welcome can be written with it. A grant that
                    // cannot be kept is logged and the add goes on (the
                    // Welcome then waits for another grant).
                    if let grant = pick.foundGrant {
                        foundGrantKeeper(grant, pick.personKey)
                    }
                    // ADR-0045 UX §7 — armed against the person's key and
                    // account id, so a queue-full refusal re-sends by itself.
                    _ = try await withMembershipRetry(
                        .add(
                            subjectKeyB64: base64Encode(pick.personKey),
                            subjectAccountIdB64: base64Encode(pick.accountId))
                    ) {
                        try await repository.addMemberToGroup(
                            groupIdHex: groupIdHex,
                            recipientAccountId: pick.accountId,
                            recipientSignaturePubkey: pick.personKey,
                            openMailbox: pick.openMailbox)
                    }
                },
                sweep: { [self] in
                    _ = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                })
            refreshMembers()
            if !failed.isEmpty {
                let names = failed.map(\.name)
                membershipError = UiText {
                    Localization.text(
                        "chat.members.add.error.some_failed",
                        texts: ["names": Localization.joinList(names)])
                }
            }
        }
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
    ///
    /// ADR-0061 section 6 — the non-refusal case now renders from its kind
    /// too: the action's own `chat.error.*` key (``errorText(for:fallbackKey:)``).
    private func reportMembershipFailure(_ error: Error, op: String, key: String) {
        membershipError = Self.errorText(for: error, fallbackKey: key)
        NSLog("%@ failed: %@", op, error.localizedDescription)
    }

    /// What the person is told when an action fails (ADR-0061 section 6): a
    /// membership refusal keeps its curated copy, a device clock that is off
    /// and a used-up allowance say so, and anything else is the action's own
    /// catalog message. The error's own prose is for the log only.
    nonisolated static func errorText(for error: Error, fallbackKey: String) -> UiText {
        if let reason = membershipRefusalReason(of: error) {
            return UiText {
                membershipRefusal(
                    reason: reason,
                    retryMaySucceed: { membershipRefusalRetryMaySucceed(reason: $0) }
                ).message
            }
        }
        if DsClientError.clockSkewCopy(in: error) != nil { return .key("ds.clock_skew") }
        if AnonTokenUnavailable.isQuotaExhausted(error) { return .key("tokens.action_allowance_used") }
        if (error as? ContentSendError) == .pollRingChanged { return .key("poll.resend_refused") }
        return .key(fallbackKey)
    }

    func refreshMembers() {
        // ADR-0045 UX §6.2 — FIRST, because every gate below depends on it.
        // A panel open is one of the paths that can be the first thing to
        // observe the terminal state (the poll that carried the commit may
        // have landed while the sheet was closed).
        refreshMembership()
        // The last roster stands behind the end dialog: once the group is
        // wiped there is none to read, and an empty list reads "0 members".
        if let roster = repository.members(groupIdHex: groupIdHex) {
            members = roster
        } else if endedTitle == nil {
            members = []
        }
        let role = repository.ownRole(groupIdHex: groupIdHex)
        // ADR-0045 UX §6.2.7 — and NOBODY may manage members once their own
        // membership has ended. Gating here rather than at each of the six
        // render sites is deliberate: `Add member`, the invite block, every
        // `Remove` and every admin toggle are all refused by the core for an
        // inactive group, and one gate cannot be partly applied. Note the
        // difference from role-gating, which hides what SOMEONE ELSE may do —
        // this hides what nobody in this group may do any more.
        canManageMembers = role != .member && !membershipHasEnded && !reentryWaiting
        // The owner cannot reach the terminal state at all (§6.2.9 — the core
        // refuses a `Remove` targeting the owner and refuses an owner's own
        // leave), so this conjunction is unreachable rather than load-bearing.
        // It is written anyway for the reason the line above is: one gate
        // cannot be partly applied, and the per-member admin toggle this flag
        // drives would otherwise be the one membership control in the panel
        // that survived on an argument about a different rule.
        isOwner = role == .owner && !membershipHasEnded && !reentryWaiting
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
        // front, but an Add members run covers several people one after
        // another, so while it is in flight a retry for one of them could
        // fire behind the user's own attempt. Standing down costs at
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
                        reportMembershipFailure(error, op: "membership retry", key: "chat.error.membership_request")
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
        // ADR-0045 S7 residual 1 / ADR-0050 2.5a — the rows the repository's
        // superseded re-request recorded when it stopped re-requesting.
        unappliedAddRows = repository.unappliedAdds(groupIdHex: groupIdHex)
        // ADR-0045 UX §7 — and the last thing, against the state just read: a
        // queue that drained since a refusal is exactly what this pass would
        // have observed.
        evaluateArmedRetries()
    }

    /// ADR-0045 S7 residual 1 — dismiss one `Not applied` row. It stays
    /// dismissed: the detection re-runs on every fold, so without the tombstone
    /// the notice could not be dismissed at all.
    func onDismissUnapplied(rowKey: String) {
        repository.dismissUnapplied(groupIdHex: groupIdHex, rowKey: rowKey)
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
            attributionsFetchedFor = nil
            return
        }
        let pending = Set(pendingJoins)
        let hex = groupIdHex
        let decision = JoinAttributionRefresh.decide(
            pending: pending,
            fetchedFor: attributionsFetchedFor,
            inFlight: attributionsInFlight,
            allowRead: { self.repository.takeJoinAttributionRead(groupIdHex: hex) })
        switch decision {
        case .keep:
            return
        case .clear:
            joinAttributions = [:]
            attributionsFetchedFor = []
        case .fetch:
            attributionsInFlight = true
            Task { [weak self] in
                guard let self else { return }
                let attrs = await self.repository.fetchJoinAttributions(groupIdHex: hex)
                self.attributionsInFlight = false
                // A failed read records nothing, so a later refresh retries
                // (behind the same floor) and the map on screen stays.
                guard let attrs else { return }
                var map: [Data: JoinAttribution] = [:]
                for a in attrs {
                    map[a.joinerSignatureKey] = JoinAttribution(
                        issuerFingerprint: a.issuerFingerprint, issuerInRoster: a.issuerInRoster)
                }
                self.joinAttributions = map
                self.attributionsFetchedFor = pending
            }
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
        if ownLeafIndex == member.leafIndex { return Localization.text("chat.author.you") }
        // ADR-0048 — the member list has room for the verified username under
        // the name, so it shows the list form (`Alice Petrova · @alice`); the
        // resolver already falls back to the fingerprint WITHOUT `@`.
        return ProfileDirectory.shared.resolve(signatureKey: member.signaturePublicKey).listLabel
    }

    /// ADR-0053 — the label of a pending join request row: the contact's name
    /// resolved through the joiner's ACCOUNT key (the request itself is keyed
    /// on their device leaf key, which no contact is stored under), else the
    /// label the row has always shown for a stranger.
    /// ADR-0084 — whose photo a pending join row shows: the joiner's account
    /// key when the request names one, else its leaf key.
    func pendingJoinPhotoKey(_ ref: Data) -> Data {
        repository.pendingExternalJoinAccountKey(groupIdHex: groupIdHex, joinerSignatureKey: ref) ?? ref
    }

    /// ADR-0084 — whose photo a roster row shows (`nil`: this device, the
    /// own photo).
    func memberPhoto(_ member: GroupMember) -> AvatarPhoto {
        ownLeafIndex == member.leafIndex ? .own : .person(member.signaturePublicKey)
    }

    /// The bare name a roster row's initials are taken from (the list label
    /// carries `· @username`, which would read as a second initial).
    func memberInitialsName(_ member: GroupMember) -> String {
        if ownLeafIndex == member.leafIndex {
            return ProfileDirectory.shared.ownProfile?.displayName ?? ProfileDirectory.shared.ownUsername ?? ""
        }
        let resolved = ProfileDirectory.shared.resolve(signatureKey: member.signaturePublicKey)
        return resolved.isPlaceholder ? "" : resolved.name
    }

    func pendingJoinLabel(_ ref: Data) -> String {
        // UX section 8 — a contact shows their contact name; otherwise the
        // name the request carries (a claim, unverified, no mark).
        let carried = AcceptedContactsStore.boundAccountIdB64.flatMap {
            repository.groupSettingsStore.requestNames(accountIdB64: $0, groupIdHex: groupIdHex)[base64Encode(ref)]
        }
        return Self.pendingJoinLabel(
            accountKeyName: repository
                .pendingExternalJoinAccountKey(groupIdHex: groupIdHex, joinerSignatureKey: ref)
                .map { ProfileDirectory.shared.resolve(signatureKey: $0) },
            leafName: carried ?? personName(signatureKeyB64: base64Encode(ref)))
    }

    /// UX section 8 — the one line under a join request: `Asked 2 hours ago`,
    /// or from a link `Invited by Anna · 2 hours ago`. `nil` when this device
    /// holds nothing about when the request came.
    func pendingJoinLine(_ ref: Data) -> String? {
        guard let accountIdB64 = AcceptedContactsStore.boundAccountIdB64,
              let meta = repository.groupSettingsStore
                .requestMeta(accountIdB64: accountIdB64, groupIdHex: groupIdHex)[base64Encode(ref)]
        else { return nil }
        let when = ChannelsFormatting.relativeLabel(
            ms: meta.askedAtMs, nowMs: ChatRepository.nowMs(), calendar: .current,
            locale: Localization.currentLocale)
        guard let inviter = meta.inviterKey else {
            return Localization.text("chat.members.join.asked", texts: ["when": when])
        }
        let directory = ProfileDirectory.shared
        let name: String
        if base64Encode(inviter) == ownIdentity {
            name = directory.ownProfile?.displayName ?? directory.ownUsername ?? directory.resolve(signatureKey: inviter).name
        } else {
            name = directory.resolve(signatureKey: inviter).name
        }
        return Localization.text("chat.members.join.invited_by", texts: ["name": name, "when": when])
    }

    /// Pure rule behind ``pendingJoinLabel(_:)``: a name the account key
    /// resolves to (anything but the placeholder) wins; otherwise the leaf
    /// key's name, else "Someone" — the pre-existing row label.
    nonisolated static func pendingJoinLabel(
        accountKeyName: ResolvedDisplayName?,
        leafName: String?
    ) -> String {
        if let accountKeyName, accountKeyName.source != .placeholder {
            return accountKeyName.name
        }
        return leafName ?? ResolvedDisplayName.placeholderSomeone
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
        if let endedTitle { return endedTitle }
        // ADR-0051 §5 — a shared name wins for ANY conversation that has one,
        // whatever its kind or member count: a two-person conversation renamed
        // from another client is still keyed as a chat here.
        if let shared = groupName { return shared }
        if !presentsAsGroup, let peer = oneToOnePeerName { return peer }
        // ADR-0087 / UX 4.3 — a solo group with no name is titled `@name`.
        if isAloneInGroup, let handle = groupHandle { return "@" + handle.handle }
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
        // UX 9.1 — the store is wiped while the end dialog stands; the name
        // the group had stays on the screen behind it.
        endedTitle ?? repository.groupName(groupIdHex: groupIdHex)
    }

    /// UX 9.1 — the conversation went from under this screen (``endedTitle``).
    /// Its screens keep what they showed and offer nothing to act on.
    public var endedUnderScreen: Bool { endedTitle != nil }

    /// ADR-0051 §3 — may this device rename the group? The owner and admins
    /// (or any member of a legacy group with no role table), read from the LIVE
    /// group, never a cached role — and nobody once their membership ended. A
    /// plain member sees the name read-only.
    var canRenameGroup: Bool {
        repository.ownRole(groupIdHex: groupIdHex) != .member && !membershipHasEnded
    }

    // MARK: - ADR-0087 section 18 — the group photo

    /// UX 4.5.2 — the header states after `Use` or `Remove`.
    enum GroupPhotoStatus: Equatable {
        case idle
        case uploading
        case failed
        case limited
        case owed
        case notAllowed
    }

    @Published var groupPhotoStatus: GroupPhotoStatus = .idle
    /// The photo or video being uploaded, kept for `Try again`.
    private var groupPhotoRetry: PendingAvatarMedia?

    /// UX 4.5.1 — the same people who rename it.
    var canSetGroupPhoto: Bool { canRenameGroup }

    /// The photo this device holds (initials when `nil`).
    var groupPhoto: ProfileAvatar? { repository.groupPhoto(groupIdHex: groupIdHex) }

    /// UX 4.5.2 — `Use` on the crop screen: upload, then send.
    func useGroupPhoto(_ media: PendingAvatarMedia) {
        guard canSetGroupPhoto else {
            groupPhotoStatus = .notAllowed
            return
        }
        groupPhotoRetry = media
        groupPhotoStatus = .uploading
        Task { @MainActor in
            let avatar: ProfileAvatar
            do {
                avatar = try await ProfileAvatarUpload.upload(media)
            } catch AttachmentBridgeError.uploadLimited {
                groupPhotoStatus = .limited
                return
            } catch {
                groupPhotoStatus = .failed
                return
            }
            // Demoted while the crop screen was open: nothing is sent.
            guard canSetGroupPhoto else {
                BlobLifecycle.shared.delete([avatar.still.blobId] + (avatar.video.map { [$0.blobId] } ?? []))
                groupPhotoStatus = .notAllowed
                return
            }
            do {
                try await repository.setGroupPhoto(groupIdHex: groupIdHex, avatar: avatar)
                groupPhotoStatus = .idle
                groupPhotoRetry = nil
            } catch {
                // Uploaded and recorded as owed: sent when the device is back.
                groupPhotoStatus = repository.groupPhotoOwed(groupIdHex: groupIdHex) ? .owed : .failed
            }
        }
    }

    /// `Try again` after a failed upload.
    func retryGroupPhoto() {
        if let media = groupPhotoRetry { useGroupPhoto(media) }
    }

    /// UX 4.5.2 — `Remove` after the confirmation.
    func removeGroupPhoto() {
        guard canSetGroupPhoto else {
            groupPhotoStatus = .notAllowed
            return
        }
        Task { @MainActor in
            do {
                try await repository.setGroupPhoto(groupIdHex: groupIdHex, avatar: nil)
                groupPhotoStatus = .idle
            } catch {
                groupPhotoStatus = repository.groupPhotoOwed(groupIdHex: groupIdHex) ? .owed : .failed
            }
        }
    }

    // MARK: - Reactions-and-emoji spec section 9 — allowed reactions

    /// Section 9 — the Info row's value.
    var allowedReactionsSummary: String {
        switch allowedReactions {
        case .all: return Localization.text("group_reactions.value.all")
        case .off: return Localization.text("group_reactions.value.off")
        case .only(let list):
            return Localization.text("group_reactions.value.count", counts: ["count": UInt64(list.count)])
        }
    }

    /// Section 9 — save the group's reactions setting.
    func saveAllowedReactions(_ value: FfiAllowedReactions) async -> Bool {
        do {
            try await repository.setAllowedReactions(groupIdHex: groupIdHex, value)
            return true
        } catch {
            NSLog("allowed reactions save failed: %@", error.localizedDescription)
            return false
        }
    }

    /// ADR-0051 §3 — send the new name (an empty string clears it). The store
    /// and the title change only after the send succeeded; a failure returns
    /// the error copy the members panel shows and leaves the title as it was.
    func saveGroupName(_ name: String) async -> UiText? {
        do {
            try await repository.setGroupName(groupIdHex: groupIdHex, name: name)
            objectWillChange.send()
            return nil
        } catch {
            NSLog("ADR-0051 rename failed: \(error)")
            return .key("chat.group_name.error.save")
        }
    }

    /// The new-group flow's hand-off (docs/ux/new-group.md §3): the chat opened
    /// with the members that did land, and says which adds did not. Rendered in
    /// the members panel's ungated notice slot.
    func noteSetupNotice(_ text: UiText) {
        membershipNotice = text
    }

    /// The header's second line: "@username" in a 1x1 chat when the other
    /// person's username is verified, "N members" in a group, else nothing.
    var conversationSubtitle: String? {
        if endedTitle != nil { return nil }
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

    /// Remove a member by leaf index: publish the removal commit to the journal
    /// (own-marked). Refuses to remove the local leaf. Fail-closed on error.
    func onRemoveMember(leafIndex: UInt32) {
        guard ownLeafIndex != leafIndex else {
            membershipError = .key("chat.refusal.fallback")
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
                // Applied at once when this device is the committer, as web
                // does: without it an owner's own removal waited for the next
                // poll that ran a sweep, which a group info screen left open
                // did not (seen on an iPhone 2026-11-10: "2 membership changes
                // are waiting to apply" on the owner's own device).
                _ = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onRemoveMember", key: "chat.error.remove_member")
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
    /// own-leaf guard dates from the direct-commit path a table-less group
    /// took (deleted by the ADR-0081 amendment of 2026-10-19).
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
            membershipError = UiText { refusal.message }
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
                // UX §2 — decided from the group read after the attempt: a
                // leave that already landed (a sweep committed it) says
                // nothing; one still waiting says so until it lands
                // (``refreshMembers()`` clears it then).
                refreshMembers()
                if let key = MembershipActionNotice.after(.leave, inEffect: membershipHasEnded) {
                    membershipNotice = .key(key)
                    leaveNoticeShown = true
                }
            } catch {
                reportMembershipFailure(error, op: "onLeaveGroup", key: "chat.error.leave")
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
        // UX 9.1 — behind the end dialog there is no exit left to take: the
        // group is gone, and `Delete this chat` would name nothing.
        if endedTitle != nil { return .hidden }
        return exitBlock(
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
    /// change is immediate — unless another device of the owner's account with
    /// a lower signature key beaconed recently (ADR-0050 2.5a item 6), in which
    /// case that device sweeps it on its next poll.
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
                membershipError = .key("chat.refusal.fallback")
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
                // Applied at once when this device is the committer; whether
                // it landed or not, no notice (UX §2 amendment): the panel
                // changes in place, or the transfer control shows it pending.
                _ = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onTransferOwnership", key: "chat.error.transfer")
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
                // Applied at once when this device is the committer (as on
                // web and as ``onRemoveMember(leafIndex:)``).
                _ = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onToggleAdmin", key: "chat.error.change_admin")
            }
        }
    }

    /// ADR-0027 (G-3), on the ADR-0045 proposal path since S7 — approve a
    /// pending external-join request (owner/admin only). Publishes ONE
    /// artifact, an ordinary admin-authored `Add` proposal; there is no commit,
    /// no epoch advance and no Welcome here. The joiner is admitted when a
    /// committer sweeps the proposal, which is what sends the Welcome to their
    /// mailbox (ADR-0053) — and ADR-0045 decision 2 gives
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
                // Applied at once when this device is the committer. No
                // notice either way (UX §2 amendment, 2026-10-29): the request
                // row goes and the person appears in the roster when it lands.
                _ = await repository.sweepIfCommitter(groupIdHex: groupIdHex)
                // Acting on the queue is what may have made room for the
                // request the quarantine turned away, so the notice about it
                // stops being current.
                repository.clearUnkeptExternalJoin(groupIdHex: groupIdHex)
                refreshMembers()
            } catch {
                reportMembershipFailure(error, op: "onApproveJoin", key: "chat.error.approve_join")
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
            reportMembershipFailure(error, op: "onRejectJoin", key: "chat.error.reject_join")
        }
    }

    /// This device's role in the group (`nil` with no role table).
    var ownRole: Role? { repository.ownRole(groupIdHex: groupIdHex) }

    /// UX section 10 — this account created the group here.
    var createdHere: Bool {
        guard presentsAsGroup, let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else { return false }
        return repository.groupSettingsStore.createdHere(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// ADR-0087 section 10 — nobody else is in this group.
    var isAloneInGroup: Bool { !membershipHasEnded && repository.isAloneInGroup(groupIdHex: groupIdHex) }

    /// ADR-0087 section 10 — `Copy invite link` on the solo block: mint (or
    /// reuse) a link and hand it back for the clipboard.
    func mintInviteLinkForCopy() async -> String? {
        if let inviteLink { return inviteLink }
        do {
            let link = try await repository.createGroupInviteLink(groupIdHex: groupIdHex)
            inviteLink = link
            return link
        } catch {
            reportMembershipFailure(error, op: "mintInviteLinkForCopy", key: "chat.error.create_link")
            return nil
        }
    }

    /// ADR-0087 section 11 — `Delete group` after its confirmation. The
    /// @name is retired first; offline, nothing changes and the error shows.
    func onDeleteAloneGroup(onDone: @escaping () -> Void) {
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                if try await repository.deleteAloneGroupRetiringName(groupIdHex: groupIdHex) { onDone() }
            } catch {
                membershipError = .key("group_handle.error.offline")
            }
        }
    }

    /// UX 4.1 — a flag change is still owed.
    var groupTypeOwed: Bool { repository.groupTypeOwed(groupIdHex: groupIdHex) }

    /// UX 4.1 — save the Group type screen; `nil` when it applied, else the
    /// catalog key of the line the screen shows.
    func saveGroupType(handle: String, isPublic: Bool) async -> String? {
        do {
            switch try await repository.saveGroupType(groupIdHex: groupIdHex, handle: handle, isPublic: isPublic) {
            case .applied: return nil
            case .owed: return "group_photo.owed"
            }
        } catch let refusal as DiscoveryClient.HandleWriteError {
            if refusal.code == "name-taken" {
                HandleTakenCheck.note(handle, .taken)
                return "group_handle.error.held"
            }
            // A 429 is discovery's rate limit, not the day's allowance.
            if refusal.status == 429 { return "common.error.too_many_attempts" }
            return "group_handle.error.failed"
        } catch let DiscoveryClientError.network(_, _) {
            return "group_handle.error.offline"
        } catch {
            return "group_handle.error.failed"
        }
    }

    /// ADR-0087 section 4 — `@name` and the header line of group info.
    var groupHandle: StoredGroupHandle? { repository.groupHandle(groupIdHex: groupIdHex) }

    /// Section 4 — "Private group · 12 members" / "Public group · ...".
    var groupHeaderLine: String {
        let count = UInt64(max(1, repository.peopleCount(groupIdHex: groupIdHex)))
        return Localization.text(
            groupHandle?.isPublic == true ? "group_info.header.public" : "group_info.header.private",
            counts: ["count": count])
    }

    /// UX section 8 — `Reject all` after its confirmation.
    func onRejectAllJoins() {
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                for ref in pendingJoins { disarmRetry(.approveJoin(subjectKeyB64: base64Encode(ref))) }
                try await repository.rejectAllExternalJoins(groupIdHex: groupIdHex)
                repository.clearUnkeptExternalJoin(groupIdHex: groupIdHex)
                refreshMembers()
            } catch {
                refreshMembers()
                reportMembershipFailure(error, op: "onRejectAllJoins", key: "chat.error.reject_join")
            }
        }
    }

    /// UX section 14.2 — the group's live invite links from any device of the
    /// account, for the invite-link screen.
    func groupInviteLinks() -> [GroupInviteLinkRow] {
        repository.groupInviteLinks(groupIdHex: groupIdHex)
    }

    /// UX section 14.2 — mint a link with a label and terms (owner/admin).
    /// Throws so the screen shows the failure where the person pressed.
    func createGroupInviteLink(label: String, expiryMs: UInt64, maxUses: UInt32) async throws {
        inviteLink = try await repository.createGroupInviteLink(
            groupIdHex: groupIdHex, label: label, expiryMs: expiryMs, maxUses: maxUses)
    }

    /// UX section 14.2 — revoke one listed link, made on any device of the
    /// account. `false` when no device holds its key.
    func revokeGroupInviteLink(_ row: GroupInviteLinkRow) async throws -> Bool {
        let revoked = try await repository.revokeGroupInviteLink(row, groupIdHex: groupIdHex)
        if inviteLink == row.url { inviteLink = nil }
        return revoked
    }

    /// The confirmation shown after Revoke (the web and Android rule): a link
    /// whose ledger row the server deleted is revoked; one this device holds
    /// no revocation key for (or a malformed one) is only cleared from the
    /// screen and must not claim more.
    nonisolated static func inviteLinkRevokeNotice(revoked: Bool) -> String {
        revoked ? "chat.invite_link.revoked" : "chat.invite_link.cleared"
    }

    /// Phase 2a3e2c-iOS — read-cursor broadcast tracker.
    /// Highest broadcast messageId; the per-`messages` effect
    /// schedules a fresh `ReadCursor` payload only when the
    /// newest visible row's id exceeds this value.
    private var lastBroadcastReadId: Int64 = -1
    /// ADR-0073 R.5 — the last read position broadcast, in timeline order.
    private var lastBroadcastReadPosition: LineageRules.TimelinePosition?
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
        // ADR-0073 R.5 — the draft and the read position belong to the
        // CONVERSATION, so they follow it into a successor group.
        groupIdBytes = Self.hexToBytes(conversationIdHex)
        guard let groupIdBytes = groupIdBytes else { return }

        // Observe the sync repository's drafts map. On every
        // change, seed locally IF the local composer is
        // empty + we haven't seeded yet.
        let key = conversationIdHex
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

    /// The debounced read-cursor write, run now (a screen left, the app going
    /// to the background). Nothing when none is waiting.
    func flushPendingReadCursor() {
        guard let work = readCursorWork, !work.isCancelled else { return }
        readCursorWork = nil
        work.perform()
        work.cancel()
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
        // ADR-0073 R.5 — a successor's message ids restart, so the position
        // is `(segment, message id)` and "past the last broadcast" is judged
        // in that order.
        let segment = repository.segmentIndex(ofGroup: groupIdHex)
        let position = LineageRules.TimelinePosition(segment: segment, messageId: highest)
        if highest < 0 { return }
        if let last = lastBroadcastReadPosition,
           LineageRules.compareTimeline(position, last) <= 0 { return }
        let target = highest
        readCursorWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self = self,
                  let sync = self.deviceGroupSync else { return }
            self.lastBroadcastReadId = target
            self.lastBroadcastReadPosition = position
            // `message_id(8 BE)` in the first segment, `message_id(8 BE) ||
            // segment(4 BE)` in later ones: byte-identical on every client,
            // since the cursor is synced.
            let idBytes = LineageRules.encodeReadPosition(position)
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
        // Cancelled first: a message sent within the debounce leaves the
        // composer where the last write left it, and the pending write would
        // save what was sent.
        draftBroadcastWork?.cancel()
        if draftClosed || text == lastBroadcastDraft { return }
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

    /// ADR-0073 R.5 — the conversation moved to another group while this
    /// screen was up (a successor became the head): the same screen, the
    /// same history above, now sending into the new group. Nothing is shown
    /// for it.
    private func followHead(_ headHex: String) {
        let previous = groupIdHex
        guard headHex != previous else { return }
        if chatOpenCounted {
            repository.chatScreenClosed(groupIdHex: previous)
            repository.chatScreenOpened(groupIdHex: headHex)
        }
        groupIdHex = headHex
        readWatermarks = repository.readWatermarks(groupIdHex: headHex)
        ongoingGroupCall = repository.groupCalls[headHex]
        // A new MLS session: the teardown claims of the old one do not carry.
        teardownClaimed = []
        repository.ensurePushRouting(groupIdHex: headHex)
    }

    /// This conversation's thread in the repository state.
    private func conversationThread(in state: ChatRepositoryState) -> ChatThread? {
        state.threads.first { $0.conversationIdHex == conversationIdHex }
    }

    private func sync(state: ChatRepositoryState) {
        let thread = conversationThread(in: state)
        if let thread { followHead(thread.groupIdHex) }
        self.messages = thread?.messages ?? []
        self.threadKnown = thread != nil
        // ADR-0045 UX §6.2 — every poll batch republishes the repository state,
        // and a removing commit arrives on a poll like any other, so this is
        // the one place that sees every path into the terminal state. It is a
        // cheap synchronous read of a boolean the core already maintains.
        refreshMembership()
        // ADR-0087 section 9 — the thread leaves the state the moment an end
        // is recorded.
        noteConversationEnd(threadPresent: thread != nil)
        // The info screen follows the group while it is open: a sweep that
        // landed, a role that changed, a person who left, a fresh external-join
        // request. This state publishes whichever poll carried the change (the
        // chat's own, or the app-wide one while the chat is covered).
        if conversationInfoOpen { refreshMembers() }
    }

    /// UX 9.1, 9.4 — the conversation left from under this screen. Answered
    /// only while the person is looking at it: otherwise the banner tells them,
    /// and this runs again if they come back to the route.
    private func noteConversationEnd(threadPresent: Bool) {
        if threadPresent {
            sawThread = true
            endAnswered = false
            endedTitle = nil
            conversationGone = false
            draftClosed = false
            return
        }
        // No thread alone is not an end: a lineage that arrived can move the
        // conversation under another id while the group stays held.
        guard sawThread, !endAnswered, !deletingChat,
              !repository.hasGroup(groupIdHex: groupIdHex) || repository.isEnding(groupIdHex: groupIdHex),
              screenVisible || conversationInfoOpen else { return }
        endAnswered = true
        draftBroadcastWork?.cancel()
        draftClosed = true
        let held = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let keptDraft = held.isEmpty ? nil : draft
        draft = ""
        let notice = repository.claimEndNotice(groupIdHex: groupIdHex)
        endedTitle = notice?.name ?? conversationTitle
        refreshMembership()
        if let notice {
            endDialog = EndDialog(name: notice.name, draft: keptDraft)
        } else {
            // The person left: the row goes with no notice.
            conversationGone = true
        }
    }

    /// `OK` on the dialog: back to Chats.
    public func endDialogClosed() {
        endDialog = nil
        conversationGone = true
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
        // ADR-0091 phase 2 — the waiting note (UX 2.2).
        let note = repository.reentryWaitingNote(groupIdHex: groupIdHex)
        // Walk-2 rule R5 — a chat that starts waiting shows no error left
        // over from before: the waiting note is what it says now. (A failed
        // read raises none on this client: the poll is best-effort.)
        if ConversationReentryRules.clearsChatErrors(wasWaiting: reentryNote != nil, isWaiting: note != nil) {
            sendError = nil
            membershipError = nil
        }
        if note != reentryNote { reentryNote = note }
        let read = repository.membershipStateNow(groupIdHex: groupIdHex)
        // Once the group is wiped there is no handle, and no handle reads as
        // still a member. The state this screen reached does not reverse.
        membership = endedTitle != nil && !membershipEnded(read) ? .ended(left: false) : read
        // UX §2 — the leave notice goes the moment the leave lands. Here, not
        // in `refreshMembers()`: a poll is what usually sees the landing, and
        // the screen read "Leave requested" beside the frozen list until
        // reopened.
        if MembershipActionNotice.leaveNoticeClears(
            shown: leaveNoticeShown, membershipEnded: membershipHasEnded) {
            membershipNotice = nil
            leaveNoticeShown = false
        }
        // A retired leaf came back (ADR-0050): the group is live again, and
        // a later end must find its teardown owed, not already claimed.
        if !membershipHasEnded, !teardownClaimed.isEmpty {
            teardownClaimed = []
            if screenVisible { startBeaconTicker() }
        }
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
        deletingChat = true
        defer { deletingChat = false }
        let deleted = repository.deleteChat(groupIdHex: groupIdHex)
        if !deleted {
            // Only reachable if the gate went stale between draw and tap.
            membershipError = .key("chat.error.delete_chat")
        }
        return deleted
    }

    // MARK: - Actions

    public func onSendClicked() {
        guard !sending else { return }
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        // `docs/ux/media-messages.md` section 11 — one message: the text and
        // every item in the tray, once no item is still uploading or failed.
        guard tray.tray.canSend(hasText: !trimmed.isEmpty) else { return }
        let stagedAttachments = tray.takeForSend()

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
                sendError = Self.errorText(for: error, fallbackKey: "chat.send.failed")
                NSLog("send failed: %@", error.localizedDescription)
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
                sendError = Self.errorText(for: error, fallbackKey: "chat.send.failed")
                NSLog("retry send failed: %@", error.localizedDescription)
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
        /// What goes on the wire: the author's real name, never "You".
        public let snapshot: ReplyRef
        /// The quoted message is this account's own: the strip reads "You".
        public var quotesOwn: Bool = false

        /// The snapshot as this device draws it.
        public var shown: ReplyRef { quotesOwn ? ReplySnapshot.shownAsOwn(snapshot) : snapshot }
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
                for: message, authorLabel: replyAuthorLabel(for: message),
                invitationLine: message.snapshotInvitationLine(conversationIdHex: groupIdHex)),
            quotesOwn: message.direction == .outgoing)
        composerFocusRequest += 1
    }

    /// `docs/ux/feed.md` section 5 — Reply from a feed item: the conversation
    /// opens with the reply strip already set. Parked until this screen is on
    /// screen (leaving the screen drops an armed reply, section 3.1), armed at
    /// once when it already is.
    private var pendingReplyId: Int64?

    public func requestReply(to messageId: Int64) {
        pendingReplyId = messageId
        if screenVisible { armPendingReply() }
    }

    private func armPendingReply() {
        guard let id = pendingReplyId else { return }
        pendingReplyId = nil
        guard let message = conversationThread(in: repository.state)?
            .messages.first(where: { $0.messageId == id && !$0.deleted })
        else { return }
        beginReply(message)
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
        guard message.messageId > 0, !message.deleted, message.ack == .sent, isForwardable(message) else { return }
        forwardingMessage = message
    }

    /// ADR-0092 — whether `message` may be forwarded by what it holds
    /// (``ForwardPayload/isForwardable(_:cardShareable:)``), the contact-card
    /// check read from the newest profile this device holds.
    func isForwardable(_ message: ChatMessage) -> Bool {
        Self.isForwardable(message)
    }

    static func isForwardable(_ message: ChatMessage) -> Bool {
        ForwardPayload.isForwardable(message) {
            ProfileDirectory.shared.cardShareable(signatureKeyB64: $0.base64EncodedString())
        }
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
        snapshotAuthorName(for: message)
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
        Task { @MainActor [weak self] in
            do {
                try await self?.forwardAwaiting(message, to: targetGroupIdHex)
            } catch {
                NSLog("ADR-0052 forward failed: \(error)")
                self?.sendError = Self.errorText(for: error, fallbackKey: "chat.forward.error.generic")
            }
        }
    }

    /// The one forward, awaited: compose section 5.1's payload, re-encrypt the
    /// attachments, send. The single `forward` and the bulk run (section 8)
    /// both go through here, so a forward is composed in exactly one place.
    func forwardAwaiting(_ message: ChatMessage, to targetGroupIdHex: String) async throws {
        let payload = ForwardPayload.forwardPayload(
            of: message,
            authorLabel: forwardAuthorLabel(for: message),
            originGroupIdHex: groupIdHex,
            // ADR-0053 section 6 — a forward out of a one-to-one carries no origin.
            sourceIsOneToOne: repository.conversationKind(groupIdHex: groupIdHex) == .chat)
        guard isForwardable(message) else { throw ContentSendError.notAllowed }
        // An article's images are its attachments, re-encrypted the same way.
        let refs = try await ForwardAttachments.reencryptAll(message.attachments)
        if let content = ForwardPayload.forwardedContent(of: message) {
            try await repository.sendContent(
                groupIdHex: targetGroupIdHex, content: content, attachments: refs, forward: payload.forward)
            return
        }
        try await repository.sendMessage(
            groupIdHex: targetGroupIdHex,
            plaintext: payload.text,
            attachments: refs,
            forward: payload.forward)
    }

    // MARK: - `docs/ux/message-actions.md` section 8 — Delete and Forward in bulk

    /// Section 8 — one delete per message, oldest first (the caller hands them
    /// in timeline order), awaiting each and stopping at the first failure.
    /// Each is the single delete: the same repository call, the same local
    /// apply, the pin records going with it (section 4.1).
    func bulkDelete(_ messages: [ChatMessage]) async -> MessageSelection.BulkOutcome<ChatMessage> {
        await MessageSelection.run(over: messages) { message in
            try await self.deleteMessageAwaiting(message)
        }
    }

    /// Section 8 — one forwarded message per selected message, oldest first,
    /// each composed exactly as the single forward composes one.
    func bulkForward(
        _ messages: [ChatMessage], to targetGroupIdHex: String
    ) async -> MessageSelection.BulkOutcome<ChatMessage> {
        await MessageSelection.run(over: messages) { message in
            try await self.forwardAwaiting(message, to: targetGroupIdHex)
        }
    }

    /// Section 5.1 "tap to jump" — where a forward's attribution leads, or
    /// `nil` when it leads nowhere on this device: the forward carried no
    /// origin, this device does not hold that conversation, or holds it but
    /// not the message. `nil` means do nothing and say nothing (section 5: an
    /// explanation would confirm the conversation exists).
    func forwardOriginJump(of forward: ForwardRef) -> (groupIdHex: String, messageId: Int64)? {
        // ADR-0073 R.5 — the origin names a GROUP; the conversation it
        // belongs to may have moved on, so the row is looked up under the id
        // it is drawn under now (its own id in the head, a history id in an
        // older segment this device holds).
        guard let origin = forward.originGroupIdHex, let messageId = forward.originMessageId else {
            return nil
        }
        let conversation = repository.conversationIdHex(ofGroup: origin)
        guard let thread = repository.state.threads.first(where: { $0.conversationIdHex == conversation }),
              let rowId = LineageViews.forwardJumpRow(
                originGroupIdB64: origin, messageId: messageId,
                headGroupIdB64: thread.groupIdHex, heldHistory: thread.historyGroupIdHexes),
              thread.messages.contains(where: {
                  $0.messageId == rowId && !ChatTimeline.isHiddenSystemRow($0)
              })
        else { return nil }
        return (thread.groupIdHex, rowId)
    }

    // MARK: - ADR-0052 decision 3 — pin

    /// The pin sheet that is open, `nil` when none is (UX spec section 4.1
    /// "The sheet"). Carries the message and the rows already narrowed to it.
    public struct PinSheetRequest: Identifiable, Equatable {
        public let message: ChatMessage
        public let options: [PinSheetOption]
        public var id: Int64 { message.messageId }
    }

    @Published public private(set) var pinSheet: PinSheetRequest?

    /// Which pin the bar shows (section 4: one at a time, newest first). An
    /// index into ``pinnedEntries``; clamped on read so a list that shrank
    /// under it wraps to the newest rather than pointing past the end.
    @Published public var pinBarIndex: Int = 0

    /// Whether the bar's list of every pin is open (section 4.1 "a `list`
    /// control at the far end opens every pin as rows").
    @Published public var pinListOpen: Bool = false

    /// Section 4 — the bar's entries: shared before personal, newest first,
    /// only messages this device holds. Derived on every read from the two
    /// stores and the timeline; ``ChatRepository/pinRevision`` and `messages`
    /// drive the re-render.
    var pinnedEntries: [PinnedEntry] {
        // ADR-0073 R.5 — the pins of every segment this device holds, each
        // under the id its row is drawn under.
        let spanned = repository.pinsAcrossSegments(headGroupIdHex: groupIdHex)
        return PinnedBar.entries(shared: spanned.shared, personal: spanned.personal, held: messages)
    }

    /// The stored pin an on-screen row id stands for: the row itself in this
    /// group, or `(older group, original id)` for a row of an older segment.
    private func pinTarget(ofRow rowId: Int64) -> LineageViews.PinTarget? {
        LineageViews.pinTargetOfRow(
            rowId: rowId,
            headGroupIdB64: groupIdHex,
            olderGroupIdsB64: repository.olderSegments(headGroupIdHex: groupIdHex))
    }

    /// The pin state of a row, read from the group that stores it.
    private func pinState(ofRow rowId: Int64) -> (shared: Bool, personal: Bool, maySetShared: Bool, older: Bool) {
        guard let target = pinTarget(ofRow: rowId) else { return (false, false, false, false) }
        // `groupIdB64` carries the hex key here: the function only compares.
        let shared = repository.sharedPins(groupIdHex: target.groupIdB64)[target.messageId]?.pinned == true
        let personal = repository.personalPins(groupIdHex: target.groupIdB64)[target.messageId] != nil
        // An older segment's shared pin is cleared on this device alone, which
        // every holder may do; a new shared pin there is refused.
        return (shared, personal, target.older ? true : maySetSharedPin, target.older)
    }

    /// The entry the bar shows now, `nil` when there is nothing to show.
    var pinnedBarEntry: PinnedEntry? {
        let entries = pinnedEntries
        guard !entries.isEmpty else { return nil }
        return entries[PinnedBar.clamped(pinBarIndex, count: entries.count)]
    }

    /// The two lines the bar and the list draw for one pin: the author (or
    /// `For you` for a personal pin, so a private bookmark never looks like
    /// something the room can see) and the excerpt — the text, or the same
    /// one-word stand-in the reply quote uses for a message without text.
    func pinnedLines(for entry: PinnedEntry) -> (author: String, excerpt: String) {
        guard let message = messages.first(where: { $0.messageId == entry.messageId }) else {
            return (author: "", excerpt: ReplySnapshot.missingExcerpt)
        }
        let taken = ReplySnapshot.replySnapshot(
            for: message, authorLabel: replyAuthorLabel(for: message),
            invitationLine: message.snapshotInvitationLine(conversationIdHex: groupIdHex))
        let snapshot = message.direction == .outgoing ? ReplySnapshot.shownAsOwn(taken) : taken
        let lines = ReplySnapshot.replyQuoteLines(snapshot)
        switch entry.scope {
        case .personal: return (author: Localization.text("chat.pinned_bar.for_you"), excerpt: lines.excerpt)
        case .shared: return (author: lines.name, excerpt: lines.excerpt)
        }
    }

    /// Section 4 — a tap on the bar: the message to scroll to (the shown
    /// pin), after which the bar advances to the next older pin, wrapping past
    /// the oldest. `nil` when the bar is empty.
    func tapPinnedBar() -> Int64? {
        let entries = pinnedEntries
        guard !entries.isEmpty else { return nil }
        let index = PinnedBar.clamped(pinBarIndex, count: entries.count)
        pinBarIndex = PinnedBar.advanced(from: index, count: entries.count)
        return entries[index].messageId
    }

    /// Whether this conversation is a one-to-one (its ADR-0062 kind — a
    /// two-person group is not one), which is what turns `Pin for everyone`
    /// into `Pin for both`.
    var isOneToOneForPins: Bool {
        repository.isOneToOne(groupIdHex: groupIdHex)
    }

    /// Section 4.1 — may this device set or clear a shared pin here? Read from
    /// the live group on every call, never cached.
    var maySetSharedPin: Bool {
        repository.maySetSharedPin(groupIdHex: groupIdHex)
    }

    /// The word the action card shows for `message`: `Pin`, `Unpin`, or no row
    /// (a row that has not landed, a tombstone, or a dead membership).
    func pinMenuState(for message: ChatMessage) -> PinMenuState {
        if LineageRules.isHistoryRowId(message.messageId) {
            // ADR-0073 R.5 — a row of an older segment offers Unpin alone,
            // and only when it is pinned.
            let state = pinState(ofRow: message.messageId)
            let row = PinSheet.menuState(
                sharedPinned: state.shared, personalPinned: state.personal, maySetShared: state.maySetShared)
            let offers = LineageViews.historyRowOffersUnpin(
                isHistoryRow: true, pinRow: row == .unpin ? .unpin : .pin)
            return offers ? .unpin : .unavailable
        }
        guard messageActionsAvailable, PinnedBar.isPinnable(message) else { return .unavailable }
        let state = pinState(ofRow: message.messageId)
        return PinSheet.menuState(
            sharedPinned: state.shared, personalPinned: state.personal, maySetShared: state.maySetShared)
    }

    /// The `Pin` / `Unpin` row was chosen: open the sheet, or — an unpin with
    /// one way to do it — act at once.
    func beginPin(_ message: ChatMessage) {
        guard pinMenuState(for: message) != .unavailable else { return }
        let state = pinState(ofRow: message.messageId)
        let options = PinSheet.options(
            sharedPinned: state.shared, personalPinned: state.personal,
            maySetShared: state.maySetShared, oneToOne: isOneToOneForPins)
        guard !options.isEmpty else { return }
        if let atOnce = PinSheet.actsAtOnce(options) {
            choosePin(atOnce, for: message)
            return
        }
        pinSheet = PinSheetRequest(message: message, options: options)
    }

    func cancelPinSheet() {
        pinSheet = nil
    }

    /// A row of the sheet was tapped. A personal choice is a local flag and
    /// lands at once; a shared one is sent first and applied only after the
    /// send succeeded (the sender applies it the way a receiver would). A
    /// failed send is said once, in the send-error slot, and changes nothing.
    func choosePin(_ choice: PinChoice, for message: ChatMessage) {
        pinSheet = nil
        guard let stored = pinTarget(ofRow: message.messageId), stored.messageId > 0 else { return }
        let target = stored.messageId
        let pinned = choice == .pinForMe || choice == .pinForEveryone
        let scope: LineageViews.PinScope =
            (choice == .pinForMe || choice == .unpinForMe) ? .personal : .shared
        // ADR-0073 R.5 — a pin of an older segment can be removed and
        // nothing else: a personal unpin writes under the older group, a
        // shared unpin there is recorded on this device alone.
        switch LineageViews.pinRoute(older: stored.older, pinned: pinned, scope: scope) {
        case .refused:
            return
        case .localOnly:
            repository.clearSharedPinLocally(groupIdHex: stored.groupIdB64, messageId: target)
            return
        case .ordinary:
            break
        }
        switch choice {
        case .pinForMe:
            repository.setPersonalPin(groupIdHex: stored.groupIdB64, messageId: target, pinned: true)
        case .unpinForMe:
            repository.setPersonalPin(groupIdHex: stored.groupIdB64, messageId: target, pinned: false)
        case .pinForEveryone, .unpinForEveryone:
            Task { @MainActor [weak self] in
                guard let self else { return }
                do {
                    try await self.repository.setSharedPin(
                        groupIdHex: self.groupIdHex, messageId: target, pinned: pinned)
                } catch {
                    NSLog("ADR-0052 shared pin failed: \(error)")
                    self.sendError = Self.errorText(for: error, fallbackKey: "chat.error.pin")
                }
            }
        }
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

    /// The name a snapshot of `message` carries (a reply quote, a forward, a
    /// saved entry): the author's REAL name, also for this account's own
    /// message (maintainer, 2026-11-04: the snapshot is read on other
    /// people's devices too, so it never says "You"); never a placeholder or
    /// a key fingerprint. `nil` when this device holds no name for the
    /// author, and the snapshot then carries none.
    func snapshotAuthorName(for message: ChatMessage) -> String? {
        switch message.direction {
        case .outgoing:
            let directory = ProfileDirectory.shared
            let own = directory.ownProfile?.displayName?.trimmingCharacters(in: .whitespacesAndNewlines)
            if let own, !own.isEmpty { return own }
            return directory.ownUsername.map { "@\($0)" }
        case .incoming, .system:
            if let key = message.authorKeyB64,
               let resolved = ProfileDirectory.shared.resolve(signatureKeyB64: key),
               !resolved.isPlaceholder {
                return resolved.name
            }
            guard let peer = repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { return nil }
            let resolved = ProfileDirectory.shared.resolve(signatureKey: peer)
            return resolved.isPlaceholder ? nil : resolved.name
        }
    }

    /// What a reply snapshot of `message` carries as its author: the real
    /// name (``snapshotAuthorName(for:)``), or no name.
    func replyAuthorLabel(for message: ChatMessage) -> String {
        snapshotAuthorName(for: message) ?? ""
    }

    /// Whether `message` quotes a message of this account's own that this
    /// device holds: its quote then reads "You" here.
    func quotesOwnMessage(_ message: ChatMessage) -> Bool {
        guard let target = message.reply?.target else { return false }
        return isOwnMessage(id: target)
    }

    /// Whether the held message `id` is this account's own.
    func isOwnMessage(id: Int64) -> Bool {
        guard id > 0 else { return false }
        return messages.last(where: { $0.messageId == id })?.direction == .outgoing
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
                sendError = Self.errorText(for: error, fallbackKey: "chat.error.edit")
                NSLog("edit failed: %@", error.localizedDescription)
            }
        }
    }

    // MARK: - ADR-0065 Save

    /// `Save` / `Remove from saved` / no row, for `message`.
    /// Any non-deleted message is savable, an older segment's row included
    /// (named by its segment and DS id, ``SavedMessageRef``).
    func saveMenuState(for message: ChatMessage, saved: [SavedMessageView]) -> SaveMenuState {
        guard let ref = savedRef(of: message), !message.deleted, message.direction != .system else {
            return .unavailable
        }
        return SaveMenuState.of(isSaved: SavedMessages.entry(in: saved, ref: ref) != nil, available: true)
    }

    /// The segment ref of a row in this conversation.
    func savedRef(of message: ChatMessage) -> SavedMessageRef? {
        conversationThread(in: repository.state)?.savedRef(rowId: message.messageId)
    }

    /// The author name kept in the snapshot.
    private func savedAuthorName(_ message: ChatMessage) -> String {
        snapshotAuthorName(for: message) ?? ""
    }

    func toggleSave(_ message: ChatMessage) {
        let center = SavedMessagesCenter.shared
        guard let ref = savedRef(of: message) else { return }
        Task { @MainActor in
            do {
                if center.isSaved(ref) {
                    try await center.unsave(ref)
                } else {
                    try await saveMessagesAwaiting([message])
                }
            } catch {
                sendError = .key("chat.error.save")
                NSLog("save failed: %@", String(describing: error))
            }
        }
    }

    /// Select mode's Save: the whole set, oldest first.
    func saveMessagesAwaiting(_ messages: [ChatMessage]) async throws {
        let items = messages.compactMap { message -> (message: ChatMessage, ref: SavedMessageRef, author: String)? in
            guard let ref = savedRef(of: message) else { return nil }
            return (message, ref, savedAuthorName(message))
        }
        try await SavedMessagesCenter.shared.save(items, kind: presentsAsGroup ? .group : .oneToOne)
    }

    /// Delete (tombstone) own message `message`.
    func deleteMessage(_ message: ChatMessage) {
        guard message.messageId > 0 else { return }
        Task { @MainActor in
            do {
                try await deleteMessageAwaiting(message)
            } catch {
                sendError = Self.errorText(for: error, fallbackKey: "chat.error.delete_message")
                NSLog("delete failed: %@", error.localizedDescription)
            }
        }
    }

    /// The one delete, awaited. The single `deleteMessage` and the bulk run
    /// (`docs/ux/message-actions.md` section 8) both go through here. A
    /// message without a server id is refused as an error rather than skipped,
    /// so a bulk run stops on it instead of counting it as done.
    func deleteMessageAwaiting(_ message: ChatMessage) async throws {
        let target = message.messageId
        guard target > 0 else { throw MessageSelectionError.notLanded }
        // ADR-0065 section 6 — the files' ids are taken before the row is a
        // tombstone; once the delete is published, the files go too.
        let blobIds = message.direction == .outgoing ? MessageBlobs.ids(of: message.attachments) : []
        try await repository.deleteMessage(groupIdHex: groupIdHex, target: target)
        BlobLifecycle.shared.delete(blobIds)
    }

    /// Why a bulk step refused before reaching the repository.
    enum MessageSelectionError: Error {
        /// The message carries no server id, so there is nothing to act on.
        case notLanded
    }

    // MARK: - Reactions (reactions-and-emoji spec)

    /// Section 10 — the message whose "You can add up to 3 reactions" line
    /// shows under its chips, for about three seconds.
    @Published public private(set) var capLineMessageId: Int64?
    private var capLineTask: Task<Void, Never>?

    /// Section 9 — what this conversation takes.
    var allowedReactions: FfiAllowedReactions { repository.allowedReactions(groupIdHex: groupIdHex) }

    /// Section 7 — the chip shape for this conversation.
    var chipConversation: FfiChipConversation { presentsAsGroup ? .group : .oneToOne }

    /// Section 5 — the row the menu offers for `message`.
    func reactionRow(for message: ChatMessage) -> ReactionRowModel {
        ReactionRules.row(
            message: message, own: ownIdentity, quick: QuickReactionSetting.shared.chosen,
            recent: RecentReactions.load(accountIdB64: AcceptedContactsStore.boundAccountIdB64),
            allowed: allowedReactions)
    }

    /// Section 7 — the chips under `message`.
    func reactionChips(for message: ChatMessage) -> FfiChipRow {
        ReactionRules.chips(
            message: message, own: ownIdentity, conversation: chipConversation, allowed: allowedReactions)
    }

    /// Sections 5, 7 — press `emoji` on `message` (the row, a chip, the
    /// picker): the shared rule decides add, remove, or nothing.
    func react(to message: ChatMessage, emoji: String) {
        let target = message.messageId
        guard target > 0, !emoji.isEmpty, !message.deleted else { return }
        let verdict = ReactionRules.verdict(
            emoji: emoji, message: message, own: ownIdentity, allowed: allowedReactions)
        let add: Bool
        switch verdict {
        case .add: add = true
        case .remove: add = false
        case .capReached:
            showCapLine(messageId: message.id)
            return
        case .notAllowed, .kindLimit:
            return
        }
        if add { RecentReactions.push(emoji, accountIdB64: AcceptedContactsStore.boundAccountIdB64) }
        Task { @MainActor in
            do {
                try await repository.reactToMessage(
                    groupIdHex: groupIdHex,
                    target: target,
                    emoji: emoji,
                    add: add,
                    targetAuthorB64: message.authorKeyB64
                )
            } catch {
                sendError = Self.errorText(for: error, fallbackKey: "chat.error.react")
                NSLog("reaction failed: %@", error.localizedDescription)
            }
        }
    }

    /// Section 6 — a double tap on the bubble: the quick reaction, or nothing,
    /// silently, when it does not apply.
    func quickReact(to message: ChatMessage) {
        guard messageActionsAvailable, message.messageId > 0, !message.deleted,
              message.direction != .system else { return }
        react(to: message, emoji: QuickReactionSetting.shared.current)
    }

    /// Section 12 — own messages with unseen reactions, oldest first.
    var unseenReactionIds: [Int64] { repository.unseenReactionIds(groupIdHex: groupIdHex) }

    func repositoryMarkSeen(_ messageId: Int64) {
        repository.markReactionsSeen(groupIdHex: groupIdHex, messageId: messageId)
    }

    /// Section 12 — a row was on screen.
    func noteRowShown(_ message: ChatMessage) {
        guard message.direction == .outgoing, message.messageId > 0 else { return }
        repository.markReactionsSeen(groupIdHex: groupIdHex, messageId: message.messageId)
    }

    private func showCapLine(messageId: Int64) {
        capLineMessageId = messageId
        capLineTask?.cancel()
        capLineTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard !Task.isCancelled, self?.capLineMessageId == messageId else { return }
            self?.capLineMessageId = nil
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
            return Localization.text("chat.typing.several")
        }
        let who = typers[0]
        // A one-to-one (its ADR-0062 kind) reads as a bare "typing…" —
        // naming the sole peer adds nothing. A two-person group names them.
        if !isGroupChat {
            return Localization.text("chat.typing.one_to_one")
        }
        if let name = displayName(for: who) {
            return Localization.text("chat.typing.named", texts: ["name": name])
        }
        return Localization.text("chat.typing.one_to_one")
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
                // ADR-0091 section 2 — refused while behind: the stop list, not a failure.
                guard !ConversationReentryRules.isBehindRefusal(error) else { return }
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
                guard !ConversationReentryRules.isBehindRefusal(error) else { return }
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

    /// A picked item that could not be prepared for sending: one sentence
    /// in the composer's error line, nothing sent.
    public func reportAttachmentProblem(_ copy: UiText) {
        sendError = copy
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
    public var pollLoopShouldContinue: Bool {
        return !membershipHasEnded
    }

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
