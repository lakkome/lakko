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

    // MARK: - ADR-0092 section 7: a contact card's Add contact

    /// What `Add contact` on a contact card came to (UX section 10.2).
    public enum ContactCardAddResult: Equatable {
        /// The request went out.
        case sent
        /// The username now resolves to another key: nothing was sent.
        case otherKey
        /// The username does not resolve under the person's setting.
        case notFound
        /// The lookup or the send failed.
        case failed
    }

    /// The ordinary username lookup, accepted only when the discovered key is
    /// the card's; then the ordinary contact request.
    public func addFromContactCard(username: String, accountKey: Data) async -> ContactCardAddResult {
        let ports = repository.peopleSearchPorts()
        do {
            let canonical = normalizeUsername(input: username)
            let oprf = try await ports.usernameOprf(canonical)
            guard let hit = try await ports.lookupUsername(oprf.lookupKey) else { return .notFound }
            guard let key = try await ports.identityKey(hit.accountId), key == accountKey else { return .otherKey }
            _ = try await repository.sendContactRequest(recipientUsername: canonical, introMessage: "")
            return .sent
        } catch {
            return .failed
        }
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

    /// ADR-0098 step 4 — the repository's state with the requests of
    /// senders known suspended left out of `pending`.
    static func hidingSuspended(_ state: ContactsState, isSuspended: (Data) -> Bool) -> ContactsState {
        let visible = SuspensionMarkRules.visibleRequests(state.pending, isSuspended: isSuspended)
        guard visible.count != state.pending.count else { return state }
        return state.with(pending: visible, lastError: state.lastError)
    }

    public init(
        repository: ContactsRepository,
        chatRepository: ChatRepository? = nil
    ) {
        self.repository = repository
        self.chatRepository = chatRepository
        self.founder = { [weak repository] contact in
            guard let repository else { throw ContactAccept.Unavailable() }
            return try await repository.foundOneToOne(with: contact)
        }
        self.route = { [weak repository] contact in
            repository?.requestRoute(toAccountKey: contact.request.fromPubkey) ?? .none
        }
        self.groupRequester = { [weak repository] contact, groupIdHex in
            guard let repository, let accountId = contact.request.fromAccountId else {
                throw ContactAccept.Unavailable()
            }
            try await repository.sendContactRequestInGroup(
                groupIdHex: groupIdHex, recipientAccountId: accountId,
                recipientAccountKey: contact.request.fromPubkey,
                recipientName: {
                    let resolved = ProfileDirectory.shared.resolve(signatureKey: contact.request.fromPubkey)
                    return resolved.isPlaceholder ? nil : resolved.name
                }())
        }
        self.conversation = { [weak chatRepository] contact in
            chatRepository?.oneToOneConversation(
                contactKey: contact.request.fromPubkey, linkedGroupIdHex: contact.groupIdHex)
        }
        // ADR-0098 step 4 — a held request whose sender the moderation log
        // shows suspended is not shown (it is kept, and comes back when the
        // suspension ends); the senders are asked about as requests arrive.
        let marks = SuspensionMarks.shared
        self.repositoryState = Self.hidingSuspended(repository.state, isSuspended: marks.isSuspended)
        repository.$state
            .combineLatest(marks.$marks)
            .receive(on: RunLoop.main)
            .sink { [weak self] newState, _ in
                self?.repositoryState = Self.hidingSuspended(newState, isSuspended: marks.isSuspended)
            }
            .store(in: &cancellables)
        repository.$state
            .map { $0.pending.map(\.fromPubkey) }
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { marks.check($0) }
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
        // A stored link that names a group (or someone else's one-to-one)
        // is no chat of theirs (2026-11-05); cleared first so the pass below
        // can link the one-to-one they do have.
        repository.dropMisdirectedLinks()
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

    /// The requests whose last Accept failed: their row says so and Accept
    /// works again.
    @Published public private(set) var acceptFailed: Set<Int64> = []
    /// The requests being accepted: a second press does nothing, or it would
    /// found a second chat.
    @Published public private(set) var accepting: Set<Int64> = []

    /// Accept a pending request (``ContactAccept``). The chat is founded
    /// first and the row moves only then. The ack follows the Welcome, never
    /// the other way round: both are written with the requester's `reply`
    /// grant (ADR-0060 section 6), and a requester closes that key as soon as
    /// the ack arrives (an ack sent first had the Welcome refused, seen live
    /// on web 2026-09-30).
    public func onAcceptClicked(messageId: Int64) {
        guard let pending = repository.state.pending.first(where: { $0.messageId == messageId }),
              accepting.insert(messageId).inserted else { return }
        acceptFailed.remove(messageId)
        Task {
            let outcome = await repository.acceptIncomingRequest(pending)
            accepting.remove(messageId)
            if outcome == .failed { acceptFailed.insert(messageId) }
        }
    }

    public func onDeclineClicked(messageId: Int64) {
        acceptFailed.remove(messageId)
        repository.declineContactRequest(messageId: messageId)
    }

    /// **Dismiss** on a row under Sent.
    public func onDismissSentClicked(_ request: SentContactRequest) {
        repository.dismissSentRequest(request)
    }

    /// The Contacts tab's badge: the incoming requests waiting, capped the
    /// way a chat row's unread count is; `nil` for none.
    public static func requestsBadge(waiting: Int) -> String? {
        guard waiting > 0 else { return nil }
        if waiting > 99 { return Localization.text("chats.row.unread_overflow") }
        return waiting.formatted(.number.locale(Localization.shared.locale))
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
    // MARK: - Message on the contact card (maintainer, 2026-11-05)

    /// The contacts whose one-to-one is being founded from Message (the
    /// button shows progress), keyed by the contact row id.
    @Published public private(set) var startingChat: Set<Int64> = []
    /// The contacts whose last founding from Message failed: the card shows
    /// `contacts.error.start_chat` and Message works again.
    @Published public private(set) var startChatFailed: Set<Int64> = []

    /// `docs/ux/who-can-find-me.md` 6.4 — what the card says after Message
    /// found no way to the person, or a request inside a group failed to
    /// send, keyed by the contact row id. A request that went out is read
    /// from Sent instead (``hasWaitingRequest(for:)``), so it survives a
    /// relaunch.
    @Published public private(set) var messageNotice: [Int64: ContactMessageNotice] = [:]

    /// Founds the one-to-one with an accepted contact and links it on the
    /// SAME contact row (``ContactsRepository/foundOneToOne(with:)``, the
    /// call the accept path's founding goes through). A seam for tests.
    var founder: (AcceptedContact) async throws -> String

    /// Which way a request to the contact goes
    /// (``ContactsRepository/requestRoute(toAccountKey:)``). A seam for tests.
    var route: (AcceptedContact) -> ContactRequestRoute

    /// Sends the request inside the shared group
    /// (``ContactsRepository/sendContactRequestInGroup(groupIdHex:recipientAccountId:recipientAccountKey:recipientName:)``).
    /// A seam for tests.
    var groupRequester: (AcceptedContact, String) async throws -> Void

    /// The shared name of a group this device holds, for the "Via {group}"
    /// line of a request that came inside it.
    public func groupName(groupIdHex: String) -> String? {
        chatRepository?.groupName(groupIdHex: groupIdHex)
    }

    /// Whether a request to the contact stands unanswered under Sent: the
    /// card says "Request sent", and Message sends nothing new.
    public func hasWaitingRequest(for contact: AcceptedContact) -> Bool {
        // Read from the repository, not from the mirrored copy, which lags
        // it by a run-loop turn: a second press right after a send must see
        // the row the first one wrote.
        SentRequests.holds(
            repository.state.sent, accountKey: contact.request.fromPubkey,
            accountId: contact.request.fromAccountId)
    }

    /// Message on the contact card, in the order of section 6.4: the chat
    /// this device holds with the contact; else, when a request to them is
    /// waiting, nothing new; else a new one-to-one through their inbox when
    /// a grant reaches it, or a contact request inside a group both are in,
    /// or nothing (the card says they cannot be messaged yet). Returns the
    /// chat to open, or `nil` when there is none to open now.
    public func openOrStartChat(for contact: AcceptedContact) async -> String? {
        if let chat = chatGroupIdHex(for: contact) { return chat }
        let rowId = contact.request.messageId
        guard contact.request.fromAccountId != nil, !hasWaitingRequest(for: contact),
              startingChat.insert(rowId).inserted
        else { return nil }
        startChatFailed.remove(rowId)
        messageNotice[rowId] = nil
        defer { startingChat.remove(rowId) }
        switch route(contact) {
        case .inbox:
            do {
                return try await founder(contact)
            } catch {
                NSLog("contact card: the one-to-one was not started: \(type(of: error))")
                startChatFailed.insert(rowId)
                return nil
            }
        case .group(let groupIdHex):
            do {
                try await groupRequester(contact, groupIdHex)
            } catch {
                NSLog("contact card: the request inside a group was not sent: \(type(of: error))")
                messageNotice[rowId] = .requestFailed
            }
            return nil
        case .none:
            messageNotice[rowId] = .cannotMessage
            return nil
        }
    }

    /// The one-to-one this device holds with a contact, by
    /// ``ChatRepository/oneToOneConversation(contactKey:linkedGroupIdHex:)``.
    /// A seam for tests; with no chat repository there is none.
    var conversation: (AcceptedContact) -> String?

    /// A person's chat is a one-to-one whose other person is that contact
    /// (``ContactConversation``). The id stored on the contact row is only a
    /// hint: one that is not held here, names a group, or names a one-to-one
    /// with someone else is not their chat (seen on web 2026-11-05: Message
    /// opened a group the person is in).
    public func chatGroupIdHex(for contact: AcceptedContact) -> String? {
        conversation(contact)
    }

    /// ADR-0098 / UX 2.1 — Report from a person's card: the person, nothing
    /// included at first, their own messages in the one-to-one offered, and
    /// `Also block` checked.
    public func reportRequest(for contact: AcceptedContact, name: String) -> ReportRequest {
        let key = contact.request.fromPubkey
        let chat = conversation(contact)
        let messages = chat.flatMap { chatRepository?.heldMessages(groupIdHex: $0) } ?? []
        return ReportRequest(
            target: .person(accountKey: key), targetName: name, conversationKind: .card,
            groupIdHex: chat,
            candidates: ReportCandidates.from(messages, onlyAuthor: base64Encode(key), name: { _ in nil }),
            followUp: .block, personName: name)
    }

    /// ADR-0098 — Report a contact request: the requester, with the request
    /// itself included (its wire as it was received; a row stored before
    /// the wire was kept carries none).
    public func reportRequest(forRequest request: IncomingContactRequest, name: String) -> ReportRequest {
        ReportRequest(
            target: .person(accountKey: request.fromPubkey), targetName: name,
            conversationKind: .contactRequest,
            fixedEvidence: request.wire.map { [FfiEvidenceItem.contactRequest(wire: $0)] } ?? [],
            followUp: .block, personName: name)
    }

    /// ADR-0098 / UX 1.2 — block a person from a card or a request row.
    public func block(accountKey: Data) { BlockList.shared.block(accountKey) }

    /// UX 1.3.
    public func unblock(accountKey: Data) { BlockList.shared.unblock(accountKey) }

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
        // Only a one-to-one names a person, and the person is its other
        // member: a row whose stored link is this id but who is not that
        // member (a link that once named a group) is not its contact.
        guard let chatRepository,
              let peer = chatRepository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
        else { return nil }
        let resolver = ReachCenter.shared
        let current = resolver.currentAccountKey(of: peer)
        func isPeer(_ row: AcceptedContact) -> Bool {
            row.request.fromPubkey == peer || resolver.currentAccountKey(of: row.request.fromPubkey) == current
        }
        let accepted = repositoryState.accepted
        return accepted.first { $0.groupIdHex == groupIdHex && isPeer($0) } ?? accepted.first(where: isPeer)
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

/// What the contact card says under Message after it could not open a chat
/// (`docs/ux/who-can-find-me.md` 6.4).
public enum ContactMessageNotice: Equatable {
    /// No grant reaches them and no group is shared: nothing was sent.
    case cannotMessage
    /// The request inside a shared group did not go out; Message works again.
    case requestFailed
}
