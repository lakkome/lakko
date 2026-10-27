// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0065 — the blob ids a message's files live under: each attachment
/// and its thumbnail. Pure.
public enum MessageBlobs {
    public static func ids(of attachments: [AttachmentRef]) -> [Data] {
        var out: [Data] = []
        var seen = Set<Data>()
        for ref in attachments {
            for id in [ref.blobId, ref.thumb?.blobId].compactMap({ $0 }) where id.count == 32 {
                if seen.insert(id).inserted { out.append(id) }
            }
        }
        return out
    }
}

/// Where a message lives for Save (round-6 rule, maintainer 2026-10-27): the
/// MLS group id of THE SEGMENT holding it and its DS id there. Never the
/// root conversation, never an on-screen history id: so a saved message
/// keeps opening after its conversation moves to a successor group.
public struct SavedMessageRef: Equatable, Hashable, Sendable {
    public let segmentIdHex: String
    public let dsId: Int64
}

extension ChatThread {
    /// The segment ref of an on-screen row: a head row is `(groupIdHex, id)`;
    /// an older-segment row (a negative history id) is its segment's group
    /// and its DS id there. `nil` for an id that names no held segment.
    public func savedRef(rowId: Int64) -> SavedMessageRef? {
        if rowId > 0 { return SavedMessageRef(segmentIdHex: groupIdHex, dsId: rowId) }
        guard let origin = LineageRules.historyRowOrigin(rowId) else { return nil }
        let index = historyGroupIdHexes.count - origin.distanceFromHead
        guard historyGroupIdHexes.indices.contains(index) else { return nil }
        return SavedMessageRef(segmentIdHex: historyGroupIdHexes[index], dsId: origin.messageId)
    }

    /// The on-screen row id of `ref` in this conversation, or `nil` when the
    /// segment is not one of its held groups.
    public func rowId(of ref: SavedMessageRef) -> Int64? {
        if ref.segmentIdHex == groupIdHex { return ref.dsId }
        guard let index = historyGroupIdHexes.firstIndex(of: ref.segmentIdHex) else { return nil }
        return LineageRules.historyRowId(ref.dsId, distanceFromHead: historyGroupIdHexes.count - index)
    }

    /// Whether `segmentIdHex` is one of this conversation's held groups.
    public func holds(segmentIdHex: String) -> Bool {
        segmentIdHex == groupIdHex || historyGroupIdHexes.contains(segmentIdHex)
    }
}

/// ADR-0065 section 4 / UX `saved-messages-and-file-lifetime.md` sections 2,
/// 3 — the pure rules of Save.
public enum SavedMessages {

    /// The ref a saved entry names.
    public static func ref(of entry: SavedMessageView) -> SavedMessageRef {
        SavedMessageRef(segmentIdHex: ChatFolders.hex(entry.conversation.id), dsId: Int64(clamping: entry.messageId))
    }

    /// Most recently saved first; equal times by the newer stamp.
    public static func ordered(_ entries: [SavedMessageView]) -> [SavedMessageView] {
        entries.sorted {
            if $0.savedAtMs != $1.savedAtMs { return $0.savedAtMs > $1.savedAtMs }
            return $0.hlc > $1.hlc
        }
    }

    /// The saved entry for `ref` (matched on the segment id and the DS id,
    /// whatever kind another device filed).
    public static func entry(in entries: [SavedMessageView], ref: SavedMessageRef) -> SavedMessageView? {
        guard ref.dsId > 0 else { return nil }
        let id = UInt64(ref.dsId)
        return entries.first { $0.messageId == id && ChatFolders.hex($0.conversation.id) == ref.segmentIdHex }
    }

    /// The attachment-kind token of a message, the catalog suffix of
    /// `saved.kind.*`: `photo`, `video`, `voice`, `video_note`, `gif`, `file`,
    /// or empty for a message with none.
    public static func kindToken(_ attachments: [AttachmentRef]) -> String {
        guard let first = attachments.first else { return "" }
        switch InlineMedia.decision(for: first).bubble {
        case .photo: return "photo"
        case .video: return "video"
        case .voice: return "voice"
        case .videoNote: return "video_note"
        case .gif: return "gif"
        case .file: return "file"
        }
    }

    /// The snapshot saved with a message: author, excerpt (the text; a file
    /// with no text names the file), the kind token.
    public static func snapshot(
        text: String, attachments: [AttachmentRef], author: String
    ) -> (author: String, excerpt: String, kind: String) {
        let kind = kindToken(attachments)
        var excerpt = text
        if excerpt.isEmpty, kind == "file" { excerpt = attachments.first?.name ?? "" }
        return (author, excerpt, kind)
    }

    /// The row's second line from the snapshot: the excerpt, or the kind's
    /// label ("Photo", "File: name").
    public static func rowLine(excerpt: String, kind: String) -> String {
        switch kind {
        case "": return excerpt
        case "file":
            return Localization.text("saved.kind.file", texts: ["name": excerpt])
        case "photo", "video", "voice", "video_note", "gif":
            let label = Localization.text("saved.kind.\(kind)")
            return excerpt.isEmpty ? label : "\(label) · \(excerpt)"
        default:
            return excerpt
        }
    }

