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
/// (process-wide); the account-inbox cursor and the pending requests live
/// in ``ContactsInboxStore``, one row per account, loaded when the
/// repository binds to a session's account and written back on every
/// change. Only the sent log and the session's generated invites stay
/// in memory (there is no server-side mirror to rebuild them from).
@MainActor
public final class ContactsRepository: ObservableObject {

    // MARK: - Published state

    @Published public private(set) var state: ContactsState = ContactsState() {
        didSet {
            // Persist the accepted list so it survives a relaunch
            // (ADR-0021 group-call name resolution needs it).
            if state.accepted != oldValue.accepted {
                AcceptedContactsStore.save(state.accepted)
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
        }
    }

    // MARK: - Wiring

    private let authRepository: AuthRepository
    private let clientFactory: () -> ContactsClient
    private let discoveryClientFactory: () -> DiscoveryClient
    private let inboxStore: ContactsInboxStore
    /// The account whose persisted inbox `state.pending` / `state.cursor`
    /// currently mirror; `nil` while logged out. Set BEFORE the state it
    /// governs is loaded so the `didSet` above never files one account's
    /// rows under another.
    private var boundAccountIdB64: String?
    private var cancellables: Set<AnyCancellable> = []

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

    public init(
        authRepository: AuthRepository,
        clientFactory: @escaping () -> ContactsClient = { ContactsClient() },
        discoveryClientFactory: @escaping () -> DiscoveryClient = { DiscoveryClient() },
        inboxStore: ContactsInboxStore = ContactsInboxStore()
    ) {
        self.authRepository = authRepository
        self.clientFactory = clientFactory
        self.discoveryClientFactory = discoveryClientFactory
        self.inboxStore = inboxStore
        // Rehydrate the persisted accepted list (ADR-0021). If the
        // `didSet` fires for this set it harmlessly re-saves the same
        // list, so the round-trip is idempotent.
        let persisted = AcceptedContactsStore.load()
        if !persisted.isEmpty {
            state = state.with(accepted: persisted)
        }
        // ADR-0050 section 2.9 — contacts carried by a pairing state bundle.
        stateBundleObserver = NotificationCenter.default.addObserver(
            forName: .lakkoStateBundleApplied, object: nil, queue: .main
        ) { [weak self] note in
            guard let contacts = note.object as? [AcceptedContact], !contacts.isEmpty else { return }
            Task { @MainActor in self?.mergeBundledContacts(contacts) }
        }
        // Follow the session: bind the persisted inbox of whichever account is
        // live, unbind on logout / forgetDevice. `@Published` on a
        // `@MainActor` class fires on the main actor, and this class is main
        // actor too, so the hop is an assertion rather than a dispatch.
        // `refreshInbox` binds on its own as well, so a poll that starts
        // before this emission is delivered is not left on the wrong cursor.
        authRepository.$identitySecrets
            .map { $0.map { base64Encode($0.accountId) } }
            .removeDuplicates()
            .sink { [weak self] accountIdB64 in
                MainActor.assumeIsolated { self?.bind(accountIdB64: accountIdB64) }
            }
            .store(in: &cancellables)
    }

    private var stateBundleObserver: NSObjectProtocol?

    // MARK: - Account binding

    /// Point `state.pending` / `state.cursor` at one account's persisted
    /// inbox (or at nothing, for `nil`). Idempotent for the already-bound
    /// account. The bound marker is switched FIRST so the `didSet` that
    /// follows never writes the outgoing account's rows under the incoming
    /// one, and never writes the incoming account's freshly loaded rows
    /// back out as an empty inbox for the outgoing one.
    private func bind(accountIdB64: String?) {
        guard boundAccountIdB64 != accountIdB64 else { return }
        boundAccountIdB64 = nil
        guard let accountIdB64 else {
            // Logged out: the in-memory inbox belongs to nobody now. The
            // persisted row stays (logout keeps per-account state for a
            // cheap re-login; `forgetDevice` is what wipes it).
            state = state.with(pending: [], cursor: 0)
            return
        }
        let persisted = inboxStore.load(accountIdB64: accountIdB64)
        boundAccountIdB64 = accountIdB64
        state = state.with(pending: persisted.pending, cursor: persisted.cursor)
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
        let result = try await client.sendContactRequest(
            senderSigningSeed: identity.ed25519Seed(),
            senderAccountId: secrets.accountId,
            senderSignaturePubkey: identity.signaturePublicKey(),
            recipientUsername: recipientUsername,
            profile: ProfileDirectory.shared.ownProfileForRequest(),
            introMessage: Data(introMessage.utf8)
        )
        let sent = SentContactRequest(
            recipientUsername: result.recipientUsername,
            recipientAccountIdB64: base64Encode(result.recipientAccountId),
            messageId: result.messageId,
            createdAt: result.createdAt
        )
        state = state.with(sent: state.sent + [sent])
        return result
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

    /// The catalog key (ADR-0058) shown in place of a transport error when
    /// the inbox poll fails; the screen renders it in the current language.
    static let inboxFetchFailureMessage = "contacts.mailbox.poll_failed"

    /// `true` when `error` is a cancellation of the poll itself rather than
    /// a failure of it: Swift's `CancellationError`, a `URLError.cancelled`,
    /// or a `DsClientError.network` wrapping one.
    static func isCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        if let url = error as? URLError, url.code == .cancelled { return true }
        if case .network(_, let underlying) = error as? DsClientError {
            return isCancellation(underlying)
        }
        return false
    }

    /// The catalog key (ADR-0058) for a Welcome this device could not join.
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
        var welcomeJoinFailure: String?
        // ADR-0050 — this DEVICE's mailbox (Welcomes sealed to this device's
        // KeyPackages), long-polled alongside the account mailbox.
        async let deviceWelcomes = pollDeviceMailboxWelcomes(
            accountIdB64: base64Encode(secrets.accountId), waitSecs: waitSecs)
        do {
            let client = clientFactory()
            let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
            let pollResult = try await client.pollStrangerInbox(
                mySignaturePubkey: identity.signaturePublicKey(),
                myX25519Secret: identity.x25519SecretKey(),
                after: state.cursor,
                waitSecs: waitSecs,
                // ADR-0053 — the account mailbox is named by the account key.
                mailboxSigner: dsMailboxSigner(identity: identity)
            )
            var newPending: [IncomingContactRequest] = []
            for event in pollResult.events {
                switch event {
                case .contactRequest(let req):
                    newPending.append(req)
                    // ADR-0048 §3 — a request's profile is bound to its
                    // `fromPubkey`, which the request signature authenticated.
                    // Kept only when strictly newer than what is held, so a
                    // replayed old request never rolls a name back.
                    if let profile = req.profile {
                        ProfileDirectory.shared.acceptReceived(
                            profile, signatureKeyB64: base64Encode(req.fromPubkey))
                    }
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
                    // ADR-0053 — a failure HERE is logged and never surfaced
                    // (the web client's rule). Since ADR-0050 every chat Welcome
                    // is sealed to a DEVICE leaf and lands in the device
                    // mailbox, whose failures still surface below. What reaches
                    // the ACCOUNT mailbox is a Welcome sealed to the account
                    // identity — a channel co-publisher Welcome, which
                    // `CoPublisher.pollForCoPublishWelcome` joins itself — and
                    // the chat identity cannot open it, so reporting it read as
                    // a failed chat join that never was one.
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
                    let remainingSent = state.sent.filter {
                        $0.recipientAccountIdB64 != accepterAccountIdB64
                    }
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
            for welcomeBytes in await deviceWelcomes {
                guard let chatRepository else { break }
                do {
                    _ = try await chatRepository.joinGroupFromWelcome(welcomeBytes: welcomeBytes)
                } catch {
                    NSLog("contacts device mailbox: joining a chat from a Welcome failed: \(error)")
                    welcomeJoinFailure = Self.welcomeJoinFailureMessage
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
            let merged = ContactsInboxMerge.merge(
                fetched: newPending, pending: state.pending, accepted: state.accepted)
            #if DEBUG
            for (request, reason) in merged.dropped {
                NSLog(
                    "contacts inbox: dropped fetched request messageId=\(request.messageId) reason=\(reason)"
                )
            }
            #endif
            state = state.with(
                pending: state.pending + merged.kept,
                cursor: ContactsInboxMerge.advancedCursor(
                    current: state.cursor, polled: pollResult.cursor),
                refreshing: false,
                lastError: welcomeJoinFailure ?? carriedJoinFailure
            )
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

    /// ADR-0050 — poll this DEVICE's mailbox (the pseudonym of the device
    /// leaf key) and return the Welcome bodies found there, oldest first. Only
    /// Welcomes are read from a device mailbox; the cursor advances past
    /// everything and is persisted per account, since a Welcome consumed once
    /// cannot be opened again. Any failure yields no Welcomes (the next poll
    /// retries from the same cursor).
    private func pollDeviceMailboxWelcomes(accountIdB64: String, waitSecs: Int) async -> [Data] {
        guard let deviceKey = chatRepository?.ownDeviceLeafKey,
              // ADR-0053 — the device mailbox is named by the device leaf key.
              let deviceSigner = chatRepository?.ownDeviceMailboxSigner else { return [] }
        let cursorKey = "me.lakko.contacts.device-mailbox-cursor.v1." + accountIdB64
        let after = Int64(UserDefaults.standard.integer(forKey: cursorKey))
        do {
            let result = try await clientFactory().pollStrangerInbox(
                mySignaturePubkey: deviceKey,
                // No contact request is addressed to a device mailbox, so no
                // real X25519 secret is needed; a request found here fails to
                // open and is dropped.
                myX25519Secret: Data(count: 32),
                after: after,
                waitSecs: waitSecs,
                mailboxSigner: deviceSigner
            )
            if result.cursor > after {
                UserDefaults.standard.set(Int(result.cursor), forKey: cursorKey)
            }
            return result.events.compactMap { event in
                if case .welcome(_, let welcomeBytes, _) = event { return welcomeBytes }
                return nil
            }
        } catch {
            return []
        }
    }

    /// ADR-0050 section 2.9 — merge the contacts carried by a pairing state
    /// bundle: contacts not already present (by signature key) are appended.
    /// Returns how many were added.
    @discardableResult
    public func mergeBundledContacts(_ contacts: [AcceptedContact]) -> Int {
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
        let remaining = state.accepted.filter { $0.request.messageId != messageId }
        guard remaining.count != state.accepted.count else { return }
        state = state.with(accepted: remaining)
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
        return try await client.sendContactAck(
            accepterSigningSeed: identity.ed25519Seed(),
            accepterAccountId: secrets.accountId,
            accepterX25519Pubkey: identity.x25519PublicKey(),
            originalRequest: request
        )
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
        let capabilityBytes: Data
        do {
            capabilityBytes = try composeInviteCapability(
                ownerSigningSeed: identity.ed25519Seed(),
                ownerAccountId: secrets.accountId,
                ownerX25519Pubkey: identity.x25519PublicKey(),
                expiryMs: UInt64(expiryMs),
                maxUses: maxUses,
                bootstrapReferral: bootstrapReferral,
                label: label
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        let discovery = authenticatedDiscoveryClient()
        let blobId: Data
        do {
            blobId = try await discovery.uploadInvite(
                capabilityBytes: capabilityBytes
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        let blobIdSlug = base64UrlEncodeNoPad(blobId)
        let url = "\(Self.inviteUrlPrefix)\(blobIdSlug)"
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
        let result: RevokeInviteResult
        do {
            result = try await discovery.revokeInvite(
                blobId: blobId,
                signingSeed: identity.ed25519Seed()
            )
        } catch {
            throw ContactsError.wrap(error)
        }
        if result == .revoked {
            state = state.with(
                outstandingInvites: state.outstandingInvites
                    .filter { $0.blobId != blobId }
            )
        }
        return result
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
    public func acceptInviteLink(slug: String) async -> AcceptInviteResult {
        guard let secrets = authRepository.identitySecrets else {
            return .requiresReauth
        }
        guard let blobId = Self.parseInviteSlug(slug) else {
            return .malformedSlug
        }

        let discovery = authenticatedDiscoveryClient()
        let capabilityBytes: Data?
        do {
            capabilityBytes = try await discovery.fetchInvite(blobId: blobId)
        } catch {
            if DiscoveryClientError.isRateLimited(error) { return .rateLimited }
            return .failed(message: (error as? LocalizedError)?.errorDescription
                ?? "fetch failed")
        }
        guard let capabilityBytes = capabilityBytes else {
            return .notFound
        }

        let capability: InviteCapability
        do {
            capability = try InviteCapability.decodeAndVerify(wire: capabilityBytes)
        } catch {
            // Signature / wire-format failure → treat as
            // not-found from the user's perspective.
            return .notFound
        }

        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        if !capability.isValidNow(nowMs: nowMs) {
            return .expired
        }

        // Snapshot bundle fields BEFORE the consume call.
        let inviterAccountId = capability.ownerAccountId()
        let inviterSigPubkey = capability.ownerSignaturePubkey()
        let inviterX25519Pubkey = capability.ownerX25519Pubkey()

        // Consume — atomic counter bump + per-joiner dedup.
        let consumeResult: ConsumeInviteResult
        do {
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
            let client = clientFactory()
            let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
            sendResult = try await client.sendContactRequestToBundle(
                senderSigningSeed: identity.ed25519Seed(),
                senderAccountId: secrets.accountId,
                senderSignaturePubkey: identity.signaturePublicKey(),
                recipientAccountId: inviterAccountId,
                recipientSignaturePubkey: inviterSigPubkey,
                recipientX25519Pubkey: inviterX25519Pubkey,
                profile: ProfileDirectory.shared.ownProfileForRequest(),
                introMessage: Data()
            )
        } catch {
            return .failed(
                message: (error as? LocalizedError)?.errorDescription
                    ?? "ContactRequest send failed"
            )
        }

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
                    recipientSignaturePubkey: inviterSigPubkey
                )
            } catch {
                groupIdHex = nil
            }
        } else {
            groupIdHex = nil
        }

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
    case expired
    case exhausted
    case notFound
    case malformedSlug
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
    /// The catalog key (ADR-0058) of the last receive failure, rendered by
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

    public init(
        request: IncomingContactRequest,
        groupIdHex: String? = nil,
        displayName: String? = nil
    ) {
        self.request = request
        self.groupIdHex = groupIdHex
        self.displayName = displayName
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
public struct SentContactRequest: Equatable {
    public let recipientUsername: String
    public let recipientAccountIdB64: String
    public let messageId: Int64
    public let createdAt: String

    public init(
        recipientUsername: String,
        recipientAccountIdB64: String,
        messageId: Int64,
        createdAt: String
    ) {
        self.recipientUsername = recipientUsername
        self.recipientAccountIdB64 = recipientAccountIdB64
        self.messageId = messageId
        self.createdAt = createdAt
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
