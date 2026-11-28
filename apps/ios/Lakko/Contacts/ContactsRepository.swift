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
                    // A removed person who is a contact again was added on
                    // purpose (the adoption rule never adds a removed key).
                    let readded = removedKeys.intersection(state.accepted.map(\.request.fromPubkey))
                    if !readded.isEmpty {
                        removedKeys.subtract(readded)
                        AcceptedContactsStore.saveRemovedKeys(removedKeys, accountIdB64: accountIdB64)
                    }
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
    /// Account keys the person removed from their contacts; the one-to-one
    /// adoption rule skips them (``AcceptedContactsStore/loadRemovedKeys(accountIdB64:)``).
    private var removedKeys: Set<Data> = []
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

    /// ADR-0087 section 15, ADR-0045 decision 2 — lands one mailbox Welcome
    /// against this account's pending publisher requests
    /// (``CoPublisherAcceptance/landPendingWelcome(_:accountIdB64:)``):
    /// `true` when it landed one, `false` when the Welcome is no request's; a
    /// throw (the landed join could not be made durable) holds the cursor.
    /// Wired by the signed-in shell; `nil` lands nothing.
    public var publisherWelcomeLander: (@MainActor (_ welcome: Data, _ accountIdB64: String) throws -> Bool)?

    /// The landing could not run (no lander wired yet, or the shell's
    /// channel state is not up) while publisher requests are pending: the
    /// poll holds its cursor so the Welcome is read again.
    struct PublisherWelcomeNotReady: Error {}

    /// Try one mailbox Welcome against this account's pending publisher
    /// requests. With nothing pending, `false` at once. A landing that could
    /// not run throws when `holdWhenNotReady` (a rendezvous row, which is a
    /// publisher Welcome by kind), else reads as `false` (a plain Welcome
    /// row goes on to the chat join).
    func landPublisherWelcome(_ welcome: Data, accountIdB64: String, holdWhenNotReady: Bool) throws -> Bool {
        // ADR-0090 — a device of an account that owns or co-publishes a
        // channel takes a PublisherGroup Welcome here too, whether it waits
        // for its first admission or holds an old group the Welcome replaces.
        guard !PendingPublisherRequests.load(accountIdB64: accountIdB64).isEmpty
                || ChannelDeviceAdmission.hasWelcomeTargets else { return false }
        do {
            guard let lander = publisherWelcomeLander else { throw PublisherWelcomeNotReady() }
            return try lander(welcome, accountIdB64)
        } catch is PublisherWelcomeNotReady {
            if holdWhenNotReady { throw PublisherWelcomeNotReady() }
            return false
        }
    }

    /// Optional bridge to the chat surface. Wired in 1.14f4. When
    /// non-nil, ``refreshInbox(waitSecs:)`` forwards Welcome
    /// envelopes to ``ChatRepository/joinGroupFromWelcome(welcomeBytes:)``
    /// so a peer's `startGroupWith` lands a live 1×1 chat on this
    /// device. The device's own accept of a request through one of the
    /// account's invite links (ADR-0079) drives `startGroupWith` against the
    /// requester when wired.
    ///
    /// Marked `weak` to avoid a retain cycle — both repositories
    /// live in `@StateObject` slots at the `LakkoApp` scope and
    /// outlive any single view tree, so a `weak` reference is
    /// safe.
    /// ADR-0101 section 7 — told when the continuity mailbox kept a channel
    /// return, so the channels runtime acts on it now. Set by the app.
    public var onChannelReturnHeld: (() -> Void)?

    /// ADR-0101 section 5 — when the continuity mailbox is read.
    let continuityWindow = ContinuityMailboxWindow()

    public weak var chatRepository: ChatRepository? {
        didSet {
            chatRepository?.owedAnswerAck = { [weak self] messageId, groupIdHex in
                await self?.ackOwedAnswer(requestMessageId: messageId, groupIdHex: groupIdHex)
            }
            // ADR-0087 section 9 — a held Welcome that landed is a Welcome.
            chatRepository?.onHeldWelcomeJoined = { [weak self] groupIdHex in
                self?.noteAnsweredByWelcome(groupIdHex: groupIdHex)
                self?.handOverJoinedPendingContacts()
            }
            // `docs/ux/who-can-find-me.md` 6.4 — a contact request one member
            // sent inside a group: opened here or dropped.
            chatRepository?.onGroupContactRequest = { [weak self] groupIdHex, envelope, senderKeyB64 in
                self?.receiveGroupContactRequest(
                    groupIdHex: groupIdHex, envelope: envelope, senderKeyB64: senderKeyB64)
            }
        }
    }

    /// ADR-0079 section 4 — the synced link-record register: the verdict on
    /// a request's link answer, the use count, the revocations, and (reworked
    /// 2026-10-17) the claims of link requests. Wired once at app start; the
    /// undecided link requests are decided when its state loads, when a
    /// claim from another device arrives, and when the register moves.
    public weak var deviceGroupSync: DeviceGroupSyncRepository? {
        didSet { observeLinkDecisionTriggers() }
    }
    private var linkDecisionTriggers: Set<AnyCancellable> = []

    private func observeLinkDecisionTriggers() {
        linkDecisionTriggers.removeAll()
        guard let sync = deviceGroupSync else { return }
        sync.$ready.removeDuplicates().sink { [weak self] _ in
            Task { @MainActor [weak self] in await self?.decideLinkRequests() }
        }.store(in: &linkDecisionTriggers)
        sync.$linkClaimsVersion.removeDuplicates().sink { [weak self] _ in
            Task { @MainActor [weak self] in await self?.decideLinkRequests() }
        }.store(in: &linkDecisionTriggers)
    }

    /// ADR-0079 section 5 — the inviters added through their link whose app
    /// has not answered yet, for the bound account.
    @Published public private(set) var invited: [PendingContact] = [] {
        didSet {
            // ADR-0079 — a pending contact is named by its link's statement
            // on every surface (a group roster included), so the resolver
            // must see the list as it changes, like the renames above.
            if invited != oldValue {
                ProfileDirectory.shared.setPendingContacts(invited)
            }
        }
    }

    /// The one-to-one each pending contact moved into this session (account
    /// id hex to group id hex): an open pending chat follows it there.
    @Published public private(set) var handedOver: [String: String] = [:]

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
            guard let accountIdB64 = note.userInfo?[StateBundle.appliedAccountKey] as? String else { return }
            let contacts = note.object as? [AcceptedContact] ?? []
            // Bundle v3 — pending contacts the bundle added to the store.
            let pendingAdded = note.userInfo?[StateBundle.pendingAddedKey] as? Bool == true
            Task { @MainActor in
                if !contacts.isEmpty { self?.mergeBundledContacts(contacts, accountIdB64: accountIdB64) }
                if pendingAdded { self?.reloadBundledPendingContacts(accountIdB64: accountIdB64) }
            }
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
        // ADR-0098 — a person learned deleted: their unanswered request goes
        // silently, and what was held for a pending contact is dropped.
        DeletedAccounts.shared.$revision
            .dropFirst()
            .sink { [weak self] _ in
                MainActor.assumeIsolated { self?.applyDeletedAccounts() }
            }
            .store(in: &cancellables)
    }

    /// ADR-0098 — drop the incoming requests of deleted accounts, and the
    /// messages held for a pending contact whose account is gone (they would
    /// never arrive). The pending contact itself stays, drawn as `Deleted
    /// account`, until the person deletes the chat. Only on a verified
    /// statement AND a directory `404` after it (`deletionEffects`
    /// `dropHeldSends`): either alone changes the name shown, never what is
    /// held.
    func applyDeletedAccounts(isDeleted given: ((Data) -> Bool)? = nil) {
        let isDeleted = given ?? { ProfileDirectory.shared.deletionDropsHeldSends(signatureKey: $0) }
        let kept = state.pending.filter { !isDeleted($0.fromPubkey) }
        if kept.count != state.pending.count { state = state.with(pending: kept) }
        for contact in invited where isDeleted(contact.signaturePubkey) {
            chatRepository?.clearPendingInvitedMessages(queueKey: contact.queueKey)
        }
    }

    /// ADR-0098 — ask the directory about the account behind a request about
    /// to be accepted. `true` only on a `404`, which is recorded; anything
    /// else (an answer, a network failure) is `false`.
    static func accountAnswersAbsent(
        accountId: Data, personKey: Data, fetch: (Data) async throws -> IdentityFetchResponse?
    ) async -> Bool {
        let answer: IdentityFetchResponse?
        do {
            answer = try await fetch(accountId)
        } catch {
            return false
        }
        let classified = DeletedAccountRules.classify(bundle: answer, heldKey: personKey)
        DeletedAccounts.shared.noteAnswer(
            personKeyB64: base64Encode(personKey), accountIdB64: base64Encode(accountId), classified)
        return classified == .absent
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
            invited = []
            removedKeys = []
            return
        }
        let persisted = inboxStore.load(accountIdB64: accountIdB64)
        let accepted = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        removedKeys = AcceptedContactsStore.loadRemovedKeys(accountIdB64: accountIdB64)
        let sent = sentStore.load(accountIdB64: accountIdB64)
        boundAccountIdB64 = accountIdB64
        loadingBoundRows = true
        // One person is one row, also for rows stored before that was so.
        let rows = Self.reconciled(
            accepted: accepted, pending: persisted.pending, samePerson: provenSamePerson)
        state = state.with(
            pending: rows.pending, accepted: rows.accepted, sent: sent, outstandingInvites: [],
            cursor: persisted.cursor)
        loadingBoundRows = false
        invited = PendingContactStore.load(accountIdB64: accountIdB64)
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
            intro: SentRequests.storedIntro(introMessage),
            startedOver: result.startedOver ? true : nil
        )
        state = state.with(sent: SentRequests.adding(sent, to: state.sent))
        return result
    }

    /// ADR-0060 — send a contact request to a person a search found: their
    /// identity bundle through the pin gate (it must name the key the search
    /// found), then the request, written with the key the search found them
    /// under (`nil` for a person in Everyone, whose mailbox is open).
    ///
    /// `docs/ux/who-can-find-me.md` section 6.1 — sent at once from the
    /// search row, with no introduction text; the request still carries the
    /// own profile and a reply key.
    public func sendContactRequest(to person: FoundPerson) async throws -> SendResult {
        let introMessage = ""
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple("no identity secrets — please re-enter your password"))
        }
        guard let accountId = person.accountId else {
            throw ContactsError.wrap(ContactsError.simple("this person cannot be reached"))
        }
        let client = clientFactory()
        let bound = binding.capture()
        let bundle = try await client.resolveVerifiedBundleForAdd(accountId: accountId)
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
        // Section 6.4 — the order Message on a card follows: the inbox when a
        // grant reaches it; when every grant is refused or no inbox address
        // is known, inside a group both are in (it then stands under Sent as
        // Waiting, recorded there); else the refusal stands.
        let sent = try await fallingBackToSharedGroup(
            accountKey: person.accountKey, accountId: accountId,
            name: person.displayName, username: person.username
        ) {
            try await self.sendRequestToBundle(
                client: client, secrets: secrets,
                recipientAccountId: accountId,
                recipientSignaturePubkey: bundle.signaturePubkey,
                recipientX25519Pubkey: bundle.x25519Pubkey,
                introMessage: Data(introMessage.utf8),
                link: person.mailbox.map { RequestLink(seed: person.writeSeed, target: $0) })
        }
        guard let result = sent else {
            return SendResult(
                recipientUsername: person.username ?? "", recipientAccountId: accountId,
                messageId: 0, createdAt: ISO8601DateFormatter().string(from: Date()))
        }
        guard bound.isCurrent else { return result }
        state = state.with(sent: SentRequests.adding(
            SentContactRequest(
                recipientUsername: person.username ?? "",
                recipientAccountIdB64: base64Encode(accountId),
                messageId: result.messageId,
                createdAt: result.createdAt,
                intro: SentRequests.storedIntro(introMessage),
                recipientName: person.displayName,
                recipientKeyB64: base64Encode(person.accountKey),
                startedOver: bundle.startedOver ? true : nil),
            to: state.sent))
        var answered = result
        answered.startedOver = bundle.startedOver
        return answered
    }

    /// Stop waiting for a request that was not answered: the row under Sent
    /// goes, and search can offer the person again. Local; the person is told
    /// nothing, and a request they already hold stays theirs to answer.
    public func dismissSentRequest(_ request: SentContactRequest) {
        state = state.with(sent: SentRequests.answered(
            recipientAccountIdB64: request.recipientAccountIdB64, in: state.sent))
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
        link: RequestLink?,
        linkAnswer: FfiLinkAnswer? = nil
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
                reach: reach,
                linkAnswer: linkAnswer)
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

    /// Kept across searches, and across accounts: a lookup key names a
    /// username, which is the same for everyone, and opens nothing.
    private let usernameLookupKeys = KeptUsernameLookupKeys()

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
            .with(kept: usernameLookupKeys)
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

    /// Why an account-mailbox Welcome did not open as a chat.
    enum MailboxWelcomeRefusal: Equatable {
        /// The core refused it as not a conversation (`NoRoleTable`, C-86): a
        /// PublisherGroup or DeviceGroup Welcome, which the channel and
        /// device paths take; never a failed chat join.
        case notAConversation
        /// A Welcome for a group this device still holds as active
        /// (ADR-0087 section 9): kept and joined once the end lands.
        case groupStillActive(String)
        /// Anything else: most are Welcomes sealed to another of this
        /// account's devices (ADR-0060 section 6), which this one cannot open.
        case notOpenedHere

        /// Whether the poll raises ``welcomeJoinFailureMessage`` for it. None
        /// does: every Welcome of the account lands in one mailbox, so a
        /// refusal here is the ordinary case, not a lost invitation (walk of
        /// 2026-11-14: a PublisherGroup Welcome raised the banner on web).
        var raisesJoinFailureBanner: Bool { false }
    }

    /// The typed reading of a failed `joinGroupFromWelcome`.
    nonisolated static func mailboxWelcomeRefusal(_ error: Error) -> MailboxWelcomeRefusal {
        if case ChatError.wrapped(_, let underlying) = error {
            return mailboxWelcomeRefusal(underlying)
        }
        if case FfiError.MembershipNoRoleTable = error { return .notAConversation }
        if let held = MembershipEndRules.stillActiveGroupIdHex(error) { return .groupStillActive(held) }
        return .notOpenedHere
    }

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
        await refreshInbox(waitSecs: waitSecs, source: .account)
    }

    /// Which mailbox one pass of ``refreshInbox(waitSecs:source:)`` reads.
    enum InboxSource: Equatable {
        /// The account mailbox (the long-polled one; ``ContactsState/cursor``).
        case account
        /// ADR-0101 section 5 — the account's CONTINUITY mailbox, for the
        /// window after a key reset, with its own cursor
        /// (``ContinuityMailboxWindow``); a short read, never a spinner.
        case continuity
    }

    func refreshInbox(waitSecs: Int, source: InboxSource) async {
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
        // ADR-0101 section 5 — the continuity mailbox is read once per tick,
        // BEFORE the account mailbox's long poll, while its window is open.
        if source == .account {
            await pollContinuityMailboxIfOpen(accountIdB64: pollAccountIdB64, secrets: secrets)
        }
        let continuityCursors = ContinuityMailboxWindow.CursorStore()
        let isManualRefresh = waitSecs == 0 && source == .account
        if isManualRefresh {
            state = state.with(refreshing: true, lastError: nil)
        }
        // A Welcome that fails to join is otherwise invisible: the cursor has
        // already moved past it, so it is never retried and the chat simply
        // never appears. Remember the failure so this poll's final state can
        // say so.
        var welcomeJoinFailure: String?
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
                after: source == .account
                    ? state.cursor : continuityCursors.cursor(accountIdB64: pollAccountIdB64),
                waitSecs: waitSecs,
                continuity: source == .continuity
            )
            // ADR-0073 R.3 — the requests that were waiting, unshown, for an
            // identity verdict are judged again first; one whose sender is no
            // longer an established contact is an ordinary request.
            var newPending: [IncomingContactRequest] = await retryWaitingKnownRequests(client: client)
            var undecidedArrived = false
            // ADR-0060 section 1 (2026-11-06) — the contacts who wrote with a
            // retired `contact` key and get the current grant, invisibly.
            var regrant: [Data] = []
            // ADR-0099 step 4 / UX `reporting-and-moderation.md` section 3 —
            // the senders of this batch's requests whose accounts the
            // moderation log shows suspended, asked about all at once under
            // one bound of a few seconds. Only a verified `suspended` counts;
            // a late or failed answer shows the request as before.
            let suspendedSenders = await suspendedRequestSenders(Self.requestSenders(pollResult.events))
            // A Welcome can only be joined once the chat layer is bootstrapped
            // for this account. A background refresh runs with no session
            // root (nothing bootstrapped it), and at launch this pass can run
            // first: it waits for the bootstrap within its long-poll budget,
            // and the Welcomes the chat layer still cannot try stay ahead of
            // the cursor (``ContactsInboxMerge/advancedCursor(current:polled:heldWelcomeIds:)``)
            // instead of being consumed. The wait is what keeps a held
            // Welcome from being re-read in a tight loop.
            let hasWelcomes = pollResult.events.contains {
                if case .welcome = $0 { return true } else { return false }
            }
            var chatReady = true
            if hasWelcomes {
                chatReady = await ChatLayerReadiness.awaitReady(
                    timeoutMs: Int64(max(waitSecs, 0)) * 1000
                ) { [weak self] in
                    self?.chatRepository?.isReady(forAccountIdB64: pollAccountIdB64) ?? false
                }
            }
            var heldWelcomeIds: [Int64] = []
            /// Base scenario 7 — silent answers from people this device holds
            /// no one-to-one with; founded for after the batch.
            var needChat: [IncomingContactAck] = []
            if !chatReady {
                NSLog("contacts inbox: Welcome row(s) kept until the chat layer is ready")
            }
            for event in pollResult.events {
                // The session ended (or changed account) while this batch
                // was in flight, or while a Welcome of it was being joined:
                // the rest belongs to the old account. Its grants, profiles
                // and Welcomes are not filed under whoever is bound now.
                guard boundAccountIdB64 == pollAccountIdB64 else { break }
                // ADR-0099 step 4 — dropped unseen before anything else
                // decides about it: no grant, profile, regrant, link-answer
                // acceptance or card.
                if Self.droppedAsSuspended(event, suspendedSenders: suspendedSenders) { continue }
                // Before anything reads it (the founder peek below needs the
                // chat layer too): a Welcome this device cannot try yet. A
                // publisher request's Welcome sent as a plain row is landed
                // as before.
                if !chatReady, case .welcome(let messageId, let welcomeBytes, _) = event {
                    if try landPublisherWelcome(welcomeBytes, accountIdB64: pollAccountIdB64, holdWhenNotReady: false) {
                        continue
                    }
                    heldWelcomeIds.append(messageId)
                    continue
                }
                let decision = Self.rowContactDecision(
                    event, reachKeys: pollResult.reachKeyByMessageId,
                    isContact: { [weak self] in self?.isCurrentContact($0) ?? false },
                    welcomeFounder: { [weak self] in self?.chatRepository?.peekWelcomeFounder(welcomeBytes: $0) },
                    verdict: { ReachCenter.shared.mailboxRowVerdict(provingKey: $0, senderIsContact: $1) })
                // A retired key from someone who is not a contact: nothing is
                // shown and nothing names them in the log.
                if decision.verdict == .drop { continue }
                if decision.verdict == .processAndRegrant, let sender = decision.sender {
                    regrant.append(sender)
                }
                switch event {
                case .contactRequest(let req, let replyGrant, let linkAnswer):
                    // ADR-0099 section 6 — a request from a blocked account is
                    // dropped unseen, whichever route it took (the inbox or an
                    // invite link): nothing of it is kept, not its grant, not
                    // its profile.
                    if Self.requestBlocked(req.fromPubkey) { continue }
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
                    // ADR-0079 section 3 (reworked 2026-10-17) — a request
                    // sent through one of this account's invite links is kept
                    // undecided (sealed on the device, not shown) and decided
                    // after this batch by ``decideLinkRequests()``: accepted
                    // with no card by the one device whose claim wins,
                    // answered, or a card with the dead-link note.
                    if let linkAnswer {
                        LinkRequestInbox(accountIdB64: pollAccountIdB64).add(LinkRequestInbox.Row(
                            request: req, linkId: linkAnswer.linkId, linkSecret: linkAnswer.linkSecret,
                            replyGrantWire: replyGrant.flatMap { try? encodeReachGrant(grant: $0) }))
                        undecidedArrived = true
                        continue
                    }
                    newPending.append(markingAsksForChat(req))
                case .publisherWelcome(_, let welcomeBytes):
                    // ADR-0087 section 15, ADR-0045 decision 2 — an owner's
                    // device approved a publisher request of this account; it
                    // lands whenever it arrives. Not a request's: nothing. A
                    // landed join that cannot be made durable throws, which
                    // holds the cursor.
                    _ = try landPublisherWelcome(welcomeBytes, accountIdB64: pollAccountIdB64, holdWhenNotReady: true)
                    continue
                case .welcome(let messageId, let welcomeBytes, _):
                    // A pending publisher request's Welcome sent as a plain
                    // Welcome row is the request's, never a chat to join.
                    if try landPublisherWelcome(welcomeBytes, accountIdB64: pollAccountIdB64, holdWhenNotReady: false) {
                        continue
                    }
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
                            let joined = try await chatRepository
                                .joinGroupFromWelcome(welcomeBytes: welcomeBytes)
                            noteAnsweredByWelcome(groupIdHex: joined)
                            // ADR-0101 section 9 — inside this account's own
                            // continuity window every conversation it joins
                            // is a returned one: ask there for its history.
                            noteReturnedConversationIfInsideWindow(
                                groupIdHex: joined, accountIdB64: pollAccountIdB64)
                            // ADR-0079 section 5 — a one-to-one with a person
                            // added through their link: what was written to
                            // them goes out now, in order.
                            handOverJoinedPendingContacts()
                        } catch {
                            // The chat layer stopped being ready between the
                            // wait above and the join (a session switch):
                            // the Welcome was never tried, so it stays.
                            if !chatRepository.isReady(forAccountIdB64: pollAccountIdB64) {
                                heldWelcomeIds.append(messageId)
                                continue
                            }
                            let refusal = Self.mailboxWelcomeRefusal(error)
                            if refusal.raisesJoinFailureBanner {
                                welcomeJoinFailure = Self.welcomeJoinFailureMessage
                            }
                            switch refusal {
                            case .groupStillActive(let held):
                                // ADR-0087 section 9 — a Welcome for a group this
                                // device still holds as active: the removal it
                                // answers may still be in that group's journal.
                                // The cursor moves past this row, so it is kept.
                                chatRepository.holdWelcomeForActiveGroup(
                                    groupIdHex: held, welcome: welcomeBytes)
                                Task { @MainActor [weak chatRepository] in
                                    await chatRepository?.resolveHeldWelcomes()
                                }
                            case .notAConversation:
                                NSLog("contacts inbox: an account-mailbox Welcome is not a conversation (a publisher or device group); not a chat")
                            case .notOpenedHere:
                                NSLog("contacts inbox: an account-mailbox Welcome did not open as a chat (expected for another device's Welcome): \(error)")
                            }
                        }
                    }
                    continue
                case .linkRefusal(let refusal):
                    guard boundAccountIdB64 == pollAccountIdB64 else { continue }
                    applyLinkRefusal(refusal)
                    continue
                case .conversationReentry(_, let envelope):
                    // ADR-0091 phase 2 route 2 — judged by the repository
                    // (the core checks the KeyPackage, the admission rule
                    // the rest); a row that fails any check is dropped.
                    guard boundAccountIdB64 == pollAccountIdB64 else { continue }
                    if let chatRepository {
                        Task { @MainActor [weak chatRepository] in
                            await chatRepository?.receiveConversationReentry(envelope: envelope, route: .mailbox)
                        }
                    }
                    continue
                case .contactAck(let ack):
                    // The session ended (or changed account) while this
                    // batch was in flight: the ack belongs to the old
                    // account, never to whoever is bound now.
                    guard boundAccountIdB64 == pollAccountIdB64 else { continue }
                    if applyContactAck(ack) { needChat.append(ack) }
                    continue
                case .channelReturn(_, let envelope):
                    // ADR-0101 section 7 — kept for the channels runtime,
                    // which verifies it against this account's previous key
                    // before acting. A sealed store throws, which holds the
                    // cursor. Only the continuity mailbox surfaces one.
                    guard source == .continuity, boundAccountIdB64 == pollAccountIdB64 else { continue }
                    try HeldChannelReturnStore().hold(base64Encode(envelope), accountIdB64: pollAccountIdB64)
                    onChannelReturnHeld?()
                    continue
                }
            }
            if !regrant.isEmpty, boundAccountIdB64 == pollAccountIdB64 {
                await chatRepository?.regrantContacts(regrant)
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
                currentKey: resolver.current(of:), isBlocked: Self.requestBlocked)
            #if DEBUG
            for (request, reason) in merged.dropped {
                NSLog(
                    "contacts inbox: dropped fetched request messageId=\(request.messageId) reason=\(reason)"
                )
            }
            #endif
            if source == .continuity {
                // Its own cursor; the account mailbox's state is untouched
                // but for the requests this batch brought.
                continuityCursors.advance(
                    accountIdB64: pollAccountIdB64,
                    to: ContactsInboxMerge.advancedCursor(
                        current: continuityCursors.cursor(accountIdB64: pollAccountIdB64),
                        polled: pollResult.cursor, heldWelcomeIds: heldWelcomeIds))
                state = state.with(pending: merged.applied(to: state.pending), lastError: state.lastError)
                return
            }
            state = state.with(
                pending: merged.applied(to: state.pending),
                cursor: ContactsInboxMerge.advancedCursor(
                    current: state.cursor, polled: pollResult.cursor, heldWelcomeIds: heldWelcomeIds),
                refreshing: false,
                lastError: welcomeJoinFailure ?? carriedJoinFailure
            )
            // Base scenario 7 — founded now, unless a Welcome of this pass
            // was kept for later (it may be that very chat).
            if !needChat.isEmpty, heldWelcomeIds.isEmpty, boundAccountIdB64 == pollAccountIdB64 {
                await foundForSilentAnswers(needChat)
            }
            // ADR-0074 section 3.5a rule 4 — after a rotation the account
            // moved to the new mailbox at once; a sender that has not seen
            // the proven change yet still writes to the old one, which is
            // READ (never written to, nothing registered there) for 30 days
            // after this device adopted, then left.
            await pollPredecessorMailboxes(accountIdB64: pollAccountIdB64)
            // ADR-0079 — a pending contact whose one-to-one exists already,
            // a link revoked on another device of the account, and the
            // undecided link requests.
            handOverJoinedPendingContacts()
            await revokeLinksRevokedElsewhere()
            // An accept whose Welcome is owed: at launch (the first poll) and
            // on every poll.
            await chatRepository?.resolveAllOwedAnswers()
            if undecidedArrived || !LinkRequestInbox(accountIdB64: pollAccountIdB64).rows().isEmpty {
                await decideLinkRequests()
            }
        } catch {
            // A cancelled poll (the screen was left, the session ended, the
            // long-poll was superseded) is not a failure: keep whatever the
            // slot held and let the next iteration run.
            if Self.isCancellation(error) {
                state = state.with(refreshing: false, lastError: state.lastError)
                return
            }
            // ADR-0101 — the continuity read never costs the account mailbox
            // its state; the next tick reads it again.
            if source == .continuity {
                NSLog("contacts inbox: the continuity mailbox could not be read: \(type(of: error))")
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
        // Not bootstrapped for this account: the previous mailboxes are left
        // unread (their cursors stay), so a Welcome there is not consumed by
        // a join that cannot run.
        guard chatRepository?.isReady(forAccountIdB64: accountIdB64) == true else { return }
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
                var held: [Int64] = []
                for event in result.events {
                    switch event {
                    case .welcome(let messageId, let welcomeBytes, _):
                        do {
                            _ = try await chatRepository?.joinGroupFromWelcome(welcomeBytes: welcomeBytes)
                        } catch {
                            // Never tried (the session ended or switched
                            // meanwhile): kept behind the cursor.
                            if chatRepository?.isReady(forAccountIdB64: accountIdB64) != true {
                                held.append(messageId)
                                continue
                            }
                            NSLog("contacts inbox: a predecessor-mailbox Welcome did not open as a chat: \(error)")
                        }
                    case .contactRequest, .contactAck, .linkRefusal, .publisherWelcome, .conversationReentry,
                         .channelReturn:
                        // Rule 5 — a predecessor mailbox carries Welcomes
                        // only. A request sent here was sealed to the old
                        // key and its sender still addresses the old
                        // mailbox; an ack answers a request whose reply
                        // grant is spent (the rotating device re-sent it
                        // from the new key); a link refusal names the old
                        // key as the requester, which no pending contact
                        // of this account matches. None is acted on.
                        continue
                    }
                }
                guard boundAccountIdB64 == accountIdB64 else { return }
                cursors.advance(
                    accountIdB64: accountIdB64, keyB64: keyB64,
                    to: ContactsInboxMerge.advancedCursor(
                        current: cursors.cursor(accountIdB64: accountIdB64, keyB64: keyB64),
                        polled: result.cursor, heldWelcomeIds: held))
            } catch {
                NSLog("contacts inbox: a predecessor mailbox could not be read: \(type(of: error))")
            }
        }
    }

    /// ADR-0060 section 1 (2026-11-06) — a current contact of this account
    /// (by proof across key rotations).
    func isCurrentContact(_ key: Data) -> Bool {
        let samePerson = provenSamePerson
        return state.accepted.contains { samePerson($0.request.fromPubkey, key) }
    }

    /// ADR-0060 section 1 (amended 2026-11-06) — the verdict on one mailbox
    /// row from the reach key that admitted it (`reachKeys`, by message id)
    /// and its sender as the row's verified content names it: a Welcome's
    /// founder is peeked before it is joined (`welcomeFounder`). A link
    /// refusal is always processed.
    nonisolated static func rowContactDecision(
        _ event: IncomingMailboxEvent, reachKeys: [Int64: Data],
        isContact: (Data) -> Bool,
        welcomeFounder: (Data) -> Data? = { _ in nil },
        verdict: (Data?, Bool) -> FfiContactKeyRowVerdict
    ) -> (verdict: FfiContactKeyRowVerdict, sender: Data?) {
        switch event {
        case .contactRequest(let req, _, _):
            return (verdict(reachKeys[req.messageId], isContact(req.fromPubkey)), req.fromPubkey)
        case .contactAck(let ack):
            return (verdict(reachKeys[ack.messageId], isContact(ack.accepterPubkey)), ack.accepterPubkey)
        case .welcome(let messageId, let welcomeBytes, _):
            // The founder, peeked before joining. A row admitted by no key is
            // processed without a peek; one whose founder cannot be read is
            // judged as a stranger's (a retired key then drops it unjoined).
            guard let proving = reachKeys[messageId] else { return (.process, nil) }
            let founder = welcomeFounder(welcomeBytes)
            return (verdict(proving, founder.map(isContact) ?? false), founder)
        case .linkRefusal, .publisherWelcome, .conversationReentry, .channelReturn:
            return (.process, nil)
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
        // A person removed from the list stays removed although the
        // conversation remains (seen on web 2026-10-24: a removed contact
        // came back on the next load).
        let samePerson = provenSamePerson
        guard !removedKeys.contains(where: { samePerson($0, signatureKey) }) else { return false }
        let next = Self.adopting(
            state.accepted, signatureKey: signatureKey, accountId: accountId,
            groupIdHex: groupIdHex, nowMs: nowMs, createdAt: createdAt,
            samePerson: samePerson,
            keepsLink: { held in
                linkIsOneToOne(held, with: signatureKey, samePerson: samePerson)
            }) ?? state.accepted
        // A request from them is gone with it (a process that died inside an
        // accept left both), carried on the row so its ack can still go out.
        let rows = Self.reconciled(
            accepted: next, pending: state.pending, samePerson: samePerson, answered: [signatureKey])
        guard rows.accepted != state.accepted || rows.pending != state.pending else { return false }
        state = state.with(pending: rows.pending, accepted: rows.accepted)
        return true
    }

    // MARK: - A request inside a shared group (`docs/ux/who-can-find-me.md` 6.4)

    /// Which way a request to the person holding `accountKey` goes: their
    /// inbox when a grant this device holds still reaches it, else inside a
    /// group both are current members of, else nowhere
    /// (``ContactRequestRoute``).
    public func requestRoute(toAccountKey accountKey: Data) -> ContactRequestRoute {
        ContactRequestRoute.decide(
            reachesInbox: ReachCenter.shared.writePlan(toAccountKey: accountKey) != nil,
            sharedGroups: sharedGroups(with: accountKey))
    }

    /// The groups (not one-to-ones) this device is an active member of and
    /// the person holding `accountKey` is a current member of, each with the
    /// time of its newest message.
    func sharedGroups(with accountKey: Data) -> [SharedGroup] {
        guard let chatRepository else { return [] }
        let samePerson = provenSamePerson
        let threads = Dictionary(
            chatRepository.state.threads.map { ($0.groupIdHex, $0) }, uniquingKeysWith: { first, _ in first })
        return chatRepository.conversationHeadIdHexes.compactMap { groupIdHex in
            guard chatRepository.groupKind(groupIdHex: groupIdHex) == .group,
                  chatRepository.isActive(groupIdHex: groupIdHex) == true,
                  let members = chatRepository.members(groupIdHex: groupIdHex),
                  members.contains(where: { samePerson($0.signaturePublicKey, accountKey) })
            else { return nil }
            let newest = threads[groupIdHex]?.messages.last
                .flatMap { ChatRepository.parseDsTimestampMs($0.createdAt) } ?? 0
            return SharedGroup(groupIdHex: groupIdHex, newestMessageMs: newest)
        }
    }

    /// Whether a request to this person stands unanswered under Sent: Message
    /// sends nothing new then (one request per person).
    public func hasWaitingRequest(toAccountKey accountKey: Data, accountId: Data?) -> Bool {
        SentRequests.holds(state.sent, accountKey: accountKey, accountId: accountId)
    }

    /// Send a contact request to a known person inside `groupIdHex`, a group
    /// both are in: the ordinary request (own profile, a fresh reply grant so
    /// the accept goes through the inbox, not the group), sealed whole to
    /// their key so the other members cannot tell whom it is for. It then
    /// stands under Sent as Waiting; no chat is created.
    public func sendContactRequestInGroup(
        groupIdHex: String,
        recipientAccountId: Data,
        recipientAccountKey: Data,
        recipientName: String?,
        recipientUsername: String? = nil
    ) async throws {
        guard let chatRepository else { throw ContactsError.requestRefused }
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple("no identity secrets — please re-enter your password"))
        }
        let bound = binding.capture()
        let client = clientFactory()
        // The key the request is sealed to comes from the verified directory
        // bundle, which must name the person this device knows.
        let bundle = try await client.resolveVerifiedBundle(accountId: recipientAccountId)
        ReachCenter.shared.reloadKeyResolver()
        let resolver = ReachCenter.shared
        guard bundle.signaturePubkey == recipientAccountKey
                || resolver.currentAccountKey(of: recipientAccountKey)
                    == resolver.currentAccountKey(of: bundle.signaturePubkey)
        else { throw ContactsError.requestRefused }
        try bound.require()
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        let replyGrant = try await ReachCenter.shared.newReplyGrant(
            purpose: ReplyPurpose.contact(recipientAccountId))
        try bound.require()
        let wire = try ContactsClient.composeRequestWire(
            senderSigningSeed: identity.ed25519Seed(),
            senderAccountId: secrets.accountId,
            recipientX25519Pubkey: bundle.x25519Pubkey,
            profile: ProfileDirectory.shared.ownProfileForRequest(),
            introMessage: Data(),
            replyGrant: replyGrant)
        let envelope = try composeGroupContactRequestEnvelope(
            recipientX25519Pub: bundle.x25519Pubkey, request: wire)
        do {
            try await chatRepository.publishApplicationMessage(
                groupIdHex: groupIdHex, plaintextBytes: envelope, personAction: true)
        } catch {
            // Never delivered: its reply grant goes with it.
            ReachCenter.shared.retireReplies(purpose: ReplyPurpose.contact(recipientAccountId))
            throw error
        }
        guard bound.isCurrent else { return }
        recordGroupRequestSent(
            recipientAccountId: recipientAccountId, recipientAccountKey: recipientAccountKey,
            recipientName: recipientName, recipientUsername: recipientUsername)
    }

    /// A request that went inside a group stands under Sent as Waiting, as
    /// any request: one per person until answered or removed. It has no
    /// mailbox row, so its id is 0.
    func recordGroupRequestSent(
        recipientAccountId: Data, recipientAccountKey: Data, recipientName: String?,
        recipientUsername: String? = nil
    ) {
        state = state.with(sent: SentRequests.adding(
            SentContactRequest(
                recipientUsername: recipientUsername ?? "",
                recipientAccountIdB64: base64Encode(recipientAccountId),
                messageId: 0,
                createdAt: ISO8601DateFormatter().string(from: Date()),
                recipientName: recipientName,
                recipientKeyB64: base64Encode(recipientAccountKey)),
            to: state.sent))
    }

    /// The groups shared with a person (``sharedGroups(with:)``). A seam for
    /// tests.
    lazy var sharedGroupsOf: (Data) -> [SharedGroup] = { [weak self] key in
        self?.sharedGroups(with: key) ?? []
    }

    /// Sends a request inside a shared group
    /// (``sendContactRequestInGroup(groupIdHex:recipientAccountId:recipientAccountKey:recipientName:recipientUsername:)``).
    /// A seam for tests.
    lazy var groupRequestSender: (
        _ groupIdHex: String, _ accountId: Data, _ accountKey: Data, _ name: String?, _ username: String?
    ) async throws -> Void = { [weak self] groupIdHex, accountId, accountKey, name, username in
        guard let self else { throw ContactsError.requestRefused }
        try await self.sendContactRequestInGroup(
            groupIdHex: groupIdHex, recipientAccountId: accountId, recipientAccountKey: accountKey,
            recipientName: name, recipientUsername: username)
    }

    /// Section 6.4 for a request to a person found by search: `send` (the
    /// inbox) first; when it is refused (every grant refused, or no inbox
    /// address known) and a group is shared, the request goes inside the
    /// group with the newest message and `nil` is returned (the Sent row is
    /// recorded by the group send); with no shared group the refusal stands.
    /// Any other failure is not a refusal and stands as it is.
    func fallingBackToSharedGroup(
        accountKey: Data, accountId: Data, name: String?, username: String?,
        send: () async throws -> SendResult
    ) async throws -> SendResult? {
        do {
            return try await send()
        } catch ContactsError.requestRefused {
            guard case .group(let groupIdHex) = ContactRequestRoute.decide(
                reachesInbox: false, sharedGroups: sharedGroupsOf(accountKey))
            else { throw ContactsError.requestRefused }
            try await groupRequestSender(groupIdHex, accountId, accountKey, name, username)
            return nil
        }
    }

    /// Whether the members of a group may find this account (its "Who can
    /// find me" setting). A seam for tests.
    var allowsGroupMembers: (Data) -> Bool = { ReachCenter.shared.allowsGroup($0) }

    /// The one-to-one this device holds with a person (``ContactConversation``
    /// for a contact row, else a live one-to-one with them). A seam for tests.
    lazy var heldOneToOne: (Data) -> String? = { [weak self] key in
        guard let self, let chatRepository = self.chatRepository else { return nil }
        let samePerson = self.provenSamePerson
        if let row = self.state.accepted.first(where: { samePerson($0.request.fromPubkey, key) }) {
            return chatRepository.oneToOneConversation(
                contactKey: row.request.fromPubkey, linkedGroupIdHex: row.groupIdHex)
        }
        return self.liveOneToOne(with: key)
    }

    /// A group request from a contact this device already has a chat with
    /// goes the mailbox's way (``handleKnownContactRequest(_:client:)``):
    /// nothing is shown. A seam for tests.
    lazy var knownContactRequestHandler: (IncomingContactRequest) -> Void = { [weak self] request in
        Task { @MainActor [weak self] in
            guard let self else { return }
            _ = await self.handleKnownContactRequest(request, client: self.clientFactory())
        }
    }

    /// A contact request one member posted inside `groupIdHex`. Dropped
    /// unless it opens with this account's key, verifies, comes from the
    /// member who posted it, and this account lets that group's members find
    /// it (or the sender is a contact already). Otherwise it joins Requests
    /// with a "Via {group}" line. Nothing is logged that names anyone.
    func receiveGroupContactRequest(groupIdHex: String, envelope: Data, senderKeyB64: String) {
        guard let secrets = authRepository.identitySecrets,
              let identity = try? Identity.fromSeed(seed: secrets.accountIdentitySeed),
              let senderKey = base64Decode(senderKeyB64)
        else { return }
        fileGroupContactRequest(
            groupIdHex: groupIdHex, envelope: envelope, senderKey: senderKey,
            ownX25519Secret: identity.x25519SecretKey(),
            createdAt: ISO8601DateFormatter().string(from: Date()))
    }

    /// The body of ``receiveGroupContactRequest(groupIdHex:envelope:senderKeyB64:)``
    /// with its inputs explicit. Returns whether a request was added.
    @discardableResult
    func fileGroupContactRequest(
        groupIdHex: String, envelope: Data, senderKey: Data, ownX25519Secret: Data, createdAt: String
    ) -> Bool {
        // ADR-0099 step 4 — a sender the moderation log already showed
        // suspended is dropped unseen before anything else decides. This
        // route files synchronously inside the group's event fold, so it
        // cannot wait for a standing; one not yet known is asked about below
        // and the request is hidden from Requests once it verifies.
        if groupRequestSenderSuspended(senderKey) { return false }
        let wire: Data
        do {
            guard let opened = try checkGroupContactRequest(
                plaintext: envelope, recipientX25519Priv: ownX25519Secret, senderAccountKey: senderKey)
            else { return false }
            wire = opened
        } catch {
            NSLog("contacts: a group contact request was dropped: \(type(of: error))")
            return false
        }
        let samePerson = provenSamePerson
        let isContact = state.accepted.contains { samePerson($0.request.fromPubkey, senderKey) }
        guard GroupContactRequestGate.shows(
            allowsGroupMembers: ChatRepository.hexToBytes(groupIdHex).map(allowsGroupMembers) ?? false,
            senderIsContact: isContact)
        else { return false }
        // A contact this device already has a chat with: the mailbox's way,
        // nothing shown. A contact WITHOUT one here is shown as a request
        // whose Accept founds the one-to-one: answering it silently with an
        // ack, as the mailbox does for a known contact, would leave no chat
        // on either side and the next Message would ask again, forever.
        let hasChat = heldOneToOne(senderKey) != nil
        guard let opened = ContactsClient.incomingRequest(
            wire: wire, messageId: GroupContactRequestGate.messageId(envelope: envelope),
            createdAt: createdAt, myX25519Secret: ownX25519Secret, viaGroupIdHex: groupIdHex)
        else { return false }
        let request = opened.request
        if hasChat {
            if isContact { knownContactRequestHandler(request) }
            return false
        }
        askedAboutGroupRequestSender(senderKey)
        if let replyGrant = opened.replyGrant {
            ReachCenter.shared.keep(
                replyGrant, carrier: .request,
                fromAccountKeyB64: base64Encode(request.fromPubkey), scopeHex: nil)
        }
        if let profile = request.profile {
            ProfileDirectory.shared.acceptReceived(
                profile, signatureKeyB64: base64Encode(request.fromPubkey),
                accountIdB64: boundAccountIdB64)
        }
        // A contact with no chat here asks for one: their row does not hide
        // the request, as it does for a mailbox request. Filed into `state`
        // synchronously, whose `didSet` writes the inbox store: this runs
        // inside the group's event fold, before the group cursor moves past
        // the row, so the app dying in between does not lose it.
        let merged = ContactsInboxMerge.merge(
            fetched: [request], pending: state.pending,
            accepted: state.accepted.filter { !samePerson($0.request.fromPubkey, senderKey) },
            currentKey: { ReachCenter.shared.currentAccountKey(of: $0) },
            isBlocked: Self.requestBlocked)
        guard !merged.kept.isEmpty else { return false }
        state = state.with(pending: merged.applied(to: state.pending))
        return true
    }

    /// Whether two account keys are one person across proven rotations
    /// (ADR-0074 section 3.7): the rule every contact row is matched by, so a
    /// person is one row whichever of their keys an event names.
    private var provenSamePerson: (Data, Data) -> Bool {
        let proven = ProvenPredecessorStore().load()
        return { proven.samePerson(base64Encode($0), base64Encode($1)) }
    }

    /// Put a person the viewer removed back on the contact list, with their
    /// one-to-one (`docs/ux/conversation-info.md` section 2.1, amended
    /// 2026-10-24): the removal is forgotten and the one-to-one rule applied.
    /// Local; sends nothing. Returns whether anything changed.
    @discardableResult
    public func restoreRemovedContact(
        signatureKey: Data,
        accountId: Data?,
        groupIdHex: String,
        nowMs: UInt64,
        createdAt: String
    ) -> Bool {
        guard let accountIdB64 = boundAccountIdB64 else { return false }
        let wasRemoved = removedKeys.remove(signatureKey) != nil
        if wasRemoved {
            AcceptedContactsStore.saveRemovedKeys(removedKeys, accountIdB64: accountIdB64)
        }
        let adopted = adoptOneToOnePeer(
            signatureKey: signatureKey, accountId: accountId, groupIdHex: groupIdHex,
            nowMs: nowMs, createdAt: createdAt)
        return wasRemoved || adopted
    }

    /// Whether the viewer removed this person from their contact list.
    public func isRemovedContact(_ signatureKey: Data) -> Bool {
        removedKeys.contains(signatureKey)
    }

    /// Whether `groupIdHex`, a link stored on a contact row, may stay: `false`
    /// only when this device holds it and it is not a one-to-one with the
    /// person (a group by its ADR-0062 kind, or a one-to-one whose other
    /// person is someone else). Unknown here (not held, a torn read) keeps
    /// it: the read rule (``ContactConversation``) never answers it anyway.
    func linkIsOneToOne(
        _ groupIdHex: String, with signatureKey: Data, samePerson: (Data, Data) -> Bool
    ) -> Bool {
        guard let chatRepository, let kind = chatRepository.groupKind(groupIdHex: groupIdHex) else {
            return true
        }
        guard kind == .oneToOne else { return false }
        guard let peer = chatRepository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { return true }
        return samePerson(peer, signatureKey)
    }

    /// Clear every stored chat link that names a conversation this device
    /// holds and that is not the person's one-to-one (2026-11-05: on web a
    /// row linked to a group the person is in, and Message opened the
    /// group). Returns whether anything changed.
    @discardableResult
    public func dropMisdirectedLinks() -> Bool {
        let samePerson = provenSamePerson
        guard let next = Self.unlinking(state.accepted, keepsLink: { row, held in
            linkIsOneToOne(held, with: row.request.fromPubkey, samePerson: samePerson)
        }) else { return false }
        state = state.with(accepted: next)
        return true
    }

    /// The pure half of ``dropMisdirectedLinks()``: the list with every link
    /// `keepsLink` refuses cleared, or `nil` when none is.
    nonisolated static func unlinking(
        _ accepted: [AcceptedContact], keepsLink: (AcceptedContact, String) -> Bool
    ) -> [AcceptedContact]? {
        var changed = false
        let rows = accepted.map { row -> AcceptedContact in
            guard let held = row.groupIdHex, !keepsLink(row, held) else { return row }
            changed = true
            return row.withGroupIdHex(nil)
        }
        return changed ? rows : nil
    }

    /// Tests only: answers ``liveOneToOne(with:)`` in place of the chat layer.
    var liveOneToOneLookup: ((Data) -> String?)?

    /// The live one-to-one this device holds with a person, if any: a
    /// conversation of one-to-one kind whose other person is `signatureKey`
    /// and whose membership is active on this device.
    func liveOneToOne(with signatureKey: Data) -> String? {
        if let liveOneToOneLookup { return liveOneToOneLookup(signatureKey) }
        guard let chatRepository else { return nil }
        return chatRepository.conversationHeadIdHexes.first { groupIdHex in
            chatRepository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) == signatureKey
                && chatRepository.isActive(groupIdHex: groupIdHex) == true
        }
    }

    /// One person is one row, and a request whose contact exists is gone:
    /// two rows of one key become one, and a pending request from a contact
    /// moves onto that contact's row. Pure.
    ///
    /// A request that asks for a chat (``IncomingContactRequest/asksForChatHere``:
    /// through a group, or listed for want of a one-to-one) is not folded
    /// unless its sender is among `answered`, the people a one-to-one was
    /// just adopted with: until then Accept is how the chat starts.
    nonisolated static func reconciled(
        accepted: [AcceptedContact], pending: [IncomingContactRequest],
        samePerson: (Data, Data) -> Bool = { $0 == $1 },
        answered: [Data] = []
    ) -> (accepted: [AcceptedContact], pending: [IncomingContactRequest]) {
        var rows: [AcceptedContact] = []
        for row in accepted {
            if let idx = rows.firstIndex(where: { samePerson($0.request.fromPubkey, row.request.fromPubkey) }) {
                rows[idx] = merging(rows[idx], with: row)
            } else {
                rows.append(row)
            }
        }
        rows = foldingRetiredKeyRows(rows)
        var left: [IncomingContactRequest] = []
        for request in pending {
            let stillAsking = request.asksForChatHere
                && !answered.contains(where: { samePerson($0, request.fromPubkey) })
            if !stillAsking, rows.contains(where: { $0.request.fromPubkey == request.fromPubkey }) {
                rows = accepting(request, into: rows, groupIdHex: nil)
            } else {
                left.append(request)
            }
        }
        return (rows, left)
    }

    /// Defect b (2026-11-28) — a row under a key that another row lists as
    /// one of its own keys of before (a sealed reset re-keyed that row) is
    /// the same person: it folds into the row that lists it, which keeps its
    /// own key, request, link and rename. One person, one row. A row of an
    /// account that started over (an unsealed reset) is never anyone's alias,
    /// so it stays. Pure.
    nonisolated static func foldingRetiredKeyRows(_ accepted: [AcceptedContact]) -> [AcceptedContact] {
        var rows = accepted
        var i = 0
        while i < rows.count {
            let retired = rows[i]
            guard let into = rows.indices.first(where: { j in
                j != i && rows[j].request.fromPubkey != retired.request.fromPubkey
                    && rows[j].answersToAlias(retired.request.fromPubkey)
                    && !retired.answersToAlias(rows[j].request.fromPubkey)
            }) else {
                i += 1
                continue
            }
            let current = rows[into]
            var previous = (current.previousKeysB64 ?? []) + (retired.previousKeysB64 ?? [])
                + [base64Encode(retired.request.fromPubkey)]
            var seen: Set<String> = [base64Encode(current.request.fromPubkey)]
            previous = previous.filter { seen.insert($0).inserted }
            rows[into] = AcceptedContact(
                request: IncomingContactRequest(
                    messageId: current.request.messageId,
                    fromPubkey: current.request.fromPubkey,
                    fromAccountId: current.request.fromAccountId ?? retired.request.fromAccountId,
                    fromPseudonym: current.request.fromPseudonym,
                    validAfterMs: current.request.validAfterMs,
                    intro: current.request.intro,
                    profile: current.request.profile,
                    createdAt: current.request.createdAt),
                groupIdHex: current.groupIdHex,
                displayName: current.displayName ?? retired.displayName,
                previousKeysB64: previous.isEmpty ? nil : previous)
            rows.remove(at: i)
        }
        return rows
    }

    /// The accepted list with `request` accepted: one more row, or, when the
    /// person already has one, that row carrying the request. A rename, a
    /// recorded chat and the keys they held stay. The row takes the request's
    /// id, which is what its owed ack is found by.
    nonisolated static func accepting(
        _ request: IncomingContactRequest, into accepted: [AcceptedContact], groupIdHex: String?,
        samePerson: (Data, Data) -> Bool = { $0 == $1 }
    ) -> [AcceptedContact] {
        guard let idx = accepted.firstIndex(where: { samePerson($0.request.fromPubkey, request.fromPubkey) }) else {
            return accepted + [AcceptedContact(request: request, groupIdHex: groupIdHex)]
        }
        var rows = accepted
        let merged = merging(
            accepted[idx], with: AcceptedContact(request: request, groupIdHex: groupIdHex))
        // The accept has just founded the one-to-one with them: that is their
        // chat, whatever the row held before.
        rows[idx] = groupIdHex.map { merged.withGroupIdHex($0) } ?? merged
        return rows
    }

    /// Two rows of one person as one. A mailbox request beats a synthetic
    /// one (``syntheticContactMessageId(signatureKey:)``), the later of two
    /// mailbox requests wins; nothing either row knew is lost.
    private nonisolated static func merging(
        _ held: AcceptedContact, with other: AcceptedContact
    ) -> AcceptedContact {
        let request = other.request.messageId > 0 || held.request.messageId < 0 ? other.request : held.request
        var previous = (held.previousKeysB64 ?? []) + (other.previousKeysB64 ?? [])
        // Two keys of one person (ADR-0074 section 3.7): the one that does
        // not name the row stays an alias of it.
        for key in [held.request.fromPubkey, other.request.fromPubkey] where key != request.fromPubkey {
            previous.append(base64Encode(key))
        }
        var seen = Set<String>()
        let keys = previous.filter { seen.insert($0).inserted }
        return AcceptedContact(
            request: IncomingContactRequest(
                messageId: request.messageId,
                fromPubkey: request.fromPubkey,
                fromAccountId: request.fromAccountId ?? held.request.fromAccountId ?? other.request.fromAccountId,
                fromPseudonym: request.fromPseudonym,
                validAfterMs: request.validAfterMs,
                intro: request.intro,
                profile: request.profile,
                createdAt: request.createdAt),
            groupIdHex: held.groupIdHex ?? other.groupIdHex,
            displayName: held.displayName ?? other.displayName,
            previousKeysB64: keys.isEmpty ? nil : keys)
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
        createdAt: String,
        samePerson: (Data, Data) -> Bool = { $0 == $1 },
        keepsLink: (String) -> Bool = { _ in true }
    ) -> [AcceptedContact]? {
        // Defect b (2026-11-28) — after a sealed reset the person's row is
        // re-keyed and the key of before is one of its aliases, while the
        // one-to-one held with that key still names it: that is the same
        // row. A reset is no proven rotation, so `samePerson` cannot see it;
        // without this the old one-to-one minted a second row of one person.
        // Nothing is linked or written for it.
        if !accepted.contains(where: { samePerson($0.request.fromPubkey, signatureKey) }),
           accepted.contains(where: { $0.answersToAlias(signatureKey) }) {
            return nil
        }
        if let idx = accepted.firstIndex(where: { samePerson($0.request.fromPubkey, signatureKey) }) {
            let old = accepted[idx]
            // A person met after a rotation (ADR-0074 section 3.7): the row
            // is under the key they asked with, the conversation under the
            // key their leaves are bound to. One person, one row.
            var previous = old.previousKeysB64 ?? []
            let alias = base64Encode(signatureKey)
            let newAlias = old.request.fromPubkey != signatureKey && !previous.contains(alias)
            if newAlias { previous.append(alias) }
            // A held link that is not their one-to-one (a group, or a
            // one-to-one with someone else) gives way to this one, which is.
            let held = old.groupIdHex.flatMap { keepsLink($0) ? $0 : nil }
            // A row adopted before the person's profile (or the invite link)
            // told this device their account id takes it now: without it they
            // cannot be added to a group (seen on an iPhone 2026-11-10: a
            // contact added through their link read "Accept this contact
            // again").
            let fillsAccountId = old.request.fromAccountId == nil && accountId != nil
            guard held == nil || newAlias || fillsAccountId else { return nil }
            var rows = accepted
            let request = fillsAccountId ? IncomingContactRequest(
                messageId: old.request.messageId,
                fromPubkey: old.request.fromPubkey,
                fromAccountId: accountId,
                fromPseudonym: old.request.fromPseudonym,
                validAfterMs: old.request.validAfterMs,
                intro: old.request.intro,
                profile: old.request.profile,
                createdAt: old.request.createdAt) : old.request
            rows[idx] = AcceptedContact(
                request: request, groupIdHex: held ?? groupIdHex,
                displayName: old.displayName, previousKeysB64: previous.isEmpty ? nil : previous)
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
        rows[idx] = accepted[idx].withGroupIdHex(groupIdHex)
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

    /// Accept a request: found the one-to-one, file the contact, ack.
    /// ``ContactAccept`` is the order and the failure rule.
    public func acceptIncomingRequest(_ request: IncomingContactRequest) async -> ContactAccept.Outcome {
        let bound = binding.capture()
        // ADR-0098 — a request from an account that is gone is not accepted:
        // the card goes and the screen says so. Only a `404` counts; a
        // network failure goes on to the ordinary accept.
        if let accountId = request.fromAccountId,
           await Self.accountAnswersAbsent(
                accountId: accountId, personKey: request.fromPubkey,
                fetch: { [discoveryClientFactory] in try await discoveryClientFactory().identityFetch(accountId: $0) }) {
            guard bound.isCurrent else { return .failed }
            declineContactRequest(messageId: request.messageId)
            return .deletedAccount
        }
        adoptStartedOverKeys(of: request)
        return await ContactAccept.run(request: request, ports: ContactAccept.Ports(
            found: { [weak self] request, accountId in
                guard let chat = self?.chatRepository else { throw ContactAccept.Unavailable() }
                let founding = try await chat.foundOneToOneAnswering(
                    requestMessageId: request.messageId, recipientAccountId: accountId,
                    recipientSignaturePubkey: request.fromPubkey)
                try bound.require()
                return founding
            },
            file: { [weak self] messageId, groupIdHex in
                self?.acceptContactRequest(messageId: messageId, groupIdHex: groupIdHex)
            },
            ack: { [weak self] request in
                _ = try await self?.sendContactAck(request: request)
            }))
    }

    /// UX 16.11 — accepting a request from the new keys of an account that
    /// started over (by Accept on its card, or through one of this account's
    /// invite links) takes those keys as the account's, the way a first pin
    /// would: the person decided, here, that it is them. The one-to-one with
    /// the old keys stays closed and its contact row stays as it is; nothing
    /// is moved. Nothing happens for any other request.
    func adoptStartedOverKeys(of request: IncomingContactRequest) {
        guard let accountId = request.fromAccountId else { return }
        let pins = PeerPinStore()
        let accountIdB64 = base64Encode(accountId)
        let requestKeyB64 = base64Encode(request.fromPubkey)
        guard StartedOverRequest.adoptsNewKeys(
            requestKeyB64: requestKeyB64,
            pendingNewKeyB64: pins.startedOver(accountIdB64: accountIdB64)?.newKeyB64) else { return }
        if pins.adoptStartedOver(
            accountIdB64: accountIdB64, newKeyB64: requestKeyB64,
            nowMs: Int64(Date().timeIntervalSince1970 * 1000)) {
            StartedOverEvents.notify(accountIdB64: accountIdB64)
        }
    }

    /// The Welcome of an accept landed late: its ack goes out now. A request
    /// still listed (the process died inside the accept) is filed first.
    func ackOwedAnswer(requestMessageId: Int64, groupIdHex: String) async {
        acceptContactRequest(messageId: requestMessageId, groupIdHex: groupIdHex)
        guard let contact = state.accepted.first(where: { $0.request.messageId == requestMessageId }) else {
            return
        }
        do {
            _ = try await sendContactAck(request: contact.request)
        } catch {
            NSLog("contact accept: the ack was not delivered: %@", ContactAccept.reason(of: error))
        }
    }

    /// An ack from the recipient of one of this account's outgoing requests:
    /// they pressed Accept. The signature was verified in the UniFFI layer.
    ///
    /// The ack makes no contact (decided 2026-11-04): a person becomes a
    /// contact only when this device joins the one-to-one with them, so a
    /// contact always has a conversation. Until the Welcome is joined the
    /// request stays under Sent, as pending, with the key that signed the ack
    /// recorded on it: the Welcome is matched to the row by that key
    /// (``noteAnsweredByWelcome(groupIdHex:)``), which then makes the contact
    /// and retires the row. A person already a contact (the Welcome came
    /// first) only loses the row.
    ///
    /// Returns `true` for a silent answer while this device holds NO live
    /// one-to-one with the person (it lost its chat while theirs still
    /// stands): the caller founds one with them after the batch
    /// (``foundForSilentAnswers(_:)``); the request stays under Sent until
    /// then.
    @discardableResult
    func applyContactAck(_ ack: IncomingContactAck) -> Bool {
        let samePerson = provenSamePerson
        let alreadyKnown = state.accepted.contains {
            samePerson($0.request.fromPubkey, ack.accepterPubkey)
        }
        let accepterAccountIdB64 = base64Encode(ack.accepterAccountId)
        // ADR-0060 — the request was answered: its `reply` key goes.
        ReachCenter.shared.retireReplies(
            purpose: ReplyPurpose.contact(ack.accepterAccountId))
        // Before the row goes: it holds the name they were found under.
        fileFoundName(
            answering: SentRequests.row(recipientAccountIdB64: accepterAccountIdB64, in: state.sent),
            signatureKey: ack.accepterPubkey)
        let liveHex = alreadyKnown ? nil : liveOneToOne(with: ack.accepterPubkey)
        if alreadyKnown || liveHex != nil {
            // Base scenario 7 (seen 2026-11-22) — the silent answer: this
            // account removed the person and asked them again; on their side
            // this account is still a contact, so their device answers with
            // the ack alone and no Welcome follows. The one-to-one both still
            // hold is the answer: they are put back with it (asking them was
            // a decision to have them back), the contact first and only then
            // the row under Sent.
            if let liveHex, SentRequests.askedAcker(
                in: state.sent, key: ack.accepterPubkey, accountIdB64: accepterAccountIdB64)
            {
                restoreRemovedContact(
                    signatureKey: ack.accepterPubkey,
                    accountId: ack.accepterAccountId,
                    groupIdHex: liveHex,
                    nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    createdAt: ISO8601DateFormatter().string(from: Date()))
            }
            let remainingSent = SentRequests.answered(
                recipientAccountIdB64: accepterAccountIdB64, in: state.sent)
            if remainingSent.count != state.sent.count {
                state = state.with(sent: remainingSent)
            }
            return false
        }
        let asked = SentRequests.askedAcker(
            in: state.sent, key: ack.accepterPubkey, accountIdB64: accepterAccountIdB64)
        let acked = SentRequests.acknowledged(
            recipientAccountIdB64: accepterAccountIdB64, byKey: ack.accepterPubkey, in: state.sent)
        if acked != state.sent { state = state.with(sent: acked) }
        return asked
    }

    /// Founds the one-to-one for a silent answer: a seam for tests. Default:
    /// ``ChatRepository/startGroupWith(recipientAccountId:recipientSignaturePubkey:kind:)``
    /// with the one-to-one kind, as Message on a contact card does.
    lazy var silentAnswerFounder: (IncomingContactAck) async throws -> String = { [weak self] ack in
        guard let chatRepository = self?.chatRepository else { throw InviteContactSendError.chatUnavailable }
        return try await chatRepository.startGroupWith(
            recipientAccountId: ack.accepterAccountId,
            recipientSignaturePubkey: ack.accepterPubkey,
            kind: .oneToOne)
    }

    /// Base scenario 7 (2026-11-22) — a person this device asked answered
    /// silently (their device still holds this account as a contact, and a
    /// one-to-one), while this device holds no one-to-one with them: no
    /// Welcome will come, so one is founded here and they are a contact with
    /// it (asking them was a decision to have them back). Their device joins
    /// it; the duplicate-one-to-one rule makes it the head of the
    /// conversation they had. A failure keeps the request under Sent.
    func foundForSilentAnswers(_ acks: [IncomingContactAck]) async {
        var seen: Set<Data> = []
        for ack in acks where seen.insert(ack.accepterPubkey).inserted {
            guard liveOneToOne(with: ack.accepterPubkey) == nil else { continue }
            let bound = binding.capture()
            do {
                let groupIdHex = try await silentAnswerFounder(ack)
                try bound.require()
                restoreRemovedContact(
                    signatureKey: ack.accepterPubkey,
                    accountId: ack.accepterAccountId,
                    groupIdHex: groupIdHex,
                    nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    createdAt: ISO8601DateFormatter().string(from: Date()))
                let remainingSent = SentRequests.answered(
                    recipientAccountIdB64: base64Encode(ack.accepterAccountId), in: state.sent)
                if remainingSent.count != state.sent.count {
                    state = state.with(sent: remainingSent)
                }
            } catch {
                NSLog("contacts inbox: founding a chat after a silent answer failed: \(type(of: error))")
            }
        }
    }

    /// `docs/ux/who-can-find-me.md` section 6.1 — the person holding
    /// `signatureKey` answered `row`: the name it found them under stands
    /// until their own profile arrives.
    private func fileFoundName(answering row: SentContactRequest?, signatureKey: Data) {
        guard let row, let claim = SentRequests.foundClaim(row, signatureKey: signatureKey) else { return }
        ProfileDirectory.shared.fileFoundName(
            claim, signatureKeyB64: base64Encode(signatureKey), accountIdB64: boundAccountIdB64)
    }

    /// A Welcome joined `groupIdHex`. When it is the one-to-one with someone
    /// this account asked, the request was answered, whether or not the ack
    /// ever arrives: their row under Sent goes and they are a contact. The
    /// reply key stays until the ack, which is written with it.
    private func noteAnsweredByWelcome(groupIdHex: String) {
        guard let peer = chatRepository?.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { return }
        let answered = SentRequests.answered(
            byPeer: peer, in: state.sent,
            keyOfAccount: { accountIdB64 in
                PeerPinStore().readPin(accountIdB64: accountIdB64)
                    .flatMap { base64Decode($0.pinnedSignaturePubkeyB64) }
            },
            current: { ReachCenter.shared.currentAccountKey(of: $0) })
        guard answered.remaining.count != state.sent.count else { return }
        // Usually ahead of the ack, which then finds no row to read the name
        // from.
        fileFoundName(
            answering: answered.accountIdB64.flatMap { SentRequests.row(recipientAccountIdB64: $0, in: state.sent) },
            signatureKey: peer)
        // The contact first, and only then the row under Sent: a search row
        // reads the two, and must never find them in neither and offer Add
        // contact to someone who accepted. Asking them was a decision to have
        // them back, so a removal does not hold here.
        restoreRemovedContact(
            signatureKey: peer,
            accountId: answered.accountIdB64.flatMap(base64Decode),
            groupIdHex: groupIdHex,
            nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
            createdAt: ISO8601DateFormatter().string(from: Date()))
        state = state.with(sent: answered.remaining)
    }

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
        // One person is one row: a one-to-one already made them a contact
        // when a process died inside the accept.
        let newAccepted = Self.accepting(
            accepted, into: state.accepted, groupIdHex: groupIdHex, samePerson: provenSamePerson)
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
    /// ADR-0099 section 6 — whether a contact request from `key` is dropped
    /// unseen (`blockVerdict(.contactRequest)`).
    static func requestBlocked(_ key: Data) -> Bool {
        BlockList.shared.verdict(.contactRequest, accountKey: key) == .drop
    }

    /// ADR-0099 step 4 — of the given request senders, the ones verified
    /// suspended, all asked about at once under one bound. Injectable for
    /// tests; the default asks ``SuspensionMarks``.
    var suspendedRequestSenders: @MainActor ([Data]) async -> Set<Data> = { keys in
        await SuspensionMarks.shared.verifiedSuspended(keys)
    }

    /// ADR-0099 step 4 — whether a group-carried request's sender is known
    /// suspended now, and the background ask for one not yet known.
    /// Injectable for tests.
    var groupRequestSenderSuspended: @MainActor (Data) -> Bool = { SuspensionMarks.shared.isSuspended($0) }
    var askedAboutGroupRequestSender: @MainActor (Data) -> Void = { SuspensionMarks.shared.check([$0]) }

    /// The senders of the contact requests in a batch.
    static func requestSenders(_ events: [IncomingMailboxEvent]) -> [Data] {
        events.compactMap { event in
            if case .contactRequest(let req, _, _) = event { return req.fromPubkey }
            return nil
        }
    }

    /// Whether `event` is a contact request whose sender is verified
    /// suspended (whichever route: the inbox, or an invite link's answer).
    static func droppedAsSuspended(_ event: IncomingMailboxEvent, suspendedSenders: Set<Data>) -> Bool {
        guard case .contactRequest(let req, _, _) = event else { return false }
        return suspendedSenders.contains(req.fromPubkey)
    }

    /// ADR-0099 section 6 / UX 1.2 — block a person: the register is written
    /// (this device at once, the account's other devices at their next sync),
    /// their pending requests go unseen, and they are removed from the
    /// contact list exactly as `Remove contact` does (the contact key
    /// rotates, ADR-0060 section 1). Returns whether the block holds.
    @discardableResult
    public func block(accountKey: Data) -> Bool {
        guard BlockList.shared.set(accountKey, blocked: true) else { return false }
        let stale = state.pending.filter { Self.requestBlocked($0.fromPubkey) }.map(\.messageId)
        if !stale.isEmpty {
            state = state.with(pending: state.pending.filter { !stale.contains($0.messageId) })
        }
        for contact in state.accepted where contact.request.fromPubkey == accountKey {
            removeContact(messageId: contact.request.messageId)
        }
        return true
    }

    /// UX 1.3 — unblock. It does not make the person a contact again.
    @discardableResult
    public func unblock(accountKey: Data) -> Bool {
        BlockList.shared.unblock(accountKey)
    }

    public func removeContact(messageId: Int64) {
        let removed = state.accepted.filter { $0.request.messageId == messageId }
        let remaining = state.accepted.filter { $0.request.messageId != messageId }
        guard remaining.count != state.accepted.count else { return }
        state = state.with(accepted: remaining)
        if let accountIdB64 = boundAccountIdB64 {
            removedKeys.formUnion(removed.map(\.request.fromPubkey))
            AcceptedContactsStore.saveRemovedKeys(removedKeys, accountIdB64: accountIdB64)
        }
        // ADR-0060 sections 1 and 4 — the removed person held this account's
        // `contact` and `fof` keys: rotate both (the old keys are unregistered
        // at once), and forget what they offered and relayed. A removed
        // contact leaves the chosen list (UX section 4.1); adding them back
        // brings them in switched off. Dropping them there already rotated
        // `fof`.
        ReachCenter.shared.rotate(.contact)
        let dropped = ReachCenter.shared.dropChosenContacts(removed.compactMap(\.request.fromAccountId))
        if !dropped { ReachCenter.shared.rotate(.fof) }
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
    ///
    /// `presentedKey` is the key in front of this device (the key a request
    /// was signed with, the key an invite link names). ADR-0101 section 11 /
    /// UX 16.11: a row under a key an unsealed reset left behind counts only
    /// for that key, so the account's NEW key is a stranger's until the
    /// person accepts it as a new contact.
    func establishedContact(ofAccountId accountId: Data, presentedKey: Data?) -> AcceptedContact? {
        let accountIdB64 = base64Encode(accountId)
        let pins = PeerPinStore()
        let pin = pins.readPin(accountIdB64: accountIdB64)
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
            presentedKeyB64: presentedKey.map(base64Encode),
            startedOverKeysB64: Set(pins.loadStartedOverKeys().keys),
            row: { contact in
                KnownContactRequest.ContactRow(
                    signaturePubkeyB64: base64Encode(contact.request.fromPubkey),
                    accountIdHex: contact.request.fromAccountId.map { groupIdHex($0) })
            })
    }

    /// How many OTHER devices the account's DeviceGroup lists, or `nil` when
    /// unknown. A seam for tests; default: the chat layer's roster.
    lazy var otherDeviceCount: () -> Int? = { [weak self] in
        guard let roster = self?.chatRepository?.ownDeviceGroupRosterProvider(), !roster.isEmpty else { return nil }
        return roster.count - 1
    }

    /// Base scenario 7 — the requests ``handleKnownContactRequest(_:client:)``
    /// listed because this device holds no one-to-one with the contact who
    /// sent them; marked ``IncomingContactRequest/asksForChat`` as listed.
    private var asksForChatIds: Set<Int64> = []

    /// `request`, marked as asking for a chat when the known-contact check
    /// listed it for want of a one-to-one.
    private func markingAsksForChat(_ request: IncomingContactRequest) -> IncomingContactRequest {
        var marked = request
        if startedOverIds.remove(request.messageId) != nil { marked.startedOver = true }
        guard asksForChatIds.remove(request.messageId) != nil else { return marked }
        marked.asksForChat = true
        return marked
    }

    /// UX 16.11 — the requests ``handleKnownContactRequest(_:client:)`` listed
    /// because they come from the new keys of an account that started over;
    /// marked ``IncomingContactRequest/startedOver`` as listed.
    private var startedOverIds: Set<Int64> = []

    /// Returns `true` when `request` came from an account that is already an
    /// established contact and was dealt with here (answered, dropped or
    /// kept waiting): the caller then lists nothing. `false` for a
    /// stranger's request.
    func handleKnownContactRequest(_ request: IncomingContactRequest, client: ContactsClient) async -> Bool {
        guard let accountIdB64 = boundAccountIdB64, let fromAccountId = request.fromAccountId else {
            return false
        }
        // UX 16.11 — a request from the NEW keys of an account whose
        // unsealed reset this device already recorded is the new account's:
        // an ordinary request card with the note. Never answered silently,
        // never merged into the old contact (which counts only for its own
        // key, ``establishedContact(ofAccountId:presentedKey:)``).
        if StartedOverRequest.isFromNewKeys(
            requestKeyB64: base64Encode(request.fromPubkey),
            newKeyB64: PeerPinStore().startedOver(accountIdB64: base64Encode(fromAccountId))?.newKeyB64 ?? "") {
            startedOverIds.insert(request.messageId)
            KnownRequestWaitingStore().drop(messageId: request.messageId, accountIdB64: accountIdB64)
            return false
        }
        // Read BEFORE the check below, which may write a first pin.
        guard establishedContact(ofAccountId: fromAccountId, presentedKey: request.fromPubkey) != nil
            || KnownContactRequest.isContactKey(
                base64Encode(request.fromPubkey),
                in: state.accepted.map { base64Encode($0.request.fromPubkey) })
        else { return false }
        let check: KnownContactRequest.Check
        do {
            check = try await client.checkContactIdentity(accountId: fromAccountId) != nil
                ? .current : .inconclusive
        } catch is SupersessionRefused {
            check = .refused
        } catch let startedOver as ContactStartedOver {
            // ADR-0101 section 11 / UX 16.11: the account's reset carried no
            // seal. A request from its NEW keys is an ordinary request with
            // a note: nothing is answered silently and nothing is moved. One
            // from any other key of that account is from devices that are
            // gone.
            guard boundAccountIdB64 == accountIdB64 else { return true }
            if StartedOverRequest.isFromNewKeys(
                requestKeyB64: base64Encode(request.fromPubkey), newKeyB64: startedOver.newKeyB64) {
                startedOverIds.insert(request.messageId)
                KnownRequestWaitingStore().drop(messageId: request.messageId, accountIdB64: accountIdB64)
                return false
            }
            check = .refused
        } catch {
            check = .inconclusive
        }
        guard boundAccountIdB64 == accountIdB64 else { return true }
        // Base scenario 7 (2026-11-22) — answered silently only while this
        // device holds a live one-to-one with them; else listed, and Accept
        // starts the chat.
        // A one-to-one whose membership ended here is no chat. Without a
        // chat layer (tests) as held.
        let holdsOneToOne: Bool = {
            guard let held = heldOneToOne(request.fromPubkey) else { return chatRepository == nil }
            return chatRepository?.isActive(groupIdHex: held) != false
        }()
        let verdict = KnownContactRequest.verdict(
            senderIsContact: true,
            check: check,
            requestKeyB64: base64Encode(request.fromPubkey),
            trustedKeyB64: PeerPinStore().readPin(accountIdB64: base64Encode(fromAccountId))?
                .pinnedSignaturePubkeyB64,
            holdsOneToOne: holdsOneToOne,
            predatesContact: KnownContactRequest.requestPredatesContact(
                requestCreatedAt: request.createdAt,
                contactSince: (establishedContact(ofAccountId: fromAccountId, presentedKey: request.fromPubkey)
                    ?? state.accepted.first { $0.request.fromPubkey == request.fromPubkey })?
                    .request.createdAt),
            siblingMayAdmit: KnownContactRequest.siblingMayAdmit(
                otherDeviceCount: otherDeviceCount(),
                requestCreatedAt: request.createdAt,
                nowMs: Int64(Date().timeIntervalSince1970 * 1000)))
        let waiting = KnownRequestWaitingStore()
        switch verdict {
        case .show:
            if !holdsOneToOne {
                asksForChatIds.insert(request.messageId)
                waiting.drop(messageId: request.messageId, accountIdB64: accountIdB64)
            }
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
            NSLog("contacts inbox: a request from a contact's account was dropped")
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
            listed.append(markingAsksForChat(request))
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
                currentKey: client.mailboxResolver().current(of:), isBlocked: Self.requestBlocked)
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
            newKey: newKey,
            startedOverKeysB64: Set(PeerPinStore().loadStartedOverKeys().keys))
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
        // The answer to a request goes out under its reply grant, then a
        // contact grant, and nothing else (``ReachCenter/answerPlans(toRequester:)``).
        // ADR-0074 section 3.5a rule 6 — a refused ack re-checks the
        // requester's address once (the client derives the mailbox from the
        // same proven-predecessor store the re-check writes).
        return try await RefusedMailboxWrite.write(
            resolve: { ReachCenter.shared.answerPlans(toRequester: request.fromPubkey) },
            recheck: {
                await PersonAddressRecheck.recheck(
                    accountKey: request.fromPubkey, accountIdHint: request.fromAccountId)
            },
            publish: { plans in
                try await RefusedMailboxWrite.tryingPlans(plans) { plan in
                    try await client.sendContactAck(
                        accepterSigningSeed: identity.ed25519Seed(),
                        accepterAccountId: secrets.accountId,
                        accepterX25519Pubkey: identity.x25519PublicKey(),
                        originalRequest: request,
                        plan: plan)
                }
            })
    }

    /// ADR-0079 amendment of 2026-10-19 — refuse a request whose invite link
    /// no longer counts: a signed refusal to the requester's mailbox through
    /// the `reply` grant the request carried. Throws when it was not written.
    public func sendLinkRefusal(request: IncomingContactRequest, linkId: Data) async throws {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(ContactsError.simple("no identity secrets"))
        }
        let client = clientFactory()
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        try await RefusedMailboxWrite.write(
            resolve: { ReachCenter.shared.answerPlans(toRequester: request.fromPubkey) },
            recheck: {
                await PersonAddressRecheck.recheck(
                    accountKey: request.fromPubkey, accountIdHint: request.fromAccountId)
            },
            publish: { plans in
                try await RefusedMailboxWrite.tryingPlans(plans) { plan in
                    try await client.sendLinkRefusal(
                        inviterSigningSeed: identity.ed25519Seed(), linkId: linkId,
                        originalRequest: request, plan: plan)
                }
            })
    }

    /// ADR-0079 amendment of 2026-10-19 — the inviter's app refused the
    /// request this account sent through their link: the pending contact it
    /// names and the messages held for it go, silently. Only a refusal
    /// signed by the key the link named, over that link and this account,
    /// removes anything. `true` when one was removed.
    @discardableResult
    func applyLinkRefusal(_ refusal: Data) -> Bool {
        guard let secrets = authRepository.identitySecrets,
              let own = try? Identity.fromSeed(seed: secrets.accountIdentitySeed).signaturePublicKey(),
              let refused = PendingContacts.refusedBy(
                invited, refusal: refusal, ownAccountKey: own,
                verify: { verifyLinkRefusal(wire: $0, inviterKey: $1, expectedLinkId: $2, ownAccountKey: $3) })
        else { return false }
        removePendingContact(accountIdHex: refused.accountIdHex)
        if let accountId = LinkClaims.bytes(hex: refused.accountIdHex) {
            ReachCenter.shared.retireReplies(purpose: ReplyPurpose.contact(accountId))
        }
        return true
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
    /// The Invite links sheet's create form picks the label, the expiry
    /// (``InviteExpiry/never`` puts the `NEVER_EXPIRES` sentinel `0` in the
    /// capability and the ledger row) and `maxUses` (`0` single use,
    /// `UInt32.max` unlimited). The defaults are 7 days, single use,
    /// non-bootstrap-referral.
    public func generateInviteLink(
        label: String = "",
        expiry: InviteExpiry = .fromNow(ms: ContactsRepository.defaultInviteExpiryMs),
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
        // UNIX millis, or 0 — the `NEVER_EXPIRES` sentinel.
        let expiryMs = expiry.expiryMs(nowMs: nowMs)

        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        // ADR-0060 section 6 — the link carries a `link` grant scoped to this
        // link, registered BEFORE the link exists, so revoking it removes
        // exactly its key.
        let linkId = generateLinkId()
        let bound = binding.capture()
        let linkGrant: FfiReachGrant
        do {
            linkGrant = try await ReachCenter.shared.newLinkGrant(
                linkId: linkId, expiresAtMs: expiryMs == 0 ? nil : UInt64(expiryMs))
        } catch {
            throw ContactsError.wrap(error)
        }
        // From here on a step that fails mints no link, so the link key
        // registered above is removed with it (web and Android do the same).
        let dropLinkKey = { ReachCenter.shared.removeLink(linkId: linkId) }
        do {
            try bound.require()
        } catch {
            dropLinkKey()
            throw error
        }
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
                linkGrant: linkGrant,
                // ADR-0079 section 1 — the page the invited person opens
                // names this account: the own display name and username,
                // signed and sealed inside the capability.
                inviterDisplayName: ProfileDirectory.shared.ownProfile?.displayName,
                inviterUsername: ProfileDirectory.shared.ownUsername
            )
        } catch {
            dropLinkKey()
            throw ContactsError.wrap(error)
        }

        // ADR-0028 amendment 2026-09-28 — the ledger gets the SEALED blob and
        // the clear fields; the link carries the secret in its fragment. An
        // upload failure is an error and mints no link (there is no inline
        // form).
        let discovery = authenticatedDiscoveryClient()
        // ADR-0078 section 10: the ledger row carries a key minted for this
        // link alone, never the account key; the seed stays on this device.
        let revocation: (seed: Data, publicKey: Data)
        do {
            revocation = try LinkRevocationKey.fresh()
        } catch {
            dropLinkKey()
            throw ContactsError.wrap(error)
        }
        let blobId: Data
        do {
            blobId = try await discovery.uploadInvite(
                mint: mint,
                revocationPubkey: revocation.publicKey,
                expiryMs: UInt64(expiryMs),
                maxUses: maxUses
            )
        } catch {
            dropLinkKey()
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
        // The secret never leaves this device; with the revocation seed and
        // what the list shows, this device's record is the only list of the
        // account's links and the only key that revokes this one.
        InviteLinkSecretStore.shared.put(
            accountIdB64: base64Encode(secrets.accountId), slug: blobIdSlug, secret: mint.linkSecret)
        OwnLinkStore.shared.put(accountIdB64: base64Encode(secrets.accountId), OwnLink(
            kind: .contact, slug: blobIdSlug, revocationSeed: revocation.seed, label: label,
            expiryMs: expiryMs, maxUses: Int64(maxUses), bootstrapReferral: bootstrapReferral,
            mintedAtMs: nowMs))
        // ADR-0079 section 4 — the link's record goes to the synced register,
        // so every device of the account auto-accepts a request through it,
        // lists it and can revoke it. Since the 2026-10-17 rework it carries
        // the revocation seed, so a revoke on any device deletes the row.
        let recorded = await deviceGroupSync?.setLinkRecord(LinkRecord(
            hlc: 0, linkId: linkId, linkSecret: mint.linkSecret, kind: .contact, ledgerId: blobId,
            revocationSeed: revocation.seed,
            expiryMs: UInt64(max(0, expiryMs)), maxUses: maxUses, uses: 0, revoked: false,
            mintedAtMs: UInt64(max(0, nowMs)), label: label, answered: [],
            targetId: Data(), inviteSeed: Data(), capability: Data())) ?? false
        guard recorded else {
            // A link this device would not remember after a restart is not
            // handed out: every request through it would wait for ever. The
            // row on the server is deleted with the link's own seed.
            _ = try? await revokeInviteLink(blobId: blobId)
            throw ContactsError.wrap(InviteLinkNotRecorded())
        }
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

    /// Revoke an outstanding invite link via the discovery service's
    /// `DELETE` endpoint, signed by the link's own revocation seed from this
    /// device's record (ADR-0078 section 10), + drop the row from
    /// ``ContactsState/outstandingInvites``.
    /// The account's synced link records (ADR-0079 section 4) and their
    /// changes; empty / `nil` while the register is not wired. The Invite
    /// links sheet reads `shared` and `editable` from them.
    public var linkRecords: [LinkRecord] { deviceGroupSync?.linkRecords ?? [] }
    public var linkRecordsPublisher: AnyPublisher<[LinkRecord], Never>? {
        deviceGroupSync?.$linkRecords.eraseToAnyPublisher()
    }

    /// Copy (`docs/ux/contact-invite-link.md` section 9): mark the contact
    /// link at `slug` copied in the synced register before its text reaches
    /// the clipboard, so no device of the account offers to edit it any more.
    /// `false` when the register holds no record for it or nothing changed
    /// (already copied).
    @discardableResult
    public func markInviteLinkShared(slug: String) async -> Bool {
        guard let record = LinkRecords.contactRecord(slug: slug, in: linkRecords),
              !record.shared else { return false }
        return await deviceGroupSync?.markLinkShared(linkId: record.linkId) ?? false
    }

    public func revokeInviteLink(blobId: Data) async throws -> RevokeInviteResult {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let accountIdB64 = base64Encode(secrets.accountId)
        let slug = base64UrlEncodeNoPad(blobId)
        // ADR-0079 section 4 — the ledger holds ONE row per link and only the
        // device that revokes the link deletes it, once, with the revocation
        // seed the synced record carries (the seed of this device's own record
        // for a record written before the 2026-10-17 rework). The deletion is
        // marked owed BEFORE the register changes, so a crash or an
        // unreachable ledger leaves it owed for this device to retry after
        // every poll (``LinkRowSweep``); the other devices of the account,
        // seeing the link revoked, send the ledger nothing.
        let record = deviceGroupSync?.linkRecords.first { $0.kind == .contact && $0.ledgerId == blobId }
        let own = OwnLinkStore.shared.link(accountIdB64: accountIdB64, kind: .contact, slug: slug)
        let seed: Data? = record.flatMap { $0.revocationSeed.count == 32 ? $0.revocationSeed : nil } ?? own?.seed
        let deleter = LinkRowDeleter.shared
        if seed != nil { deleter.owed.set(accountIdB64: accountIdB64, slug: slug, owed: true) }
        // Marked revoked in the register first, from whichever device: every
        // device stops accepting through the link at once.
        if let record { await deviceGroupSync?.revokeLinkRecord(linkId: record.linkId) }
        guard let seed else {
            throw ContactsError.wrap(ContactsError.simple(
                "this device keeps no revocation key for that link"))
        }
        let discovery = authenticatedDiscoveryClient()
        let bound = binding.capture()
        let outcome: LinkRowDeleter.Outcome
        do {
            outcome = try await deleter.deleteOwed(
                accountIdB64: accountIdB64, blobId: blobId, seed: seed,
                delete: { try await discovery.revokeInvite(blobId: $0, signingSeed: $1) })
        } catch {
            throw ContactsError.wrap(error)
        }
        if outcome == .unreachable {
            // Still owed: this device's own record (the fallback seed) stays
            // until the ledger answers; the link is dead on every device
            // already (the register refuses it).
            if bound.isCurrent {
                ReachCenter.shared.removeLink(blobId: blobId)
                dropOutstandingInvite(blobId: blobId)
            }
            throw ContactsError.wrap(LinkRowDeletionOwed())
        }
        forgetRevokedLinkLocally(accountIdB64: accountIdB64, blobId: blobId, bound: bound.isCurrent)
        if case .deleted(let result) = outcome { return result }
        return .revoked
    }

    /// The ledger could not be reached for a revoked link's row: the deletion
    /// is owed and retried after every poll.
    struct LinkRowDeletionOwed: Error {}

    /// Drop this device's local traces of a contact link whose ledger row is
    /// dealt with: its own record, its secret, its reach key and the session
    /// row. Idempotent. `bound == false` (the account changed during a wait)
    /// keeps to the stores filed under `accountIdB64`.
    private func forgetRevokedLinkLocally(accountIdB64: String, blobId: Data, bound: Bool) {
        let slug = base64UrlEncodeNoPad(blobId)
        OwnLinkStore.shared.remove(accountIdB64: accountIdB64, kind: .contact, slug: slug)
        // `docs/ux/client-redesign.md` section 5.3 — the link is gone, so is
        // this device's secret.
        InviteLinkSecretStore.shared.remove(accountIdB64: accountIdB64, slug: slug)
        guard bound else { return }
        // ADR-0060 — the link's key goes with it.
        ReachCenter.shared.removeLink(blobId: blobId)
        dropOutstandingInvite(blobId: blobId)
    }

    private func dropOutstandingInvite(blobId: Data) {
        guard state.outstandingInvites.contains(where: { $0.blobId == blobId }) else { return }
        state = state.with(
            outstandingInvites: state.outstandingInvites.filter { $0.blobId != blobId })
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

        // ADR-0078 section 10: each link is revoked with its own revocation
        // seed from this device's record; there is no owner list, so a link
        // this device has no record of is another device's to revoke.
        _ = predecessorSeeds
        let discovery = authenticatedDiscoveryClient()
        for link in SpentGrants.linksToRevoke(spent: reach.spentLinks) {
            let kind: OwnLink.Kind = link.call ? .call : .contact
            let slug = base64UrlEncodeNoPad(link.blobId)
            var answers: [SpentGrants.RevokeAnswer] = []
            if let seed = OwnLinkStore.shared.link(accountIdB64: accountIdB64, kind: kind, slug: slug)?.seed {
                do {
                    let result = link.call
                        ? try await discovery.revokeCallInvite(blobId: link.blobId, signingSeed: seed)
                        : try await discovery.revokeInvite(blobId: link.blobId, signingSeed: seed)
                    answers.append(result == .revoked ? .revoked : .notFoundOrNotOwner)
                } catch {
                    answers.append(.failed)
                }
            }
            guard boundAccountIdB64 == accountIdB64 else { throw SessionNotReady() }
            // No record: nothing this device can revoke; the reach key goes.
            let verdict = answers.isEmpty ? SpentGrants.LinkVerdict.notOurs : SpentGrants.linkVerdict(answers)
            if verdict == .owed { owed += 1 }
            guard SpentGrants.dropsLocally(verdict, mintedHere: link.linkIdB64 != nil) else { continue }
            OwnLinkStore.shared.remove(accountIdB64: accountIdB64, kind: kind, slug: slug)
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
        // A scope with no ledger row (a link whose upload never landed) has
        // nothing to revoke.
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

    /// The live `/i/` links this device minted and keeps a record of
    /// (ADR-0078 section 10). No ledger row names its owner, so there is no
    /// server list: a link minted on another device is listed there.
    ///
    /// Throws ``ContactsError/wrapped`` when the session has no identity
    /// secrets (caller should prompt the user to re-enter their password).
    public func listActiveInvites() async throws -> ListInvitesResult {
        guard let secrets = authRepository.identitySecrets else {
            throw ContactsError.wrap(
                ContactsError.simple(
                    "no identity secrets — please re-enter your password"
                )
            )
        }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let iso = ISO8601DateFormatter()
        let accountIdB64 = base64Encode(secrets.accountId)
        // ADR-0079 section 4 — the synced register lists every device's
        // links, with their use counts. The link's secret is in the record,
        // so Copy works on every device.
        if let sync = deviceGroupSync, sync.ready {
            let rows = LinkRecords.listedContactLinks(sync.linkRecords, nowMs: UInt64(max(0, nowMs)))
                .map { record -> ActiveInvite in
                    let slug = base64UrlEncodeNoPad(record.ledgerId)
                    if InviteLinkSecretStore.shared.secret(accountIdB64: accountIdB64, slug: slug) == nil {
                        InviteLinkSecretStore.shared.put(
                            accountIdB64: accountIdB64, slug: slug, secret: record.linkSecret)
                    }
                    return ActiveInvite(
                        blobIdSlug: slug,
                        expiryMs: record.expiryMs,
                        maxUses: record.maxUses,
                        usedCount: record.uses,
                        label: record.label,
                        bootstrapReferral: false,
                        createdAt: iso.string(
                            from: Date(timeIntervalSince1970: TimeInterval(record.mintedAtMs) / 1000)))
                }
            return .list(rows)
        }
        let rows = OwnLinkStore.shared.all(accountIdB64: accountIdB64)
            .filter { $0.kind == .contact && !$0.isExpired(nowMs: nowMs) }
            .map { link in
                ActiveInvite(
                    blobIdSlug: link.slug,
                    expiryMs: UInt64(max(0, link.expiryMs)),
                    maxUses: UInt32(clamping: link.maxUses),
                    // Not known here: the use count lives in the synced
                    // register, so the row shows the limit, not uses left.
                    usedCount: nil,
                    label: link.label,
                    bootstrapReferral: link.bootstrapReferral,
                    createdAt: iso.string(from: Date(timeIntervalSince1970: TimeInterval(link.mintedAtMs) / 1000)))
            }
        return .list(rows)
    }

    /// Revoke every live link this device keeps a record of, each with its
    /// own revocation seed (ADR-0078 section 10). Account deletion runs this
    /// first, while the session still works: the directory cannot delete an
    /// account's links any more. Best effort; returns how many could not be
    /// reached.
    @discardableResult
    public func revokeAllOwnLinks() async -> Int {
        guard let secrets = authRepository.identitySecrets else { return 0 }
        return await OwnLinks.revokeAll(
            accountIdB64: base64Encode(secrets.accountId), discovery: authenticatedDiscoveryClient())
    }

    /// Default invite expiry — 7 days. Mirrors the Android +
    /// Web sides.
    public nonisolated static let defaultInviteExpiryMs: Int64 = 7 * 24 * 60 * 60 * 1000

    /// Server-side sentinel for `max_uses = single-use`.
    public nonisolated static let singleUse: UInt32 = 0

    /// Wire `expiry_ms` sentinel for a link that never expires
    /// (`NEVER_EXPIRES`).
    public nonisolated static let neverExpires: Int64 = 0

    /// URL prefix Lakko shares its invite links under: the web origin
    /// of the stack this build talks to plus the web
    /// `composePointerInviteUrl` path, so a link generated on either
    /// side opens on the other.
    public nonisolated static var inviteUrlPrefix: String { ApiEndpoints.webOrigin + "/i/" }

    /// The 6-byte ledger id an `/i/<slug>` names, or `nil` for any other slug:
    /// exactly 8 base64url characters decoding to ``DiscoveryClient`` 's id
    /// width. The retired 43-character id, an inline sealed blob (the removed
    /// form) and everything else are refused before any request.
    public nonisolated static func parseInviteSlug(_ slug: String) -> Data? {
        guard slug.count == Int(inviteBlobIdUrlLen()),
              isBase64Url(slug),
              let bytes = base64UrlDecodeNoPad(slug),
              bytes.count == Int(inviteBlobIdLen()) else { return nil }
        return bytes
    }

    // MARK: - ADR-0079: the invite page (B) and the pending contact

    /// Open an `/i/<slug>#<secret>` link for the invite page
    /// (`docs/ux/contact-invite-link.md` section 3): fetch the sealed blob,
    /// open it under the fragment's secret, and decide the page's state. No
    /// use of the link is spent and nothing is sent here.
    public func openInviteLink(slug token: String) async -> InviteOpenResult {
        guard authRepository.identitySecrets != nil else { return .requiresReauth }
        // ADR-0028 amendment 2026-09-28 — `token` is `slug#fragment`; a
        // missing or malformed fragment is a damaged link, which the page
        // shows exactly like one that no longer works.
        let bound = binding.capture()
        let fetched = await fetchAndOpenLink(token: token, client: authenticatedDiscoveryClient())
        guard bound.isCurrent else { return .requiresReauth }
        let blobId: Data, linkSecret: Data, capability: InviteCapability
        switch fetched {
        case .opened(let b, let s, let c): (blobId, linkSecret, capability) = (b, s, c)
        case .dead: return .dead
        case .rateLimited: return .rateLimited
        case .failed(let message): return .failed(message: message)
        }
        let ownerAccountId = capability.ownerAccountId()
        let opened = OpenedInvite(
            blobId: blobId,
            capabilityBytes: capability.wire(),
            ownerAccountId: ownerAccountId,
            ownerSignaturePubkey: capability.ownerSignaturePubkey(),
            ownerX25519Pubkey: capability.ownerX25519Pubkey(),
            linkId: capability.linkId(),
            linkSecret: linkSecret,
            linkGrant: capability.linkGrant(),
            displayName: capability.inviterDisplayName(),
            username: capability.inviterUsername(),
            label: capability.label(),
            bootstrapReferral: capability.bootstrapReferral())
        // UX 16.11 — the link names keys this device does not hold for an
        // account it pins: the identity check runs first, so an unsealed
        // reset not yet seen here is recorded before the page decides (it
        // decides "Add", not "already in your contacts"). The outcome itself
        // is not used: the decision reads the stores the check wrote.
        let pinned = PeerPinStore().readPin(accountIdB64: base64Encode(ownerAccountId))?.pinnedSignaturePubkeyB64
        if let pinned, pinned != base64Encode(opened.ownerSignaturePubkey) {
            _ = try? await clientFactory().checkContactIdentity(accountId: ownerAccountId)
            guard bound.isCurrent else { return .requiresReauth }
        }
        return inviteOpenResult(for: opened)
    }

    /// A pending contact is the inviter of a link only under the link's own
    /// key: one recorded for keys an account has since left (an unsealed
    /// reset, UX 16.11) is another account's.
    nonisolated static func pendingIsInviter(
        _ contact: PendingContact, accountIdHex: String, signatureKey: Data
    ) -> Bool {
        contact.accountIdHex == accountIdHex && contact.signaturePubkey == signatureKey
    }

    /// The ledger row of an `/i/` link opened under its fragment's secret.
    private enum LinkFetch {
        case opened(blobId: Data, linkSecret: Data, capability: InviteCapability)
        /// Revoked, expired, used up, unknown or damaged.
        case dead
        case rateLimited
        case failed(String)
    }

    /// Fetch the sealed blob of `token` (`slug#fragment`) and open it under
    /// the fragment's secret. The ledger GET needs no session; nothing is
    /// spent and nothing is sent. A missing or malformed fragment is a
    /// damaged link (ADR-0028 amendment 2026-09-28).
    private func fetchAndOpenLink(
        token: String, client: DiscoveryClient, hasSession: Bool = true
    ) async -> LinkFetch {
        let parts = InviteLinkSecret.split(token)
        guard let blobId = Self.parseInviteSlug(parts.base),
              let linkSecret = InviteLinkSecret.read(parts.fragment) else { return .dead }
        let row: (sealed: Data, usedUp: Bool)?
        do {
            row = try await client.fetchInviteRow(blobId: blobId)
        } catch {
            if DiscoveryClientError.isRateLimited(error) { return .rateLimited }
            return .failed((error as? LocalizedError)?.errorDescription ?? "fetch failed")
        }
        guard let row, !Self.usedUpReadsDead(usedUp: row.usedUp, hasSession: hasSession),
              let capability = try? InviteCapability.openLink(sealed: row.sealed, linkSecret: linkSecret),
              capability.isValidNow(nowMs: UInt64(Date().timeIntervalSince1970 * 1000))
        else { return .dead }
        return .opened(blobId: blobId, linkSecret: linkSecret, capability: capability)
    }

    /// A used-up link is dead to a visitor with no session. With a session
    /// the flag is ignored: this person may be one who already used the link,
    /// bound for "already in your contacts", and only the consume can say.
    static func usedUpReadsDead(usedUp: Bool, hasSession: Bool) -> Bool {
        usedUp && !hasSession
    }

    /// The signed-out invite page (`docs/ux/contact-invite-link.md` section
    /// 3, "Signed out"; ADR-0079 amendment of 2026-10-26): open the link with
    /// no session, only to say who invites. The ledger serves the sealed blob
    /// without a session and it is opened on this device; nothing is spent
    /// and nothing is sent. Signing in or creating an account then lands on
    /// the default page through the parked link.
    public func previewInviteLink(slug token: String) async -> InvitePreviewResult {
        switch await fetchAndOpenLink(token: token, client: discoveryClientFactory(), hasSession: false) {
        case .opened(_, _, let capability):
            return .preview(InvitePreview(
                displayName: capability.inviterDisplayName(), username: capability.inviterUsername(),
                label: capability.label(), ownerSignaturePubkey: capability.ownerSignaturePubkey()))
        case .dead: return .dead
        case .rateLimited: return .rateLimited
        case .failed(let message):
            NSLog("[invite] signed-out open failed: \(message)")
            return .failed
        }
    }

    /// The page's state for a link already opened (``InvitePageDecision``),
    /// decided from this device's lists alone, so the page can decide again
    /// when the contact list or the pending contacts change while it is open
    /// (the inviter's answer to an earlier Add lands).
    public func inviteOpenResult(for opened: OpenedInvite) -> InviteOpenResult {
        guard let secrets = authRepository.identitySecrets else { return .requiresReauth }
        // ADR-0073 R.7 case 1 / UX 16.6 — "already a contact" is the pin or a
        // verified binding naming the row's key.
        // UX 16.11 — an old row of an account that started over counts only
        // for its own key: the new account's link is an ordinary Add page.
        let established = establishedContact(
            ofAccountId: opened.ownerAccountId, presentedKey: opened.ownerSignaturePubkey)
        let ownerHex = groupIdHex(opened.ownerAccountId)
        let decision = InvitePageDecision.decide(
            isOwn: opened.ownerAccountId == secrets.accountId,
            established: established != nil,
            pending: invited.contains {
                Self.pendingIsInviter($0, accountIdHex: ownerHex, signatureKey: opened.ownerSignaturePubkey)
            })
        switch decision {
        case .own:
            return .own
        case .established:
            guard let established else { return .opened(opened) }
            // Their chat is the one-to-one with them, never the stored link
            // by itself (it may name a group they are in).
            let chat = chatRepository?.oneToOneConversation(
                contactKey: established.request.fromPubkey, linkedGroupIdHex: established.groupIdHex)
            return .knownContact(
                contactMessageId: established.request.messageId, groupIdHex: chat,
                statedName: opened.statedName, invite: opened)
        case .pending:
            return .pendingInviter(accountIdHex: ownerHex, statedName: opened.statedName, invite: opened)
        case .open:
            return .opened(opened)
        }
    }

    /// **Add** on the invite page (ADR-0079 sections 2 and 5, amended
    /// 2026-10-19): the link's ledger use is spent first; a link used up or
    /// gone since the page opened keeps NOTHING (no pending contact, no held
    /// message) and the page goes to Contacts. Only then the inviter is
    /// recorded as a pending contact, BEFORE the request carrying the link
    /// answer is sent (an answer that comes back at once always finds it),
    /// and removed again when the send fails. This device founds no
    /// conversation: the inviter's device does, and its Welcome lands here.
    public func addThroughInviteLink(_ invite: OpenedInvite) async -> InviteAddResult {
        guard let secrets = authRepository.identitySecrets else { return .requiresReauth }
        let bound = binding.capture()
        // The link carries its owner's keys and this flow never fetches the
        // bundle: the same pin question is asked of them before anything is
        // spent or sealed (an attested reset is accepted there, silently).
        // UX 16.11 — the link of an account that started over names its NEW
        // keys: Add takes them as the new account's (the old contact and its
        // closed one-to-one stay as they are).
        do {
            try await clientFactory().refuseContactRequestToUnverifiedKeysForAdd(
                accountId: invite.ownerAccountId, recipientSignaturePubkey: invite.ownerSignaturePubkey)
        } catch {
            return .failed(message: "the link's keys are not the ones trusted for this account")
        }
        guard bound.isCurrent else { return .requiresReauth }
        guard let accountIdB64 = boundAccountIdB64 else { return .requiresReauth }
        // 2026-10-24 — the visitor removed the inviter and the one-to-one
        // remains: no pending contact; the inviter is put back with that
        // conversation once the request is sent (``InviteRestoreOrder``).
        if let liveGroupIdHex = liveOneToOne(with: invite.ownerSignaturePubkey) {
            do {
                let outcome = try await InviteRestoreOrder.run(
                    consumeLive: { try await consumeInviteUse(invite, secrets: secrets) },
                    sendRequest: { try await sendLinkAnswer(invite, secrets: secrets) },
                    restore: {
                        guard bound.isCurrent, boundAccountIdB64 == accountIdB64 else {
                            throw InviteAddStop(result: .requiresReauth)
                        }
                        restoreRemovedContact(
                            signatureKey: invite.ownerSignaturePubkey, accountId: invite.ownerAccountId,
                            groupIdHex: liveGroupIdHex,
                            nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
                            createdAt: ISO8601DateFormatter().string(from: Date()))
                    })
                return outcome == .dead ? .dead : .restored(groupIdHex: liveGroupIdHex)
            } catch let stop as InviteAddStop {
                return stop.result
            } catch {
                return .failed(message: (error as? LocalizedError)?.errorDescription ?? "request send failed")
            }
        }
        let contact = PendingContact(
            accountIdHex: groupIdHex(invite.ownerAccountId),
            signaturePubkey: invite.ownerSignaturePubkey,
            x25519Pubkey: invite.ownerX25519Pubkey,
            displayName: invite.displayName ?? "",
            username: invite.username ?? "",
            addedAtMs: Int64(Date().timeIntervalSince1970 * 1000),
            linkId: invite.linkId)
        let outcome: InviteAddOrder.Outcome
        do {
            outcome = try await InviteAddOrder.run(
                consumeLive: { try await consumeInviteUse(invite, secrets: secrets) },
                recordPending: {
                    guard bound.isCurrent, boundAccountIdB64 == accountIdB64 else {
                        throw InviteAddStop(result: .requiresReauth)
                    }
                    setInvited(PendingContacts.adding(contact, to: invited), accountIdB64: accountIdB64)
                },
                sendRequest: { try await sendLinkAnswer(invite, secrets: secrets) },
                forgetPending: {
                    if boundAccountIdB64 == accountIdB64 {
                        setInvited(invited.filter { $0.accountIdHex != contact.accountIdHex }, accountIdB64: accountIdB64)
                    }
                })
        } catch let stop as InviteAddStop {
            return stop.result
        } catch {
            return .failed(message: (error as? LocalizedError)?.errorDescription ?? "request send failed")
        }
        if outcome == .dead { return .dead }
        return .added(contact)
    }

    /// **Open chat** on the "already in your contacts" page
    /// (`docs/ux/contact-invite-link.md` section 3, amended 2026-10-24): the
    /// ledger use is spent and the request carrying the link answer is sent,
    /// so an inviter who removed this person gets them back on their device,
    /// with the conversation they have. No pending contact is written; a dead
    /// link sends nothing (``KnownContactAnswerOrder``).
    public func answerAsKnownContact(_ invite: OpenedInvite) async -> KnownContactAnswerResult {
        guard let secrets = authRepository.identitySecrets else { return .requiresReauth }
        let bound = binding.capture()
        // The same pin question as Add asks before anything is spent.
        do {
            try await clientFactory().refuseContactRequestToUnverifiedKeys(
                accountId: invite.ownerAccountId, recipientSignaturePubkey: invite.ownerSignaturePubkey)
        } catch {
            return .failed(message: "the link's keys are not the ones trusted for this account")
        }
        guard bound.isCurrent else { return .requiresReauth }
        do {
            let outcome = try await KnownContactAnswerOrder.run(
                consumeLive: { try await consumeInviteUse(invite, secrets: secrets) },
                sendRequest: {
                    guard bound.isCurrent else { throw InviteAddStop(result: .requiresReauth) }
                    try await sendLinkAnswer(invite, secrets: secrets)
                })
            return outcome == .dead ? .dead : .sent
        } catch let stop as InviteAddStop {
            switch stop.result {
            case .rateLimited: return .rateLimited
            case .requiresReauth: return .requiresReauth
            case .failed(let message): return .failed(message: message)
            case .dead: return .dead
            case .added, .restored: return .sent
            }
        } catch {
            return .failed(message: (error as? LocalizedError)?.errorDescription ?? "request send failed")
        }
    }

    /// The ledger's soft use count for an opened link: `false` when it
    /// answered used up or gone. Used before by this account: sent again, the
    /// inviter's device answers it either way. A refusal throws
    /// ``InviteAddStop`` with the page's result.
    private func consumeInviteUse(_ invite: OpenedInvite, secrets: IdentitySecrets) async throws -> Bool {
        let result: ConsumeInviteResult
        do {
            result = try await authenticatedDiscoveryClient().consumeInvite(
                blobId: invite.blobId, capabilityBytes: invite.capabilityBytes,
                joinerAccountId: secrets.accountId)
        } catch {
            if DiscoveryClientError.isRateLimited(error) { throw InviteAddStop(result: .rateLimited) }
            throw InviteAddStop(
                result: .failed(message: (error as? LocalizedError)?.errorDescription ?? "consume failed"))
        }
        switch result {
        case .exhausted, .notFound: return false
        case .alreadyConsumed, .consumed: return true
        }
    }

    /// The request carrying the link answer (ADR-0079 section 2), written to
    /// the owner's closed mailbox under the link's grant. ADR-0078 — the
    /// grant carries the owner's mailbox binding, verified against the key
    /// the request is sealed to.
    private func sendLinkAnswer(_ invite: OpenedInvite, secrets: IdentitySecrets) async throws {
        let link = MailboxTarget.verifiedOrNil(grant: invite.linkGrant, accountKey: invite.ownerSignaturePubkey)
            .map { RequestLink(seed: invite.linkGrant.seed, target: $0) }
        _ = try await sendRequestToBundle(
            client: clientFactory(), secrets: secrets,
            recipientAccountId: invite.ownerAccountId,
            recipientSignaturePubkey: invite.ownerSignaturePubkey,
            recipientX25519Pubkey: invite.ownerX25519Pubkey,
            introMessage: Data(),
            link: link,
            linkAnswer: FfiLinkAnswer(linkId: invite.linkId, linkSecret: invite.linkSecret))
    }

    /// Pairing state bundle v3 — the bundle added pending contacts to the
    /// store of `accountIdB64`: the bound list takes them, keeping any record
    /// written here meanwhile (``PendingContacts/adding(_:to:)``).
    func reloadBundledPendingContacts(accountIdB64: String) {
        guard boundAccountIdB64 == accountIdB64 else { return }
        var list = invited
        for contact in PendingContactStore.load(accountIdB64: accountIdB64)
        where !list.contains(where: { $0.accountIdHex == contact.accountIdHex }) {
            list = PendingContacts.adding(contact, to: list)
        }
        if list != invited { setInvited(list, accountIdB64: accountIdB64) }
    }

    private func setInvited(_ list: [PendingContact], accountIdB64: String) {
        PendingContactStore.save(list, accountIdB64: accountIdB64)
        if boundAccountIdB64 == accountIdB64 { invited = list }
    }

    /// The pending contact `accountIdHex`, while one is held.
    public func pendingContact(accountIdHex: String) -> PendingContact? {
        invited.first { $0.accountIdHex == accountIdHex }
    }

    /// B deletes a pending contact (UX section 7 item 4): the record goes,
    /// and so do the messages kept for it in the held-send queue. `false`
    /// when no such contact is held.
    @discardableResult
    public func removePendingContact(accountIdHex: String) -> Bool {
        guard let accountIdB64 = boundAccountIdB64,
              let contact = invited.first(where: { $0.accountIdHex == accountIdHex }) else { return false }
        chatRepository?.clearPendingInvitedMessages(queueKey: contact.queueKey)
        setInvited(invited.filter { $0.accountIdHex != accountIdHex }, accountIdB64: accountIdB64)
        return true
    }

    /// ADR-0079 section 5 — the pending contacts whose one-to-one now exists
    /// on this device (the Welcome joined in this poll, or before the record
    /// was read): their messages move to the conversation's held-send queue
    /// and the record goes. Run after every poll; a contact with no
    /// conversation yet stays.
    public func handOverJoinedPendingContacts() {
        guard let chatRepository, let accountIdB64 = boundAccountIdB64, !invited.isEmpty else { return }
        var remaining = invited
        for thread in chatRepository.state.threads {
            guard let facts = chatRepository.pendingInvitedFacts(groupIdHex: thread.groupIdHex),
                  let contact = PendingContacts.forConversation(
                    remaining, oneToOne: facts.oneToOne, personKeys: facts.personKeys)
            else { continue }
            guard chatRepository.handOverPendingInvitedMessages(
                queueKey: contact.queueKey, toGroupIdHex: thread.groupIdHex) else { continue }
            // The account id the link gave is the one fact the contact row
            // cannot learn from the conversation: kept on the row before the
            // pending record goes.
            if let accountId = StateBundle.hexDecode(contact.accountIdHex) {
                let now = UInt64(Date().timeIntervalSince1970 * 1000)
                adoptOneToOnePeer(
                    signatureKey: contact.signaturePubkey, accountId: accountId,
                    groupIdHex: thread.groupIdHex, nowMs: now,
                    createdAt: ISO8601DateFormatter().string(from: Date()))
            }
            remaining.removeAll { $0.accountIdHex == contact.accountIdHex }
            handedOver[contact.accountIdHex] = thread.groupIdHex
        }
        if remaining.count != invited.count { setInvited(remaining, accountIdB64: accountIdB64) }
    }

    // MARK: - ADR-0079: the inviter's device (A)

    /// The winning device's accept of a request sent through one of this
    /// account's live invite links (ADR-0079 section 3, order of 2026-10-17,
    /// ``LinkClaims/runBuild(_:)``): its own founding carried on; else a
    /// link-founded note from another device answered by writing the note's
    /// Welcome to the requester (never founding); else found in ONE commit
    /// with every other device of this account, record the founding, publish
    /// the note, and only then write the Welcome to this account's own
    /// mailbox and then the requester's. `false` when it did not complete;
    /// every record stays, and the next pass takes it up where it stopped.
    func acceptThroughLink(
        _ request: IncomingContactRequest, linkAnswer: FfiLinkAnswer,
        key: String, claims: LinkClaims.Record
    ) async -> Bool {
        guard let chatRepository, let sync = deviceGroupSync,
              let fromAccountId = request.fromAccountId else { return false }
        let bound = binding.capture()
        return await LinkClaims.runBuild(LinkClaims.BuildDeps(
            founded: { claims.own(key)?.founded },
            note: { claims.note(key) },
            prepare: { [weak self] in
                // UX 16.11 — a request through this account's link from the
                // new keys of an account that started over is a new contact:
                // its keys are taken before the identity gate of the founding.
                self?.adoptStartedOverKeys(of: request)
                return try await chatRepository.prepareLinkOneToOne(
                    recipientAccountId: fromAccountId, recipientSignaturePubkey: request.fromPubkey)
            },
            record: { claims.setFounded(key, $0) },
            postNote: { founded in
                // Published once (a sealed note already recorded is not
                // published again), then judged at its row: it counts only
                // when sealed at the journal's epoch there.
                let own = claims.own(key)
                if own?.noteConfirmed == true { return true }
                if own?.noteWire == nil {
                    guard let groupId = base64Decode(founded.groupIdB64),
                          let welcome = base64Decode(founded.welcomeB64) else {
                        throw ChatError.argument("the founding is malformed")
                    }
                    _ = try await sync.postLinkFounded(
                        linkId: linkAnswer.linkId, requesterKey: request.fromPubkey,
                        groupId: groupId, welcome: welcome,
                        beforePublish: { claims.setNoteWire(key, wireHex: $0) })
                }
                guard await sync.waitForJournalReadPastNow() else { return false }
                if claims.own(key)?.noteConfirmed == true { return true }
                // Read past it and it did not count (straddled by a commit)
                // or no row carried it: posted again at the next pass.
                claims.dropUnlandedNote(key)
                return false
            },
            hold: { founded in chatRepository.holdLinkOneToOne(groupIdB64: founded.groupIdB64) },
            deliver: { founded in try await chatRepository.deliverLinkWelcome(founded) },
            forward: { note in try await chatRepository.forwardLinkWelcome(note, toRequester: request.fromPubkey) },
            finish: { [weak self] groupIdHex in
                guard let self, bound.isCurrent else { return }
                if !self.state.accepted.contains(where: { $0.request.fromPubkey == request.fromPubkey }) {
                    self.state = self.state.with(accepted: self.state.accepted + [
                        AcceptedContact(request: request, groupIdHex: groupIdHex)
                    ])
                } else if let groupIdHex, let next = Self.linking(
                    self.state.accepted, signatureKey: request.fromPubkey, groupIdHex: groupIdHex) {
                    // The one-to-one just founded for them is their chat.
                    self.state = self.state.with(accepted: next)
                }
                do {
                    _ = try await self.sendContactAck(request: request)
                } catch {
                    // Best effort, as on a manual accept.
                }
                await sync.recordLinkUse(linkId: linkAnswer.linkId, requesterKey: request.fromPubkey)
            }))
    }

    /// One decision pass at a time; a trigger during a pass runs one more.
    private var linkPassRunning = false
    private var linkPassAgain = false

    /// ADR-0079 (reworked 2026-10-17) — decide every undecided request that
    /// answers one of the account's invite links (``LinkClaims/step``): no
    /// timer; a request waits, kept and unshown, until the synced state is
    /// loaded, and only the device whose claim is first in the DeviceGroup
    /// journal builds the one-to-one. The claim record is read and written
    /// on the main actor between the awaits, so one decision never
    /// interleaves with another (C-62 sub-invariant 5).
    public func decideLinkRequests() async {
        if linkPassRunning {
            linkPassAgain = true
            return
        }
        linkPassRunning = true
        defer { linkPassRunning = false }
        repeat {
            linkPassAgain = false
            await decideLinkRequestsOnce()
        } while linkPassAgain
    }

    private func decideLinkRequestsOnce() async {
        guard let accountIdB64 = boundAccountIdB64 else { return }
        let bound = binding.capture()
        let claims = LinkClaims.Record(accountIdB64: accountIdB64)
        let inbox = LinkRequestInbox(accountIdB64: accountIdB64)
        // Whether a device in the CURRENT roster claimed the request (a claim
        // only from a device since removed answers nothing). `false` while
        // the roster is not known.
        let answeredElsewhere: (String) -> Bool = { [weak self] key in
            let seen = claims.seen(key)
            guard !seen.isEmpty, let roster = self?.deviceGroupSync?.linkClaimRoster() else { return false }
            return (try? linkClaimAnsweredElsewhere(
                claims: Self.ffiClaims(seen), rosterKeys: roster.roster)) ?? false
        }
        // ADR-0099 step 4 — before a held request is decided (it would be
        // accepted with no card), its sender's standing is asked, all rows at
        // once under one bound: a verified suspension drops it unseen; a late
        // or failed answer decides it as before.
        let rows = inbox.rows()
        let suspendedRequesters = rows.isEmpty
            ? [] : await suspendedRequestSenders(rows.map(\.request.fromPubkey))
        for row in rows {
            guard bound.isCurrent else { return }
            let req = row.request
            let key = LinkClaims.requestKeyHex(linkId: row.linkId, requesterKey: req.fromPubkey)
            if suspendedRequesters.contains(req.fromPubkey) {
                claims.forgetOwn(key)
                inbox.remove(messageId: req.messageId)
                continue
            }
            // ADR-0079 amendment of 2026-10-19 — the moment the request was
            // written decides the link's expiry: the time the DS stamped on
            // the row, kept with the request across a restart. A stamp that
            // does not read counts as written after any expiry (fail-closed).
            let pressedAtMs = req.createdAtMs ?? UInt64.max
            let verdict: LinkClaims.LinkAnswerVerdictValue? = deviceGroupSync.flatMap { sync in
                sync.linkAnswerVerdictAt(
                    linkId: row.linkId, linkSecret: row.linkSecret, requesterKey: req.fromPubkey,
                    pressedAtMs: pressedAtMs)
            }.map {
                switch $0 {
                case .accept: return .accept
                case .answered: return .answered
                case .dead: return .dead
                case .unknown: return .unknown
                }
            }
            // A requester this device already holds a live one-to-one with
            // is put back on the contact list with it rather than given a
            // second conversation (amended 2026-10-24).
            let liveGroupIdHex = liveOneToOne(with: req.fromPubkey)
            var step = LinkClaims.step(
                own: claims.own(key), answeredElsewhere: answeredElsewhere(key), verdict: verdict,
                noteHeld: claims.note(key) != nil, liveOneToOne: liveGroupIdHex != nil)
            switch step {
            case .wait:
                continue
            case .restore:
                // A person removed and invited again: back on the list with
                // the conversation they already have. Nothing is founded or
                // sent; a live link records its use, as the requester's
                // consume did.
                guard let liveGroupIdHex else { continue }
                restoreRemovedContact(
                    signatureKey: req.fromPubkey, accountId: req.fromAccountId,
                    groupIdHex: liveGroupIdHex,
                    nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    createdAt: ISO8601DateFormatter().string(from: Date()))
                if verdict == .accept, let sync = deviceGroupSync {
                    await sync.recordLinkUse(linkId: row.linkId, requesterKey: req.fromPubkey)
                }
                guard bound.isCurrent else { return }
                claims.forgetOwn(key)
                inbox.remove(messageId: req.messageId)
                continue
            case .drop:
                claims.forgetOwn(key)
                inbox.remove(messageId: req.messageId)
                continue
            case .refuse:
                // The link does not count: no card. The requester is answered
                // with a refusal and the request goes; a failed write keeps it
                // for the next pass.
                do {
                    try await sendLinkRefusal(request: req, linkId: row.linkId)
                } catch {
                    NSLog("ADR-0079: link refusal not sent this pass")
                    continue
                }
                guard bound.isCurrent else { return }
                claims.forgetOwn(key)
                inbox.remove(messageId: req.messageId)
                continue
            case .postClaim, .decide, .recheck, .build:
                break
            }
            guard let sync = deviceGroupSync else { continue }
            if step == .postClaim {
                // The sealed claim is recorded before the publish: whether or
                // not the publish answers, the receive loop recognises its
                // row by those bytes and files its id as this device's.
                do {
                    _ = try await sync.postLinkClaim(
                        linkId: row.linkId,
                        requesterKey: req.fromPubkey,
                        beforePublish: { claims.addOwnWire(key, wireHex: $0) })
                    step = .decide
                } catch {
                    NSLog("ADR-0079: link claim not posted this pass")
                    continue
                }
            }
            if step == .decide || step == .recheck {
                // Every row up to (and past) this device's claims is applied
                // once a fetch that started after them found the journal
                // drained. A wait that times out keeps the request undecided
                // for the next pass. A re-check reads no journal: every claim
                // of this device is known and only the roster can change.
                if step == .decide {
                    guard await sync.waitForJournalReadPastNow(), bound.isCurrent else { continue }
                }
                guard let roster = sync.linkClaimRoster() else { continue }
                // A sealed claim no row carried did not land.
                claims.dropUnlanded(key)
                let decide = LinkClaims.claimsToDecide(
                    own: claims.own(key), seen: claims.seen(key), ownKeyHex: LinkClaims.hex(roster.ownKey))
                guard let outcome = try? linkClaimOutcome(
                    ownKey: roster.ownKey, claims: Self.ffiClaims(decide), rosterKeys: roster.roster)
                else { continue }
                switch outcome {
                case .lost:
                    // Another device's claim is first: it answers the request.
                    // Kept, unshown, until the register says it was
                    // answered; if that device is removed first, a re-check
                    // hands it on.
                    claims.setSettled(key)
                    continue
                case .noWinner:
                    // No claim from the roster landed: post again next pass.
                    claims.forgetOwn(key)
                    continue
                case .won(let messageId):
                    claims.setWon(key, messageId: Int64(messageId))
                    step = .build
                }
            }
            if step == .refuse {
                // Won, but the link was revoked before this device answered:
                // the revocation wins (ADR-0079 amendment of 2026-10-19).
                do {
                    try await sendLinkRefusal(request: req, linkId: row.linkId)
                } catch {
                    NSLog("ADR-0079: link refusal not sent this pass")
                    continue
                }
                guard bound.isCurrent else { return }
                claims.forgetOwn(key)
                inbox.remove(messageId: req.messageId)
                continue
            }
            if step == .build {
                // The Welcome below is addressed from the requester's reply
                // grant: filed again here, where the reach center is active.
                if let grant = row.replyGrant {
                    ReachCenter.shared.keep(
                        grant, carrier: .request,
                        fromAccountKeyB64: base64Encode(req.fromPubkey), scopeHex: nil)
                }
                let answer = FfiLinkAnswer(linkId: row.linkId, linkSecret: row.linkSecret)
                if await acceptThroughLink(req, linkAnswer: answer, key: key, claims: claims), bound.isCurrent {
                    inbox.remove(messageId: req.messageId)
                    claims.forgetOwn(key)
                }
            }
        }
    }

    /// The claim decision's inputs in the binding's shape; an author whose
    /// hex does not decode is left out.
    nonisolated static func ffiClaims(_ claims: [LinkClaims.Observed]) -> [FfiObservedLinkClaim] {
        claims.compactMap { claim in
            guard claim.id > 0, let author = LinkClaims.bytes(hex: claim.author) else { return nil }
            return FfiObservedLinkClaim(messageId: UInt64(claim.id), authorKey: author)
        }
    }

    /// ADR-0079 section 4 (reworked 2026-10-17) — run after every poll
    /// (``LinkRowSweep``): retry the ledger-row deletions this device owes
    /// (it revoked the link and the ledger could not be reached, or the app
    /// stopped in between), and for a link another device of the account
    /// revoked drop this device's local traces only, sending the ledger
    /// nothing: the revoking device deletes the row. The one exception is a
    /// record with no seed (written before the rework), whose row no other
    /// device can delete: this device deletes it once with its own seed.
    public func revokeLinksRevokedElsewhere() async {
        guard let accountIdB64 = boundAccountIdB64 else { return }
        let revoked = (deviceGroupSync?.linkRecords ?? [])
            .filter { $0.kind == .contact && $0.revoked && !$0.ledgerId.isEmpty }
            .map { LinkRowSweep.RevokedLink(blobId: $0.ledgerId, recordSeed: $0.revocationSeed) }
        let deleter = LinkRowDeleter.shared
        guard !revoked.isEmpty || !deleter.owed.slugs(accountIdB64: accountIdB64).isEmpty else { return }
        let discovery = authenticatedDiscoveryClient()
        let bound = binding.capture()
        await LinkRowSweep.run(
            accountIdB64: accountIdB64,
            revoked: revoked,
            ownSeed: { OwnLinkStore.shared.link(accountIdB64: accountIdB64, kind: .contact, slug: $0)?.seed },
            deleter: deleter,
            delete: { try await discovery.revokeInvite(blobId: $0, signingSeed: $1) },
            forget: { [weak self] blobId in
                self?.forgetRevokedLinkLocally(
                    accountIdB64: accountIdB64, blobId: blobId, bound: bound.isCurrent)
            })
    }
}

/// What opening an `/i/` link gave the invite page (ADR-0079).
public enum InviteOpenResult: Equatable {
    case opened(OpenedInvite)
    /// The link was revoked, expired, used up, is unknown, or is damaged:
    /// one state on the page.
    case dead
    /// The link is this account's own.
    case own
    /// The inviter is already an established contact: the page offers
    /// Open chat (the route key is the contact's request id, and the
    /// one-to-one when this device holds it). `invite` is the opened link,
    /// whose answer Open chat still delivers (amended 2026-10-24).
    case knownContact(
        contactMessageId: Int64, groupIdHex: String?, statedName: String?, invite: OpenedInvite? = nil)
    /// The visitor already pressed Add on one of the inviter's links and
    /// waits for the answer (a pending contact for the owner's account): the
    /// page says they are contacts and Open chat opens the waiting chat,
    /// sending nothing (2026-10-24).
    case pendingInviter(accountIdHex: String, statedName: String?, invite: OpenedInvite? = nil)
    case rateLimited
    case requiresReauth
    case failed(message: String)
}

/// What the signed-out invite page shows of a link (amended 2026-10-26).
public struct InvitePreview: Equatable {
    public let displayName: String?
    public let username: String?
    public let label: String
    public let ownerSignaturePubkey: Data

    /// The name the link states (``OpenedInvite/statedName(displayName:username:)``).
    public var statedName: String? {
        OpenedInvite.statedName(displayName: displayName, username: username)
    }
}

/// What opening a link with no session gave the signed-out invite page.
public enum InvitePreviewResult: Equatable {
    case preview(InvitePreview)
    /// Revoked, expired, used up, unknown or damaged: one state.
    case dead
    case rateLimited
    case failed
}

/// What **Add** did.
/// A step of **Add** that ends it with a result of its own (a refused
/// consume, a session that moved).
private struct InviteAddStop: Error {
    let result: InviteAddResult
}

/// What **Open chat** on the "already in your contacts" page did with the
/// link's answer (amended 2026-10-24).
public enum KnownContactAnswerResult: Equatable {
    /// The request carrying the link answer was written: open the chat.
    case sent
    /// The link no longer works: nothing was sent; open the chat.
    case dead
    case rateLimited
    case requiresReauth
    /// A network or server failure: the page stays, Open chat comes back.
    case failed(message: String)
}

public enum InviteAddResult: Equatable {
    /// The inviter is a pending contact now; the page closes into the chat.
    case added(PendingContact)
    /// The inviter, removed earlier, is a contact again with the one-to-one
    /// that remained; the page closes into it (2026-10-24).
    case restored(groupIdHex: String)
    /// Used up or gone since the page opened.
    case dead
    case rateLimited
    case requiresReauth
    /// A network or server failure: the page stays, Add comes back.
    case failed(message: String)
}

/// An opened `/i/` link: what the page shows and what **Add** needs.
public struct OpenedInvite: Equatable {
    public let blobId: Data
    public let capabilityBytes: Data
    public let ownerAccountId: Data
    public let ownerSignaturePubkey: Data
    public let ownerX25519Pubkey: Data
    public let linkId: Data
    /// SECRET: the link answer's secret.
    public let linkSecret: Data
    /// SECRET: the link's reach grant, used for this one write.
    public let linkGrant: FfiReachGrant
    public let displayName: String?
    public let username: String?
    public let label: String
    public let bootstrapReferral: Bool

    public static func == (a: OpenedInvite, b: OpenedInvite) -> Bool {
        a.blobId == b.blobId && a.linkId == b.linkId
    }

    /// The name the link states: the display name, else `@username`, else
    /// `nil` (the catalog's "this person").
    public var statedName: String? {
        Self.statedName(displayName: displayName, username: username)
    }

    /// The one rule for the name a link states.
    public static func statedName(displayName: String?, username: String?) -> String? {
        if let name = displayName?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
            return name
        }
        if let username, !username.isEmpty { return "@\(username)" }
        return nil
    }
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

/// Accepting a contact request: the order and the failure rule, over injected
/// steps. Nothing is filed until the group stands, so a request whose accept
/// failed stays listed and Accept works again; there is never an accepted
/// contact without a chat. The ack follows only after the Welcome landed.
public enum ContactAccept {

    public enum Outcome: Equatable {
        /// The Welcome landed; the contact is filed with its chat.
        case accepted(groupIdHex: String)
        /// The group stands and the requester's Welcome is owed: the contact
        /// is filed with its chat, and the ack waits for the Welcome.
        case welcomeOwed(groupIdHex: String)
        /// Nothing was delivered and nothing stands: the request stays.
        case failed
        /// ADR-0098 — the directory answered that the person's account is
        /// gone: the request was removed and nothing was founded.
        case deletedAccount
    }

    /// No chat bridge is wired.
    public struct Unavailable: Error {}

    struct Ports {
        var found: @MainActor (_ request: IncomingContactRequest, _ accountId: Data) async throws -> AnswerFounding
        var file: @MainActor (_ messageId: Int64, _ groupIdHex: String) -> Void
        var ack: @MainActor (_ request: IncomingContactRequest) async throws -> Void
    }

    /// A fixed reason for a log line: never an identifier, never error text
    /// (a DS error names the mailbox).
    static func reason(of error: Error) -> String {
        if error is MailboxUnknown { return "no-mailbox" }
        if error is Unavailable { return "no-chat" }
        if RefusedMailboxWrite.isRefusal(error) { return "refused" }
        if case .wrapped(_, let inner)? = error as? ChatError { return reason(of: inner) }
        if case .wrapped(let inner)? = error as? ContactsError { return reason(of: inner) }
        return "failed"
    }

    @MainActor
    static func run(request: IncomingContactRequest, ports: Ports) async -> Outcome {
        guard let accountId = request.fromAccountId else {
            NSLog("contact accept failed: no-account-id")
            return .failed
        }
        let founding: AnswerFounding
        do {
            founding = try await ports.found(request, accountId)
        } catch {
            NSLog("contact accept failed: %@", reason(of: error))
            return .failed
        }
        ports.file(request.messageId, founding.groupIdHex)
        switch founding {
        case .welcomeOwed(let groupIdHex):
            return .welcomeOwed(groupIdHex: groupIdHex)
        case .delivered(let groupIdHex):
            do {
                try await ports.ack(request)
            } catch {
                // The Welcome landed: the chat stands and the requester sees
                // it. Only their reply key stays open.
                NSLog("contact accept: the ack was not delivered: %@", reason(of: error))
            }
            return .accepted(groupIdHex: groupIdHex)
        }
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

    /// This contact with its chat link replaced (`nil` clears it); the rest
    /// of the row stays.
    func withGroupIdHex(_ groupIdHex: String?) -> AcceptedContact {
        AcceptedContact(
            request: request, groupIdHex: groupIdHex, displayName: displayName,
            previousKeysB64: previousKeysB64)
    }

    /// The contact's stable account id (base64), when the request carried
    /// one. What the reset record is found by across the key change.
    public var accountIdB64: String? {
        request.fromAccountId.map { base64Encode($0) }
    }

    /// This contact re-keyed to `key` after an accepted reset: the rename,
    /// the account id, the chat link and the row id stay with the person, and
    /// the key it held is appended to the display aliases.
    /// Whether `key` is one of this row's keys of before (an alias kept when
    /// a sealed reset or a proven rotation re-keyed it), not its own key.
    func answersToAlias(_ key: Data) -> Bool {
        request.fromPubkey != key && (previousKeysB64?.contains(base64Encode(key)) ?? false)
    }

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
    /// The name the person was found under, `nil` when the request went out
    /// by username alone: the Sent row shows it.
    public let recipientName: String?
    /// The person's account key (base64) as found: the Sent row's photo, and
    /// one of the two things a search row is matched to this list by.
    public let recipientKeyB64: String?
    /// The key that signed their ContactAck (base64), once it came: what the
    /// Welcome of the one-to-one they found is matched to this row by. The ack
    /// makes no contact; joining that Welcome does (2026-11-04).
    public var ackedKeyB64: String?
    /// Alignment item D (UX 16.11) — the person started over and the request
    /// went to their new account: the row says Lakko could not confirm it is
    /// the same person. Optional so a row written before decodes.
    public var startedOver: Bool?

    public init(
        recipientUsername: String,
        recipientAccountIdB64: String,
        messageId: Int64,
        createdAt: String,
        intro: String = "",
        recipientName: String? = nil,
        recipientKeyB64: String? = nil,
        ackedKeyB64: String? = nil,
        startedOver: Bool? = nil
    ) {
        self.ackedKeyB64 = ackedKeyB64
        self.startedOver = startedOver
        self.recipientUsername = recipientUsername
        self.recipientAccountIdB64 = recipientAccountIdB64
        self.messageId = messageId
        self.createdAt = createdAt
        self.intro = intro
        self.recipientName = recipientName
        self.recipientKeyB64 = recipientKeyB64
    }
}

/// An invite link the owner has just generated this session.
///
/// When a new invite link stops working: a period from the moment it is
/// minted, or never (the wire `NEVER_EXPIRES` sentinel `0`).
public enum InviteExpiry: Equatable, Sendable {
    case fromNow(ms: Int64)
    case never

    /// The wire `expiry_ms` for a link minted at `nowMs`: UNIX millis, or `0`
    /// for ``never``.
    public func expiryMs(nowMs: Int64) -> Int64 {
        switch self {
        case .fromNow(let ms): return nowMs + ms
        case .never: return ContactsRepository.neverExpires
        }
    }
}

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
    ///
    /// The row is the one under the previous key; only when none is, the row
    /// naming the account. A row under a key an UNSEALED reset left behind
    /// (`startedOverKeysB64`, UX 16.11) is the old account and is never the
    /// one re-keyed by a later reset of the new one.
    public static func afterReset(
        accepted: [AcceptedContact],
        accountIdB64: String,
        previousKeyB64: String,
        newKey: Data,
        startedOverKeysB64: Set<String> = []
    ) -> [AcceptedContact] {
        guard let idx = accepted.firstIndex(where: { base64Encode($0.request.fromPubkey) == previousKeyB64 })
            ?? accepted.firstIndex(where: {
                $0.accountIdB64 == accountIdB64
                    && !startedOverKeysB64.contains(base64Encode($0.request.fromPubkey))
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

/// An invite link whose record could not be kept in the account's synced
/// state: it is revoked instead of handed out.
struct InviteLinkNotRecorded: Error {}


// MARK: - A request inside a shared group (`docs/ux/who-can-find-me.md` 6.4)

/// A group both people are current members of, with the time of its newest
/// message (0 when none is held).
public struct SharedGroup: Equatable {
    public let groupIdHex: String
    public let newestMessageMs: Int64

    public init(groupIdHex: String, newestMessageMs: Int64) {
        self.groupIdHex = groupIdHex
        self.newestMessageMs = newestMessageMs
    }
}

/// Which way a contact request to a known person goes (section 6.4): one
/// order, wherever a request is sent or a one-to-one is started.
public enum ContactRequestRoute: Equatable {
    /// A grant this device holds still reaches their inbox: today's path.
    case inbox
    /// Inside this group, which both are current members of.
    case group(groupIdHex: String)
    /// Nothing can be sent.
    case none

    /// The inbox first; else the shared group with the newest message (the
    /// lower id on a tie, so the choice does not depend on the order the
    /// groups were listed in); else none.
    public static func decide(reachesInbox: Bool, sharedGroups: [SharedGroup]) -> ContactRequestRoute {
        if reachesInbox { return .inbox }
        let best = sharedGroups.min { a, b in
            a.newestMessageMs != b.newestMessageMs
                ? a.newestMessageMs > b.newestMessageMs
                : a.groupIdHex < b.groupIdHex
        }
        return best.map { .group(groupIdHex: $0.groupIdHex) } ?? .none
    }
}

/// The receiver's own rules for a request that came inside a group.
public enum GroupContactRequestGate {
    /// Shown only when this account lets that group's members find it, or
    /// the sender is a contact already; otherwise dropped without a trace.
    public static func shows(allowsGroupMembers: Bool, senderIsContact: Bool) -> Bool {
        allowsGroupMembers || senderIsContact
    }

    /// The row id of a request that came inside a group: no mailbox id
    /// exists, so a negative id derived from the envelope (the same envelope
    /// read twice is one row, and a mailbox id, always positive, never meets
    /// it).
    public static func messageId(envelope: Data) -> Int64 {
        var hasher = SHA256()
        hasher.update(data: Data("lakko/ios/group-contact-request-row/v1".utf8))
        hasher.update(data: envelope)
        let digest = Array(hasher.finalize())
        var value: UInt64 = 0
        for byte in digest.prefix(8) { value = (value << 8) | UInt64(byte) }
        return -Int64(value & 0x3FFF_FFFF_FFFF_FFFF) - 1
    }
}
