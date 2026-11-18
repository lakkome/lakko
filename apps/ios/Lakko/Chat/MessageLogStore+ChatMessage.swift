// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0041 Phase 3 — the `LakkoCore`-dependent half of ``MessageLogStore``.
///
/// The store proper (``MessageLogStore`` + the pure Codable
/// ``MessageLogPayload`` / ``PersistedGroup`` / ``PersistedMessage`` shapes)
/// was made **FFI-clean** (no `import LakkoCore`) so it can be compiled into
/// the `LakkoNotificationService` (NSE) target, which links the SLIM
/// `LakkoCoreExtension` framework — NOT the full `LakkoCore`. The NSE only
/// needs the per-group **cursor** (`getLog(...)?.groups[hex]?.cursor`), a pure
/// `Int64` on a Codable struct, so none of the app-only `ChatMessage` /
/// `AttachmentRef` conversion below is reachable from the extension process.
///
/// These bridge helpers — the live in-memory ``ChatMessage`` ↔ persisted-JSON
/// conversions the foreground app's ``ChatRepository`` uses — depend on the
/// full `LakkoCore` UniFFI surface (`ChatMessage`, `AttachmentRef`,
/// `Direction`, `Reaction`), so they live in **this** file, which is compiled
/// only into the main `Lakko` app target. Splitting them out is a pure
/// relocation: the app-visible API is unchanged.

// MARK: - ChatMessage ↔ PersistedMessage helpers

extension PersistedMessage {
    /// The rows of a group that get written to the log: the ones that
    /// reached the server.
    ///
    /// A row still in flight, or one whose send failed, is not written. It
    /// has no server id to be keyed on, the other side has never heard of
    /// it, and a relaunch that restored it would show a message as if it had
    /// been sent. That is also why a failed send stops being retryable at a
    /// relaunch: keeping it would be a durable outbox, which is a different
    /// thing and is unbuilt.
    public static func persistable(_ messages: [ChatMessage]) -> [PersistedMessage] {
        messages.filter { $0.ack == .sent }.map(PersistedMessage.init(from:))
    }

    /// Build the JSON projection from a live in-memory
    /// ``ChatMessage``.
    public init(from msg: ChatMessage) {
        self.messageId = msg.messageId
        self.direction = {
            switch msg.direction {
            case .outgoing: return "outgoing"
            case .incoming: return "incoming"
            case .system: return "system"
            }
        }()
        // ADR-0023 — a deleted message is stored EMPTY. The bubble and the
        // conversation row both draw the tombstone from `deleted` alone, so
        // nothing reads either of these again; keeping them would write the
        // words of a message someone chose to delete - and the content keys
        // of its attachments, which open the blobs on the server - to this
        // device for as long as the transcript lives, in exchange for
        // nothing. The web client stores it the same way.
        self.text = msg.deleted ? "" : msg.text
        self.attachments = msg.deleted ? [] : msg.attachments.map(PersistedAttachment.init(from:))
        self.createdAt = msg.createdAt
        // ADR-0023 — the author's signature key, written verbatim for BOTH
        // directions: the key we stamped at send, and the key the receive loop
        // resolved at the epoch that named the sender. The store is the record
        // of who wrote this, not a place to re-derive it from whoever is
        // holding the device now.
        self.authorKeyB64 = AuthorIdentity.toPersisted(msg.authorKeyB64)
        self.edited = msg.edited
        self.deleted = msg.deleted
        self.reactions = msg.reactions.map {
            PersistedReaction(
                reactorKeyB64: $0.reactor, emoji: $0.emoji,
                remove: $0.add ? nil : true, at: $0.at == 0 ? nil : $0.at)
        }
        // ADR-0052 — a deleted reply is stored without its quote, for the
        // same reason as the text above: the tombstone draws no quote block,
        // so the snapshot would be read by nothing and is somebody's words.
        self.reply = msg.deleted
            ? nil
            : msg.reply.map {
                PersistedReply(target: $0.target, author: $0.author, excerpt: $0.excerpt)
            }
        // ADR-0052 decision 2 — the same for a deleted forward: the tombstone
        // draws no attribution line, and the name is somebody's.
        self.forward = msg.deleted
            ? nil
            : msg.forward.map {
                PersistedForward(
                    author: $0.author,
                    originGroupIdHex: $0.originGroupIdHex,
                    originMessageId: $0.originMessageId)
            }
        self.unsupportedVersion = msg.unsupportedVersion
        self.lastOpId = msg.lastOpId
        // ADR-0090 — a deleted content message is stored empty too.
        self.content = msg.deleted ? nil : msg.content
    }

