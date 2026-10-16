// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import CryptoKit
import Foundation
import LakkoCore

/// State holder for the user's contacts surface.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/ContactsContext.tsx` (1.17e5)
///   * `apps/android/.../contacts/ContactsRepository.kt` (1.15e5)
///
/// Wraps ``ContactsClient`` and pairs it with the active session's
/// ``IdentitySecrets`` from ``AuthRepository``. Exposes a single
/// `@Published` ``state`` slot for the ViewModel to observe.
///
/// **Scope**: send + poll + accept + decline + clear (1.14e3),
/// invite-link generate / revoke / consume (1.14e7 owner /
/// 1.14e8 joiner), and Welcome-to-`ChatRepository` forwarding
/// plus `startGroupWith`-on-accept (1.14f4) — all wired.
///
/// **Persistence.** The accepted list lives in ``AcceptedContactsStore``
/// and the account-inbox cursor and the pending requests in
/// ``ContactsInboxStore``, each one row per account, loaded when the
/// repository binds to a session's account and written back on every
/// change. With no account bound the list is empty and nothing is written:
/// a second account signed in on the same device never sees the first
/// account's contacts. Only the sent log and the session's generated invites stay
/// in memory (there is no server-side mirror to rebuild them from).
@MainActor
public final class ContactsRepository: ObservableObject {

    // MARK: - Published state

    @Published public private(set) var state: ContactsState = ContactsState() {
        didSet {
            // Persist the accepted list so it survives a relaunch
            // (ADR-0021 group-call name resolution needs it).
            if state.accepted != oldValue.accepted {
                // Filed under the bound account only; while logged out (or
                // mid-switch, see `bind`) the list belongs to nobody.
                if let accountIdB64 = boundAccountIdB64 {
                    AcceptedContactsStore.save(state.accepted, accountIdB64: accountIdB64)
                }
                // ADR-0048 — a local rename is the first rung of the display
                // name precedence everywhere, so the resolver must see it.
                ProfileDirectory.shared.setRenames(from: state.accepted)
            }
            // Persist the inbox cursor + pending list for the bound account.
            // Runs AFTER the mutation landed in `state`, so a cursor is only
            // ever written once the batch it came from is applied, and every
            // pending mutation (fetch / accept / decline) is captured. With no
            // bound account (logged out, or mid-teardown) nothing is written.
            if let accountIdB64 = boundAccountIdB64,
               state.pending != oldValue.pending || state.cursor != oldValue.cursor
            {
                inboxStore.save(
                    PersistedContactsInbox(cursor: state.cursor, pending: state.pending),
                    accountIdB64: accountIdB64
                )
            }
            // ADR-0074 section 3.5a rule 5 — the unanswered outgoing requests
            // survive a process death (a re-send after a key rotation needs
            // the recipient and the intro). Not while the bound account's
            // rows are being loaded: a Keychain that could not be read must
            // not be written back as "none".
            if let accountIdB64 = boundAccountIdB64, !loadingBoundRows, state.sent != oldValue.sent {
                do {
                    try sentStore.save(state.sent, accountIdB64: accountIdB64)
                } catch {
                    NSLog("contacts: the sent requests were not persisted")
                }
            }
        }
    }

    /// Set while ``bind(accountIdB64:)`` loads the bound account's rows.
    private var loadingBoundRows = false

    // MARK: - Wiring

    private let authRepository: AuthRepository
    private let clientFactory: () -> ContactsClient
    private let discoveryClientFactory: () -> DiscoveryClient
    private let inboxStore: ContactsInboxStore
    private let sentStore: SentRequestsStoring
    /// The account whose persisted inbox `state.pending` / `state.cursor`
    /// currently mirror; `nil` while logged out. Set BEFORE the state it
    /// governs is loaded so the `didSet` above never files one account's
    /// rows under another.
    private var boundAccountIdB64: String?
    private var cancellables: Set<AnyCancellable> = []

    /// Advanced whenever the bound account changes (``bind(accountIdB64:)``,
    /// ``clear()``). A flow that suspends captures it first and checks it
    /// before it writes ``state`` or goes on: the secrets it read before the
    /// `await` are its own account's, while ``state``, the profile directory,
    /// the reach keys and the chat repository are by then whoever is signed
    /// in.
    let binding = BindingEpoch()

    /// Build a ``DiscoveryClient`` factory that pre-stamps every
    /// returned client with the current session's bearer token.
    /// Phase 1.5b: every protected discovery write (register,
    /// identity upload, keypackage upload, invite upload, invite
    /// revoke) needs the `Authorization: Bearer <token>` header;
    /// the simplest threading model is to read
    /// ``AuthRepository/identitySecrets`` at factory-call time +
    /// install via ``DiscoveryClient/setSessionToken(_:)``.
    private func authenticatedDiscoveryClient() -> DiscoveryClient {
        let client = discoveryClientFactory()
        client.setSessionToken(authRepository.identitySecrets?.sessionToken)
        return client
    }

    /// Optional bridge to the chat surface. Wired in 1.14f4. When
    /// non-nil, ``refreshInbox(waitSecs:)`` forwards Welcome
    /// envelopes to ``ChatRepository/joinGroupFromWelcome(welcomeBytes:)``
    /// so a peer's `startGroupWith` lands a live 1×1 chat on this
    /// device. The ``acceptInviteLink(slug:)`` orchestrator drives
    /// `startGroupWith` against the invite owner when wired.
    ///
    /// Marked `weak` to avoid a retain cycle — both repositories
    /// live in `@StateObject` slots at the `LakkoApp` scope and
    /// outlive any single view tree, so a `weak` reference is
    /// safe.
    public weak var chatRepository: ChatRepository?

    public convenience init(
        authRepository: AuthRepository,
        clientFactory: @escaping () -> ContactsClient = { ContactsClient() },
        discoveryClientFactory: @escaping () -> DiscoveryClient = { DiscoveryClient() },
        inboxStore: ContactsInboxStore = ContactsInboxStore(),
        sentStore: SentRequestsStoring = KeychainSentRequestsStore()
    ) {
        self.init(
            authRepository: authRepository,
            clientFactory: clientFactory,
            discoveryClientFactory: discoveryClientFactory,
            inboxStore: inboxStore,
            sentStore: sentStore,
            accountBinding: authRepository.$identitySecrets
                .map { $0.map { base64Encode($0.accountId) } }
                .eraseToAnyPublisher()
        )
    }

    /// - Parameter accountBinding: the signed-in account (base64 id), `nil`
    ///   while logged out. Production follows ``AuthRepository/identitySecrets``;
    ///   tests drive it directly so two accounts can be switched without a
    ///   network session.
    init(
        authRepository: AuthRepository,
        clientFactory: @escaping () -> ContactsClient = { ContactsClient() },
        discoveryClientFactory: @escaping () -> DiscoveryClient = { DiscoveryClient() },
        inboxStore: ContactsInboxStore = ContactsInboxStore(),
        sentStore: SentRequestsStoring = KeychainSentRequestsStore(),
        accountBinding: AnyPublisher<String?, Never>
    ) {
        self.authRepository = authRepository
        self.clientFactory = clientFactory
        self.discoveryClientFactory = discoveryClientFactory
        self.inboxStore = inboxStore
        self.sentStore = sentStore
        // ADR-0050 section 2.9 — contacts carried by a pairing state bundle
        // (or a backup restore), merged only into the account they belong to.
        stateBundleObserver = NotificationCenter.default.addObserver(
            forName: .lakkoStateBundleApplied, object: nil, queue: .main
        ) { [weak self] note in
            guard let contacts = note.object as? [AcceptedContact], !contacts.isEmpty,
                  let accountIdB64 = note.userInfo?[StateBundle.appliedAccountKey] as? String
            else { return }
            Task { @MainActor in self?.mergeBundledContacts(contacts, accountIdB64: accountIdB64) }
        }
        // Follow the session: bind the persisted accepted list and inbox of
        // whichever account is live, unbind on logout / forgetDevice. The
        // accepted list is rehydrated here (ADR-0021), not at init: before a
        // session there is no account to read it for. `@Published` on a
        // `@MainActor` class fires on the main actor, and this class is main
        // actor too, so the hop is an assertion rather than a dispatch.
        // `refreshInbox` binds on its own as well, so a poll that starts
        // before this emission is delivered is not left on the wrong cursor.
        accountBinding
            .removeDuplicates()
            .sink { [weak self] accountIdB64 in
                MainActor.assumeIsolated { self?.bind(accountIdB64: accountIdB64) }
            }
            .store(in: &cancellables)
    }

    private var stateBundleObserver: NSObjectProtocol?

    // MARK: - Account binding

    /// Point `state.accepted`, `state.pending` and `state.cursor` at one
    /// account's persisted rows (or at nothing, for `nil`). Idempotent for
    /// the already-bound account. The bound marker is switched FIRST so the
    /// `didSet` that follows never writes the outgoing account's rows under
    /// the incoming one, and never writes the incoming account's freshly
    /// loaded rows back out as an empty list for the outgoing one.
    private func bind(accountIdB64: String?) {
        guard boundAccountIdB64 != accountIdB64 else { return }
        binding.advance()
        boundAccountIdB64 = nil
        guard let accountIdB64 else {
            // Logged out: the in-memory contacts and inbox belong to nobody
            // now. The persisted rows stay (logout keeps per-account state
            // for a cheap re-login; `forgetDevice` is what wipes them). The
            // sent log and generated invites were the outgoing account's too.
            state = state.with(
                pending: [], accepted: [], sent: [], outstandingInvites: [], cursor: 0)
            return
        }
        let persisted = inboxStore.load(accountIdB64: accountIdB64)
        let accepted = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        let sent = sentStore.load(accountIdB64: accountIdB64)
        boundAccountIdB64 = accountIdB64
        loadingBoundRows = true
        state = state.with(
            pending: persisted.pending, accepted: accepted, sent: sent, outstandingInvites: [],
            cursor: persisted.cursor)
        loadingBoundRows = false
    }

    // MARK: - Send

    /// Resolve the username typed in the search box, compose a
    /// `ContactRequest`, publish it to the recipient's mailbox.
    public func sendContactRequest(
        recipientUsername: String,
        introMessage: String
    ) async throws -> SendResult {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }

