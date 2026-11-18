// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0052 decision 2 / `docs/ux/message-actions.md` sections 5 and 5.1 —
/// the pure rules of a forward: what is sent when a conversation is picked,
/// what a received envelope becomes, and what the attribution line says.
///
/// Pure functions on purpose, like ``ReplySnapshot``: the picker's send, the
/// bubble and the receive loop all agree on one shape, and the twins on the
/// other clients say the same words.
public enum ForwardPayload {

    /// The line the bubble draws when a forward carries no name.
    public static var anonymousLine: String { Localization.text("chat.forward.label") }

    /// What is sent when `message` is forwarded, decided at the moment a
    /// conversation is picked.
    ///
    /// - The text is the message's own text VERBATIM — which is the edited
    ///   text when it was edited, because that is what `text` holds after an
    ///   edit lands. The quote of a reply is not carried (section 5.1), and
    ///   neither are reactions.
    /// - `authorLabel` is what the timeline calls the original author; the
    ///   caller resolves it, because the name is the timeline's business and
    ///   this helper knows nothing about contacts. Blank means no name.
    /// - A message that is itself a forward keeps the ORIGINAL author's name
    ///   and the ORIGINAL origin, never the forwarder's ("a chain does not
    ///   rewrite its own history"): what it carried is passed on, and when it
    ///   carried nothing, nothing is invented.
    /// - Otherwise the origin is this conversation and this message's DS id;
    ///   a row that has not landed has no id and so no origin.
    ///
    /// Nothing is length-bounded here: the core truncates an over-long name
    /// and drops an over-long origin when the envelope is composed.
    /// - ADR-0053 section 6: a forward whose SOURCE conversation is a
    ///   one-to-one carries NO origin at all — neither this conversation's id
    ///   nor one the message carried in. The only other holder of a
    ///   one-to-one is its other participant, so the reference would reach
    ///   people with no way to use it.
    public static func forwardPayload(
        of message: ChatMessage,
        authorLabel: String?,
        originGroupIdHex: String,
        sourceIsOneToOne: Bool
    ) -> (text: String, forward: ForwardRef) {
        if let carried = message.forward {
            return (
                text: message.text,
                forward: ForwardRef(
                    author: carried.author,
                    originGroupIdHex: sourceIsOneToOne ? nil : carried.originGroupIdHex,
                    originMessageId: sourceIsOneToOne ? nil : carried.originMessageId)
            )
        }
        let author = (authorLabel ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let hasId = message.messageId > 0 && !sourceIsOneToOne
        return (
            text: message.text,
            forward: ForwardRef(
                author: author.isEmpty ? nil : author,
                originGroupIdHex: hasId ? originGroupIdHex : nil,
                originMessageId: hasId ? message.messageId : nil)
        )
    }

    // MARK: - Content messages (ADR-0090, forwarding amended 2026-11-18)

    /// Whether `message` may be forwarded, by what it holds. A plain message
    /// may. A location and an article may, sent again as the same content. A
    /// contact card may unless the newest profile this device holds from the
    /// person on the card says their card may not be shared
    /// (`cardShareable(accountKey)`, `true` when nothing is held). A poll or a
    /// checklist may not: its votes and ticks belong to the conversation it
    /// was posted in. Content this build cannot read may not either.
    public static func isForwardable(
        _ message: ChatMessage, cardShareable: (Data) -> Bool
    ) -> Bool {
        isForwardable(content: message.content, cardShareable: cardShareable)
    }

    /// The same rule over the stored content alone (`nil` = a plain message).
    public static func isForwardable(
        content stored: PersistedContent?, cardShareable: (Data) -> Bool
    ) -> Bool {
        guard let stored else { return true }
        switch ContentModel.content(stored) {
        case .location?, .article?: return true
        case .contactCard(let card)?: return cardShareable(card.accountKey)
        case .poll?, .checklist?, nil: return false
        }
    }

    /// The content a forward of `message` sends again, or `nil` for a plain
    /// message (and for content that is never forwarded, which
    /// ``isForwardable(_:cardShareable:)`` already keeps out).
    public static func forwardedContent(of message: ChatMessage) -> ContentDraft? {
        forwardedContent(message.content)
    }

    public static func forwardedContent(_ stored: PersistedContent?) -> ContentDraft? {
        guard let stored else { return nil }
        switch ContentModel.content(stored) {
        case .location(let location)?: return .location(location)
        case .contactCard(let card)?: return .contactCard(card)
        case .article(let article)?: return .article(article)
        case .poll?, .checklist?, nil: return nil
        }
    }

    /// The ``ForwardRef`` a received envelope becomes, or `nil` when it
    /// carried no forward field at all (an ordinary message).
    ///
    /// The origin group id arrives as STANDARD base64 of the raw id (the wire
    /// shape) and is stored as the app's hex; an id that does not decode
    /// drops the whole origin pair rather than half of it. A blank name is
    /// no name.
    public static func received(
        author: String?,
        originGroupIdB64: String?,
        originMessageId: Int64?
    ) -> ForwardRef? {
        let name = author?.trimmingCharacters(in: .whitespacesAndNewlines)
        let cleanName = (name?.isEmpty ?? true) ? nil : name
        let originHex = originGroupIdB64.flatMap(groupIdHex(fromB64:))
        guard cleanName != nil || (originHex != nil && originMessageId != nil) else {
            return nil
        }
        return ForwardRef(
            author: cleanName,
            originGroupIdHex: originHex,
            originMessageId: originMessageId)
    }

    /// The one line the bubble draws (section 5.1): "Forwarded from <name>",
    /// or "Forwarded" when no name was carried. Also the button's
    /// accessibility label, so the two cannot disagree. The name is folded
    /// to one line because it came off the wire.
    public static func attributionLine(_ forward: ForwardRef) -> String {
        let name = ReplySnapshot.oneLine(forward.author ?? "")
        return name.isEmpty ? anonymousLine : Localization.text("chat.forward.from", texts: ["name": name])
    }

    // MARK: - Group id shapes

    /// The app keys a conversation by the hex of its MLS group id; the
    /// envelope carries the same bytes as standard base64. `nil` for a string
    /// that is not even-length hex.
    public static func groupIdB64(fromHex hex: String) -> String? {
        guard hex.count % 2 == 0, !hex.isEmpty else { return nil }
        var bytes = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            bytes.append(byte)
            index = next
        }
        return bytes.base64EncodedString()
    }