    /// Inverse — reconstruct a runtime ``ChatMessage`` from
    /// the JSON projection. Returns `nil` on a malformed
    /// `direction` discriminator (older clients parsing a
    /// future variant).
    public func toChatMessage() -> ChatMessage? {
        let dir: Direction
        switch direction {
        case "outgoing": dir = .outgoing
        case "incoming": dir = .incoming
        case "system": dir = .system
        default: return nil
        }
        return ChatMessage(
            messageId: messageId,
            direction: dir,
            text: text,
            attachments: attachments.compactMap { $0.toAttachmentRef() },
            createdAt: createdAt,
            // Absent on a row written before this field existed, and `nil` when
            // it could not be resolved — both restore as `nil`, which fails the
            // edit / delete gate closed rather than falling back to our CURRENT
            // key, which would invent an author the store never recorded.
            authorKeyB64: AuthorIdentity.persisted(authorKeyB64),
            edited: edited,
            deleted: deleted,
            reactions: reactions.map {
                Reaction(
                    reactor: $0.reactorKeyB64, emoji: $0.emoji,
                    add: $0.remove != true, at: $0.at ?? 0)
            },
            reply: reply.map {
                ReplyRef(target: $0.target, author: $0.author, excerpt: $0.excerpt)
            },
            forward: forward.map {
                ForwardRef(
                    author: $0.author,
                    originGroupIdHex: $0.originGroupIdHex,
                    originMessageId: $0.originMessageId)
            },
            unsupportedVersion: unsupportedVersion,
            lastOpId: lastOpId,
            content: content
        )
    }
}

extension PersistedAttachment {
    public init(from ref: AttachmentRef) {
        self.init(
            blobIdB64: ref.blobId.base64EncodedString(),
            keyB64: ref.key.base64EncodedString(),
            nonceB64: ref.nonce.base64EncodedString(),
            mime: ref.mime,
            size: ref.size,
            name: ref.name,
            // ADR-0047 — the media metadata rides along verbatim. It already
            // passed the core's bounds on the way in (received refs) or was
            // produced by this client (sent refs), so nothing is re-validated
            // here; the render decision re-reads the MIME allowlist anyway.
            kind: ref.kind,
            durationMs: ref.durationMs,
            waveformB64: ref.waveform?.base64EncodedString(),
            width: ref.width,
            height: ref.height,
            thumb: ref.thumb.map(PersistedThumb.init(from:)),
            title: ref.title,
            artist: ref.artist
        )
    }

    public func toAttachmentRef() -> AttachmentRef? {
        guard let blobId = Data(base64Encoded: blobIdB64),
              let key = Data(base64Encoded: keyB64),
              let nonce = Data(base64Encoded: nonceB64) else {
            return nil
        }
        // A waveform that no longer decodes, or exceeds the envelope bound,
        // is dropped; the attachment survives (the ADR-0047 envelope rule).
        let waveform = waveformB64
            .flatMap { Data(base64Encoded: $0) }
            .flatMap { (1...Waveform.maxBytes).contains($0.count) ? $0 : nil }
        return AttachmentRef(
            blobId: blobId,
            key: key,
            nonce: nonce,
            mime: mime,
            size: size,
            name: name,
            kind: kind,
            durationMs: durationMs,
            waveform: waveform,
            width: width,
            height: height,
            thumb: thumb?.toThumbRef(),
            title: title,
            artist: artist
        )
    }
}

extension PersistedThumb {
    public init(from thumb: ThumbRef) {
        self.init(
            blobIdB64: thumb.blobId.base64EncodedString(),
            keyB64: thumb.key.base64EncodedString(),
            nonceB64: thumb.nonce.base64EncodedString(),
            mime: thumb.mime,
            size: thumb.size,
            width: thumb.width,
            height: thumb.height
        )
    }

    /// `nil` when a byte field no longer decodes — the poster is dropped and
    /// the bubble shows a neutral circle.
    public func toThumbRef() -> ThumbRef? {
        guard let blobId = Data(base64Encoded: blobIdB64),
              let key = Data(base64Encoded: keyB64),
              let nonce = Data(base64Encoded: nonceB64) else {
            return nil
        }
        return ThumbRef(
            blobId: blobId,
            key: key,
            nonce: nonce,
            mime: mime,
            size: size,
            width: width,
            height: height
        )
    }
}
