// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// State + dispatcher for ``ContactsScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/ContactsRoute.tsx` (1.17e6)
///   * `apps/android/.../ui/contacts/ContactsViewModel.kt`
///     (1.15e6 + 1.15e7's `pollOnceLongPoll`)
///
/// **Scope**: pull-to-refresh + accept/decline +
/// requires-reauth panel (1.14e4), the invite generate/revoke
/// surface (1.14e7; 1.15e10 on Android), and continuous
/// `pollOnceLongPoll` (1.14e5) — all wired.
///
/// `@MainActor` because every mutation writes to a `@Published`
/// slot that SwiftUI reads on the main runloop. The repository's
/// own `@Published var state` is mirrored here through a Combine
/// subscription so the view can observe one ObservableObject.
///
/// There is no add-by-username form any more (`docs/ux/who-can-find-me.md`
/// section 6.1): a person is found through the search field
/// (``PeopleSearchModel``), which sends the request from its row.
@MainActor
public final class ContactsViewModel: ObservableObject {

    // MARK: - Ephemeral status

    @Published public private(set) var generatingInvite: Bool = false
    @Published public private(set) var inviteError: UiText?
    /// The last invite link the user generated this session.
    /// Surfaced as a copyable / shareable bottom-sheet on the
    /// Contacts screen; cleared once the user dismisses or
    /// generates another one.
    @Published public var lastGeneratedInvite: GeneratedInvite?

    // MARK: - Mirror of the repository's state

    /// Mirrored from ``ContactsRepository/state``. SwiftUI's
    /// `@ObservedObject` traverses one ObservableObject at a time;
    /// having the VM expose the repository's snapshot here means
    /// the view can subscribe to just the VM and still see every
    /// repository update.
    @Published public private(set) var repositoryState: ContactsState = ContactsState()

    /// ADR-0079 section 4 — the people added through their invite link whose
    /// app has not answered yet, mirrored from ``ContactsRepository/invited``.
    @Published public private(set) var invited: [PendingContact] = []

    /// The pending contacts the list shows: those not yet an accepted
    /// contact on this device.
    public var shownInvited: [PendingContact] {
        PendingContacts.shown(
            invited, oneToOnePeerKeys: Set(repositoryState.accepted.map(\.request.fromPubkey)))
    }

    // MARK: - Wiring

    private let repository: ContactsRepository