    /// The inverse: `nil` for a string that is not base64 or decodes to no
    /// bytes at all.
    public static func groupIdHex(fromB64 b64: String) -> String? {
        guard let bytes = Data(base64Encoded: b64), !bytes.isEmpty else { return nil }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }
}

/// ADR-0052 decision 2, OQ1 — the attachments of a forward are re-encrypted
/// under fresh keys and uploaded again, so the two conversations never share
/// a stored blob (the storage tier could otherwise see one object fetched by
/// members of both). The cost is an upload per forwarded attachment; it is
/// paid.
public enum ForwardAttachments {

    /// Download and authenticate `ref` through the same path the bubbles use,
    /// then encrypt and upload it as a new blob, keeping the media metadata
    /// (kind, duration, waveform, dimensions) and re-encrypting the poster
    /// thumb the same way. A thumb that cannot be fetched is dropped rather
    /// than failing the whole attachment: the receiving bubble then shows its
    /// neutral placeholder, exactly as it does for a note sent without one.
    public static func reencrypt(
        _ ref: AttachmentRef,
        storage: StorageClient = StorageClient()
    ) async throws -> AttachmentRef {
        let bytes = try await AttachmentBridge.downloadAndDecrypt(ref: ref, storage: storage)
        var thumb: ThumbRef?
        if let old = ref.thumb {
            if let thumbBytes = try? await AttachmentBridge.downloadAndDecrypt(
                thumb: old, storage: storage)
            {
                thumb = try? await AttachmentBridge.encryptAndUploadThumb(
                    bytes: thumbBytes,
                    mime: old.mime ?? "image/jpeg",
                    width: old.width,
                    height: old.height,
                    storage: storage)
            }
        }
        let hasMedia = ref.kind != nil || ref.durationMs != nil || ref.waveform != nil
            || ref.width != nil || ref.height != nil || thumb != nil
        return try await AttachmentBridge.encryptAndUpload(
            bytes: bytes,
            mime: ref.mime,
            name: ref.name,
            media: hasMedia
                ? MediaAttachmentMeta(
                    kind: ref.kind,
                    durationMs: ref.durationMs,
                    waveform: ref.waveform,
                    width: ref.width,
                    height: ref.height,
                    thumb: thumb)
                : nil,
            storage: storage)
    }

    /// Every attachment of a message, re-encrypted in order. One failure
    /// fails the forward: a message sent with some of its attachments is not
    /// the message that was forwarded.
    public static func reencryptAll(
        _ refs: [AttachmentRef],
        storage: StorageClient = StorageClient()
    ) async throws -> [AttachmentRef] {
        var out: [AttachmentRef] = []
        out.reserveCapacity(refs.count)
        for ref in refs {
            out.append(try await reencrypt(ref, storage: storage))
        }
        return out
    }
}
