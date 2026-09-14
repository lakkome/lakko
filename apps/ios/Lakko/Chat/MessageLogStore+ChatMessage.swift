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
        self.text = msg.text
        self.attachments = msg.attachments.map(PersistedAttachment.init(from:))
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
            PersistedReaction(reactorKeyB64: $0.reactor, emoji: $0.emoji)
        }
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
                Reaction(reactor: $0.reactorKeyB64, emoji: $0.emoji)
            }
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
            thumb: ref.thumb.map(PersistedThumb.init(from:))
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
            thumb: thumb?.toThumbRef()
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