        let client = clientFactory()
        // Build the sender's canonical AccountIdentity from the
        // phrase-derived seed (ADR-0017 — was the OPAQUE export key
        // pre-ADR-0017). The build is sub-millisecond (HKDF + Ed25519
        // + X25519); a fresh handle per operation keeps the lifetime
        // story simple — Swift ARC drops the Rust-side handle on scope
        // exit.
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        let bound = binding.capture()
        let result = try await client.sendContactRequest(
            senderSigningSeed: identity.ed25519Seed(),
            senderAccountId: secrets.accountId,
            senderSignaturePubkey: identity.signaturePublicKey(),
            recipientUsername: recipientUsername,
            profile: ProfileDirectory.shared.ownProfileForRequest(),
            introMessage: Data(introMessage.utf8),
            reach: Self.requestReach()
        )
        // Sent as the account that asked; the sent log is by now another
        // account's (or nobody's).
        guard bound.isCurrent else { return result }
        let sent = SentContactRequest(
            recipientUsername: result.recipientUsername,
            recipientAccountIdB64: base64Encode(result.recipientAccountId),
            messageId: result.messageId,
            createdAt: result.createdAt,
            intro: SentRequests.storedIntro(introMessage)
        )
        state = state.with(sent: SentRequests.adding(sent, to: state.sent))
        return result
    }

    /// ADR-0060 — send a contact request to a person a search found: their
    /// identity bundle through the pin gate (it must name the key the search
    /// found), then the request, written with the key the search found them
    /// under (`nil` for a person in Everyone, whose mailbox is open).
    public func sendContactRequest(to person: FoundPerson, introMessage: String) async throws -> SendResult {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple("no identity secrets — please re-enter your password"))
        }
        guard let accountId = person.accountId else {
            throw ContactsError.wrap(ContactsError.simple("this person cannot be reached"))
        }
        let client = clientFactory()
        let bound = binding.capture()
        let bundle = try await client.resolveVerifiedBundle(accountId: accountId)
        // ADR-0074 section 3.5a — the search may have found the person under
        // a key they have since rotated away BY PROOF (the gate above just
        // recorded it): the same person. An unproven mismatch is refused.
        ReachCenter.shared.reloadKeyResolver()
        guard PeopleSearch.acceptsPublishedKey(
            found: person.accountKey, published: bundle.signaturePubkey,
            currentOfFound: ReachCenter.shared.currentAccountKey(of: person.accountKey)
        ) else {
            throw ContactsError.requestRefused
        }
        // The request below carries the own profile and a reply key, both
        // read from what is bound NOW: never another account's.
        try bound.require()
        let result = try await sendRequestToBundle(
            client: client, secrets: secrets,
            recipientAccountId: accountId,
            recipientSignaturePubkey: bundle.signaturePubkey,
            recipientX25519Pubkey: bundle.x25519Pubkey,
            introMessage: Data(introMessage.utf8),
            link: person.mailbox.map { RequestLink(seed: person.writeSeed, target: $0) })
        guard bound.isCurrent else { return result }
        state = state.with(sent: SentRequests.adding(
            SentContactRequest(
                recipientUsername: person.username ?? "",
                recipientAccountIdB64: base64Encode(accountId),
                messageId: result.messageId,
                createdAt: result.createdAt,
                intro: SentRequests.storedIntro(introMessage)),
            to: state.sent))
        return result
    }

    /// A contact request against a bundle in hand, with rule 6 of ADR-0074
    /// section 3.5a: when the recipient's mailbox refuses the write, their
    /// identity bundle is fetched through the fail-closed gate once, and when
    /// that PROVED the key in hand a predecessor the request is sealed again
    /// to the current keys and sent to the current mailbox, written with
    /// whatever grant this device holds for the person (a link's own grant
    /// opened the old mailbox only and is spent, rule 5). A re-check that
    /// proves nothing leaves the refusal as it was.
    func sendRequestToBundle(
        client: ContactsClient,
        secrets: IdentitySecrets,
        recipientAccountId: Data,
        recipientSignaturePubkey: Data,
        recipientX25519Pubkey: Data,
        introMessage: Data,
        link: RequestLink?
    ) async throws -> SendResult {
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        func send(reach: ContactRequestReach) async throws -> SendResult {
            try await client.sendContactRequestToBundle(
                senderSigningSeed: identity.ed25519Seed(),
                senderAccountId: secrets.accountId,
                senderSignaturePubkey: identity.signaturePublicKey(),
                recipientAccountId: recipientAccountId,
                recipientSignaturePubkey: recipientSignaturePubkey,
                recipientX25519Pubkey: recipientX25519Pubkey,
                profile: ProfileDirectory.shared.ownProfileForRequest(),
                introMessage: introMessage,
                reach: reach)
        }
        do {
            return try await send(reach: Self.requestReach(link: link))
        } catch ContactsError.requestRefused {
            await PersonAddressRecheck.recheck(
                accountKey: recipientSignaturePubkey, accountIdHint: recipientAccountId)
            guard ReachCenter.shared.currentAccountKey(of: recipientSignaturePubkey)
                    != recipientSignaturePubkey else {
                throw ContactsError.requestRefused
            }
            // The first attempt's reply grant was registered and never
            // delivered: it goes before the retry mints its own.
            ReachCenter.shared.retireReplies(purpose: ReplyPurpose.contact(recipientAccountId))
            return try await send(reach: Self.requestReach())
        }
    }

    /// The production lookups a people search makes.
    public func peopleSearchPorts() -> PeopleSearchPorts {
        let factory = clientFactory
        return PeopleSearchPorts(
            lookupUsername: { try await DiscoveryClient().lookupPublic(lookupKey: $0) },
            usernameOprf: { try await DiscoveryClient().usernameOprf($0) },
            nameTermOprf: { try await DiscoveryClient().nameTermOprf($0) },
            nameLookup: { try await DiscoveryClient().nameLookup(termLookupKey: $0) },
            identityKey: { accountId in
                try await factory().resolveVerifiedBundle(accountId: accountId).signaturePubkey
            },
            reachCheck: { target, seed in
                // ADR-0078 — the mailbox the result's verified binding names.
                try await DsClient().reachCheck(pseudonym: target.address, reachSeed: seed)
            },
            verifyCertificate: { cert, lookupKey, accountId in
                await ProfileDirectory.shared.verifyCertificate(
                    certificate: cert, lookupKey: lookupKey, accountId: accountId)
            })
            .with(currentKey: { key in
                // ADR-0074 section 3.5a — read after the gated fetch, from
                // the store the gate writes (the resolver reloads from it).
                await MainActor.run {
                    ReachCenter.shared.reloadKeyResolver()
                    return ReachCenter.shared.currentAccountKey(of: key)
                }
            })
    }

    /// ADR-0060 section 6 — an outgoing request carries a fresh `reply` grant
    /// (registered first, retired when the recipient answers) and is written
    /// with whatever grant this device holds for the recipient. `link` is a
    /// mailbox the request found its recipient through (a link grant, a
    /// search result's record, with the seed that opens it or none for an
    /// Everyone mailbox), used for this one request and never kept.
    ///
    /// ADR-0078 — the mailbox is the VERIFIED one the link or a held grant
    /// names; with neither nothing is sent.
    static func requestReach(link: RequestLink? = nil) -> ContactRequestReach {
        ContactRequestReach(
            replyGrant: { recipientAccountId in
                try await ReachCenter.shared.newReplyGrant(
                    purpose: ReplyPurpose.contact(recipientAccountId))
            },
            plan: { recipientKey in
                if let link {
                    return MailboxWritePlan(
                        target: link.target,
                        candidates: link.seed.map { [DsMailboxWriteAuth(reachSeed: $0)] } ?? [])
                }
                return await MainActor.run {
                    ReachCenter.shared.writePlan(toAccountKey: recipientKey)
                }
            })
    }

    /// Resolve `@username -> (accountId, signaturePubkey)` through the
    /// G-4-verified ``ContactsClient/resolveVerifiedRecipient(username:)``
    /// path. Used by the group-chat add-member flow (``ChatViewModel``);
    /// the fresh `clientFactory()` client carries the same pin store the
    /// 1×1 ContactRequest path verifies against.
    public func resolveVerifiedRecipient(
        username: String
    ) async throws -> (accountId: Data, signaturePubkey: Data) {
        let client = clientFactory()
        return try await client.resolveVerifiedRecipient(username: username)
    }

    // MARK: - Poll

    /// The catalog key (ADR-0061) shown in place of a transport error when
    /// the inbox poll fails; the screen renders it in the current language.
    static let inboxFetchFailureMessage = "contacts.mailbox.poll_failed"

    /// `true` when `error` is a cancellation of the poll itself rather than
    /// a failure of it: Swift's `CancellationError`, a `URLError.cancelled`,
    /// or a `DsClientError.network` wrapping one.
    ///
    /// The rule itself is ``Cancellation/isCancellation(_:)``, shared by every
    /// screen; this name is kept for the inbox poll and its tests.
    static func isCancellation(_ error: Error) -> Bool {
        Cancellation.isCancellation(error)
    }

    /// The catalog key (ADR-0061) for a Welcome this device could not join.
    static let welcomeJoinFailureMessage = "contacts.mailbox.join_failed"

    /// Poll the stranger inbox for new ContactRequest / Welcome
    /// envelopes. Updates ``ContactsState/pending`` with any new
    /// requests + bumps the cursor.
    ///
    /// **Welcome forwarding** (1.14f4) is wired when a
    /// ``chatRepository`` bridge is attached: each Welcome event
    /// is forwarded to `ChatRepository.joinGroupFromWelcome`,
    /// best-effort (a single bad Welcome doesn't block the rest
    /// of the poll). With no bridge attached the Welcome is
    /// skipped; the cursor advances either way so we don't
    /// re-process the row.
    ///
    /// - Parameter waitSecs: DS long-poll budget. `0` (the
    ///   default) returns immediately whatever's there — manual
    ///   refresh; the UI spinner blips on/off. Non-zero values
    ///   hold the connection up to that many seconds; the
    ///   ``ContactsState/refreshing`` flag is **not** raised so
    ///   the spinner doesn't flicker for the full long-poll
    ///   window (1.14e5's continuous-polling loop uses
    ///   `waitSecs = 30`).
    public func refreshInbox(waitSecs: Int = 0) async {
        guard let secrets = authRepository.identitySecrets else {
            state = state.with(requiresReauth: true)
            return
        }
        // Secrets are present, so any earlier "sign in again" verdict is
        // stale (e.g. a poll that ran before a login in this process).
        if state.requiresReauth {
            state = state.with(requiresReauth: false, lastError: state.lastError)
        }
        // Bind the persisted inbox of this account before the cursor is
        // read, so the first poll of a session never starts from `0` when a
        // cursor is on disk. The account is remembered so a batch that
        // outlives the session (logout mid-poll) is dropped, never filed
        // under whoever signs in next.
        let pollAccountIdB64 = base64Encode(secrets.accountId)
        bind(accountIdB64: pollAccountIdB64)
        let isManualRefresh = waitSecs == 0
        if isManualRefresh {
            state = state.with(refreshing: true, lastError: nil)
        }
        // A Welcome that fails to join is otherwise invisible: the cursor has
        // already moved past it, so it is never retried and the chat simply
        // never appears. Remember the failure so this poll's final state can
        // say so.
        let welcomeJoinFailure: String? = nil
        // ADR-0060 section 6 — one inbound mailbox per person: every Welcome,
        // for any of this account's devices, lands in the ACCOUNT mailbox.
        // ADR-0078 — device mailboxes are gone (none is claimed or read).
        do {
            let client = clientFactory()
            let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
            // ADR-0053 / ADR-0078 — the account mailbox is named by the
            // account's mailbox key, derived from its seed.
            let pollResult = try await client.pollStrangerInbox(
                accountIdentity: identity,
                after: state.cursor,
                waitSecs: waitSecs
            )
            // ADR-0073 R.3 — the requests that were waiting, unshown, for an
            // identity verdict are judged again first; one whose sender is no
            // longer an established contact is an ordinary request.
            var newPending: [IncomingContactRequest] = await retryWaitingKnownRequests(client: client)
            for event in pollResult.events {
                // The session ended (or changed account) while this batch
                // was in flight, or while a Welcome of it was being joined:
                // the rest belongs to the old account. Its grants, profiles
                // and Welcomes are not filed under whoever is bound now.
                guard boundAccountIdB64 == pollAccountIdB64 else { break }
                switch event {
                case .contactRequest(let req, let replyGrant):
                    // ADR-0060 section 6 — the requester's `reply` grant, filed
                    // under the key the request signature authenticated; the
                    // accept (Welcome) and the ack are written with it.
                    if let replyGrant {
                        ReachCenter.shared.keep(
                            replyGrant, carrier: .request,
                            fromAccountKeyB64: base64Encode(req.fromPubkey), scopeHex: nil)
                    }
                    // ADR-0048 §3 — a request's profile is bound to its
                    // `fromPubkey`, which the request signature authenticated.
                    // Kept only when strictly newer than what is held, so a
                    // replayed old request never rolls a name back.
                    if let profile = req.profile {
                        ProfileDirectory.shared.acceptReceived(
                            profile, signatureKeyB64: base64Encode(req.fromPubkey),
                            accountIdB64: pollAccountIdB64)
                    }
                    // ADR-0073 R.3 — a request from an account that is ALREADY
                    // an established contact (it reset its keys and is writing
                    // from the new ones) is never a request row: it is
                    // answered silently, dropped, or kept waiting for a
                    // verdict. Nothing is shown either way.
                    if await handleKnownContactRequest(req, client: client) { continue }
                    newPending.append(req)
                case .welcome(_, let welcomeBytes, _):
                    // 1.14f4 wires `ChatRepository.joinGroupFromWelcome`
                    // here. Best-effort: a single bad Welcome
                    // doesn't block the inbox poll from advancing
                    // the cursor for the rest of the events.
                    // Cursor has already advanced so we won't
                    // re-process this row on the next poll.
                    // `joinGroupFromWelcome` republishes this device's
                    // KeyPackage on success AND failure, so the directory
                    // never keeps serving a KeyPackage the Welcome consumed.
                    //
                    // ADR-0060 section 6 — every Welcome for this account now
                    // lands here: a chat Welcome sealed to this device's leaf,
                    // one sealed to another of this account's devices (which
                    // this device cannot open), and a channel co-publisher
                    // Welcome sealed to the account identity (which
                    // `CoPublisher.pollForCoPublishWelcome` joins itself). A
                    // failure is therefore logged, not surfaced: most failures
                    // here are Welcomes that were never this device's to open.
                    if let chatRepository = chatRepository {
                        do {
                            _ = try await chatRepository
                                .joinGroupFromWelcome(welcomeBytes: welcomeBytes)
                        } catch {
                            NSLog("contacts inbox: an account-mailbox Welcome did not open as a chat (expected for a co-publisher Welcome): \(error)")
                        }
                    }
                    continue
                case .contactAck(let ack):
                    // The session ended (or changed account) while this
                    // batch was in flight: the ack belongs to the old
                    // account, never to whoever is bound now.
                    guard boundAccountIdB64 == pollAccountIdB64 else { continue }
                    // The recipient of one of our outgoing requests pressed
                    // Accept. The signature was verified in the UniFFI layer,
                    // so this is the moment the person becomes a contact on
                    // this side too: the Welcome that follows creates the
                    // chat, never the contact row (a requester whose request
                    // was accepted on Android had a named chat and an empty
                    // Contacts tab, seen live on 2026-09-16). One row per
                    // person; the matching sent request is retired.
                    let alreadyKnown = state.accepted.contains {
                        $0.request.fromPubkey == ack.accepterPubkey
                    }
                    let accepterAccountIdB64 = base64Encode(ack.accepterAccountId)
                    // ADR-0060 — the request was answered: its `reply` key goes.
                    ReachCenter.shared.retireReplies(
                        purpose: ReplyPurpose.contact(ack.accepterAccountId))
                    let remainingSent = SentRequests.answered(
                        recipientAccountIdB64: accepterAccountIdB64, in: state.sent)
                    if alreadyKnown {
                        if remainingSent.count != state.sent.count {
                            state = state.with(sent: remainingSent)
                        }
                        continue
                    }
                    let syntheticRequest = IncomingContactRequest(
                        messageId: ack.messageId,
                        fromPubkey: ack.accepterPubkey,
                        fromAccountId: ack.accepterAccountId,
                        fromPseudonym: ack.ackToPseudonym,
                        validAfterMs: ack.acceptedAtMs,
                        intro: nil,
                        createdAt: ack.createdAt
                    )
                    state = state.with(
                        accepted: state.accepted + [
                            AcceptedContact(request: syntheticRequest, groupIdHex: nil)
                        ],
                        sent: remainingSent
                    )
                    continue
                }
            }
            // Keep a join failure visible across the background long-poll
            // loop (whose next quiet poll would otherwise clear it within
            // seconds); a manual refresh is the user acknowledging it.
            let carriedJoinFailure = !isManualRefresh
                && state.lastError == Self.welcomeJoinFailureMessage
                ? Self.welcomeJoinFailureMessage
                : nil
            // The session ended (or changed account) while this batch was in
            // flight: its rows and cursor belong to the old account. Drop the
            // batch rather than file it under the new binding.
            guard boundAccountIdB64 == pollAccountIdB64 else {
                state = state.with(refreshing: false, lastError: carriedJoinFailure)
                return
            }
            // Dedupe against the CURRENT state (an accept / decline may have
            // landed during the awaits above): a request already pending, or
            // from a sender already accepted, is not re-added.
            //
            // ADR-0074 section 3.5a rule 5 — one request per person: a
            // request from the key that superseded a pending request's key BY
            // PROOF replaces that row, silently.
            await client.proveRequesterRotations(
                ContactsInboxMerge.rotationCandidates(fetched: newPending, pending: state.pending))
            guard boundAccountIdB64 == pollAccountIdB64 else {
                state = state.with(refreshing: false, lastError: carriedJoinFailure)
                return
            }
            let resolver = client.mailboxResolver()
            let merged = ContactsInboxMerge.merge(
                fetched: newPending, pending: state.pending, accepted: state.accepted,
                currentKey: resolver.current(of:))
            #if DEBUG
            for (request, reason) in merged.dropped {
                NSLog(
                    "contacts inbox: dropped fetched request messageId=\(request.messageId) reason=\(reason)"
                )
            }
            #endif
            state = state.with(
                pending: merged.applied(to: state.pending),
                cursor: ContactsInboxMerge.advancedCursor(
                    current: state.cursor, polled: pollResult.cursor),
                refreshing: false,
                lastError: welcomeJoinFailure ?? carriedJoinFailure
            )
            // ADR-0074 section 3.5a rule 4 — after a rotation the account
            // moved to the new mailbox at once; a sender that has not seen
            // the proven change yet still writes to the old one, which is
            // READ (never written to, nothing registered there) for 30 days
            // after this device adopted, then left.
            await pollPredecessorMailboxes(accountIdB64: pollAccountIdB64)
        } catch {
            // A cancelled poll (the screen was left, the session ended, the
            // long-poll was superseded) is not a failure: keep whatever the
            // slot held and let the next iteration run.
            if Self.isCancellation(error) {
                state = state.with(refreshing: false, lastError: state.lastError)
                return
            }
            // Background long-polls surface failures on the `lastError`
            // slot too — the UI renders one plain sentence, never the
            // transport's own description (endpoint paths and pseudonyms
            // are not user copy). The raw error goes to the log; the
            // polling loop retries on its next iteration.
            NSLog("contacts inbox: poll failed: \(error)")
            state = state.with(refreshing: false, lastError: Self.inboxFetchFailureMessage)
        }
    }

    /// When the predecessor mailboxes were last read (unix ms), per process.
    private var lastPredecessorPollMs: Int64 = 0

    /// ADR-0074 section 3.5a rule 4 — read the mailbox of each of this
    /// account's own PREDECESSOR keys, for the Welcomes of people who have
    /// not yet seen the new key (a contact request or an ack found there is
    /// not acted on, rule 5). Each key of the chain has its own cursor and
    /// its own 30-day window. Read only (the claim a first
    /// read may make registers the access key, never a reach key), and only
    /// for ``PredecessorMailbox/readWindowMs`` after this device adopted the
    /// rotation that retired the key; after that the mailbox is left. A
    /// short read with a cursor of its own, at most once per
    /// ``PredecessorMailbox/pollIntervalMs``. Best effort: a failure is
    /// logged and the next round reads again.
    private func pollPredecessorMailboxes(accountIdB64: String) async {
        let seeds = KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64)
        guard !seeds.isEmpty else { return }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        guard PredecessorMailbox.due(lastPollMs: lastPredecessorPollMs, nowMs: now) else { return }
        lastPredecessorPollMs = now
        let cursors = PredecessorMailbox.CursorStore()
        let adoptions = PredecessorMailbox.AdoptionStore()
        for seed in seeds {
            guard let identity = try? Identity.fromSeed(seed: seed) else { continue }
            let keyB64 = base64Encode(identity.signaturePublicKey())
            guard !PredecessorMailbox.readable(
                keysB64: [keyB64], accountIdB64: accountIdB64, nowMs: now, adoptions: adoptions
            ).isEmpty else { continue }
            do {
                // ADR-0078 — the predecessor mailbox is the mailbox key of
                // the predecessor SEED.
                let result = try await clientFactory().pollStrangerInbox(
                    accountIdentity: identity,
                    after: cursors.cursor(accountIdB64: accountIdB64, keyB64: keyB64),
                    waitSecs: 0
                )
                guard boundAccountIdB64 == accountIdB64 else { return }
                for event in result.events {
                    switch event {
                    case .welcome(_, let welcomeBytes, _):
                        do {
                            _ = try await chatRepository?.joinGroupFromWelcome(welcomeBytes: welcomeBytes)
                        } catch {
                            NSLog("contacts inbox: a predecessor-mailbox Welcome did not open as a chat: \(error)")
                        }
                    case .contactRequest, .contactAck:
                        // Rule 5 — a predecessor mailbox carries Welcomes
                        // only. A request sent here was sealed to the old
                        // key and its sender still addresses the old
                        // mailbox; an ack answers a request whose reply
                        // grant is spent (the rotating device re-sent it
                        // from the new key). Neither is acted on.
                        continue
                    }
                }
                guard boundAccountIdB64 == accountIdB64 else { return }
                cursors.advance(accountIdB64: accountIdB64, keyB64: keyB64, to: result.cursor)
            } catch {
                NSLog("contacts inbox: a predecessor mailbox could not be read: \(type(of: error))")
            }
        }
    }

    /// ADR-0050 section 2.9 — merge the contacts carried by a pairing state
    /// bundle: contacts not already present (by signature key) are appended.
    /// Returns how many were added.
    @discardableResult
    public func mergeBundledContacts(_ contacts: [AcceptedContact], accountIdB64: String) -> Int {
        // Contacts filed for another account (a pairing or restore that
        // finished after a switch) are not this list's to show.
        guard boundAccountIdB64 == accountIdB64 else { return 0 }
        let have = Set(state.accepted.map { $0.request.fromPubkey })
        let fresh = contacts.filter { !have.contains($0.request.fromPubkey) }
        guard !fresh.isEmpty else { return 0 }
        state = state.with(accepted: state.accepted + fresh)
        return fresh.count
    }

    // MARK: - A one-to-one conversation IS a contact

    /// The one rule behind the Contacts tab: a person this account has a
    /// one-to-one conversation with is a contact, full stop. Until 2026-09-26 a
    /// contact row was written only by Accept and by an incoming ContactAck,
    /// while the conversation itself arrives by another event (the Welcome, or
    /// a second device being admitted to the account's conversations), so any
    /// path that delivered the conversation without the acknowledgement left a
    /// named chat and an empty Contacts tab - seen live on a second device.
    ///
    /// Adds a row for `signatureKey` when none exists, or completes an existing
    /// row's `groupIdHex` when it was never recorded. Returns whether anything
    /// changed. Never overwrites a recorded chat, a display name or an account
    /// id the row already holds.
    @discardableResult
    public func adoptOneToOnePeer(
        signatureKey: Data,
        accountId: Data?,
        groupIdHex: String,
        nowMs: UInt64,
        createdAt: String
    ) -> Bool {
        guard let next = Self.adopting(
            state.accepted, signatureKey: signatureKey, accountId: accountId,
            groupIdHex: groupIdHex, nowMs: nowMs, createdAt: createdAt)
        else { return false }
        state = state.with(accepted: next)
        return true
    }

    /// The pure half of ``adoptOneToOnePeer(signatureKey:accountId:groupIdHex:nowMs:createdAt:)``:
    /// the accepted list after adopting the peer, or `nil` when it is already
    /// complete. A row minted here carries a synthetic request (no mailbox
    /// message ever existed): the id is derived from the key, negative so it
    /// can never collide with a mailbox id, and the pseudonym is zero like the
    /// invite-link path's.
    nonisolated static func adopting(
        _ accepted: [AcceptedContact],
        signatureKey: Data,
        accountId: Data?,
        groupIdHex: String,
        nowMs: UInt64,
        createdAt: String
    ) -> [AcceptedContact]? {
        if let idx = accepted.firstIndex(where: { $0.request.fromPubkey == signatureKey }) {
            let old = accepted[idx]
            guard old.groupIdHex == nil else { return nil }
            var rows = accepted
            rows[idx] = AcceptedContact(
                request: old.request, groupIdHex: groupIdHex, displayName: old.displayName)
            return rows
        }
        let synthetic = IncomingContactRequest(
            messageId: syntheticContactMessageId(signatureKey: signatureKey),
            fromPubkey: signatureKey,
            fromAccountId: accountId,
            fromPseudonym: Data(repeating: 0, count: 32),
            validAfterMs: nowMs,
            intro: nil,
            createdAt: createdAt
        )
        return accepted + [AcceptedContact(request: synthetic, groupIdHex: groupIdHex)]
    }

    // MARK: - Found the one-to-one with a contact

    /// Found the one-to-one conversation with an accepted contact and link
    /// it on the contact record. Mirror of the web `foundOneToOneWith`.
    ///
    /// The creation itself is the one call every contact flow makes
    /// (``ChatRepository/startGroupWith(recipientAccountId:recipientSignaturePubkey:kind:recipientPlan:)``
    /// with the one-to-one kind, as Accept and the invite-link accept do):
    /// ADR-0062, a contact's conversation is a one-to-one for good. This adds
    /// the link, for a caller whose contact is already in the accepted list:
    /// the call screen's `Invite contacts`, when the invited contact has
    /// never been written to from this device (`docs/ux/call-links.md`
    /// section 3).
    ///
    /// - Returns: the new conversation's id.
    public func foundOneToOne(with contact: AcceptedContact) async throws -> String {
        guard let chatRepository else {
            throw InviteContactSendError.chatUnavailable
        }
        guard let accountId = contact.request.fromAccountId else {
            throw InviteContactSendError.contactHasNoAccountId
        }
        let bound = binding.capture()
        let groupIdHex = try await chatRepository.startGroupWith(
            recipientAccountId: accountId,
            recipientSignaturePubkey: contact.request.fromPubkey,
            kind: .oneToOne
        )
        try bound.require()
        if let next = Self.linking(
            state.accepted, signatureKey: contact.request.fromPubkey, groupIdHex: groupIdHex) {
            state = state.with(accepted: next)
        }
        return groupIdHex
    }

    /// The accepted list with `groupIdHex` recorded on the contact holding
    /// `signatureKey`, or `nil` when nothing changes (no such contact, or the
    /// link is already that one). Unlike ``adopting(_:signatureKey:accountId:groupIdHex:nowMs:createdAt:)``
    /// this REPLACES a recorded link: the caller has just founded the
    /// conversation because the recorded one, if any, is gone.
    nonisolated static func linking(
        _ accepted: [AcceptedContact], signatureKey: Data, groupIdHex: String
    ) -> [AcceptedContact]? {
        guard let idx = accepted.firstIndex(where: { $0.request.fromPubkey == signatureKey }),
              accepted[idx].groupIdHex != groupIdHex else { return nil }
        var rows = accepted
        rows[idx] = AcceptedContact(
            request: accepted[idx].request, groupIdHex: groupIdHex,
            displayName: accepted[idx].displayName)
        return rows
    }

    /// A stable, negative row id for a contact that has no mailbox message:
    /// the first eight bytes of SHA-256 over the key, sign-forced negative.
    /// Negative because the mailbox assigns positive ids, so the two spaces
    /// never meet, and stable so the row keeps its identity across launches.
    nonisolated static func syntheticContactMessageId(signatureKey: Data) -> Int64 {
        let digest = SHA256.hash(data: signatureKey)
        var value: UInt64 = 0
        for byte in digest.prefix(8) { value = (value << 8) | UInt64(byte) }
        let magnitude = Int64(value & 0x7FFF_FFFF_FFFF_FFFF)
        return magnitude == 0 ? -1 : -magnitude
    }

    // MARK: - Accept / decline (local-state mutations)

    /// Mark an incoming ContactRequest as accepted locally.
    ///
    /// Moves the matching pending row to ``ContactsState/accepted``
    /// with an optional `groupIdHex` — set by the orchestration
    /// layer (1.14f's `ContactsViewModel.onAcceptClicked`) when it
    /// has just called `ChatRepository.startGroupWith` for this
    /// peer. The UI uses the id to offer an "Open chat" affordance
    /// on the accepted row.
    public func acceptContactRequest(
        messageId: Int64,
        groupIdHex: String? = nil
    ) {
        let pending = state.pending
        guard let idx = pending.firstIndex(where: { $0.messageId == messageId }) else {
            return
        }
        let accepted = pending[idx]
        var remaining = pending
        remaining.remove(at: idx)
        let newAccepted = state.accepted + [
            AcceptedContact(request: accepted, groupIdHex: groupIdHex)
        ]
        state = state.with(pending: remaining, accepted: newAccepted)
    }

    /// Set (or clear) the user-chosen display name for an accepted contact
    /// (ADR-0021 follow-up). A blank name clears it, reverting the UI + the
    /// group-call roster to the short pubkey fingerprint. The mutation flows
    /// through `state`'s `didSet`, which persists the accepted list.
    public func renameContact(messageId: Int64, to displayName: String) {
        let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        let accepted = state.accepted
        guard let idx = accepted.firstIndex(
            where: { $0.request.messageId == messageId }
        ) else {
            return
        }
        var updated = accepted
        updated[idx].displayName = trimmed.isEmpty ? nil : trimmed
        state = state.with(accepted: updated)
    }

    /// Remove an accepted contact from this device's contact list
    /// (`docs/ux/conversation-info.md` section 2.1, "Remove contact"). Local
    /// only, the mirror of ``declineContactRequest(messageId:)`` for the
    /// accepted list: the peer is not told and no message is sent.
    ///
    /// An existing 1x1 chat is left exactly as it is — leaving a conversation
    /// is the conversation's own action — which is what produces the "person
    /// who is not a contact" state the info layer already handles: the
    /// conversation stays, and what it can say about them shrinks to what it
    /// can still read from the roster.
    ///
    /// The mutation flows through `state`'s `didSet`, so the persisted
    /// accepted list and the name directory's renames follow.
    public func removeContact(messageId: Int64) {
        let removed = state.accepted.filter { $0.request.messageId == messageId }
        let remaining = state.accepted.filter { $0.request.messageId != messageId }
        guard remaining.count != state.accepted.count else { return }
        state = state.with(accepted: remaining)
        // ADR-0060 sections 1 and 4 — the removed person held this account's
        // `contact` and `fof` keys: rotate both (the old keys are unregistered
        // at once), and forget what they offered and relayed.
        ReachCenter.shared.rotate(.contact)
        ReachCenter.shared.rotate(.fof)
        for contact in removed {
            ReachCenter.shared.dropIntroductions(
                fromAccountKeyB64: base64Encode(contact.request.fromPubkey))
        }
    }

    // MARK: - ADR-0073 phase 2 — a contact reset their account keys (UX 16)

    /// The accepted contact whose account id is `accountIdB64`, or `nil`.
    /// The lookup every reset path uses: the record must be found by the
    /// stable account id across the key change, never by a key.
    public func contact(accountIdB64: String) -> AcceptedContact? {
        state.accepted.first { $0.accountIdB64 == accountIdB64 }
    }

    /// ADR-0073 R.2 "the re-pin" — the gate accepted a reset of this account:
    /// re-key the contact row to the new key, keeping the rename, the account
    /// id, the chat link and the row id, and appending the key it held to
    /// the display aliases. A second row already under the new key (a contact
    /// request from the new identity accepted before the reset was noticed)
    /// folds into this one: one person, one row. Nothing is shown.
    ///
    /// Does nothing for an account this device has no contact row for (a
    /// group member it never pinned): the pin store holds the superseded
    /// keys, which is what the chat layer reads.
    // MARK: - ADR-0073 R.3: a request from an account that is already a contact

    /// The contact an account id is ESTABLISHED as here, or `nil`. "Already
    /// a contact" is the pin (its current key or a proven predecessor) or a
    /// verified binding naming the row's own key: a row whose account id came
    /// only from the peer's own profile establishes nothing, so a member who
    /// claimed this account's id cannot make its request vanish.
    func establishedContact(ofAccountId accountId: Data) -> AcceptedContact? {
        let accountIdB64 = base64Encode(accountId)
        let pin = PeerPinStore().readPin(accountIdB64: accountIdB64)
        let pinKeys = KnownContactRequest.pinKeys(of: pin.map {
            KnownContactRequest.PinKeys(
                currentKeyB64: $0.pinnedSignaturePubkeyB64,
                provenPredecessorsB64: ProvenPredecessorStore().load()
                    .predecessors(of: $0.pinnedSignaturePubkeyB64))
        })
        return KnownContactRequest.establishedContactOfAccount(
            accepted: state.accepted,
            accountIdHex: groupIdHex(accountId),
            verified: KnownContactRequest.Verified(
                pinKeysB64: pinKeys,
                bindingKeyB64: VerifiedBindingStore().read(accountIdB64: accountIdB64)?.keyB64),
            row: { contact in
                KnownContactRequest.ContactRow(
                    signaturePubkeyB64: base64Encode(contact.request.fromPubkey),
                    accountIdHex: contact.request.fromAccountId.map { groupIdHex($0) })
            })
    }

    /// Returns `true` when `request` came from an account that is already an
    /// established contact and was dealt with here (answered, dropped or
    /// kept waiting): the caller then lists nothing. `false` for a
    /// stranger's request.
    func handleKnownContactRequest(_ request: IncomingContactRequest, client: ContactsClient) async -> Bool {
        guard let accountIdB64 = boundAccountIdB64, let fromAccountId = request.fromAccountId else {
            return false
        }
        // Read BEFORE the check below, which may write a first pin.
        guard establishedContact(ofAccountId: fromAccountId) != nil else { return false }
        let check: KnownContactRequest.Check
        do {
            check = try await client.checkContactIdentity(accountId: fromAccountId) != nil
                ? .current : .inconclusive
        } catch is SupersessionRefused {
            check = .refused
        } catch {
            check = .inconclusive
        }
        guard boundAccountIdB64 == accountIdB64 else { return true }
        let verdict = KnownContactRequest.verdict(
            senderIsContact: true,
            check: check,
            requestKeyB64: base64Encode(request.fromPubkey),
            trustedKeyB64: PeerPinStore().readPin(accountIdB64: base64Encode(fromAccountId))?
                .pinnedSignaturePubkeyB64)
        let waiting = KnownRequestWaitingStore()
        switch verdict {
        case .show:
            return false
        case .answer:
            // The ack is the whole of the answer on this path: the reset the
            // check just accepted (or accepted earlier) is what moves the
            // conversation.
            do {
                _ = try await sendContactAck(request: request)
            } catch {
                NSLog("contacts inbox: silent ack to an existing contact failed")
            }
            waiting.drop(messageId: request.messageId, accountIdB64: accountIdB64)
        case .wait:
            // Kept on disk: the cursor is about to move past this row.
            waiting.hold(request, accountIdB64: accountIdB64)
        case .drop:
            waiting.drop(messageId: request.messageId, accountIdB64: accountIdB64)
        }
        return true
    }

    /// Judge again the requests that were waiting for a verdict. Returns the
    /// ones whose sender is no longer an established contact: ordinary
    /// requests, for the caller to list.
    func retryWaitingKnownRequests(client: ContactsClient) async -> [IncomingContactRequest] {
        guard let accountIdB64 = boundAccountIdB64 else { return [] }
        let store = KnownRequestWaitingStore()
        var listed: [IncomingContactRequest] = []
        for request in store.load(accountIdB64: accountIdB64) {
            if await handleKnownContactRequest(request, client: client) { continue }
            guard boundAccountIdB64 == accountIdB64 else { return [] }
            store.drop(messageId: request.messageId, accountIdB64: accountIdB64)
            listed.append(request)
        }
        return listed
    }

    /// A reset was accepted: the waiting requests are judged again now, and
    /// one that turned out to be an ordinary request is listed.
    private func retryWaitingAfterReset() {
        Task { @MainActor [weak self] in
            guard let self, let accountIdB64 = self.boundAccountIdB64,
                  !KnownRequestWaitingStore().load(accountIdB64: accountIdB64).isEmpty else { return }
            let client = self.clientFactory()
            let listed = await self.retryWaitingKnownRequests(client: client)
            guard !listed.isEmpty, self.boundAccountIdB64 == accountIdB64 else { return }
            let merged = ContactsInboxMerge.merge(
                fetched: listed, pending: self.state.pending, accepted: self.state.accepted,
                currentKey: client.mailboxResolver().current(of:))
            self.state = self.state.with(pending: merged.applied(to: self.state.pending))
        }
    }

    public func applyAcceptedReset(_ event: ResetAccepted) {
        retryWaitingAfterReset()
        guard let newKey = base64Decode(event.newKeyB64), newKey.count == 32 else { return }
        let next = ContactRekey.afterReset(
            accepted: state.accepted,
            accountIdB64: event.accountIdB64,
            previousKeyB64: event.previousKeyB64,
            newKey: newKey)
        guard next != state.accepted else { return }
        state = state.with(accepted: next)
    }

    /// Send a `ContactAck` back to the original sender's stranger
    /// inbox so their UI flips from "pending" to "accepted"
    /// without waiting on the Welcome arrival (Phase 1.9b1).
    ///
    /// Best-effort: this is called from the view-model's
    /// `onAcceptClicked` after the local state mutation; a network
    /// failure here is logged at the orchestration layer and the
    /// local "accepted" state stands on its own (the user already
    /// trusted the peer).
    public func sendContactAck(
        request: IncomingContactRequest
    ) async throws -> PublishedAck {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let client = clientFactory()
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        // ADR-0074 section 3.5a rule 6 — a refused ack re-checks the
        // requester's address once (the client derives the mailbox from the
        // same proven-predecessor store the re-check writes).
        return try await RefusedMailboxWrite.write(
            resolve: {
                AckAttempt(plan: ReachCenter.shared.writePlan(toAccountKey: request.fromPubkey))
            },
            recheck: {
                await PersonAddressRecheck.recheck(
                    accountKey: request.fromPubkey, accountIdHint: request.fromAccountId)
            },
            publish: { attempt in
                try await client.sendContactAck(
                    accepterSigningSeed: identity.ed25519Seed(),
                    accepterAccountId: secrets.accountId,
                    accepterX25519Pubkey: identity.x25519PublicKey(),
                    originalRequest: request,
                    plan: attempt.plan)
            })
    }

    /// One attempt at an ack: who it is addressed to and how it is signed.
    private struct AckAttempt: Equatable {
        let plan: MailboxWritePlan?
    }

    /// Compute the Signal-style safety number (Phase 1.9b3 —
    /// ADR-0005 §13.5) between the local identity and the
    /// supplied contact pubkey. Returns the canonical 71-byte
    /// ASCII string the user reads OOB to verify their pinned
    /// keys agree.
    ///
    /// Throws ``ContactsError/wrapped`` when the session has no
    /// identity secrets (the caller should prompt the user to
    /// re-enter their password) or when the UniFFI surface
    /// rejects the pubkey shape.
    public func safetyNumber(against theirPubkey: Data) throws -> String {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        return try computeSafetyNumber(
            myPubkey: identity.signaturePublicKey(),
            theirPubkey: theirPubkey
        )
    }

    /// Drop an incoming request from the pending list without
    /// acknowledging it to the sender. Mirror of the web Decline
    /// semantics — server doesn't get told, the row just disappears
    /// locally.
    public func declineContactRequest(messageId: Int64) {
        state = state.with(
            pending: state.pending.filter { $0.messageId != messageId }
        )
    }

    /// Clear the in-memory state. Unbinds the persisted inbox first so the
    /// reset is not written back to disk as an empty inbox for the account
    /// that was live; the persisted row is left for the next bind.
    public func clear() {
        binding.advance()
        boundAccountIdB64 = nil
        state = ContactsState()
    }

    // MARK: - Invite link generate / revoke (1.14e7)

    /// Generate a fresh capability-based invite link.
    ///
    /// 1.14e7 ships with **fixed defaults** matching Android
    /// 1.15e10: 7-day expiry, single-use (`maxUses = 0`),
    /// non-bootstrap-referral. A custom-config sheet (user-typed
    /// label / expiry / max_uses) layers on later when UX pushes
    /// back.
    public func generateInviteLink(
        label: String = "",
        expiryMillisFromNow: Int64 = ContactsRepository.defaultInviteExpiryMs,
        maxUses: UInt32 = ContactsRepository.singleUse,
        bootstrapReferral: Bool = false
    ) async throws -> GeneratedInvite {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let expiryMs = nowMs + expiryMillisFromNow

        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        // ADR-0060 section 6 — the link carries a `link` grant scoped to this
        // link, registered BEFORE the link exists, so revoking it removes
        // exactly its key.
        let linkId = generateLinkId()
        let bound = binding.capture()
        let linkGrant: FfiReachGrant
        do {
            linkGrant = try await ReachCenter.shared.newLinkGrant(
                linkId: linkId, expiresAtMs: UInt64(expiryMs))
        } catch {
            throw ContactsError.wrap(error)
        }
        try bound.require()
        let mint: InviteLinkMint
        do {
            mint = try composeInviteCapability(
                ownerSigningSeed: identity.ed25519Seed(),
                ownerAccountId: secrets.accountId,
                ownerX25519Pubkey: identity.x25519PublicKey(),
                expiryMs: UInt64(expiryMs),
                maxUses: maxUses,
                bootstrapReferral: bootstrapReferral,
                label: label,
                linkId: linkId,
                linkGrant: linkGrant
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        // ADR-0028 amendment 2026-09-28 — the ledger gets the SEALED blob and
        // the clear fields; the link carries the secret in its fragment. A
        // contact invite has no inline form, so an upload failure is an error.
        let discovery = authenticatedDiscoveryClient()
        let blobId: Data
        do {
            blobId = try await discovery.uploadInvite(
                mint: mint,
                expiryMs: UInt64(expiryMs),
                maxUses: maxUses,
                label: label,
                bootstrapReferral: bootstrapReferral
            )
        } catch {
            throw ContactsError.wrap(error)
        }
        // The reach record and the invite list below are the bound
        // account's: a link made for an account that has since left is not
        // filed under the next one.
        try bound.require()

        // ADR-0060 section 6 — the link key is registered; tie it to the ledger id
        // so a revoke by blob id removes exactly its key.
        ReachCenter.shared.attachBlob(linkId: linkId, blobId: blobId)
        let blobIdSlug = base64UrlEncodeNoPad(blobId)
        let url = InviteLinkSecret.link(
            base: "\(Self.inviteUrlPrefix)\(blobIdSlug)", secret: mint.linkSecret)
        // The secret never leaves this device, so a link listed by the server
        // later can be copied again only from here.
        InviteLinkSecretStore.shared.put(
            accountIdB64: base64Encode(secrets.accountId), slug: blobIdSlug, secret: mint.linkSecret)
        let invite = GeneratedInvite(
            blobId: blobId,
            blobIdSlug: blobIdSlug,
            url: url,
            label: label,
            expiryMs: expiryMs,
            maxUses: Int64(maxUses),
            createdAtMs: nowMs
        )
        state = state.with(
            outstandingInvites: state.outstandingInvites + [invite]
        )
        return invite
    }

    /// Revoke an outstanding invite link via the discovery
    /// service's owner-signed `DELETE` endpoint + drop the row
    /// from ``ContactsState/outstandingInvites``.
    public func revokeInviteLink(blobId: Data) async throws -> RevokeInviteResult {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        let discovery = authenticatedDiscoveryClient()
        let bound = binding.capture()
        let result: RevokeInviteResult
        do {
            result = try await discovery.revokeInvite(
                blobId: blobId,
                signingSeed: identity.ed25519Seed()
            )
        } catch {
            throw ContactsError.wrap(error)
        }
        guard bound.isCurrent else {
            // Revoked on the server as the account that asked. This device's
            // secret for it is filed under that account and goes; the reach
            // record and the list are by now someone else's.
            InviteLinkSecretStore.shared.remove(
                accountIdB64: base64Encode(secrets.accountId), slug: base64UrlEncodeNoPad(blobId))
            return result
        }
        // ADR-0060 — the link's key goes with it (also on "already gone").
        ReachCenter.shared.removeLink(blobId: blobId)
        if result == .revoked {
            state = state.with(
                outstandingInvites: state.outstandingInvites
                    .filter { $0.blobId != blobId }
            )
        }
        // `docs/ux/client-redesign.md` section 5.3 — the link is gone (revoked
        // now, or already gone on the server), so is this device's secret.
        InviteLinkSecretStore.shared.remove(
            accountIdB64: base64Encode(secrets.accountId), slug: base64UrlEncodeNoPad(blobId))
        return result
    }

    // MARK: - Spent by a key rotation (ADR-0074 section 3.5a rule 5)

    /// The account's keys moved: every link this session generated names the
    /// old mailbox. They leave the list at once, so the UI never offers a
    /// dead link; the ledger rows are revoked by
    /// ``spendGrantsAfterKeyRotation(predecessorSeeds:)``.
    public func dropSpentLinksFromList() {
        guard !state.outstandingInvites.isEmpty else { return }
        state = state.with(outstandingInvites: [])
    }

    /// One spend round is enough at a time (the rotation's owed step and the
    /// session-start retry both come through here).
    private var spendingGrants = false

    /// ADR-0074 section 3.5a rule 5 — what this account granted before its
    /// keys were rotated is spent:
    ///
    ///   * its own `/i/` and `/c/` links are revoked at their ledger, signed
    ///     by the key that issued them (a predecessor), and removed from this
    ///     device's lists and stores;
    ///   * each unanswered contact request is sent again from the new key
    ///     (the recipient's bundle through the fail-closed gate, a fresh
    ///     reply grant for the new mailbox) and the stored entry replaced. A
    ///     request that can no longer be sent is dropped rather than shown as
    ///     pending. The re-sent request carries the intro stored with the
    ///     original (``SentRequestsStoring``); a request sent before that
    ///     store existed carries none.
    ///
    /// Throws while any revoke or re-send got no verdict, so the rotation
    /// keeps the step owed; what did land is not repeated by the retry.
    public func spendGrantsAfterKeyRotation(predecessorSeeds: [Data]) async throws {
        guard let secrets = authRepository.identitySecrets else { throw SessionNotReady() }
        guard !spendingGrants else { throw SessionNotReady() }
        spendingGrants = true
        defer { spendingGrants = false }
        let accountIdB64 = base64Encode(secrets.accountId)
        let reach = ReachCenter.shared
        var owed = 0

        // The keys that may have issued a link: this account's old ones,
        // newest first.
        let issuers = predecessorSeeds.reversed().compactMap {
            try? Identity.fromSeed(seed: $0).ed25519Seed()
        }
        // Links.
        var listed: [Data] = []
        if !issuers.isEmpty {
            do {
                // Signed by the CURRENT key: the directory verifies the list
                // query against the published identity.
                if case .list(let rows) = try await listActiveInvites() {
                    listed = rows.compactMap { base64UrlDecodeNoPad($0.blobIdSlug) }
                }
            } catch {
                owed += 1
            }
        }
        let discovery = authenticatedDiscoveryClient()
        for link in SpentGrants.linksToRevoke(spent: reach.spentLinks, listedInviteBlobIds: listed) {
            var answers: [SpentGrants.RevokeAnswer] = []
            for issuer in issuers {
                do {
                    let result = link.call
                        ? try await discovery.revokeCallInvite(blobId: link.blobId, signingSeed: issuer)
                        : try await discovery.revokeInvite(blobId: link.blobId, signingSeed: issuer)
                    answers.append(result == .revoked ? .revoked : .notFoundOrNotOwner)
                    if result == .revoked { break }
                } catch {
                    answers.append(.failed)
                }
            }
            guard boundAccountIdB64 == accountIdB64 else { throw SessionNotReady() }
            let verdict = SpentGrants.linkVerdict(answers)
            if verdict == .owed { owed += 1 }
            guard SpentGrants.dropsLocally(verdict, mintedHere: link.linkIdB64 != nil) else { continue }
            if let linkIdB64 = link.linkIdB64 { reach.forgetSpentLink(linkIdB64: linkIdB64) }
            if !link.call {
                InviteLinkSecretStore.shared.remove(
                    accountIdB64: accountIdB64, slug: base64UrlEncodeNoPad(link.blobId))
            }
            if state.outstandingInvites.contains(where: { $0.blobId == link.blobId }) {
                state = state.with(
                    outstandingInvites: state.outstandingInvites.filter { $0.blobId != link.blobId })
            }
        }
        // A scope with no ledger row (an inline link) has nothing to revoke.
        for link in reach.spentLinks where link.blobIdB64 == nil {
            reach.forgetSpentLink(linkIdB64: link.linkIdB64)
        }

        // Unanswered contact requests.
        let recipients = SpentGrants.requestsToResend(
            spentReplies: reach.spentReplies,
            acceptedAccountIds: state.accepted.compactMap(\.request.fromAccountId))
        let client = clientFactory()
        for recipient in recipients {
            var failure: Error?
            var replacement: SentContactRequest?
            let intro = SentRequests.intro(for: base64Encode(recipient), in: state.sent)
            do {
                let bundle = try await client.resolveVerifiedBundle(accountId: recipient)
                let result = try await sendRequestToBundle(
                    client: client, secrets: secrets,
                    recipientAccountId: recipient,
                    recipientSignaturePubkey: bundle.signaturePubkey,
                    recipientX25519Pubkey: bundle.x25519Pubkey,
                    introMessage: Data(intro.utf8), link: nil)
                replacement = SentContactRequest(
                    recipientUsername: "", recipientAccountIdB64: base64Encode(recipient),
                    messageId: result.messageId, createdAt: result.createdAt, intro: intro)
            } catch {
                failure = error
            }
            guard boundAccountIdB64 == accountIdB64 else { throw SessionNotReady() }
            let verdict = SpentGrants.resendVerdict(failure)
            if verdict == .owed {
                owed += 1
                continue
            }
            reach.forgetSpentReplies(purpose: ReplyPurpose.contact(recipient))
            let next = SpentGrants.sentList(
                state.sent, recipientAccountIdB64: base64Encode(recipient),
                verdict: verdict, replacement: replacement)
            if next != state.sent { state = state.with(sent: next) }
        }
        // Every other spent reply scope (a request already answered, a `/g/`
        // join) has nothing to re-send here: it is dropped.
        for reply in reach.spentReplies
        where !recipients.contains(where: { ReplyPurpose.contact($0) == reply.purpose }) {
            reach.forgetSpentReplies(purpose: reply.purpose)
        }
        guard owed == 0 else {
            throw ContactsError.wrap(ContactsError.simple(
                "\(owed) link(s) or request(s) granted before the key rotation are still owed"))
        }
    }

    /// Rule 5 on a device that did not run the rotation (a kept device, or
    /// any device after a restart): the same round over what THIS device
    /// granted, best effort. What does not land stays in the reach record
    /// and is retried at the next session start.
    public func spendGrantsAfterAdoptingKeyRotation() async {
        guard let secrets = authRepository.identitySecrets else { return }
        let reach = ReachCenter.shared
        guard !reach.spentLinks.isEmpty || !reach.spentReplies.isEmpty else { return }
        do {
            try await spendGrantsAfterKeyRotation(
                predecessorSeeds: authRepository.ownPredecessorSeeds(
                    accountIdB64: base64Encode(secrets.accountId)))
        } catch {
            NSLog("ADR-0074 3.5a: links or requests granted before the key rotation are still owed")
        }
    }

    /// The signed-in account's id (base64), the key the per-account invite
    /// link secrets are stored under; `nil` when signed out.
    public var inviteAccountIdB64: String? {
        authRepository.identitySecrets.map { base64Encode($0.accountId) }
    }

    /// Owner-signed list of every live invite this account owns
    /// across all devices (Phase 1.18d2b-list). Distinct from
    /// ``ContactsState/outstandingInvites`` which only tracks
    /// invites generated in the current session — the server-side
    /// list is authoritative across the user's full device fleet.
    ///
    /// Throws ``ContactsError/wrapped`` when the session has no
    /// identity secrets (caller should prompt the user to
    /// re-enter their password) or when the discovery client
    /// surfaces a transport failure.
    public func listActiveInvites() async throws -> ListInvitesResult {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        let discovery = authenticatedDiscoveryClient()
        do {
            return try await discovery.listInvitesByOwner(
                accountId: secrets.accountId,
                signingSeed: identity.ed25519Seed()
            )
        } catch {
            throw ContactsError.wrap(error)
        }
    }

    /// Default invite expiry — 7 days. Mirrors the Android +
    /// Web sides.
    public static let defaultInviteExpiryMs: Int64 = 7 * 24 * 60 * 60 * 1000

    /// Server-side sentinel for `max_uses = single-use`.
    public static let singleUse: UInt32 = 0

    /// URL prefix Lakko shares its invite links under: the web origin
    /// of the stack this build talks to plus the web
    /// `composePointerInviteUrl` path, so a link generated on either
    /// side opens on the other.
    public nonisolated static var inviteUrlPrefix: String { ApiEndpoints.webOrigin + "/i/" }

    /// The 6-byte ledger id an `/i/<slug>` names, or `nil` for any other slug:
    /// exactly 8 base64url characters decoding to ``DiscoveryClient`` 's id
    /// width. A contact invite has no inline form, so the retired 43-character
    /// id and everything else are refused before any request.
    public nonisolated static func parseInviteSlug(_ slug: String) -> Data? {
        guard slug.count == Int(inviteBlobIdUrlLen()),
              isBase64Url(slug),
              let bytes = base64UrlDecodeNoPad(slug),
              bytes.count == Int(inviteBlobIdLen()) else { return nil }
        return bytes
    }

    // MARK: - Joiner-side invite consume (1.14e8)

    /// Open + verify an invite-link slug, fetch the capability
    /// bytes, consume (atomically bumps the server's used-counter
    /// + records this joiner in the dedup side table), and drive
    /// the ContactRequest send. Surfaces every distinct step as
    /// an ``AcceptInviteResult`` variant so the
    /// ``InviteLandingScreen`` (1.14e9) can render the right
    /// outcome copy.
    ///
    /// Steps:
    ///   1. base64url-decode the slug to recover the 6-byte
    ///      `blob_id` (exactly 8 characters; the retired
    ///      43-character id is refused as malformed).
    ///   2. `DiscoveryClient.fetchInvite(blobId)` → capability wire
    ///      bytes (404 → `notFound`).
    ///   3. `InviteCapability.decodeAndVerify(wire)` — local
    ///      Ed25519 signature check (a malicious server cannot
    ///      serve substituted bytes).
    ///   4. `isValidNow(now)` — expiry check.
    ///   5. `DiscoveryClient.consumeInvite(blobId, capabilityBytes,
    ///      joinerAccountId)` — atomic counter bump + dedup tag.
    ///   6. `ContactsClient.sendContactRequestToBundle(...)` —
    ///      ship a ContactRequest to the inviter's stranger inbox.
    ///   7. `ChatRepository.startGroupWith(...)` (1.14f) — drives
    ///      a live 1×1 group against the inviter when the chat
    ///      bridge is wired, returning `groupIdHex`; falls back to
    ///      `nil` (best-effort) if the bridge is absent or the
    ///      call fails.
    public func acceptInviteLink(slug token: String) async -> AcceptInviteResult {
        guard let secrets = authRepository.identitySecrets else {
            return .requiresReauth
        }
        // ADR-0028 amendment 2026-09-28 — `token` is `slug#fragment`; a
        // missing or malformed fragment is a damaged link, refused before any
        // request.
        let parts = InviteLinkSecret.split(token)
        guard let blobId = Self.parseInviteSlug(parts.base) else {
            return .malformedSlug
        }
        guard let linkSecret = InviteLinkSecret.read(parts.fragment) else {
            return .damaged
        }

        // This accept is shielded from the screen's cancellation, so it runs
        // to its end even after a sign-out. Every step below reads something
        // of the BOUND account (the session token, the own profile, a reply
        // key, the chat repository, the accepted list), so it stops as soon
        // as the account that tapped the link is no longer the one bound.
        let bound = binding.capture()
        let discovery = authenticatedDiscoveryClient()
        let sealed: Data?
        do {
            sealed = try await discovery.fetchInvite(blobId: blobId)
        } catch {
            if DiscoveryClientError.isRateLimited(error) { return .rateLimited }
            return .failed(message: (error as? LocalizedError)?.errorDescription
                ?? "fetch failed")
        }
        guard bound.isCurrent else { return .requiresReauth }
        guard let sealed = sealed else {
            return .notFound
        }

        let capability: InviteCapability
        do {
            capability = try InviteCapability.openLink(sealed: sealed, linkSecret: linkSecret)
        } catch {
            // Wrong secret, another kind, tampered, substituted, or a
            // capability that does not verify: the one damaged refusal.
            return .damaged
        }
        // The joiner tag is computed over the PLAINTEXT capability.
        let capabilityBytes = capability.wire()

        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        if !capability.isValidNow(nowMs: nowMs) {
            return .expired
        }

        // Snapshot bundle fields BEFORE the consume call.
        let inviterAccountId = capability.ownerAccountId()
        let inviterSigPubkey = capability.ownerSignaturePubkey()
        let inviterX25519Pubkey = capability.ownerX25519Pubkey()
        // ADR-0078 — the link grant carries the owner's mailbox binding (the
        // core verified it against the owner's key when the capability
        // opened); verified again here against the key the request is sealed
        // to, so the request and the Welcome go to the mailbox the owner reads.
        let linkGrant = capability.linkGrant()
        let inviteLink = MailboxTarget.verifiedOrNil(grant: linkGrant, accountKey: inviterSigPubkey)
            .map { RequestLink(seed: linkGrant.seed, target: $0) }

        // The link carries its owner's keys and this flow never fetches the
        // bundle, so it never passes the pin gate. Ask the same question of
        // the link's keys here, before the link is consumed and before
        // anything is sealed to them: for a pinned account whose link names
        // another key, the published identity goes through the fail-closed
        // gate (an attested reset is accepted there, silently), and a key
        // that is still not the account's trusted key is refused (old keys
        // do not come back). A stranger (no pin) proceeds.
        do {
            try await clientFactory().refuseContactRequestToUnverifiedKeys(
                accountId: inviterAccountId, recipientSignaturePubkey: inviterSigPubkey)
        } catch {
            return .failed(message: "the link's keys are not the ones trusted for this account")
        }

        // ADR-0073 R.7 case 1 / UX 16.6 — a link from a person who is already
        // a contact (a reset loses their contacts; they may hand out a new
        // link) OPENS that contact: no request is sent, no use of the link
        // is spent and no result screen is shown in between. The
        // check above ran R.2 first, so an attested reset was accepted there
        // and an unproven change was refused. "Already a contact" is the pin
        // or a verified binding naming the row's key: a row whose account id
        // came only from a peer's own profile does not swallow this person's
        // link.
        if let existing = establishedContact(ofAccountId: inviterAccountId) {
            // The link opens that contact: no result screen, no use spent.
            return .knownContact(contactMessageId: existing.request.messageId)
        }

        // Consume — atomic counter bump + per-joiner dedup.
        let consumeResult: ConsumeInviteResult
        do {
            guard bound.isCurrent else { return .requiresReauth }
            let discoveryConsume = authenticatedDiscoveryClient()
            consumeResult = try await discoveryConsume.consumeInvite(
                blobId: blobId,
                capabilityBytes: capabilityBytes,
                joinerAccountId: secrets.accountId
            )
        } catch {
            if DiscoveryClientError.isRateLimited(error) { return .rateLimited }
            return .failed(message: (error as? LocalizedError)?.errorDescription
                ?? "consume failed")
        }
        guard bound.isCurrent else { return .requiresReauth }
        switch consumeResult {
        case .exhausted:
            return .exhausted
        case .notFound:
            return .notFound
        case .alreadyConsumed:
            return .alreadyAccepted
        case .consumed:
            break // proceed to ContactRequest
        }

        // Ship a ContactRequest to the inviter's stranger inbox.
        // The bundle is inline in the capability — no discovery
        // round-trip needed.
        let sendResult: SendResult
        do {
            // ADR-0060 section 6 — written with the link's grant, used for
            // this flow only and never kept.
            sendResult = try await sendRequestToBundle(
                client: clientFactory(), secrets: secrets,
                recipientAccountId: inviterAccountId,
                recipientSignaturePubkey: inviterSigPubkey,
                recipientX25519Pubkey: inviterX25519Pubkey,
                introMessage: Data(),
                link: inviteLink)
        } catch {
            return .failed(
                message: (error as? LocalizedError)?.errorDescription
                    ?? "ContactRequest send failed"
            )
        }
        guard bound.isCurrent else { return .requiresReauth }

        // 1.14f4 lights up the chat bridge — when wired, drive
        // `ChatRepository.startGroupWith` against the invite
        // owner so the joiner has a live 1×1 group on accept.
        // Best-effort: a failure here drops back to the
        // pre-1.14f4 "purely local marker" semantics with
        // `groupIdHex = nil`; the UI surfaces a "chat creation
        // failed, retry" hint.
        let groupIdHex: String?
        if let chatRepository = chatRepository {
            do {
                groupIdHex = try await chatRepository.startGroupWith(
                    recipientAccountId: inviterAccountId,
                    recipientSignaturePubkey: inviterSigPubkey,
                    kind: .oneToOne,
                    recipientPlan: inviteLink.map {
                        MailboxWritePlan(
                            target: $0.target,
                            candidates: $0.seed.map { [DsMailboxWriteAuth(reachSeed: $0)] } ?? [])
                    }
                )
            } catch {
                groupIdHex = nil
            }
        } else {
            groupIdHex = nil
        }
        guard bound.isCurrent else { return .requiresReauth }

        // Append the new contact to the accepted list so the user
        // sees the result. Construct a synthetic
        // IncomingContactRequest shell — the invite flow bypassed
        // the stranger inbox, but the AcceptedContact UI shape
        // expects one.
        let syntheticRequest = IncomingContactRequest(
            messageId: sendResult.messageId,
            fromPubkey: inviterSigPubkey,
            fromAccountId: inviterAccountId,
            fromPseudonym: Data(repeating: 0, count: 32),
            validAfterMs: nowMs,
            intro: nil,
            createdAt: sendResult.createdAt
        )
        state = state.with(
            accepted: state.accepted + [
                AcceptedContact(request: syntheticRequest, groupIdHex: groupIdHex)
            ]
        )
        return .accepted(
            recipientAccountId: inviterAccountId,
            inviterFingerprint: capability.ownerFingerprint(),
            groupIdHex: groupIdHex
        )
    }
}

/// Discriminated outcome of
/// ``ContactsRepository/acceptInviteLink(slug:)``.
///
/// Each variant maps to a distinct user-visible state on the
/// ``InviteLandingScreen`` (1.14e9):
///
///   * ``accepted(recipientAccountId:groupIdHex:)`` — every step
///     succeeded.
///   * ``alreadyAccepted`` — the server's per-joiner dedup tag
///     already had a row.
///   * ``expired`` — the capability's `expiry_ms` is in the past.
///   * ``exhausted`` — server returned 410 (max_uses ceiling hit).
///   * ``notFound`` — server returned 404 (revoked / expired /
///     never-issued).
///   * ``malformedSlug`` — the URL slug is not valid base64url or
///     doesn't decode to a 32-byte blob_id.
///   * ``requiresReauth`` — ``AuthRepository/identitySecrets`` is
///     `nil`.
///   * ``failed(message:)`` — any other underlying failure.
public enum AcceptInviteResult: Equatable {
    /// Accept succeeded. `inviterFingerprint` is the 71-char
    /// Signal-style safety number derived from the inviter's
    /// Ed25519 pubkey — UI surfaces it after Accept so the
    /// user can verify out-of-band whom they just connected
    /// to (defence-in-depth against link-substitution
    /// attacks; ROADMAP 3c3 / calls.md C-28).
    case accepted(
        recipientAccountId: Data,
        inviterFingerprint: String,
        groupIdHex: String?
    )
    case alreadyAccepted
    /// ADR-0073 R.7 case 1 / UX 16.6 — the link is from a person who is
    /// already an established contact: nothing was sent and no use of the
    /// link was spent. The landing OPENS that contact (the route's key is the
    /// contact's request id) and shows no result of its own.
    case knownContact(contactMessageId: Int64)
    case expired
    case exhausted
    case notFound
    case malformedSlug
    /// ADR-0028 amendment 2026-09-28 — the link's fragment secret is missing
    /// or malformed, or the ledger's blob does not open under it.
    case damaged
    /// `429` from the discovery ledger on the fetch or the consume: the link
    /// may be fine, the server asked this network address to slow down. The
    /// screen says "try again later", never "invalid".
    case rateLimited
    case requiresReauth
    case failed(message: String)
}

// MARK: - State shape

/// Snapshot of every field the Contacts surface renders.
///
/// Swift `struct` with explicit `with(...)` mutators rather than
/// Kotlin's `data class .copy(named = ...)` — Swift doesn't ship a
/// `.copy(namedFieldOverride:)` shorthand, so a small fan-out of
/// `with` helpers keeps the call sites readable without exposing
/// every field as `var`.
public struct ContactsState: Equatable {
    /// Verified incoming requests waiting for Accept / Decline.
    public let pending: [IncomingContactRequest]
    /// Locally-accepted requests — in 1.14e3 these are inert (no
    /// MLS group has been created). 1.14f turns them into real
    /// chat sessions.
    public let accepted: [AcceptedContact]
    /// Outgoing requests we've sent in this session. Survives
    /// process death — there's no on-server "sent" mirror to
    /// rehydrate from, so a fresh process starts with an empty
    /// list.
    public let sent: [SentContactRequest]
    /// Owner-side invite links generated this session. Empty in
    /// 1.14e3; populated by 1.14e7's generate-invite UI.
    public let outstandingInvites: [GeneratedInvite]
    /// Cursor for the stranger-inbox poll.
    public let cursor: Int64
    public let refreshing: Bool
    public let requiresReauth: Bool
    /// The catalog key (ADR-0061) of the last receive failure, rendered by
    /// the screen in the current interface language; never transport prose.
    public let lastError: String?

    public init(
        pending: [IncomingContactRequest] = [],
        accepted: [AcceptedContact] = [],
        sent: [SentContactRequest] = [],
        outstandingInvites: [GeneratedInvite] = [],
        cursor: Int64 = 0,
        refreshing: Bool = false,
        requiresReauth: Bool = false,
        lastError: String? = nil
    ) {
        self.pending = pending
        self.accepted = accepted
        self.sent = sent
        self.outstandingInvites = outstandingInvites
        self.cursor = cursor
        self.refreshing = refreshing
        self.requiresReauth = requiresReauth
        self.lastError = lastError
    }

    /// Field-wise copy with overrides. Optional parameters mirror
    /// the Kotlin `.copy(...)` shape. `Optional<T?>` would let us
    /// distinguish "skip" vs "set to nil" but for the current
    /// fields the explicit nil-clear semantics are not needed —
    /// `lastError = nil` means "clear", which is exactly what
    /// callers want.
    public func with(
        pending: [IncomingContactRequest]? = nil,
        accepted: [AcceptedContact]? = nil,
        sent: [SentContactRequest]? = nil,
        outstandingInvites: [GeneratedInvite]? = nil,
        cursor: Int64? = nil,
        refreshing: Bool? = nil,
        requiresReauth: Bool? = nil,
        lastError: String? = nil
    ) -> ContactsState {
        ContactsState(
            pending: pending ?? self.pending,
            accepted: accepted ?? self.accepted,
            sent: sent ?? self.sent,
            outstandingInvites: outstandingInvites ?? self.outstandingInvites,
            cursor: cursor ?? self.cursor,
            refreshing: refreshing ?? self.refreshing,
            requiresReauth: requiresReauth ?? self.requiresReauth,
            lastError: lastError
        )
    }
}

/// A request the user has accepted locally.
public struct AcceptedContact: Equatable, Codable {
    public let request: IncomingContactRequest
    /// Populated when the Accept flow drove
    /// `ChatRepository.startGroupWith` for this peer (1.14f,
    /// landed). `nil` only when the chat bridge was absent or
    /// `startGroupWith` failed — the contacts UI then offers a
    /// retry instead of the "Open chat" affordance.
    public let groupIdHex: String?
    /// User-chosen display name for this contact (ADR-0021 follow-up).
    /// `nil` until the user renames the contact; the UI + the group-call
    /// roster then fall back to `shortFingerprintLabel`. A `Codable`
    /// optional — a contact persisted before this field existed decodes
    /// with `displayName == nil`, preserving the old fingerprint display.
    public var displayName: String?
    /// ADR-0073 R.2 "the re-pin" — the account keys this contact held before
    /// each reset this device accepted (base64, oldest first). DISPLAY
    /// aliases for stored history only: a previous key is never accepted for
    /// new traffic, a new pin, a role or a send. `nil` for a contact that
    /// never reset.
    public var previousKeysB64: [String]?

    public init(
        request: IncomingContactRequest,
        groupIdHex: String? = nil,
        displayName: String? = nil,
        previousKeysB64: [String]? = nil
    ) {
        self.request = request
        self.groupIdHex = groupIdHex
        self.displayName = displayName
        self.previousKeysB64 = previousKeysB64
    }

    /// The contact's stable account id (base64), when the request carried
    /// one. What the reset record is found by across the key change.
    public var accountIdB64: String? {
        request.fromAccountId.map { base64Encode($0) }
    }

    /// This contact re-keyed to `key` after an accepted reset: the rename,
    /// the account id, the chat link and the row id stay with the person, and
    /// the key it held is appended to the display aliases.
    func withSignatureKey(_ key: Data) -> AcceptedContact {
        var previous = previousKeysB64 ?? []
        let held = base64Encode(request.fromPubkey)
        if request.fromPubkey != key, !previous.contains(held) { previous.append(held) }
        return AcceptedContact(
            request: IncomingContactRequest(
                messageId: request.messageId,
                fromPubkey: key,
                fromAccountId: request.fromAccountId,
                fromPseudonym: request.fromPseudonym,
                validAfterMs: request.validAfterMs,
                intro: request.intro,
                profile: request.profile,
                createdAt: request.createdAt),
            groupIdHex: groupIdHex,
            displayName: displayName,
            previousKeysB64: previous.isEmpty ? nil : previous)
    }

    /// Short pubkey-fingerprint label this peer is shown by elsewhere in
    /// the app (first 8 bytes of the Ed25519 pubkey, colon-grouped hex) —
    /// the fallback the ADR-0021 group-call roster resolver surfaces for a
    /// recognised contact that has no user-chosen `displayName` yet.
    public var shortFingerprintLabel: String {
        let hex = request.fromPubkey.prefix(8)
            .map { String(format: "%02x", $0) }
            .joined()
        var out = ""
        for (i, ch) in hex.enumerated() {
            if i > 0 && i % 4 == 0 { out.append(":") }
            out.append(ch)
        }
        return out
    }

    /// The label to show for this contact: the user-chosen `displayName`
    /// when set (and non-blank), else the short pubkey fingerprint. This is
    /// the single source the contacts UI + the group-call roster resolver
    /// both read, so a rename surfaces everywhere consistently.
    public var displayLabel: String {
        if let name = displayName, !name.isEmpty { return name }
        return shortFingerprintLabel
    }
}

/// Outgoing contact request, surfaced on the UI as an "I asked
/// @<name> to connect" row.
public struct SentContactRequest: Equatable, Codable {
    public let recipientUsername: String
    public let recipientAccountIdB64: String
    public let messageId: Int64
    public let createdAt: String
    /// The intro the person wrote, kept so a re-send after a key rotation
    /// carries it (ADR-0074 section 3.5a rule 5). Empty when none.
    public let intro: String

    public init(
        recipientUsername: String,
        recipientAccountIdB64: String,
        messageId: Int64,
        createdAt: String,
        intro: String = ""
    ) {
        self.recipientUsername = recipientUsername
        self.recipientAccountIdB64 = recipientAccountIdB64
        self.messageId = messageId
        self.createdAt = createdAt
        self.intro = intro
    }
}

/// An invite link the owner has just generated this session.
///
/// Produced by ``ContactsRepository/generateInviteLink(...)``
/// and cleared by ``ContactsRepository/revokeInviteLink(blobId:)``
/// (1.14e7, landed). Surfaced via
/// ``ContactsState/outstandingInvites``.
public struct GeneratedInvite: Equatable {
    /// Raw 32-byte content-addressed id.
    public let blobId: Data
    /// base64url-no-pad of `blobId` — the slug portion of `url`.
    public let blobIdSlug: String
    /// `https://lakko.me/i/<slug>` — the URL the owner shares.
    public let url: String
    /// Human-visible label (empty when the user didn't set one).
    public let label: String
    /// UNIX millis when this invite stops being honoured.
    public let expiryMs: Int64
    /// Server-enforced consume ceiling.
    public let maxUses: Int64
    /// UNIX millis when the row was generated.
    public let createdAtMs: Int64

    public init(
        blobId: Data,
        blobIdSlug: String,
        url: String,
        label: String,
        expiryMs: Int64,
        maxUses: Int64,
        createdAtMs: Int64
    ) {
        self.blobId = blobId
        self.blobIdSlug = blobIdSlug
        self.url = url
        self.label = label
        self.expiryMs = expiryMs
        self.maxUses = maxUses
        self.createdAtMs = createdAtMs
    }
}

/// ADR-0074 Stage A — the mailbox of an account's own predecessor key: how
/// often it is read, and the cursor it is read from.
enum PredecessorMailbox {

    /// At most one read per minute (chosen, not measured): it carries
    /// Welcomes and first contact requests, never chat.
    static let pollIntervalMs: Int64 = 60_000

    static func due(lastPollMs: Int64, nowMs: Int64) -> Bool {
        lastPollMs == 0 || nowMs - lastPollMs >= pollIntervalMs
    }

    /// The per-(account, predecessor key) mailbox cursors, in `UserDefaults`
    /// (a public key and a row id). Cleared by the account wipe.
    struct CursorStore {
        static let prefix = "lakko.contacts.predecessor-inbox-cursor.v1."
        private let defaults: UserDefaults

        init(defaults: UserDefaults = .standard) {
            self.defaults = defaults
        }

        private static func key(accountIdB64: String) -> String { prefix + accountIdB64 }

        func cursor(accountIdB64: String, keyB64: String) -> Int64 {
            ((defaults.dictionary(forKey: Self.key(accountIdB64: accountIdB64)) as? [String: NSNumber])?[keyB64])?
                .int64Value ?? 0
        }

        /// Move the cursor forward; it never goes back.
        func advance(accountIdB64: String, keyB64: String, to polled: Int64) {
            var map = (defaults.dictionary(forKey: Self.key(accountIdB64: accountIdB64)) as? [String: NSNumber]) ?? [:]
            let current = map[keyB64]?.int64Value ?? 0
            guard polled > current else { return }
            map[keyB64] = NSNumber(value: polled)
            defaults.set(map, forKey: Self.key(accountIdB64: accountIdB64))
        }

        func clear(accountIdB64: String) {
            defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
        }
    }
}

// MARK: - ADR-0073 R.2 — the contact row after an accepted reset

/// The pure half of ``ContactsRepository/applyAcceptedReset(_:)``.
public enum ContactRekey {

    /// The accepted list after the account `accountIdB64` reset from
    /// `previousKeyB64` to `newKey`.
    ///
    /// | a row for the account (by id, else by the previous key) | a row under the new key | result                                   |
    /// |----------------------------------------------------------|-------------------------|------------------------------------------|
    /// | none                                                     | any                     | unchanged                                |
    /// | yes, already under the new key                           | -                       | unchanged                                |
    /// | yes                                                      | none                    | that row re-keyed, the old key an alias  |
    /// | yes                                                      | another row             | re-keyed, and the other row is dropped   |
    public static func afterReset(
        accepted: [AcceptedContact],
        accountIdB64: String,
        previousKeyB64: String,
        newKey: Data
    ) -> [AcceptedContact] {
        guard let idx = accepted.firstIndex(where: {
            $0.accountIdB64 == accountIdB64 || base64Encode($0.request.fromPubkey) == previousKeyB64
        }) else { return accepted }
        if accepted[idx].request.fromPubkey == newKey { return accepted }
        let rekeyed = accepted[idx].withSignatureKey(newKey)
        var out: [AcceptedContact] = []
        for (i, contact) in accepted.enumerated() {
            if i == idx {
                out.append(rekeyed)
            } else if contact.request.fromPubkey != newKey {
                out.append(contact)
            }
        }
        return out
    }
}

/// ADR-0078 — a mailbox a contact request found its recipient through (a link
/// grant, a search result's record), VERIFIED, with the reach seed that opens
/// it (`nil` for an Everyone mailbox, written with a `mailbox-write` token).
public struct RequestLink: Equatable {
    public let seed: Data?
    public let target: MailboxTarget

    public init(seed: Data?, target: MailboxTarget) {
        self.seed = seed
        self.target = target
    }
}
