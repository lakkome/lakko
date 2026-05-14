// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Application-message orchestrator on top of MLS groups.
///
/// Mirror of:
///
///   * `apps/web/src/chat/client.ts` (1.17f2)
///   * `apps/android/.../chat/ChatClient.kt` (1.15f2)
///
/// Two operations:
///
///   * ``sendMessage(identity:group:plaintext:)`` —
///     `Group.createApplicationMessage(identity:plaintext:)` →
///     POST to `/ds/group/{group_id_hex}/messages` via
///     ``DsClient/publishGroup(groupId:payload:)``. Returns the DS
///     message id (also the cursor value).
///
///   * ``pollMessages(identity:group:after:waitSecs:)`` —
///     ``DsClient/fetchGroup(groupId:after:limit:waitSecs:)`` for
///     the group's journal → for each row,
///     `group.processIncoming(identity:bytes:)` → dispatch to
///     ``IncomingChatEvent``. Application messages carry plaintext
///     + an `.application(_:)` discriminator; commits / proposals
///     advance MLS state but surface as `.system(...)` rows the UI
///     can render as a thin separator (or skip).
///
/// ## Self-message echo
///
/// `Group.createApplicationMessage` produces wire bytes that look
/// like a normal MLS application message — the DS does not
/// deduplicate, and a local poll WILL pick our own messages back
/// up. OpenMLS can't decrypt the sender's own application messages
/// at the same epoch, so it would surface as a `processIncoming`
/// failure: we'd see a "can't decrypt own message" exception,
/// swallow it, and the UI would never see the row.
///
/// Instead we **track the DS ids of our own publishes** during the
/// session and drop matches on receive. The set lives on the
/// ``ChatClient`` instance — swapping clients (e.g. logout) loses
/// the list; pending echoes after a swap surface as the
/// processIncoming-failed path above, which is harmless but
/// leaves a single noisy log entry per orphaned echo.
public final class ChatClient {

    private let ds: DsClient

    /// Track DS message ids we ourselves just published, so the
    /// next poll can drop the echo. Synchronised via an internal
    /// serial DispatchQueue because ``sendMessage`` and
    /// ``pollMessages`` may run on different `Task` actors;
    /// `Set<Int64>` is value-typed but needs an external lock for
    /// concurrent mutation.
    private var ownPublishedIds: Set<Int64> = []
    private let lockQueue = DispatchQueue(label: "me.lakko.chat.client.ownPublished")

    public init(ds: DsClient = DsClient()) {
        self.ds = ds
    }

    /// Encrypt `plaintext` for the current group epoch and post
    /// the wire bytes to the DS group journal.
    ///
    /// Caller keeps `group` alive — the `Group` handle carries the
    /// MLS ratchet state; dropping it would lose every subsequent
    /// message. 1.14f5 wires snapshot persistence so a restart can
    /// rehydrate the state from disk.
    public func sendMessage(
        identity: Identity,
        group: Group,
        plaintext: Data
    ) async throws -> SendMessageResult {
        guard !plaintext.isEmpty else {
            throw ChatError.argument("plaintext must be non-empty")
        }

        let wireBytes: Data
        do {
            wireBytes = try group.createApplicationMessage(
                identity: identity,
                plaintext: plaintext
            )
        } catch {
            throw ChatError.wrapped(
                "createApplicationMessage failed",
                underlying: error
            )
        }

        let groupId: Data
        do {
            groupId = try group.groupId()
        } catch {
            throw ChatError.wrapped(
                "group.groupId unavailable",
                underlying: error
            )
        }

        let publish: PublishResult
        do {
            publish = try await ds.publishGroup(
                groupId: groupId,
                payload: wireBytes
            )
        } catch {
            throw ChatError.wrapped("DS publish failed", underlying: error)
        }
        registerOwnPublishedId(publish.id)

        return SendMessageResult(
            messageId: publish.id,
            createdAt: publish.createdAt
        )
    }

    /// Register a DS message id as one we published — typically a
    /// commit returned by `ContactsClient.addMemberToGroup`'s
    /// broadcast step. The next ``pollMessages(identity:group:after:waitSecs:)``
    /// will see the id and drop it via the `ownPublishedIds`
    /// filter, same path ``sendMessage(identity:group:plaintext:)``
    /// uses for application messages.
    public func markOwnPublishedCommit(messageId: Int64) {
        registerOwnPublishedId(messageId)
    }