    /// The key the per-account invite link secrets are read under.
    var inviteAccountIdB64: String? { repository.inviteAccountIdB64 }
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
        self.invited = repository.invited
        repository.$invited
            .receive(on: RunLoop.main)
            .sink { [weak self] list in self?.invited = list }
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
    /// has carried one (ADR-0048 v2), rides along as it does on an accepted
    /// row.
    public func adoptOneToOnePeers() {
        guard let chatRepository else { return }
        var accountIds: [String: Data] = [:]
        for entry in ProfileDirectory.shared.profilesWithAccountId() {
            accountIds[entry.signatureKeyB64] = entry.accountId
        }
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        let createdAt = ISO8601DateFormatter().string(from: Date())
        for groupIdHex in chatRepository.conversationHeadIdHexes {
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

        guard let chatRepository = chatRepository,
              let fromAccountId = pending.fromAccountId else {
            // No chat bridge wired OR v1 legacy envelope without a
            // sender account-id — fall back to the local-marker
            // semantics.
            Task { await sendAckBestEffort(pending) }
            repository.acceptContactRequest(messageId: messageId)
            return
        }
        Task {
            let groupIdHex: String?
            do {
                groupIdHex = try await chatRepository.startGroupWith(
                    recipientAccountId: fromAccountId,
                    recipientSignaturePubkey: pending.fromPubkey,
                    kind: .oneToOne
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
            // AFTER the chat's Welcome, never before it: see
            // ``sendAckBestEffort(_:)``.
            await sendAckBestEffort(pending)
        }
    }

    /// 1.9b1: ship a ContactAck back to the original sender's mailbox so
    /// their UI can flip from "pending" to "accepted". Best-effort — a
    /// network failure here doesn't unwind the local accept.
    ///
    /// Sent once the 1:1 chat's Welcome has been written. Both are written
    /// with the requester's `reply` grant (ADR-0060 section 6), and a
    /// requester closes that key as soon as the ack arrives: an ack sent
    /// first (or in parallel) let an online requester close the key before
    /// the Welcome landed, and the Welcome was refused (seen live on web
    /// 2026-09-30).
    private func sendAckBestEffort(_ pending: IncomingContactRequest) async {
        do {
            _ = try await repository.sendContactAck(request: pending)
        } catch {
            // best-effort
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

    /// The person behind a detail screen: the accepted contact, else a person
    /// the viewer removed whose one-to-one remains
    /// (`docs/ux/conversation-info.md` section 2.1, amended 2026-10-24) — read
    /// off the live conversation, as the one-to-one rule reads it, under the
    /// row id the rule would give them (``ContactsRepository/syntheticContactMessageId(signatureKey:)``).
    /// Never listed; its card offers `Add to contacts`.
    public func person(messageId: Int64) -> AcceptedContact? {
        contact(messageId: messageId) ?? oneToOnePeerWithoutRow(messageId: messageId)
    }

    /// Whether the person behind a detail screen is on the contact list.
    public func isContact(messageId: Int64) -> Bool {
        contact(messageId: messageId) != nil
    }

    /// The row id a conversation header opens the person card under: the
    /// contact's, else — a person the viewer removed, whose conversation
    /// remains — the id ``person(messageId:)`` reads them back under. `nil`
    /// when the other person cannot be read.
    public func personMessageId(forOneToOneChat groupIdHex: String) -> Int64? {
        if let contact = contact(forOneToOneChat: groupIdHex) { return contact.request.messageId }
        guard let peer = chatRepository?.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { return nil }
        return ContactsRepository.syntheticContactMessageId(signatureKey: peer)
    }

    /// The other person of a one-to-one this device holds who has no contact
    /// row and whose synthetic row id is `messageId`, as a row the card can
    /// draw.
    private func oneToOnePeerWithoutRow(messageId: Int64) -> AcceptedContact? {
        guard messageId < 0, let chatRepository else { return nil }
        let have = Set(repositoryState.accepted.map(\.request.fromPubkey))
        for groupIdHex in chatRepository.conversationHeadIdHexes {
            guard let peer = chatRepository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex),
                  !have.contains(peer),
                  ContactsRepository.syntheticContactMessageId(signatureKey: peer) == messageId
            else { continue }
            let accountId = ProfileDirectory.shared.profilesWithAccountId()
                .first { $0.signatureKeyB64 == base64Encode(peer) }?.accountId
            return ContactsRepository.adopting(
                [], signatureKey: peer, accountId: accountId, groupIdHex: groupIdHex,
                nowMs: 0, createdAt: "")?.first
        }
        return nil
    }

    /// `Add to contacts` on the card of a person the viewer removed: back on
    /// the list with the conversation they have. Local; sends nothing.
    public func onAddContactClicked(person: AcceptedContact) {
        guard let groupIdHex = chatGroupIdHex(for: person) else { return }
        repository.restoreRemovedContact(
            signatureKey: person.request.fromPubkey,
            accountId: person.request.fromAccountId,
            groupIdHex: groupIdHex,
            nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
            createdAt: ISO8601DateFormatter().string(from: Date()))
    }

    /// The 1x1 chat with this contact: the one linked on the contact record
    /// while this device still holds it, else the one-to-one whose two people
    /// are this account and the contact (the case where the OTHER side
    /// accepted and the Welcome created the chat here). The rule is
    /// ``ContactConversation``, shared with the call screen's
    /// `Invite contacts`. A read over the live rosters, no state change.
    public func chatGroupIdHex(for contact: AcceptedContact) -> String? {
        guard let chatRepository else { return contact.groupIdHex }
        return chatRepository.oneToOneConversation(
            contactKey: contact.request.fromPubkey, linkedGroupIdHex: contact.groupIdHex)
    }

    /// The accepted contact on the other side of a conversation between two
    /// people, or `nil` when this device holds none for them.
    ///
    /// The inverse of ``chatGroupIdHex(for:)`` and reads the same two facts:
    /// the id the accept flow recorded, else the other member's key. A read
    /// over the live roster, no state change.
    /// The contact row a group member opens (`docs/ux/conversation-info.md`
    /// section 2.2), by the member's person (account) key; `nil` for a
    /// member who is not a contact. A proven predecessor key names the same
    /// person.
    public func contactMessageId(forPersonKey personKey: Data) -> Int64? {
        let current = ReachCenter.shared.currentAccountKey(of: personKey)
        return repositoryState.accepted.first {
            $0.request.fromPubkey == personKey
                || ReachCenter.shared.currentAccountKey(of: $0.request.fromPubkey) == current
        }?.request.messageId
    }

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

    // MARK: - Invite link actions (1.14e7; `docs/ux/contact-invite-link.md` section 9)

    /// **Create link**: a link with the default terms (no label, 7 days, one
    /// person) at once, no form before it. Surfaces the resulting
    /// ``GeneratedInvite`` in ``lastGeneratedInvite``; a failure lands in
    /// ``inviteError``.
    func onCreateLinkClicked() {
        guard !generatingInvite, let params = InviteCreateForm().params else { return }
        generatingInvite = true
        inviteError = nil
        inviteNotice = nil
        Task {
            do {
                let invite = try await mint(params)
                generatingInvite = false
                lastGeneratedInvite = invite
                await loadActiveInvites()
            } catch {
                generatingInvite = false
                inviteError = Self.contactsErrorText(for: error, fallbackKey: "chat.error.create_link")
            }
        }
    }

    /// **Save** on the edit form of the link at `slug`: a NEW link with
    /// `params` first, then the old one deleted by the path of **Delete
    /// link** (old first would leave nothing on a failure). A failed mint
    /// leaves the old link and keeps the form open; `onSaved` runs once the
    /// new link exists. A failed mint says `invite.links.error.edit`, a
    /// failed delete of the old link `invite.links.error.delete` (as web and
    /// Android).
    func onSaveLinkTermsClicked(
        slug: String, params: InviteCreateForm.Params, onSaved: @escaping () -> Void
    ) {
        guard !generatingInvite, revokingActiveInviteSlug == nil else { return }
        generatingInvite = true
        inviteError = nil
        inviteNotice = nil
        Task {
            let invite: GeneratedInvite
            do {
                invite = try await mint(params)
            } catch {
                generatingInvite = false
                NSLog("[contacts] link terms: the new link was not made: \(error)")
                inviteError = .key("invite.links.error.edit")
                return
            }
            generatingInvite = false
            lastGeneratedInvite = invite
            onSaved()
            do {
                try await deleteLink(slug: slug)
            } catch {
                // The new link exists; the old one still works and stays
                // listed, with its own Delete.
                NSLog("[contacts] link terms: the old link was not deleted: \(error)")
                inviteError = .key("invite.links.error.delete")
            }
            await loadActiveInvites()
        }
    }

    private func mint(_ params: InviteCreateForm.Params) async throws -> GeneratedInvite {
        // The sender does not decide whether the other person has an
        // account: the invite page handles both.
        try await repository.generateInviteLink(
            label: params.label, expiry: params.expiry, maxUses: params.maxUses,
            bootstrapReferral: false)
    }

    /// Drop the create / edit error when a form opens or closes.
    func clearInviteError() { inviteError = nil }

    /// The synced record of the contact link at `slug`, when the register
    /// holds one.
    func linkRecord(slug: String) -> LinkRecord? {
        LinkRecords.contactRecord(slug: slug, in: linkRecords)
    }

    /// **Copy**, before the clipboard: the link is marked copied in the
    /// account's synced state (and sent to the other devices), so no device
    /// offers to edit it any more. Showing the QR code does not mark it.
    func onCopyLinkClicked(slug: String) async {
        await repository.markInviteLinkShared(slug: slug)
    }

    /// **Delete link** (the old Revoke), once confirmed when it had to be
    /// (``LinkRecords/deleteNeedsConfirmation(_:)``): the register marks it
    /// revoked, the ledger row is deleted with the link's revocation seed and
    /// its reach key goes. `Link deleted.` after it; a failure says so.
    func onDeleteLinkClicked(slug: String) {
        guard revokingActiveInviteSlug == nil, !generatingInvite else { return }
        revokingActiveInviteSlug = slug
        activeInvitesError = nil
        inviteError = nil
        inviteNotice = nil
        Task {
            defer { revokingActiveInviteSlug = nil }
            do {
                try await deleteLink(slug: slug)
                inviteNotice = .key("invite.links.deleted")
            } catch {
                NSLog("[contacts] delete link failed: \(error)")
                activeInvitesError = .key("invite.links.error.delete")
            }
        }
    }

    /// The one delete path, shared by **Delete link** and the edit form's
    /// **Save** (``ContactsRepository/revokeInviteLink(blobId:)``). The row
    /// leaves the list on success and when the ledger row was already gone
    /// (the same end state).
    private func deleteLink(slug: String) async throws {
        guard let blobId = base64UrlDecodeNoPad(slug) else {
            throw ContactsError.simple("not an invite link id")
        }
        _ = try await repository.revokeInviteLink(blobId: blobId)
        activeInvites = activeInvites.filter { $0.blobIdSlug != slug }
        deletedLinkSlugs.insert(slug)
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
    /// The notice under the list after a delete (`Link deleted.`).
    @Published public private(set) var inviteNotice: UiText?
    /// The account's synced link records (ADR-0079 section 4), live: whether
    /// a row offers Edit and whether Delete asks first are read from them.
    @Published public private(set) var linkRecords: [LinkRecord] = []
    /// Links deleted from this sheet: the session copy of one
    /// (``ContactsState/outstandingInvites``) whose ledger row was already
    /// gone stays in the repository and must not come back as a row.
    @Published public private(set) var deletedLinkSlugs: Set<String> = []
    private var linkRecordsSubscription: AnyCancellable?

    /// Follow the register once it is wired (at app start, which may come
    /// after this view model): a copy, use or delete on any device of the
    /// account changes the rows at once.
    private func followLinkRecords() {
        guard linkRecordsSubscription == nil,
              let publisher = repository.linkRecordsPublisher else { return }
        linkRecords = repository.linkRecords
        linkRecordsSubscription = publisher
            .dropFirst()
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] records in
                guard let self else { return }
                self.linkRecords = records
                Task { await self.loadActiveInvites() }
            }
    }

    /// Load the owner-side active-invites list.
    public func loadActiveInvites() async {
        followLinkRecords()
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
            // The screen was left while the list was loading: not a failure
            // of the load. The next visit loads it again.
            if Cancellation.endedByCancellation(error) { return }
            activeInvitesError = Self.contactsErrorText(for: error, fallbackKey: "invite.links.error.load")
        }
    }

    // MARK: - Error mapping

    /// How to render a Contacts failure (ADR-0061 section 6; the web
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
        case .usernameUnknown?:
            // `docs/ux/who-can-find-me.md` section 6.1, "Not found": the same
            // answer whether the name is unregistered or its owner does not
            // let this person find them (ADR-0060). Never claim which, and
            // never echo the name back.
            return .key("search.people.not_found")
        case .requestRefused?:
            // `docs/ux/who-can-find-me.md` section 6.1, "Request refused".
            return .key("search.people.request_refused")
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
    /// The copy for a contacts error as plain text, the form the unit
    /// tests pin (ADR-0060 "Not found" wording among them).
    nonisolated static func userFacing(_ err: ContactsError) -> String {
        contactsErrorText(for: err, fallbackKey: "contacts.error.send").text
    }

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
    ///   - loggedReset: the log marker the refusal carries: `false` = the
    ///     log shows the key WITHOUT an owner reset; `nil` = off /
    ///     unreachable / inconclusive. `true` is never passed any more (an
    ///     attested reset is accepted silently and reaches no refusal) and
    ///     renders as `unconfirmed` if it ever arrives.
    ///   - peerLabel: the handle the user typed (e.g. `@alice`), rendered
    ///     verbatim as the subject; `nil` / empty picks the unnamed message.
    nonisolated static func supersessionRefusedMessage(
        loggedReset: Bool?,
        peerLabel: String?
    ) -> String {
        // One catalog message per verdict, and an unnamed twin of each
        // rather than "This contact" as an argument (ADR-0061 batch 4). All
        // `security_critical`.
        let verdict = loggedReset == false ? "unexplained" : "unconfirmed"
        if let label = peerLabel, !label.isEmpty {
            return Localization.text("contacts.identity_changed.\(verdict)", texts: ["name": label])
        }
        return Localization.text("contacts.identity_changed.\(verdict)_unnamed")
    }
}
