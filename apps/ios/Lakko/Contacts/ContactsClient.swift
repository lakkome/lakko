// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Orchestrates the send / poll sides of the
/// `lakko-contacts::ContactRequest` flow against the live
/// `lakko-svc-discovery` + `lakko-svc-ds` services.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/client.ts` (1.17e5)
///   * `apps/android/.../contacts/ContactsClient.kt` (1.15e5)
///
/// Three operations:
///
///   * ``sendContactRequest(senderSigningSeed:senderAccountId:senderSignaturePubkey:recipientUsername:introMessage:)``
///     — resolve `@username` → `account_id` via
///     ``DiscoveryClient/lookup(username:)`` → fetch identity
///     bundle → derive recipient mailbox pseudonym from their
///     Ed25519 pubkey → compose envelope via UniFFI
///     `composeContactRequest` → publish to recipient's mailbox via
///     ``DsClient/publishMailbox(pseudonym:payload:)``, wrapped
///     with ``EnvelopeKind/contactRequest``.
///
///   * ``sendContactRequestToBundle(senderSigningSeed:senderAccountId:senderSignaturePubkey:recipientAccountId:recipientSignaturePubkey:recipientX25519Pubkey:introMessage:)``
///     — same compose+publish path but skipping the lookup +
///     identityFetch round-trips when the caller already has the
///     recipient's bundle in hand (the invite-link flow in 1.14e8).
///
///   * ``pollStrangerInbox(mySignaturePubkey:myX25519Secret:after:waitSecs:)``
///     — fetch the user's own
///     `IdentityPseudonym.fromIdentity`-derived mailbox via
///     ``DsClient/fetchMailbox(pseudonym:after:limit:waitSecs:)``,
///     parse each envelope's kind byte, decode + verify each
///     ContactRequest, surface a mixed list of
///     ``IncomingMailboxEvent`` entries + a fresh cursor. Welcome
///     envelopes pass through as
///     ``IncomingMailboxEvent/welcome(messageId:welcomeBytes:createdAt:)``
///     for the chat surface (1.14f) to consume.
public final class ContactsClient {

    private let discovery: DiscoveryClient
    private let ds: DsClient

    public init(
        discovery: DiscoveryClient = DiscoveryClient(),
        ds: DsClient = DsClient()
    ) {
        self.discovery = discovery
        self.ds = ds
    }

    // MARK: - Send

