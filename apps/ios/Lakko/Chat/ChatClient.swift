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

    /// Fail-closed MLS-ratchet persister wired by ``ChatRepository``.
    ///
    /// ``sendMessage(identity:group:plaintext:)`` invokes it AFTER
    /// `createApplicationMessage` (which advances the sender ratchet to
    /// generation N+1) and BEFORE the DS publish. If it THROWS,
    /// `sendMessage` throws and never publishes — the gen-N ciphertext
    /// never leaves the device, so the ratchet is safe to re-consume
    /// generation N after a reload (nonce-reuse-on-crash defence, mirror
    /// of the web `ChatClient.onPersist` / `persistDurableOrThrow`).
    ///
    /// `nil` for tests / standalone callers with no reload-from-snapshot
    /// path — such callers persist nothing, so a reload restores no group
    /// and cannot re-consume a generation; they are outside this bug's
    /// blast radius, and the invocation is skipped. Production
    /// (``ChatRepository``) always wires it, so the gate is live there.
    public var ratchetPersister: (() async throws -> Void)?

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
    ///
    /// With ``ratchetPersister`` wired (production, via ``ChatRepository``),
    /// the post-advance (generation N+1) ratchet snapshot is durably
    /// persisted **before** the DS publish, and a failed persist ABORTS
    /// the send (fail-closed) so the gen-N ciphertext never reaches the DS
    /// without its N+1 ratchet state on disk — see the persist-before-
    /// publish comment below for the nonce-reuse-on-crash rationale.
    public func sendMessage(
        identity: Identity,
        group: Group,
        plaintext: Data
    ) async throws -> SendMessageResult {
        guard !plaintext.isEmpty else {
            throw ChatError.argument("plaintext must be non-empty")
        }

        // `createApplicationMessage` ADVANCES the MLS sender ratchet: it
        // consumes the current generation N (deriving the AEAD key+nonce
        // from N) and leaves the in-memory OpenMLS provider at generation
        // N+1. Because the provider is in-memory, that N+1 state lives
        // ONLY in the snapshot until it is persisted.
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

        // SECURITY (nonce-reuse-on-crash) — PERSIST BEFORE PUBLISH,
        // FAIL-CLOSED. The N+1 ratchet state produced just above MUST be
        // durably on disk before the gen-N ciphertext reaches the DS.
        // Otherwise a crash (or a silently-failed persist) between the
        // publish and the persist would reload the ratchet at N, and the
        // next send would re-consume generation N — an identical
        // (key, nonce) on a different plaintext (C1 XOR C2 = P1 XOR P2
        // plaintext leak to the zero-trust server + Poly1305 tag forgery).
        // So AWAIT the durable persist; if it does NOT land, ABORT — do
        // NOT publish — so the gen-N ciphertext never leaves this device
        // (only one gen-N ciphertext can ever exist on the wire, so gen N
        // stays safe to re-consume after a reload). A crash in the NEW
        // window (after this persist, before the publish) merely SKIPS
        // generation N; RFC 9420 receivers tolerate sender-ratchet gaps,
        // so there is no reuse either way. Mirror of the web
        // `persistDurableOrThrow()` call in `apps/web/src/chat/client.ts`.
        if let ratchetPersister {
            do {
                try await ratchetPersister()
            } catch {
                throw ChatError.wrapped(
                    "MLS snapshot persist failed — send aborted to prevent nonce reuse",
                    underlying: error
                )
            }
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

    /// ADR-0042 — broadcast a CONTROL application message to the group
    /// journal (currently: the push-routing-secret distribution envelope).
    ///
    /// Mirrors ``sendMessage(identity:group:plaintext:)``'s
    /// persist-before-publish fail-closed discipline —
    /// `createApplicationMessage` advances the sender ratchet, so the
    /// post-advance state MUST be durably persisted (via the wired
    /// ``ratchetPersister``) before the ciphertext reaches the DS (the AEAD
    /// nonce-reuse-on-crash class). Unlike ``sendMessage`` it fires NO
    /// user-facing push wake: a routing-secret handshake should not wake
    /// devices (a member without the secret cannot be woken under
    /// `push_gid` yet anyway — they catch up on the next foreground poll).
    ///
    /// Records the DS id in the own-published registry so the broadcaster's
    /// own next poll drops the echo (the owner already holds the secret it
    /// minted). Returns the DS message id.
    @discardableResult
    public func broadcastControl(
        identity: Identity,
        group: Group,
        controlBytes: Data
    ) async throws -> Int64 {
        guard !controlBytes.isEmpty else {
            throw ChatError.argument("controlBytes must be non-empty")
        }

        let wireBytes: Data
        do {
            wireBytes = try group.createApplicationMessage(
                identity: identity,
                plaintext: controlBytes
            )
        } catch {
            throw ChatError.wrapped("createApplicationMessage failed", underlying: error)
        }

        let groupId: Data
        do {
            groupId = try group.groupId()
        } catch {
            throw ChatError.wrapped("group.groupId unavailable", underlying: error)
        }

        // PERSIST BEFORE PUBLISH, FAIL-CLOSED — same rationale as
        // `sendMessage`. A failed persist ABORTS the broadcast so the
        // gen-N ciphertext never reaches the DS without its N+1 ratchet
        // state durably on disk.
        if let ratchetPersister {
            do {
                try await ratchetPersister()
            } catch {
                throw ChatError.wrapped(
                    "MLS snapshot persist failed — control broadcast aborted to prevent nonce reuse",
                    underlying: error
                )
            }
        }

        let publish: PublishResult
        do {
            publish = try await ds.publishGroup(groupId: groupId, payload: wireBytes)
        } catch {
            throw ChatError.wrapped("DS publishGroup (control) failed", underlying: error)
        }
        registerOwnPublishedId(publish.id)
        return publish.id
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
                // ADR-0022 — the CHAT receive loop enforces the group-admin
                // role policy: an add / remove / role-change commit whose
                // committer's role does not authorise it is REFUSED (throws
                // here) and skipped, never merged (fail-closed). A group with
                // no role table (legacy) behaves like the open
                // `processIncoming`. This is the chat group, not the SFU call
                // group (the call path reuses the chat exporter, not a separate
                // Group), so the call-path caveat does not apply here.
                processed = try group.processIncomingVerifyingRoles(
                    identity: identity,
                    bytes: message.payload
                )
            } catch {
                // Bad frame, wrong epoch, malformed wire, OR a role-unauthorized
                // commit refused by the gate. Drop + advance the cursor.
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
        case .application(let data, let senderLeaf):
            // Phase 3a8/9 — sniff for call-signaling proto
            // bytes before running the chat-envelope parser.
            // Cheap unambiguous discriminator: the proto's
            // first wire tag is one of 0x52 / 0x5A / 0x62 /
            // 0x6A (CallSignalingPayload.body oneof fields
            // 10..13); the chat envelope starts with `{`
            // (0x7B). No overlap.
            if Self.looksLikeCallSignaling(data) {
                return .callSignaling(
                    messageId: messageId,
                    payloadBytes: data,
                    createdAt: createdAt
                )
            }
            // ADR-0042 — sniff for a push-routing-secret distribution
            // envelope before the chat parser. The FFI codec is fail-closed
            // (domain + version + exact-length gate), so a non-envelope
            // plaintext returns `nil` and falls through to the normal chat
            // parse. Never a timeline bubble — the receive loop persists the
            // secret + subscribes under the derived opaque `push_gid`.
            if let secret = PushGroupRouting.tryDecodeSecretEnvelope(data) {
                return .pushRoutingSecret(
                    messageId: messageId,
                    secret: secret,
                    createdAt: createdAt
                )
            }
            // ADR-0023: feed the application bytes through the
            // structured ``parseBodyEnvelope(bytes:)`` surface,
            // which branches a plain chat message from an
            // edit / delete / reaction op. The plaintext was
            // already authenticated by the MLS Application
            // message's MAC, so the worst case is a malformed
            // envelope from a buggy peer — never a tampered
            // payload. Legacy pre-1.19 plain-UTF-8 bytes and
            // unknown / ambiguous op envelopes degrade to a
            // `.chat` body via the parser's bounded fail-closed
            // fallback. The `senderLeaf` (the original author's
            // MLS leaf index) rides every event so the receive
            // loop can enforce the author-only edit/delete gate
            // + attribute reactions.
            let parsed = AttachmentBridge.parseBodyEnvelopeBytes(data)
            switch parsed.kind {
            case .chat:
                return .application(
                    messageId: messageId,
                    text: parsed.text,
                    attachments: parsed.attachments,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            case .edit:
                return .op(
                    messageId: messageId,
                    op: .edit,
                    target: parsed.target ?? 0,
                    text: parsed.text,
                    emoji: nil,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            case .delete:
                return .op(
                    messageId: messageId,
                    op: .delete,
                    target: parsed.target ?? 0,
                    text: "",
                    emoji: nil,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            case .reaction:
                return .op(
                    messageId: messageId,
                    op: .reaction,
                    target: parsed.target ?? 0,
                    text: "",
                    emoji: parsed.emoji,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            case .typing:
                // ADR-0024 — an ephemeral typing signal. `target` is
                // unused; the bool rides `isTyping`. The receive loop
                // folds it into per-leaf ephemeral UI state (no bubble).
                return .op(
                    messageId: messageId,
                    op: .typing(isTyping: parsed.isTyping ?? false),
                    target: 0,
                    text: "",
                    emoji: nil,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            case .readReceipt:
                // ADR-0024 — a read receipt. `target` is the read-up-to
                // watermark (the highest messageId the sender has read).
                // The receive loop advances a monotone per-leaf watermark.
                return .op(
                    messageId: messageId,
                    op: .readReceipt,
                    target: parsed.target ?? 0,
                    text: "",
                    emoji: nil,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            }
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
        case .externalJoinProposal(let joinerSignatureKey):
            // ADR-0027 (G-3) — surface the joiner's signature key the members
            // sheet needs to approve / reject the request, rather than a
            // content-less system row.
            return .externalJoinRequest(
                messageId: messageId,
                joinerSignatureKey: joinerSignatureKey,
                createdAt: createdAt
            )
        }
    }

    /// Phase 3a8/9 — cheap byte-sniff for the
    /// `CallSignalingPayload` proto. Returns `true` when
    /// the first byte matches one of the oneof body tags
    /// (0x52 / 0x5A / 0x62 / 0x6A for fields 10..13). Chat
    /// envelopes start with `{` (0x7B); the sets don't
    /// overlap. A false positive downstream surfaces as a
    /// proto-decode error and the caller swallows.
    private static func looksLikeCallSignaling(_ bytes: Data) -> Bool {
        guard let first = bytes.first else { return false }
        return first == 0x52 || first == 0x5A || first == 0x62 || first == 0x6A
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
///
/// Phase 1.19c3: `.application` now carries
/// `(text, attachments)` from the 1.19a JSON envelope (parsed
/// via the UniFFI ``parseEnvelope(bytes:)`` surface from 1.19b).
/// Legacy pre-1.19 plain-UTF-8 bytes collapse cleanly to
/// `(text: <utf-8 lossy>, attachments: [])` via the parser's
/// documented fallback paths — existing bubbles render
/// unchanged.
public enum IncomingChatEvent: Equatable {
    /// Application-data message (user-visible chat row).
    ///
    /// ADR-0023: now carries the original author's MLS leaf
    /// index (`senderLeaf`). `nil` for a frame whose sender
    /// leaf the FFI couldn't surface (a control message, an
    /// external sender) — such a message can never be a valid
    /// edit / delete target.
    case application(
        messageId: Int64,
        text: String,
        attachments: [AttachmentRef],
        senderLeaf: UInt32?,
        createdAt: String
    )
    /// ADR-0023 — an edit / delete / reaction op carried in the
    /// same JSON envelope shape as a chat message. The
    /// repository folds it into the *target* message (by
    /// `target` == the target's DS `messageId`) rather than
    /// rendering it as a standalone bubble. The op itself still
    /// consumes a DS `messageId` and surfaces here so the
    /// receive loop applies it exactly once (the poll is
    /// incremental).
    case op(
        messageId: Int64,
        op: OpKind,
        target: Int64,
        text: String,
        emoji: String?,
        /// The op sender's MLS leaf index. The author-only gate
        /// (edit / delete) checks it against the target's
        /// stored author leaf; reactions attribute the toggle
        /// to it. `nil` ⇒ the op is dropped (fail-closed).
        senderLeaf: UInt32?,
        createdAt: String
    )
    /// Commit / proposal / external-join-proposal frame. MLS
    /// state advanced, no user-visible bytes.
    case system(messageId: Int64, systemKind: SystemKind, createdAt: String)
    /// Phase 3a8/9 — call-signaling event lifted out of an
    /// MLS application message. The chat repository
    /// dispatches it into the ``CallSession`` state machine
    /// instead of the chat timeline.
    case callSignaling(messageId: Int64, payloadBytes: Data, createdAt: String)
    /// ADR-0027 (G-3) — an external-join *request*: a non-member posted an
    /// external Add proposal to this group's journal and the MLS core retained
    /// it pending an owner / admin approval. Carries the joiner's leaf
    /// signature key (the `proposalRef` handle the approve / reject repository
    /// methods take). Not a timeline bubble — the view model folds it into a
    /// pending list rendered in the members sheet (the retained set,
    /// `group.pendingExternalJoins()`, is the source of truth; this event is
    /// the nudge that a new request landed).
    case externalJoinRequest(messageId: Int64, joinerSignatureKey: Data, createdAt: String)
    /// ADR-0042 — an in-group push-routing-secret distribution envelope
    /// (the group OWNER broadcast the group's `push_routing_secret`). Never
    /// a timeline bubble: the receive loop persists the secret + subscribes
    /// under the derived opaque `push_gid` (invariant #5). Classified by
    /// its domain-separated envelope prefix via the FFI fail-closed codec.
    case pushRoutingSecret(messageId: Int64, secret: Data, createdAt: String)
}

/// ADR-0024 — an inbound ephemeral chat-status signal lifted out of
/// the poll fold. Typing + read-receipt ops render no bubble and never
/// touch the message timeline; they surface through the repository's
/// `chatStatusSink` so the view model can fold them into ephemeral
/// per-leaf typing state + a monotone per-leaf read watermark.
public enum ChatStatusEvent: Equatable {
    /// The member at `senderLeaf` is (or stopped) typing. `createdAt`
    /// is the DS row's ISO-8601 timestamp — the view model drops a
    /// signal older than the expiry window (re-sync replay guard).
    case typing(senderLeaf: UInt32, isTyping: Bool, createdAt: String)
    /// The member at `senderLeaf` has read up to `upTo` (inclusive).
    /// The view model advances a monotone watermark (a lower value
    /// never overwrites a higher one — re-sync replay is idempotent).
    case readReceipt(senderLeaf: UInt32, upTo: Int64)
}

/// ADR-0023 / ADR-0024 — which mutation or status signal an
/// ``IncomingChatEvent/op`` carries.
public enum OpKind: Equatable {
    case edit
    case delete
    case reaction
    /// ADR-0024 — an ephemeral typing signal. `true` ⇒ the sender is
    /// typing now; `false` ⇒ they stopped (blur / sent). Never renders
    /// a bubble — folds into per-leaf ephemeral UI state.
    case typing(isTyping: Bool)
    /// ADR-0024 — a read receipt. The op's `target` is the read-up-to
    /// watermark (the highest messageId the sender has read). Advances a
    /// monotone per-leaf watermark; never renders a bubble.
    case readReceipt
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
