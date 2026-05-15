// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// In-memory state holder for the user's contacts surface.
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
/// **Phase 1.14e3 scope**: send + poll + accept (local-state-only)
/// + decline + clear. Invite-link generate / revoke / consume land
/// in 1.14e7 (owner) and 1.14e8 (joiner) respectively. Welcome-to-
/// ChatRepository forwarding lands in 1.14f when the chat surface
/// arrives.
///
/// **No persistence yet** — pending requests + sent log live in
/// memory only. They drop on process death; on next launch the
/// receiver re-polls the stranger inbox from cursor `0` and rebuilds
/// the list.
@MainActor
public final class ContactsRepository: ObservableObject {

    // MARK: - Published state

    @Published public private(set) var state: ContactsState = ContactsState()

    // MARK: - Wiring

    private let authRepository: AuthRepository
    private let clientFactory: () -> ContactsClient
    private let discoveryClientFactory: () -> DiscoveryClient

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
        discoveryClientFactory: @escaping () -> DiscoveryClient = { DiscoveryClient() }
    ) {
        self.authRepository = authRepository
        self.clientFactory = clientFactory
        self.discoveryClientFactory = discoveryClientFactory
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
        // Build the sender's Identity from the OPAQUE export key.
        // The build is sub-millisecond (HKDF + Ed25519 + X25519);
        // a fresh handle per operation keeps the lifetime story
        // simple — Swift ARC drops the Rust-side handle on scope
        // exit.
        let identity = try Identity.fromSeed(seed: secrets.exportKey)
        let result = try await client.sendContactRequest(
            senderSigningSeed: identity.ed25519Seed(),
            senderAccountId: secrets.accountId,
            senderSignaturePubkey: identity.signaturePublicKey(),
            recipientUsername: recipientUsername,
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

    // MARK: - Poll

    /// Poll the stranger inbox for new ContactRequest / Welcome
    /// envelopes. Updates ``ContactsState/pending`` with any new
    /// requests + bumps the cursor.
    ///
    /// **Welcome forwarding** is a no-op in 1.14e3 — the chat
    /// surface (and the `ChatRepository` that owns
    /// `joinGroupFromWelcome`) lands in 1.14f. Welcomes drop on
    /// the floor; the cursor still advances so we don't re-process
    /// them. 1.14f wires the forwarding hook through the same
    /// initializer pattern the Kotlin side uses.
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
        let isManualRefresh = waitSecs == 0
        if isManualRefresh {
            state = state.with(refreshing: true, lastError: nil)
        }
        do {
            let client = clientFactory()
            let identity = try Identity.fromSeed(seed: secrets.exportKey)
            let pollResult = try await client.pollStrangerInbox(
                mySignaturePubkey: identity.signaturePublicKey(),
                myX25519Secret: identity.x25519SecretKey(),
                after: state.cursor,
                waitSecs: waitSecs
            )
            var newPending: [IncomingContactRequest] = []
            for event in pollResult.events {
                switch event {
                case .contactRequest(let req):
                    newPending.append(req)
                case .welcome(_, let welcomeBytes, _):
                    // 1.14f4 wires `ChatRepository.joinGroupFromWelcome`
                    // here. Best-effort: a single bad Welcome
                    // doesn't block the inbox poll from advancing
                    // the cursor for the rest of the events.
                    // Cursor has already advanced so we won't
                    // re-process this row on the next poll.
                    if let chatRepository = chatRepository {
                        do {
                            _ = try await chatRepository
                                .joinGroupFromWelcome(welcomeBytes: welcomeBytes)
                        } catch {
                            // best-effort
                        }
                    }
                    continue
                case .contactAck:
                    // 1.9b1: the recipient of one of our outgoing
                    // ContactRequests pressed Accept. The signature
                    // is already verified inside the UniFFI layer;
                    // the ack carries the accepter's pubkey +
                    // X25519 pubkey + account_id. The cursor has
                    // already advanced so we won't re-process the
                    // same row; UI-side "flip sent → accepted"
                    // mutation against the matching row in
                    // `sentLog` lands as a follow-up — the network
                    // primitive is in place and the Welcome that
                    // typically follows an ack still drives the
                    // canonical accepted-state insertion via the
                    // chatRepository hand-off above.
                    continue
                }
            }
            state = state.with(
                pending: state.pending + newPending,
                cursor: pollResult.cursor,
                refreshing: false,
                lastError: nil
            )
        } catch {
            // Background long-polls surface failures on the
            // `lastError` slot too — the UI renders a small inline
            // hint without flipping `refreshing`. The caller (a
            // polling loop) will retry on the next iteration.
            state = state.with(
                refreshing: false,
                lastError: (error as? LocalizedError)?.errorDescription
                    ?? String(describing: error)
            )
        }
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
        let identity = try Identity.fromSeed(seed: secrets.exportKey)
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
        let identity = try Identity.fromSeed(seed: secrets.exportKey)
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

    /// Clear the in-memory state. Called from the AuthRepository
    /// hook when the session ends — pending requests + sent log
    /// do not persist across logouts.
    public func clear() {
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
        expiryMillisFromNow: Int64 = Self.defaultInviteExpiryMs,
        maxUses: UInt32 = Self.singleUse,
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

        let identity = try Identity.fromSeed(seed: secrets.exportKey)
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
        let identity = try Identity.fromSeed(seed: secrets.exportKey)
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

    /// Default invite expiry — 7 days. Mirrors the Android +
    /// Web sides.
    public static let defaultInviteExpiryMs: Int64 = 7 * 24 * 60 * 60 * 1000

    /// Server-side sentinel for `max_uses = single-use`.
    public static let singleUse: UInt32 = 0

    /// URL prefix Lakko shares its invite links under. Stays in
    /// sync with the web `composePointerInviteUrl` host + path so a
    /// link generated on web works when consumed on iOS and vice
    /// versa.
    public static let inviteUrlPrefix: String = "https://lakko.me/i/"

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
    ///   1. base64url-decode the slug to recover the 32-byte
    ///      `blob_id`.
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
    ///   7. `ChatRepository.startGroupWith(...)` — lands in 1.14f;
    ///      `groupIdHex` is always `nil` in 1.14e8.
    public func acceptInviteLink(slug: String) async -> AcceptInviteResult {
        guard let secrets = authRepository.identitySecrets else {
            return .requiresReauth
        }
        guard let blobId = base64UrlDecodeNoPad(slug), blobId.count == 32 else {
            return .malformedSlug
        }

        let discovery = authenticatedDiscoveryClient()
        let capabilityBytes: Data?
        do {
            capabilityBytes = try await discovery.fetchInvite(blobId: blobId)
        } catch {
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
            let identity = try Identity.fromSeed(seed: secrets.exportKey)
            sendResult = try await client.sendContactRequestToBundle(
                senderSigningSeed: identity.ed25519Seed(),
                senderAccountId: secrets.accountId,
                senderSignaturePubkey: identity.signaturePublicKey(),
                recipientAccountId: inviterAccountId,
                recipientSignaturePubkey: inviterSigPubkey,
                recipientX25519Pubkey: inviterX25519Pubkey,
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
    case accepted(recipientAccountId: Data, groupIdHex: String?)
    case alreadyAccepted
    case expired
    case exhausted
    case notFound
    case malformedSlug
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
public struct AcceptedContact: Equatable {
    public let request: IncomingContactRequest
    /// Populated when 1.14f's Accept flow has just driven
    /// `ChatRepository.startGroupWith` for this peer. Until 1.14f
    /// lands this is always `nil`; the contacts UI surfaces a
    /// "Open chat (coming in 1.14f)" placeholder hint then.
    public let groupIdHex: String?

    public init(request: IncomingContactRequest, groupIdHex: String? = nil) {
        self.request = request
        self.groupIdHex = groupIdHex
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
/// Empty placeholder in 1.14e3 — the generate / revoke methods
/// land in 1.14e7. We commit the struct shape here so
/// ``ContactsState/outstandingInvites`` doesn't need a follow-up
/// type migration when the methods arrive.
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
