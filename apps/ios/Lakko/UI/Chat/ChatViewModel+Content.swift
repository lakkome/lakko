// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import LakkoCore
import UIKit
import UniformTypeIdentifiers

// ADR-0092 — the chat screen's side of the attachment menu and the content
// kinds: which rows the menu has, what the poll editor offers, and every
// content action, each a thin call into ``ChatRepository``.

extension ChatViewModel {

    /// The role the menu rule reads.
    var contentRole: ConversationRole {
        switch repository.ownRole(groupIdHex: groupIdHex) {
        case .owner?: return .owner
        case .admin?: return .admin
        default: return .member
        }
    }

    var menuConversationKind: ConversationListKind {
        conversationKind == .chat ? .chat : .group
    }

    /// UX section 2.4 — the paperclip menu's rows, decided by the core.
    func attachmentMenuRows(hasCamera: Bool, hasLocation: Bool) -> [AttachmentMenuRow] {
        attachmentMenu(
            kind: menuConversationKind, role: contentRole, hasCamera: hasCamera, hasLocation: hasLocation,
            callHoldsCamera: activeCall != nil || groupCallHost.activeCallId(groupIdHex: groupIdHex) != nil)
    }

    /// UX section 7.1 — whether the poll editor shows the Anonymous switch.
    var pollAnonymityHere: PollAnonymity {
        let accounts = UInt32(repository.memberAccountKeys(groupIdHex: groupIdHex).count)
        return pollAnonymity(kind: menuConversationKind, accounts: accounts)
    }

    /// This device's account key (the conversation key), as the folds name it.
    var ownContentKey: Data? {
        repository.conversationAccountIdentity?.signaturePublicKey()
    }

    /// Whether `accountKey` is the owner or an admin here.
    func isOwnerOrAdmin(_ accountKey: Data) -> Bool {
        repository.groups[groupIdHex]?.isOwnerOrAdmin(accountKey) ?? false
    }

    // MARK: Actions

    func sendContent(_ draft: ContentDraft, attachments: [AttachmentRef] = []) {
        let repository = repository, groupIdHex = groupIdHex
        Task { @MainActor [weak self] in
            do {
                try await repository.sendContent(groupIdHex: groupIdHex, content: draft, attachments: attachments)
            } catch {
                NSLog("ADR-0092: content send failed")
                self?.reportAttachmentProblem(.key("attach.error.not_sent"))
            }
        }
    }

    func votePoll(messageId: Int64, choices: [UInt8]) {
        runContent { try await $0.votePoll(groupIdHex: $1, pollMessageId: messageId, choices: choices) }
    }

    func closeContent(messageId: Int64) {
        runContent { try await $0.closeContent(groupIdHex: $1, messageId: messageId) }
    }

    func tickChecklist(messageId: Int64, item: ChecklistItemRef, done: Bool) {
        runContent { try await $0.tickChecklist(groupIdHex: $1, messageId: messageId, item: item, done: done) }
    }

    func addChecklistItem(messageId: Int64, text: String) {
        runContent { try await $0.addChecklistItem(groupIdHex: $1, messageId: messageId, text: text) }
    }

    func editChecklist(messageId: Int64, body: ChecklistBody) {
        runContent { try await $0.editChecklist(groupIdHex: $1, messageId: messageId, body: body) }
    }

    func editArticle(messageId: Int64, article: Article, attachments: [AttachmentRef]) {
        runContent {
            try await $0.editArticle(groupIdHex: $1, messageId: messageId, article: article, attachments: attachments)
        }
    }

    private func runContent(_ op: @escaping (ChatRepository, String) async throws -> Void) {
        let repository = repository, groupIdHex = groupIdHex
        Task { @MainActor [weak self] in
            do {
                try await op(repository, groupIdHex)
            } catch {
                NSLog("ADR-0092: content op failed")
                self?.reportAttachmentProblem(.key("attach.error.not_sent"))
            }
        }
    }

    /// The account's own choice in an anonymous poll (register or queued).
    func registerChoice(messageId: Int64) -> [UInt8]? {
        PollChoiceCenter.shared.choice(groupIdHex: groupIdHex, pollMessageId: messageId)
    }

    // MARK: Article images and audio

    /// Upload an article image (processed as a photo, UX section 11.1).
    func uploadArticleImage(_ bytes: Data) async -> AttachmentRef? {
        let stamp = String(UUID().uuidString.prefix(8)).lowercased()
        guard let prepared = await Task.detached(priority: .userInitiated, operation: {
            PhotoVideoPrep.preparePhoto(bytes, stamp: stamp)
        }).value, let payload = prepared.bytes else {
            reportAttachmentProblem(.key("attach.preview.photo_unreadable"))
            return nil
        }
        do {
            return try await AttachmentBridge.encryptAndUpload(
                bytes: payload, mime: prepared.mime, name: prepared.name,
                media: MediaAttachmentMeta(kind: nil, width: prepared.width, height: prepared.height, thumb: nil))
        } catch {
            reportAttachmentProblem(.key("attach.error.not_sent"))
            return nil
        }
    }

    /// UX section 6 — an audio file joins the composer tray as a file chip;
    /// its own tags (title, artist, duration, cover art) are read on this
    /// device when it uploads (``ComposerTrayUpload``).
    func onAudioPicked(url: URL) {
        tray.add([.audio(url)])
    }
}

/// The MIME type of a picked file, from its extension.
enum UTTypeMime {
    static func of(_ url: URL) -> String? {
        UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
    }
}


/// UX section 6 — an audio file's own tags.
enum AudioTags {
    struct Read {
        var title: String?
        var artist: String?
        var durationMs: UInt32?
        var cover: (data: Data, width: UInt32, height: UInt32)?
    }

    static func read(url: URL) async -> Read {
        let asset = AVURLAsset(url: url)
        var out = Read()
        if let duration = try? await asset.load(.duration), duration.isNumeric {
            let ms = duration.seconds * 1000
            if ms >= 1, ms < Double(UInt32.max) { out.durationMs = UInt32(ms) }
        }
        let items = (try? await asset.load(.commonMetadata)) ?? []
        for item in items {
            guard let key = item.commonKey else { continue }
            switch key {
            case .commonKeyTitle: out.title = try? await item.load(.stringValue)
            case .commonKeyArtist: out.artist = try? await item.load(.stringValue)
            case .commonKeyArtwork:
                if let data = try? await item.load(.dataValue), let image = UIImage(data: data),
                   let cg = image.cgImage, let fitted = PhotoVideoPrep.fitThumb(scaled(cg)) {
                    out.cover = fitted
                }
            default: break
            }
        }
        return out
    }

    /// Scale cover art to at most 320 px.
    private static func scaled(_ image: CGImage) -> CGImage {
        let side = max(image.width, image.height)
        guard side > PhotoVideoPrep.thumbMaxPixel else { return image }
        let k = CGFloat(PhotoVideoPrep.thumbMaxPixel) / CGFloat(side)
        let w = Int(CGFloat(image.width) * k), h = Int(CGFloat(image.height) * k)
        guard let ctx = CGContext(
            data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return image }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage() ?? image
    }

    /// A tag within the wire bound (UTF-8 bytes), trimmed; `nil` when empty.
    static func bounded(_ text: String, _ limits: ContentLimits) -> String? {
        var t = text.trimmingCharacters(in: .whitespacesAndNewlines)
            .filter { !$0.unicodeScalars.contains { $0.properties.generalCategory == .control || $0.properties.generalCategory == .format } }
        while t.utf8.count > Int(limits.maxAudioTagBytes) { t.removeLast() }
        return t.isEmpty ? nil : t
    }
}
