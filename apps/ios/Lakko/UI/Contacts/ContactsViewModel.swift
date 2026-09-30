// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// State + dispatcher for ``ContactsScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/ContactsRoute.tsx` (1.17e6)
///   * `apps/android/.../ui/contacts/ContactsViewModel.kt`
///     (1.15e6 + 1.15e7's `pollOnceLongPoll`)
///
/// **Scope**: search/send + pull-to-refresh + accept/decline +
/// requires-reauth panel (1.14e4), the invite generate/revoke
/// surface (1.14e7; 1.15e10 on Android), and continuous
/// `pollOnceLongPoll` (1.14e5) — all wired.
///
/// `@MainActor` because every mutation writes to a `@Published`
/// slot that SwiftUI reads on the main runloop. The repository's
/// own `@Published var state` is mirrored here through a Combine
/// subscription so the view can observe one ObservableObject; the
/// VM-owned form fields (`usernameInput`, `introInput`) round out
/// the picture.
@MainActor
public final class ContactsViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var usernameInput: String = ""
    @Published public var introInput: String = ""

    // MARK: - Ephemeral status

    @Published public private(set) var sending: Bool = false
    @Published public private(set) var sendError: UiText?
    @Published public private(set) var generatingInvite: Bool = false
    @Published public private(set) var inviteError: UiText?
    /// The last invite link the user generated this session.
    /// Surfaced as a copyable / shareable bottom-sheet on the
    /// Contacts screen; cleared once the user dismisses or
    /// generates another one.
    @Published public var lastGeneratedInvite: GeneratedInvite?
    /// Bumped once per successful ``onSendClicked()``; the add-by-username
    /// sheet dismisses on it (UX spec section 5.3).
    @Published public private(set) var sendSucceededCount: Int = 0

    /// `docs/ux/navigation.md` section 4.3 — bumped when the conversation
    /// list's ONE network lookup found a person and handed their name here.
    /// The Contacts screen watches it and opens the add-contact sheet with the
    /// name already filled.
    ///
    /// A counter rather than a flag, because the same name looked up twice has
    /// to reopen the sheet the second time, and a flag that is already `true`
    /// changes nothing.
    @Published public private(set) var addByUsernameRequests: Int = 0

    // MARK: - Mirror of the repository's state

    /// Mirrored from ``ContactsRepository/state``. SwiftUI's
    /// `@ObservedObject` traverses one ObservableObject at a time;
    /// having the VM expose the repository's snapshot here means
    /// the view can subscribe to just the VM and still see every
    /// repository update.
    @Published public private(set) var repositoryState: ContactsState = ContactsState()

    // MARK: - Wiring

    private let repository: ContactsRepository
    /// Optional bridge to the chat surface. When non-nil,
    /// ``onAcceptClicked(messageId:)`` drives
    /// ``ChatRepository/startGroupWith(recipientAccountId:recipientSignaturePubkey:)``
    /// against the sender's published bundle before flipping the
    /// contact row from pending → accepted; the resulting
    /// `groupIdHex` lands on the resulting ``AcceptedContact`` so
    /// the UI can surface an "Open chat" link.
    private weak var chatRepository: ChatRepository?
    private var cancellables: Set<AnyCancellable> = []

    public init(
        repository: ContactsRepository,
        chatRepository: ChatRepository? = nil
    ) {
        self.repository = repository
        self.chatRepository = chatRepository
        self.repositoryState = repository.state
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] newState in
                self?.repositoryState = newState
            }
            .store(in: &cancellables)
        // A one-to-one conversation IS a contact: whenever the set of
        // conversations this device holds changes, every one-to-one peer
        // without a contact row gets one (`ContactsRepository.adoptOneToOnePeer`).
        chatRepository?.$state
            .map { Set($0.threads.map(\.groupIdHex)) }
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.adoptOneToOnePeers() }
            .store(in: &cancellables)
    }

    /// Give every one-to-one peer a contact row (see
    /// `ContactsRepository.adoptOneToOnePeer`). The account id, when a profile
    /// has carried one (ADR-0048 v2), rides along so the row can name the
    /// person in a group call like an accepted one does.
    public func adoptOneToOnePeers() {
        guard let chatRepository else { return }
        var accountIds: [String: Data] = [:]
        for entry in ProfileDirectory.shared.profilesWithAccountId() {
            accountIds[entry.signatureKeyB64] = entry.accountId
        }
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        let createdAt = ISO8601DateFormatter().string(from: Date())
        for groupIdHex in chatRepository.knownGroupIdHexes {
            guard let peer = chatRepository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { continue }
            repository.adoptOneToOnePeer(
                signatureKey: peer,
                accountId: accountIds[base64Encode(peer)],
                groupIdHex: groupIdHex,
                nowMs: nowMs,
                createdAt: createdAt
            )
        }
    }

    // MARK: - Actions

    /// Send a contact request. Surfaces success by clearing the
    /// form; failures land in ``sendError``.
    public func onSendClicked() {
        guard !sending else { return }
        // ADR-0048 §4 — no lookup is sent for a name that fails the rule.
        let username: String
        switch checkUsername(usernameInput) {
        case .empty:
            sendError = .key("username.error.empty")
            return
        case .invalid(let reason):
            sendError = UiText { reason.message }
            return
        case .valid(let canonical):
            username = canonical
        }

        sending = true
        sendError = nil
        let intro = introInput

        // The handle the user typed, prefixed with `@`, as the subject
        // of a reset-aware ``SupersessionRefused`` message (KT-log S4).
        let peerLabel = "@\(username)"

        Task {
            do {
                _ = try await repository.sendContactRequest(
                    recipientUsername: username,
                    introMessage: intro
                )
                sending = false
                sendError = nil
                usernameInput = ""
                introInput = ""
                sendSucceededCount += 1
            } catch let refused as SupersessionRefused {
                // KT-log S4 — the peer's AccountIdentity key changed and
                // could not be authorised by a supersession proof
                // (fail-closed, ADR-0016). Surface a RESET-AWARE message
                // so the user can distinguish an owner-initiated identity
                // reset (ADR-0017 lost-everything fresh-start) from a
                // possible compromise, using the best-effort
                // transparency-log marker the refusal carries. The gate
                // already refused regardless of the log — this only
                // colours the prompt.
                sending = false
                let loggedReset = refused.loggedReset
                sendError = UiText {
                    Self.supersessionRefusedMessage(loggedReset: loggedReset, peerLabel: peerLabel)
                }
            } catch {
                sending = false
                sendError = Self.contactsErrorText(
                    for: error, fallbackKey: "contacts.error.send",
                    quotaKey: "tokens.lookup_allowance_used")
            }
        }
    }

    /// ADR-0048 §4 — the inline reason under the add-contact field, or `nil`.
    public var usernameHint: String? {
        checkUsername(usernameInput).inlineMessage
    }

    /// Fill the add-contact form with a name the lookup resolved and ask the
    /// Contacts screen to open it.
    ///
    /// The lookup only ever hands over a name the discovery service confirmed
    /// exists, so the form opens ready to send rather than ready to be
    /// re-typed — but it stays a form, because the contact request asks for an
    /// introduction and sending one silently on the person's behalf is not
    /// what they asked for by typing a name into a search field.
    public func prefillAddByUsername(_ username: String) {
        usernameInput = username
        sendError = nil
        addByUsernameRequests += 1
    }

    /// Pull-to-refresh: one immediate inbox fetch plus the live-invites
    /// list. The long-poll loop keeps running regardless.
    public func pullToRefresh() async {
        await repository.refreshInbox()
        await loadActiveInvites()
    }

    // MARK: - Continuous foreground polling (1.14e5)

    /// One long-poll tick — used by ``ContactsScreen``'s
    /// `.task { … }` modifier in a `while !Task.isCancelled` loop.
    /// This is the foreground path; a backgrounded app is covered
    /// opportunistically by the `BGAppRefreshTask` wired in
    /// ``LakkoApp`` (G-29, see ``ContactBackgroundRefresh``). An
    /// APNs `content-available` push for immediate background
    /// surfacing is a separate follow-up.
    ///
    /// `waitSecs` is passed straight to the DS long-poll budget;
    /// the server clamps to its own `MAX_WAIT_SECS`. 30 matches
    /// the canonical chat-app cadence + the Android side's
    /// `DEFAULT_LONG_POLL_SECS`.
    public func pollOnceLongPoll(waitSecs: Int = ContactsViewModel.defaultLongPollSecs) async {
        await repository.refreshInbox(waitSecs: waitSecs)
    }

    /// DS long-poll budget for the foreground-poll loop. Mirrors
    /// `ContactsViewModel.DEFAULT_LONG_POLL_SECS` on Android.
    public static let defaultLongPollSecs: Int = 30

    /// Accept a pending request:
    ///
    ///   1. If a chat bridge is wired AND the request carries a
    ///      `fromAccountId` (the sender's UUIDv7 — present on
    ///      every v2 ContactRequest, absent on v1 legacy
    ///      envelopes), call
    ///      ``ChatRepository/startGroupWith(recipientAccountId:recipientSignaturePubkey:)``
    ///      to fetch the sender's KeyPackage and ship them a
    ///      Welcome.
    ///   2. Move the row from `pending → accepted`, stashing the
    ///      resulting `groupIdHex` (or `nil` if the bridge is
    ///      unwired / the chat handshake failed).
    ///
    /// Failures during the chat handshake fall through to the
    /// "move row to accepted with `groupIdHex = nil`" branch —
    /// the user still sees the Accept confirmed in their UI; the
    /// sender just won't get the Welcome (Phase-1 trade-off the
    /// UI surfaces as "chat creation failed, retry").
    public func onAcceptClicked(messageId: Int64) {
        let pending = repository.state.pending.first { $0.messageId == messageId }
        guard let pending = pending else { return }

        // 1.9b1: ship a ContactAck back to the original sender's
        // mailbox so their UI can flip from "pending" to
        // "accepted" without waiting on the Welcome arrival.
        // Best-effort — a network failure here doesn't unwind the
        // local accept (the user already trusted the peer); we
        // log and move on. The signed ack is independent of MLS
        // state, so the sender can light up their address book
        // even if the group-creation step below stalls.
        Task {
            do {
                _ = try await repository.sendContactAck(request: pending)
            } catch {
                // best-effort
            }
        }

        guard let chatRepository = chatRepository,
              let fromAccountId = pending.fromAccountId else {
            // No chat bridge wired OR v1 legacy envelope without a
            // sender account-id — fall back to the local-marker
            // semantics.
            repository.acceptContactRequest(messageId: messageId)
            return
        }
        Task {
            let groupIdHex: String?
            do {
                groupIdHex = try await chatRepository.startGroupWith(
                    recipientAccountId: fromAccountId,
                    recipientSignaturePubkey: pending.fromPubkey
                )
            } catch {
                // chat handshake failed — fall back to the local-
                // marker path with a "chat creation failed" hint
                // on the accepted row.
                groupIdHex = nil
            }
            repository.acceptContactRequest(
                messageId: messageId,
                groupIdHex: groupIdHex
            )
        }
    }

    public func onDeclineClicked(messageId: Int64) {
        repository.declineContactRequest(messageId: messageId)
    }

    // MARK: - Contact detail (UX spec section 5.3)

    /// The accepted contact behind a detail screen, or `nil` once it is gone.
    public func contact(messageId: Int64) -> AcceptedContact? {
        repositoryState.accepted.first { $0.request.messageId == messageId }
    }

    /// The 1x1 chat with this contact: the one the accept flow created, else
    /// the two-person chat whose other member holds the contact's key (the
    /// case where the OTHER side accepted and the Welcome created the chat
    /// here). A read over the live rosters, no state change.
    public func chatGroupIdHex(for contact: AcceptedContact) -> String? {
        guard let chatRepository else { return contact.groupIdHex }
        // The linked id is an address only while this device still holds the
        // chat: a link whose chat is gone (deleted here, or never present on
        // this device) opened a dead chat - seen live on web 2026-09-26, and
        // the same trust existed here.
        let known = chatRepository.knownGroupIdHexes
        if let linked = contact.groupIdHex, known.contains(linked) { return linked }
        let key = contact.request.fromPubkey
        return known.first {
            chatRepository.oneToOnePeerSignatureKey(groupIdHex: $0) == key
        }
    }

    /// The accepted contact on the other side of a conversation between two
    /// people, or `nil` when this device holds none for them.
    ///
    /// The inverse of ``chatGroupIdHex(for:)`` and reads the same two facts:
    /// the id the accept flow recorded, else the other member's key. A read
    /// over the live roster, no state change.
    public func contact(forOneToOneChat groupIdHex: String) -> AcceptedContact? {
        if let recorded = repositoryState.accepted.first(where: { $0.groupIdHex == groupIdHex }) {
            return recorded
        }
        guard let chatRepository,
              let peer = chatRepository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
        else { return nil }
        return repositoryState.accepted.first { $0.request.fromPubkey == peer }
    }

    /// Whether the call buttons on a contact detail can act: a chat exists and
    /// this device's membership in it has not ended (the same gate the chat
    /// header applies to its own call buttons).
    public func canCall(groupIdHex: String) -> Bool {
        guard let chatRepository else { return false }
        let thread = chatRepository.state.threads.first { $0.groupIdHex == groupIdHex }
        return !(thread?.membershipEnded ?? false)
    }

    /// The safety number between this account and the contact (the existing
    /// ``ContactsRepository/safetyNumber(against:)``), or `nil` when the
    /// session cannot compute it (no identity secrets).
    public func safetyNumber(for contact: AcceptedContact) -> String? {
        try? repository.safetyNumber(against: contact.request.fromPubkey)
    }

    /// Set (or clear, when blank) the user-chosen display name for an
    /// accepted contact (ADR-0021 follow-up). The repository persists the
    /// change and the group-call roster picks it up on the next call.
    public func onRenameContactClicked(messageId: Int64, name: String) {
        repository.renameContact(messageId: messageId, to: name)
    }

    /// `docs/ux/conversation-info.md` section 2.1 — drop the contact from this
    /// device's list. Local only, and it deliberately leaves any existing chat
    /// behind; see ``ContactsRepository/removeContact(messageId:)``.
    public func onRemoveContactClicked(messageId: Int64) {
        repository.removeContact(messageId: messageId)
    }

    /// Whether this device's membership in the contact's 1x1 chat has ended
    /// (ADR-0045 UX 6.2). No chat at all is not an ended one: it is a chat
    /// that was never set up, which the screen says in its own words.
    public func membershipEnded(groupIdHex: String) -> Bool {
        guard let chatRepository else { return false }
        return chatRepository.state.threads
            .first { $0.groupIdHex == groupIdHex }?.membershipEnded ?? false
    }

    /// ADR-0045 UX 6.2.4 — forget a conversation this device is out of.
    ///
    /// The repository re-checks the ended membership itself rather than
    /// trusting the control this was drawn behind, and the caller is expected
    /// to have confirmed first: this destroys a transcript that exists nowhere
    /// else, with no undo.
    @discardableResult
    public func onDeleteChatClicked(groupIdHex: String) -> Bool {
        chatRepository?.deleteChat(groupIdHex: groupIdHex) ?? false
    }

    // MARK: - Invite link actions (1.14e7)

    /// Generate a fresh single-use 7-day invite link. Surfaces the
    /// resulting ``GeneratedInvite`` in ``lastGeneratedInvite``;
    /// failures land in ``inviteError``.
    public func onGenerateInviteClicked() {
        guard !generatingInvite else { return }
        generatingInvite = true
        inviteError = nil
        Task {
            do {
                let invite = try await repository.generateInviteLink()
                generatingInvite = false
                inviteError = nil
                lastGeneratedInvite = invite
                // The sheet lists the server-side set; pull it so the new
                // link shows with its server row rather than only the
                // session copy.
                await loadActiveInvites()
            } catch {
                generatingInvite = false
                inviteError = Self.contactsErrorText(for: error, fallbackKey: "chat.error.create_link")
            }
        }
    }

    /// Revoke an outstanding invite by `blobId`. On success the
    /// row is dropped from ``ContactsState/outstandingInvites``
    /// automatically by ``ContactsRepository/revokeInviteLink(blobId:)``.
    public func onRevokeInviteClicked(blobId: Data) {
        Task {
            do {
                _ = try await repository.revokeInviteLink(blobId: blobId)
            } catch {
                inviteError = Self.contactsErrorText(for: error, fallbackKey: "chat.error.revoke_link")
            }
        }
    }

    /// Dismiss the "just generated this link" sheet without
    /// affecting the persistent invite list.
    public func dismissGeneratedInvite() {
        lastGeneratedInvite = nil
    }

    // MARK: - Active invites (Phase 1.18d2b-list)

    /// Whether the "Active invites" panel has ever successfully
    /// loaded its list in this session. Distinguishes "not loaded yet"
    /// from "loaded and empty" — the empty-state copy differs.
    @Published public private(set) var activeInvitesLoaded: Bool = false
    /// Owner-side list pulled via the signed
    /// `POST /discovery/invite/by-owner` endpoint. Loaded when the
    /// Contacts tab opens and on pull-to-refresh (``loadActiveInvites()``).
    @Published public private(set) var activeInvites: [ActiveInvite] = []
    @Published public private(set) var loadingActiveInvites: Bool = false
    @Published public private(set) var activeInvitesError: UiText?
    /// `blobIdSlug` of the row currently being revoked, when any.
    /// The screen disables the Revoke button on that row + shows
    /// a spinner.
    @Published public private(set) var revokingActiveInviteSlug: String?

    /// Load the owner-side active-invites list.
    public func loadActiveInvites() async {
        guard !loadingActiveInvites else { return }
        loadingActiveInvites = true
        activeInvitesError = nil
        defer { loadingActiveInvites = false }
        do {
            let result = try await repository.listActiveInvites()
            switch result {
            case .list(let rows):
                activeInvites = rows
            case .notFoundOrNotOwner:
                // No identity bundle for this account, or
                // signature mismatch — surface as a clean
                // empty list. The most common reason is "this
                // owner has never uploaded an invite", same
                // ergonomics as the web client.
                activeInvites = []
            }
            activeInvitesLoaded = true
        } catch {
            activeInvitesError = Self.contactsErrorText(for: error, fallbackKey: "invite.links.error.load")
        }
    }

    /// Revoke an active invite from the loaded list. Drops the
    /// row locally on success or on a 404 (server-side already
    /// gone — same terminal state).
    public func onRevokeActiveInviteClicked(slug: String) {
        guard revokingActiveInviteSlug == nil else { return }
        guard let row = activeInvites.first(where: { $0.blobIdSlug == slug }) else {
            return
        }
        guard let blobId = base64UrlDecodeNoPad(row.blobIdSlug) else {
            activeInvitesError = .key("chat.error.revoke_link")
            return
        }
        revokingActiveInviteSlug = slug
        activeInvitesError = nil
        Task {
            defer { revokingActiveInviteSlug = nil }
            do {
                _ = try await repository.revokeInviteLink(blobId: blobId)
                activeInvites = activeInvites.filter { $0.blobIdSlug != slug }
            } catch {
                activeInvitesError = Self.contactsErrorText(for: error, fallbackKey: "chat.error.revoke_link")
            }
        }
    }

    // MARK: - Error mapping

    /// How to render a Contacts failure (ADR-0058 section 6; the web
    /// `contactsErrorText`): from its kind, never from its description. A
    /// used-up allowance says so (`quotaKey`: the search copy for a lookup,
    /// the action copy for a link), an unknown username and a person who has
    /// not finished setting up have their own messages, a rate limit and a
    /// network failure the common ones; anything else is logged and shown as
    /// the action's own `fallbackKey`.
    nonisolated static func contactsErrorText(
        for error: Error,
        fallbackKey: String,
        quotaKey: String = "tokens.action_allowance_used"
    ) -> UiText {
        if let token = AnonTokenUnavailable.find(in: error) {
            if case .quotaExhausted = token { return .key(quotaKey) }
            return .key("common.error.offline")
        }
        if let refused = error as? SupersessionRefused {
            let loggedReset = refused.loggedReset
            return UiText { supersessionRefusedMessage(loggedReset: loggedReset, peerLabel: nil) }
        }
        switch error as? ContactsError {
        case .usernameUnknown(let normalised)?:
            return .key("chat.members.add.error.unknown_username", texts: ["name": "@\(normalised)"])
        case .noIdentityBundle?:
            return .key("chat.members.add.error.no_bundle")
        case .wrapped(let cause)?:
            if cause is SupersessionRefused || cause is ContactsError {
                return contactsErrorText(for: cause, fallbackKey: fallbackKey, quotaKey: quotaKey)
            }
        case nil:
            break
        }
        if DiscoveryClientError.isRateLimited(error) { return .key("common.error.too_many_attempts") }
        if isNetworkFailure(error) { return .key("common.error.offline") }
        NSLog("[contacts] action failed: \(error)")
        return .key(fallbackKey)
    }

    /// A discovery or DS request that never reached the server, directly or
    /// behind the contacts wrapper.
    nonisolated private static func isNetworkFailure(_ error: Error) -> Bool {
        if case .network? = error as? DiscoveryClientError { return true }
        if case .network? = error as? DsClientError { return true }
        if error is URLError { return true }
        if case .wrapped(let cause)? = error as? ContactsError { return isNetworkFailure(cause) }
        return false
    }

    /// KT-log S4 — render the reset-aware copy for a fail-closed
    /// ``SupersessionRefused``. The PURE `(loggedReset, peerLabel)` → copy
    /// mapping (mirror of the web `supersessionRefusedMessage`); kept a
    /// `static` so it can be unit-tested without the SwiftUI view.
    ///
    /// The refusal itself is NEVER changed by the log — gate 1 throws
    /// regardless of `loggedReset`; this only colours the message.
    ///
    /// - Parameters:
    ///   - loggedReset: the best-effort transparency-log reset marker the
    ///     refusal carries (`true` = confirmed owner reset, `false` = no
    ///     owner reset shown, `nil` = off / unreachable / inconclusive).
    ///   - peerLabel: the handle the user typed (e.g. `@alice`), rendered
    ///     verbatim as the subject; `nil` / empty picks the unnamed message.
    nonisolated static func supersessionRefusedMessage(
        loggedReset: Bool?,
        peerLabel: String?
    ) -> String {
        // One catalog message per verdict, and an unnamed twin of each
        // rather than "This contact" as an argument (ADR-0058 batch 4). All
        // `security_critical`.
        let verdict: String
        switch loggedReset {
        case .some(true): verdict = "reset"
        case .some(false): verdict = "unexplained"
        case .none: verdict = "unconfirmed"
        }
        if let label = peerLabel, !label.isEmpty {
            return Localization.text("contacts.identity_changed.\(verdict)", texts: ["name": label])
        }
        return Localization.text("contacts.identity_changed.\(verdict)_unnamed")
    }
}