    /// The message's own time (unix ms) for the snapshot, 0 when unknown.
    public static func sentAtMs(_ message: ChatMessage) -> UInt64 {
        UInt64(max(0, DsTimestamp.parseMs(message.createdAt) ?? 0))
    }

    /// UX section 3 — the date a Saved row shows: the snapshot's message
    /// time, else the held message's time, else (both unknown) the save time.
    public static func rowDateMs(snapshotSentAtMs: UInt64, heldSentAtMs: UInt64?, savedAtMs: UInt64) -> UInt64 {
        if snapshotSentAtMs > 0 { return snapshotSentAtMs }
        if let heldSentAtMs, heldSentAtMs > 0 { return heldSentAtMs }
        return savedAtMs
    }

    /// Whether another save fits under the client limit.
    public static func canSaveMore(currentCount: Int, adding: Int = 1) -> Bool {
        currentCount + adding <= Int(savedMaxClient())
    }
}

/// Why a save did not happen.
public enum SavedMessagesError: Error, Equatable {
    case notReady
    case limitReached
}

/// The app's saved messages, over the DeviceGroup synced state.
@MainActor
public final class SavedMessagesCenter: ObservableObject {
    public static let shared = SavedMessagesCenter()

    @Published public private(set) var entries: [SavedMessageView] = []

    private weak var sync: DeviceGroupSyncRepository?
    private var watch: AnyCancellable?
    private var threadsWatch: AnyCancellable?

    /// Bind to the account's sync runtime (the app shell), and to the chat
    /// repository whose deletions leave the saved list.
    public func bind(sync: DeviceGroupSyncRepository, chat: ChatRepository) {
        self.sync = sync
        watch = sync.$saved.sink { [weak self] list in self?.entries = list }
        threadsWatch = chat.$state.sink { [weak self] state in
            Task { @MainActor in await self?.dropDeleted(threads: state.threads) }
        }
    }

    public func isSaved(_ ref: SavedMessageRef) -> Bool {
        SavedMessages.entry(in: entries, ref: ref) != nil
    }

    /// Save `messages`, each named by the segment that holds it. Throws
    /// ``SavedMessagesError`` or the publish's error.
    public func save(
        _ messages: [(message: ChatMessage, ref: SavedMessageRef, author: String)],
        kind: FolderConversationKind
    ) async throws {
        guard let sync, sync.ready else { throw SavedMessagesError.notReady }
        var seen = Set<SavedMessageRef>()
        let fresh = messages.filter {
            $0.ref.dsId > 0 && !$0.message.deleted && !isSaved($0.ref) && seen.insert($0.ref).inserted
        }
        guard !fresh.isEmpty else { return }
        guard SavedMessages.canSaveMore(currentCount: entries.count, adding: fresh.count) else {
            throw SavedMessagesError.limitReached
        }
        let nowMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        var registers: [SavedMessageRegister] = []
        for item in fresh {
            guard let segmentId = ChatFolders.bytes(fromHex: item.ref.segmentIdHex) else {
                throw SavedMessagesError.notReady
            }
            let ref = FolderConversationRef(kind: kind, id: segmentId)
            let snap = SavedMessages.snapshot(
                text: item.message.text, attachments: item.message.attachments, author: item.author)
            registers.append(try composeSavedMessage(
                hlc: sync.mintHlc(), conversation: ref, messageId: UInt64(item.ref.dsId),
                savedAtMs: nowMs, sentAtMs: SavedMessages.sentAtMs(item.message), author: snap.author, excerpt: snap.excerpt, attachmentKind: snap.kind,
                blobIds: MessageBlobs.ids(of: item.message.attachments)))
        }
        try await sync.applySaved(registers)
    }

    /// Remove saved entries (`Remove from saved`, or the message was deleted).
    public func unsave(_ targets: [SavedMessageView]) async throws {
        guard let sync, sync.ready else { throw SavedMessagesError.notReady }
        guard !targets.isEmpty else { return }
        let registers = try targets.map {
            try composeSavedRemoval(hlc: sync.mintHlc(), conversation: $0.conversation, messageId: $0.messageId)
        }
        try await sync.applySaved(registers)
    }

    public func unsave(_ ref: SavedMessageRef) async throws {
        guard let entry = SavedMessages.entry(in: entries, ref: ref) else { return }
        try await unsave([entry])
    }

    /// UX section 3 — a saved message its author deleted for everyone leaves
    /// Saved, as it leaves the pinned bar.
    func dropDeleted(threads: [ChatThread]) async {
        let gone = SavedMessagesCenter.deletedEntries(entries, threads: threads)
        guard !gone.isEmpty else { return }
        try? await unsave(gone)
    }

    /// The saved entries whose message this device holds as deleted. Pure.
    nonisolated static func deletedEntries(_ entries: [SavedMessageView], threads: [ChatThread]) -> [SavedMessageView] {
        var deleted = Set<SavedMessageRef>()
        for thread in threads {
            for message in thread.messages where message.deleted {
                if let ref = thread.savedRef(rowId: message.messageId) { deleted.insert(ref) }
            }
        }
        guard !deleted.isEmpty else { return [] }
        return entries.filter { deleted.contains(SavedMessages.ref(of: $0)) }
    }

    func clear() {
        entries = []
        watch = nil
        threadsWatch = nil
        sync = nil
    }
}