    /// Pull all rows newer than `after` for the group's journal,
    /// decrypt application messages, return a list of discriminated
    /// ``IncomingChatEvent``s + a fresh cursor.
    ///
    /// Each `group.processIncoming(identity:bytes:)` mutates MLS
    /// provider storage (application messages drain keys, commits
    /// replace state, proposals queue). 1.14f5 wires a persist
    /// hook here so the snapshot stays in sync.
    ///
    /// Bad frames / wrong epochs / malformed wire are silently
    /// dropped — the cursor still advances so we don't re-process.
    /// Same posture as the contacts inbox poll.
    public func pollMessages(
        identity: Identity,
        group: Group,
        after: Int64 = 0,
        waitSecs: Int = 0
    ) async throws -> PollMessagesResult {
        let groupId: Data
        do {
            groupId = try group.groupId()
        } catch {
            throw ChatError.wrapped(
                "group.groupId unavailable",
                underlying: error
            )
        }

        let raw: FetchResult
        do {
            raw = try await ds.fetchGroup(
                groupId: groupId,
                after: after,
                waitSecs: waitSecs
            )
        } catch {
            throw ChatError.wrapped("DS fetch failed", underlying: error)
        }

        var events: [IncomingChatEvent] = []
        var cursor = after
        for message in raw.messages {
            cursor = message.id

            // Drop our own echoes — the DS doesn't dedupe and
            // OpenMLS can't decrypt them on the sender side at
            // the same epoch.
            if takeOwnPublishedId(message.id) {
                continue
            }

            let processed: ProcessedMessage
            do {
                processed = try group.processIncoming(
                    identity: identity,
                    bytes: message.payload
                )
            } catch {
                // Bad frame, wrong epoch, malformed wire. Drop +
                // advance the cursor.
                continue
            }
            events.append(
                Self.toEvent(
                    messageId: message.id,
                    createdAt: message.createdAt,
                    processed: processed
                )
            )
        }

        return PollMessagesResult(
            events: events,
            cursor: cursor,
            hasMore: raw.hasMore
        )
    }

    // MARK: - Own-published id registry

    private func registerOwnPublishedId(_ id: Int64) {
        lockQueue.sync { _ = ownPublishedIds.insert(id) }
    }

    /// Atomically test-and-remove. Returns `true` if the id was
    /// in the set (caller drops the message); `false` otherwise.
    private func takeOwnPublishedId(_ id: Int64) -> Bool {
        lockQueue.sync { ownPublishedIds.remove(id) != nil }
    }

    // MARK: - ProcessedMessage → IncomingChatEvent

    private static func toEvent(
        messageId: Int64,
        createdAt: String,
        processed: ProcessedMessage
    ) -> IncomingChatEvent {
        switch processed {
        case .application(let data):
            // UTF-8 decode with replacement on malformed bytes —
            // the plaintext was already authenticated by the MLS
            // Application message's MAC, so the worst case is
            // malformed UTF-8 from a buggy peer, not a tampered
            // payload.
            let plaintext = String(data: data, encoding: .utf8) ?? ""
            return .application(
                messageId: messageId,
                plaintext: plaintext,
                createdAt: createdAt
            )
        case .commit:
            return .system(
                messageId: messageId,
                systemKind: .commit,
                createdAt: createdAt
            )
        case .proposal:
            return .system(
                messageId: messageId,
                systemKind: .proposal,
                createdAt: createdAt
            )
        case .externalJoinProposal:
            return .system(
                messageId: messageId,
                systemKind: .externalJoinProposal,
                createdAt: createdAt
            )
        }
    }
}

// MARK: - Result types

/// Result of ``ChatClient/sendMessage(identity:group:plaintext:)``.
public struct SendMessageResult: Equatable {
    /// DS-assigned monotonic id (also the cursor value for the
    /// next ``ChatClient/pollMessages(identity:group:after:waitSecs:)``
    /// call).
    public let messageId: Int64
    /// ISO-8601 timestamp from the DS.
    public let createdAt: String

    public init(messageId: Int64, createdAt: String) {
        self.messageId = messageId
        self.createdAt = createdAt
    }
}

/// Discriminated chat event surfaced by
/// ``ChatClient/pollMessages(identity:group:after:waitSecs:)``.
public enum IncomingChatEvent: Equatable {
    /// Application-data message (user-visible chat row).
    case application(messageId: Int64, plaintext: String, createdAt: String)
    /// Commit / proposal / external-join-proposal frame. MLS
    /// state advanced, no user-visible bytes.
    case system(messageId: Int64, systemKind: SystemKind, createdAt: String)
}

/// Kind discriminator for ``IncomingChatEvent/system(messageId:systemKind:createdAt:)``.
public enum SystemKind: Equatable {
    case commit
    case proposal
    case externalJoinProposal
}

/// Result of ``ChatClient/pollMessages(identity:group:after:waitSecs:)``.
public struct PollMessagesResult: Equatable {
    public let events: [IncomingChatEvent]
    /// Fresh cursor — pass back as
    /// ``ChatClient/pollMessages(identity:group:after:waitSecs:)``'s
    /// `after` argument.
    public let cursor: Int64
    /// `true` if the DS reported more rows past the last returned
    /// id; caller should poll again immediately with the new
    /// cursor.
    public let hasMore: Bool

    public init(events: [IncomingChatEvent], cursor: Int64, hasMore: Bool) {
        self.events = events
        self.cursor = cursor
        self.hasMore = hasMore
    }
}

// MARK: - Errors

public enum ChatError: Error {
    /// Programmer error — surfaced before any network call.
    case argument(String)
    /// Wraps any underlying failure (DS, MLS process, FFI). The
    /// `underlying` carries the original error for diagnostics.
    case wrapped(String, underlying: Error)
}

extension ChatError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .wrapped(let message, let underlying):
            return "\(message): \(underlying)"
        }
    }
}