    /// Send a `ContactRequest` envelope to `@recipientUsername`,
    /// sealed under the recipient's published X25519 key.
    public func sendContactRequest(
        senderSigningSeed: Data,
        senderAccountId: Data,
        senderSignaturePubkey: Data,
        recipientUsername: String,
        introMessage: Data
    ) async throws -> SendResult {
        let lookup: LookupResult
        do {
            lookup = try await discovery.lookup(username: recipientUsername)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let recipientAccountId = lookup.account else {
            throw ContactsError.usernameUnknown(lookup.normalisedUsername)
        }

        let bundle: IdentityFetchResponse?
        do {
            bundle = try await discovery.identityFetch(accountId: recipientAccountId)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let bundle = bundle else {
            throw ContactsError.noIdentityBundle(recipientAccountId)
        }
        guard let recipientSignaturePub = base64Decode(bundle.signaturePubkey),
              let recipientX25519Pub = base64Decode(bundle.x25519Pubkey) else {
            throw ContactsError.wrap(
                ContactsError.simple("malformed identity bundle on the wire")
            )
        }

        let published = try await composeAndPublishContactRequest(
            senderSigningSeed: senderSigningSeed,
            senderAccountId: senderAccountId,
            senderSignaturePubkey: senderSignaturePubkey,
            recipientSignaturePubkey: recipientSignaturePub,
            recipientX25519Pubkey: recipientX25519Pub,
            introMessage: introMessage
        )

        return SendResult(
            recipientUsername: lookup.normalisedUsername,
            recipientAccountId: recipientAccountId,
            messageId: published.messageId,
            createdAt: published.createdAt
        )
    }

    /// Send a `ContactRequest` directly against a known recipient
    /// bundle — **without** going through the discovery
    /// `lookup → identityFetch` round-trip.
    ///
    /// The invite-link flow (1.14e8) is the canonical caller: the
    /// `InviteCapability` already carries `ownerAccountId`,
    /// `ownerSignaturePubkey`, and `ownerX25519Pubkey`, so the
    /// joiner has everything needed to seal an envelope without
    /// touching `services/discovery`.
    public func sendContactRequestToBundle(
        senderSigningSeed: Data,
        senderAccountId: Data,
        senderSignaturePubkey: Data,
        recipientAccountId: Data,
        recipientSignaturePubkey: Data,
        recipientX25519Pubkey: Data,
        introMessage: Data
    ) async throws -> SendResult {
        let published = try await composeAndPublishContactRequest(
            senderSigningSeed: senderSigningSeed,
            senderAccountId: senderAccountId,
            senderSignaturePubkey: senderSignaturePubkey,
            recipientSignaturePubkey: recipientSignaturePubkey,
            recipientX25519Pubkey: recipientX25519Pubkey,
            introMessage: introMessage
        )
        return SendResult(
            recipientUsername: "",
            recipientAccountId: recipientAccountId,
            messageId: published.messageId,
            createdAt: published.createdAt
        )
    }

    /// Shared compose-and-publish path used by both send surfaces.
    /// Mirror of the Kotlin private method — pulled out so the two
    /// public flows don't drift on subtle details (mailbox-pseudonym
    /// derivation, envelope-kind wrapping, sender-pseudonym choice).
    private func composeAndPublishContactRequest(
        senderSigningSeed: Data,
        senderAccountId: Data,
        senderSignaturePubkey: Data,
        recipientSignaturePubkey: Data,
        recipientX25519Pubkey: Data,
        introMessage: Data
    ) async throws -> PublishedRequest {
        let recipientPseudonym: Data
        let senderPseudonym: Data
        do {
            recipientPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: recipientSignaturePubkey).asBytes()
            senderPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: senderSignaturePubkey).asBytes()
        } catch {
            throw ContactsError.wrap(error)
        }

        let validAfterMs = UInt64(Date().timeIntervalSince1970 * 1000)

        let wireBytes: Data
        do {
            wireBytes = try composeContactRequest(
                senderSigningSeed: senderSigningSeed,
                senderAccountId: senderAccountId,
                recipientX25519Pub: recipientX25519Pubkey,
                fromPseudonym: senderPseudonym,
                validAfterMs: validAfterMs,
                introMessage: introMessage
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        let envelope = wrapEnvelope(kind: .contactRequest, body: wireBytes)
        let publish: PublishResult
        do {
            publish = try await ds.publishMailbox(
                pseudonym: recipientPseudonym,
                payload: envelope
            )
        } catch {
            throw ContactsError.wrap(error)
        }
        return PublishedRequest(messageId: publish.id, createdAt: publish.createdAt)
    }

    // MARK: - ContactAck

    /// Send a `ContactAck` envelope back to the original sender's
    /// stranger inbox (Phase 1.9b1).
    ///
    /// Called from the recipient's "Accept" handler after they
    /// have decided to accept an incoming ``IncomingContactRequest``.
    /// The ack is independent of MLS group creation — splitting
    /// the two surfaces lets the original sender's UI flip from
    /// "pending" to "accepted" without waiting on the joiner's
    /// `startGroupWith` to complete.
    ///
    /// **Routing:** the ack body carries
    /// `ackToPseudonym = original.fromPseudonym`. The publish
    /// target is the sender's stranger-inbox mailbox — derived
    /// from `original.fromPubkey` via
    /// `IdentityPseudonym.fromIdentity`.
    public func sendContactAck(
        accepterSigningSeed: Data,
        accepterAccountId: Data,
        accepterX25519Pubkey: Data,
        originalRequest: IncomingContactRequest
    ) async throws -> PublishedAck {
        let senderMailbox: Data
        do {
            senderMailbox = try IdentityPseudonym
                .fromIdentity(pubkey: originalRequest.fromPubkey).asBytes()
        } catch {
            throw ContactsError.wrap(error)
        }

        let acceptedAtMs = UInt64(Date().timeIntervalSince1970 * 1000)

        let wireBytes: Data
        do {
            wireBytes = try composeContactAck(
                accepterSigningSeed: accepterSigningSeed,
                accepterAccountId: accepterAccountId,
                accepterX25519Pub: accepterX25519Pubkey,
                ackToPseudonym: originalRequest.fromPseudonym,
                acceptedAtMs: acceptedAtMs
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        let envelope = wrapEnvelope(kind: .contactAck, body: wireBytes)
        let publish: PublishResult
        do {
            publish = try await ds.publishMailbox(
                pseudonym: senderMailbox,
                payload: envelope
            )
        } catch {
            throw ContactsError.wrap(error)
        }
        return PublishedAck(messageId: publish.id, createdAt: publish.createdAt)
    }

    // MARK: - Poll

    /// Poll the user's stranger-inbox mailbox for new
    /// `ContactRequest`s + Welcome blobs.
    ///
    /// Malformed entries on the wire are **dropped silently** —
    /// the verifier rejects bad signatures, decoding errors are
    /// skipped (the cursor still advances, so we won't re-process
    /// the same row).
    public func pollStrangerInbox(
        mySignaturePubkey: Data,
        myX25519Secret: Data,
        after: Int64 = 0,
        waitSecs: Int = 0
    ) async throws -> PollResult {
        let myPseudonym: Data
        do {
            myPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: mySignaturePubkey).asBytes()
        } catch {
            throw ContactsError.wrap(error)
        }

        let raw: FetchResult
        do {
            raw = try await ds.fetchMailbox(
                pseudonym: myPseudonym,
                after: after,
                waitSecs: waitSecs
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        var events: [IncomingMailboxEvent] = []
        var cursor = after
        for message in raw.messages {
            cursor = message.id
            switch parseEnvelope(message.payload) {
            case .unknown:
                // Future / unknown kind byte — cursor advances so
                // we don't re-process. No logger plumbed yet.
                continue
            case .welcome(let body):
                events.append(
                    .welcome(
                        messageId: message.id,
                        welcomeBytes: body,
                        createdAt: message.createdAt
                    )
                )
            case .contactAck(let body):
                // 1.9b1: signature is verified inside the UniFFI
                // surface; bad-signature / malformed-wire entries
                // drop silently and the cursor still advances.
                let parsed: ContactAck
                do {
                    parsed = try ContactAck.decodeAndVerify(wire: body)
                } catch {
                    continue
                }
                let ack = IncomingContactAck(
                    messageId: message.id,
                    accepterPubkey: parsed.accepterPubkey(),
                    accepterAccountId: parsed.accepterAccountId(),
                    accepterX25519Pubkey: parsed.accepterX25519Pubkey(),
                    ackToPseudonym: parsed.ackToPseudonym(),
                    acceptedAtMs: parsed.acceptedAtMs(),
                    createdAt: message.createdAt
                )
                events.append(.contactAck(ack))
            case .contactRequest(let body):
                let parsed: ContactRequest
                do {
                    parsed = try ContactRequest.decodeAndVerify(wire: body)
                } catch {
                    // Bad signature, malformed wire bytes, or some
                    // other verifier failure. Drop and move on —
                    // the cursor advances so we won't re-process
                    // the same row.
                    continue
                }

                let intro: String?
                do {
                    let plaintext = try parsed.openEnvelope(
                        recipientX25519Priv: myX25519Secret
                    )
                    intro = String(data: plaintext, encoding: .utf8) ?? ""
                } catch {
                    intro = nil
                }

                let req = IncomingContactRequest(
                    messageId: message.id,
                    fromPubkey: parsed.fromPubkey(),
                    fromAccountId: parsed.fromAccountId(),
                    fromPseudonym: parsed.fromPseudonym(),
                    validAfterMs: parsed.validAfterMs(),
                    intro: intro,
                    createdAt: message.createdAt
                )
                events.append(.contactRequest(req))
            }
        }
        return PollResult(events: events, cursor: cursor, hasMore: raw.hasMore)
    }
}

// MARK: - Result types

private struct PublishedRequest {
    let messageId: Int64
    let createdAt: String
}

/// Result of ``ContactsClient/sendContactRequest(...)`` and
/// ``ContactsClient/sendContactRequestToBundle(...)``.
public struct SendResult: Equatable {
    /// Normalised form of the username that was resolved, or `""`
    /// if the caller used `sendContactRequestToBundle` (no
    /// discovery round-trip happened).
    public let recipientUsername: String
    /// Raw 16-byte recipient account_id.
    public let recipientAccountId: Data
    /// DS-assigned monotonic id for the published envelope.
    public let messageId: Int64
    /// ISO-8601 timestamp from the DS.
    public let createdAt: String

    public init(
        recipientUsername: String,
        recipientAccountId: Data,
        messageId: Int64,
        createdAt: String
    ) {
        self.recipientUsername = recipientUsername
        self.recipientAccountId = recipientAccountId
        self.messageId = messageId
        self.createdAt = createdAt
    }
}

/// One verified incoming ContactRequest.
public struct IncomingContactRequest: Equatable {
    public let messageId: Int64
    /// Sender's Ed25519 identity public key (32 bytes). The
    /// signature on the request was verified against this.
    public let fromPubkey: Data
    /// Sender's account_id (16-byte UUIDv7). `nil` for legacy v1
    /// envelopes — the Accept-driven group creation path requires
    /// this and the UI surfaces a "legacy request" message when
    /// it's missing.
    public let fromAccountId: Data?
    /// Sender's per-recipient pseudonym (32 bytes).
    public let fromPseudonym: Data
    public let validAfterMs: UInt64
    /// UTF-8-decoded intro message, or `nil` when there was no
    /// intro / it didn't decrypt. Empty string is a valid payload.
    public let intro: String?
    public let createdAt: String

    public init(
        messageId: Int64,
        fromPubkey: Data,
        fromAccountId: Data?,
        fromPseudonym: Data,
        validAfterMs: UInt64,
        intro: String?,
        createdAt: String
    ) {
        self.messageId = messageId
        self.fromPubkey = fromPubkey
        self.fromAccountId = fromAccountId
        self.fromPseudonym = fromPseudonym
        self.validAfterMs = validAfterMs
        self.intro = intro
        self.createdAt = createdAt
    }
}

/// One verified incoming ``ContactAck`` (Phase 1.9b1).
///
/// The accept-reply that closes the `sendContactRequest`
/// handshake from the original sender's perspective. The
/// Ed25519 signature on the ack has already been verified
/// against ``accepterPubkey``; the caller still has to match
/// ``ackToPseudonym`` against the pending-sent table to pair
/// it with the original request the user fired off.
public struct IncomingContactAck: Equatable {
    public let messageId: Int64
    /// Accepter's Ed25519 identity public key (32 bytes). The
    /// signature on the ack was verified against this.
    public let accepterPubkey: Data
    /// Accepter's `account_id` (16-byte UUIDv7).
    public let accepterAccountId: Data
    /// Accepter's X25519 HPKE public key (32 bytes). Stored
    /// alongside the accepter's pubkey in the local address
    /// book so future replies can be sealed without a fresh
    /// discovery round-trip.
    public let accepterX25519Pubkey: Data
    /// The original request's `from_pseudonym` field — the
    /// receiver matches this against their own identity
    /// pseudonym (and the pending-sent table) to confirm the
    /// ack is for them.
    public let ackToPseudonym: Data
    /// When the accepter pressed Accept (ms since epoch).
    /// Sender-attested; not authoritative — the DS
    /// ``createdAt`` is the trust anchor.
    public let acceptedAtMs: UInt64
    public let createdAt: String

    public init(
        messageId: Int64,
        accepterPubkey: Data,
        accepterAccountId: Data,
        accepterX25519Pubkey: Data,
        ackToPseudonym: Data,
        acceptedAtMs: UInt64,
        createdAt: String
    ) {
        self.messageId = messageId
        self.accepterPubkey = accepterPubkey
        self.accepterAccountId = accepterAccountId
        self.accepterX25519Pubkey = accepterX25519Pubkey
        self.ackToPseudonym = ackToPseudonym
        self.acceptedAtMs = acceptedAtMs
        self.createdAt = createdAt
    }
}

/// Receipt returned by ``ContactsClient/sendContactAck(...)``.
public struct PublishedAck: Equatable {
    public let messageId: Int64
    public let createdAt: String

    public init(messageId: Int64, createdAt: String) {
        self.messageId = messageId
        self.createdAt = createdAt
    }
}

/// Discriminated mailbox event surfaced by
/// ``ContactsClient/pollStrangerInbox(...)``.
public enum IncomingMailboxEvent: Equatable {
    case contactRequest(IncomingContactRequest)
    case welcome(messageId: Int64, welcomeBytes: Data, createdAt: String)
    /// 1.9b1 — original recipient's accept-reply.
    case contactAck(IncomingContactAck)
}

/// Result of ``ContactsClient/pollStrangerInbox(...)``.
public struct PollResult: Equatable {
    public let events: [IncomingMailboxEvent]
    /// Fresh cursor for the next poll.
    public let cursor: Int64
    /// `true` if the DS reported more rows past the last returned
    /// id — caller should poll again with the new cursor.
    public let hasMore: Bool

    public init(events: [IncomingMailboxEvent], cursor: Int64, hasMore: Bool) {
        self.events = events
        self.cursor = cursor
        self.hasMore = hasMore
    }
}

// MARK: - Errors

/// Single error type carrying a discriminated cause variant so the
/// UI can branch on the kind without `as?`-walking the underlying
/// client error trees.
public enum ContactsError: Error {
    /// `@<normalisedUsername>` is not registered.
    case usernameUnknown(String)
    /// The recipient's identity bundle is missing from the
    /// discovery service. They've claimed the username but haven't
    /// uploaded a bundle — typically a stale account or a client
    /// mid-rollout.
    case noIdentityBundle(Data)
    /// Any other failure — wraps a ``DiscoveryClientError`` /
    /// ``DsClientError`` / UniFFI `FfiError`. The UI surfaces the
    /// wrapped message verbatim.
    case wrapped(Error)

    public static func wrap(_ error: Error) -> ContactsError {
        if let ce = error as? ContactsError { return ce }
        return .wrapped(error)
    }

    /// Helper that wraps a free-form string as a `wrapped`
    /// variant. Used for "malformed identity bundle on the wire"
    /// and similar non-Error string failures.
    public static func simple(_ message: String) -> Error {
        struct Simple: LocalizedError {
            let message: String
            var errorDescription: String? { message }
        }
        return Simple(message: message)
    }
}

extension ContactsError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .usernameUnknown(let u):
            return "username @\(u) is not registered"
        case .noIdentityBundle:
            return "no identity bundle for the given account id"
        case .wrapped(let cause):
            return (cause as? LocalizedError)?.errorDescription
                ?? String(describing: cause)
        }
    }
}
