// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import Foundation

/// `docs/ux/connection-state.md` section 7.3 — what a media message's upload
/// stage needs from the network. The live ports go through
/// ``AttachmentBridge`` and the GIF proxy; a test replaces them.
struct OutboxMediaPorts {
    /// Encrypt and upload the file; the returned reference carries `media`.
    var upload: @MainActor (
        _ bytes: Data, _ mime: String, _ name: String, _ media: MediaAttachmentMeta,
        _ onProgress: @escaping @Sendable (Double) -> Void
    ) async throws -> AttachmentRef
    /// Encrypt and upload a preview (JPEG): a video note's poster, a photo's
    /// or video's thumbnail, an audio file's cover.
    var uploadPoster: @MainActor (_ bytes: Data, _ width: UInt32?, _ height: UInt32?) async throws -> ThumbRef
    /// Fetch a picked GIF's bytes through the proxy.
    var fetchGif: @MainActor (_ url: URL) async throws -> (bytes: Data, mime: String)

    static func live(sessionToken: @escaping () -> String?) -> OutboxMediaPorts {
        OutboxMediaPorts(
            upload: { bytes, mime, name, media, onProgress in
                try await AttachmentBridge.encryptAndUpload(
                    bytes: bytes, mime: mime, name: name, media: media, onProgress: onProgress)
            },
            uploadPoster: { bytes, width, height in
                try await AttachmentBridge.encryptAndUploadThumb(
                    bytes: bytes, mime: "image/jpeg", width: width, height: height)
            },
            fetchGif: { url in
                switch await GifClient(sessionToken: sessionToken)
                    .fetchMedia(url, maxBytes: GifClient.maxFullBytes) {
                case .ok(let media): return (media.bytes, media.mime)
                case .failure(let failure): throw OutboxGifFetchError(failure: failure)
                }
            })
    }
}

/// A GIF fetch that did not return bytes. The proxy client reports a request
/// that got no answer as `unavailable`, so that one counts as a network
/// failure (the message waits); the others are refusals.
struct OutboxGifFetchError: Error {
    let failure: GifFailure
    var isNetwork: Bool { failure == .unavailable }
}

/// Why a media message's upload stage stopped for a reason of its own.
enum OutboxMediaError: Error {
    /// The prepared file is gone (it could not be written and the app was
    /// restarted, or it no longer opens): the message cannot be sent.
    case fileMissing
    /// The row was deleted while its upload ran.
    case rowGone
}

/// Section 7.3 — the upload progress of the media rows of this account whose
/// upload is running, and the last refusal the composer says something about.
@MainActor
public final class OutboxUploadProgress: ObservableObject {
    public enum Refusal: Equatable {
        /// ADR-0065 / UX 4.2 — the sending limit.
        case limited
        /// UX storage-full — the server has no room for new files.
        case storageFull
    }

    /// Fraction sent, by ``ChatRepository/outboxKey(groupIdHex:localId:)``.
    @Published private(set) var fractions: [String: Double] = [:]
    /// The refusal of the last upload that stopped on one; cleared when the
    /// next upload starts.
    @Published public private(set) var refusal: Refusal?

    func begin(_ key: String) {
        fractions[key] = 0
        refusal = nil
    }

    func set(_ key: String, _ fraction: Double) {
        guard fractions[key] != nil else { return }
        fractions[key] = fraction
    }

    func end(_ key: String) {
        fractions.removeValue(forKey: key)
    }

    func noteRefusal(_ error: Error) {
        if case AttachmentBridgeError.uploadLimited = error { refusal = .limited }
        if case AttachmentBridgeError.storageFull = error { refusal = .storageFull }
    }
}

/// One attachment of a message being sent whose upload has not finished
/// (section 7.3: the upload is part of the send, in the conversation's
/// order): a voice message, a video note or a GIF from the media controller,
/// or a photo, video or file from the composer tray.
public struct OutboxMediaInput {
    /// The file the upload sends; `nil` for a GIF still to be fetched.
    public var bytes: Data?
    /// A GIF's proxy path, fetched as the first step of the upload.
    public var gifSourceURL: URL?
    public var name: String
    public var mime: String
    public var meta: MediaAttachmentMeta
    /// The preview uploaded beside it (JPEG) and its size.
    public var poster: Data?
    public var posterWidth: UInt32?
    public var posterHeight: UInt32?
    /// ADR-0047 section 6 — the micro thumbnail the reference carries.
    public var inlineThumb: Data?
    /// ADR-0092 section 2 — an audio file's tags.
    public var title: String?
    public var artist: String?

    public init(
        bytes: Data?, gifSourceURL: URL? = nil, name: String, mime: String,
        meta: MediaAttachmentMeta, poster: Data? = nil, posterWidth: UInt32? = nil,
        posterHeight: UInt32? = nil, inlineThumb: Data? = nil, title: String? = nil,
        artist: String? = nil
    ) {
        self.bytes = bytes
        self.gifSourceURL = gifSourceURL
        self.name = name
        self.mime = mime
        self.meta = meta
        self.poster = poster
        self.posterWidth = posterWidth
        self.posterHeight = posterHeight
        self.inlineThumb = inlineThumb
        self.title = title
        self.artist = artist
    }
}

/// One attachment of a message handed to ``ChatRepository/sendMessageWithUploads``:
/// the reference of an upload that finished, or what is still to upload.
public enum OutboxPart {
    case ready(AttachmentRef)
    case pending(OutboxMediaInput)
}

extension ChatRepository {

    // MARK: - The store

    /// The signed-in account's outbox, or `nil` when signed out.
    var sendOutbox: SendOutbox? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return SendOutbox(
            ownerAccountId: accountIdB64, sealer: heldSendSealer, baseDirectory: sendOutboxBaseDirectory)
    }

    /// The key a media row's upload progress is filed under while its upload
    /// has not finished, and the local file of its first such attachment.
    nonisolated static func outboxKey(groupIdHex: String, localId: Int64) -> String {
        "outbox-\(groupIdHex)-\(localId)"
    }

    /// Every record tells the repository when one of its unsent rows moves.
    func installOutboxHooks() {
        for (hex, record) in groups where record.onUnsentChange == nil {
            record.onUnsentChange = { [weak self] in
                MainActor.assumeIsolated { self?.syncOutbox(groupIdHex: hex) }
            }
        }
    }

    /// Write `groupIdHex`'s entries again from its rows: every outgoing row
    /// that never reached the server (sending counts as waiting: a send cut
    /// short by a crash goes again), except the ones the held queue keeps.
    /// A failed write never stops a send; the rows stay in memory.
    func syncOutbox(groupIdHex: String) {
        guard !outboxWritesPaused, let outbox = sendOutbox, let record = groups[groupIdHex] else { return }
        let held = Set(heldRows.values.filter { $0.groupIdHex == groupIdHex }.map(\.localId))
        let entries = Self.outboxEntries(rows: record.messages, heldLocalIds: held)
        if !outbox.save(conversationHex: groupIdHex, entries: entries) {
            NSLog("[outbox] entries not written; the unsent messages are kept while the app runs")
        }
    }

    /// The outbox entries `rows` come to. Pure.
    nonisolated static func outboxEntries(rows: [ChatMessage], heldLocalIds: Set<Int64>) -> [SendOutbox.Entry] {
        rows.filter {
            $0.messageId <= 0 && $0.localId != 0 && $0.direction == .outgoing && $0.ack != .sent
                && !$0.deleted && !heldLocalIds.contains($0.localId)
        }.map { row in
            SendOutbox.Entry(
                localId: row.localId,
                writtenAtMs: ContentTime.ms(row.createdAt),
                state: row.ack == .failed ? .notSent : .waiting,
                autoSendAttempts: row.autoSendAttempts,
                row: PersistedMessage(from: row),
                media: row.outboxMedia)
        }
    }

    /// The row an entry restores, in its state.
    nonisolated static func restoredRow(_ entry: SendOutbox.Entry) -> ChatMessage? {
        guard let base = entry.row.toChatMessage(), base.direction == .outgoing else { return nil }
        var row = ChatMessage(
            messageId: 0, direction: .outgoing, text: base.text, attachments: base.attachments,
            createdAt: base.createdAt, authorKeyB64: base.authorKeyB64,
            ack: entry.state == .notSent ? .failed : .waiting,
            localId: entry.localId, reply: base.reply, forward: base.forward, content: base.content)
        row.autoSendAttempts = entry.autoSendAttempts
        row.outboxMedia = entry.media
        return row
    }

    /// Section 7.3 — at the session landing: put the unsent rows the outbox
    /// kept back at the end of their conversations, in the order they were
    /// written and in their state, drop the entries of conversations this
    /// device no longer holds, and start the passes when the servers answer.
    func restoreOutbox() {
        guard let outbox = sendOutbox, let accountIdB64 = currentAccountIdB64 else { return }
        let known = Set(knownGroupsStore.load(accountIdB64: accountIdB64))
        var changed = false
        for (hex, entries) in outbox.all() {
            guard let record = groups[hex] else {
                // A conversation this device no longer holds at all: its
                // entries go. One it holds but could not load keeps them.
                if !known.contains(hex) { outbox.save(conversationHex: hex, entries: []) }
                continue
            }
            let present = Set(record.messages.map(\.id))
            for entry in entries.sorted(by: { $0.writtenAtMs < $1.writtenAtMs })
            where !present.contains(entry.localId) {
                guard let row = Self.restoredRow(entry) else { continue }
                record.restoreUnsentRow(row)
                changed = true
            }
        }
        outbox.removeOrphanFiles()
        if changed { state = state.with(threads: buildThreadList()) }
        if connectionPhaseNow() == .online { runAllWaitingSends() }
    }

    /// A conversation was deleted here: its unsent messages go with it.
    func forgetOutbox(groupIdHex: String) {
        guard let outbox = sendOutbox else { return }
        for entry in outbox.list(conversationHex: groupIdHex) {
            for media in entry.media { removePreparedFiles(media, outbox: outbox) }
        }
        outbox.save(conversationHex: groupIdHex, entries: [])
    }

    private func removePreparedFiles(_ media: SendOutbox.PreparedMedia, outbox: SendOutbox?) {
        for id in [media.fileId, media.posterFileId].compactMap({ $0 }) where !id.isEmpty {
            outbox?.removeFile(id: id)
            outboxFallbackFiles.removeValue(forKey: id)
        }
    }

    private func preparedBytes(_ id: String, outbox: SendOutbox?) -> Data? {
        guard !id.isEmpty else { return nil }
        return outbox?.readFile(id: id) ?? outboxFallbackFiles[id]
    }

    /// The bytes a media row still being uploaded plays from (its key is
    /// ``outboxKey(groupIdHex:localId:)``; its first unfinished attachment),
    /// or `nil`.
    func outboxLocalFile(key: String) -> (bytes: Data, name: String)? {
        for (hex, record) in groups {
            for row in record.messages where !row.outboxMedia.isEmpty
            && Self.outboxKey(groupIdHex: hex, localId: row.localId) == key {
                guard let media = row.outboxMedia.first,
                      let bytes = preparedBytes(media.fileId, outbox: sendOutbox) else { return nil }
                return (bytes, media.name)
            }
        }
        return nil
    }

    /// The placeholder reference a slot draws while its upload has not
    /// finished: the media metadata and micro thumbnail, an empty blob id.
    nonisolated static func outboxPlaceholder(_ media: SendOutbox.PreparedMedia) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(), key: Data(), nonce: Data(), mime: media.mime, size: 0,
            name: media.name, kind: media.kind, durationMs: media.durationMs,
            waveform: media.waveform, width: media.width, height: media.height, thumb: nil,
            title: media.title, artist: media.artist, inlineThumb: media.inlineThumb)
    }

    // MARK: - Sending media

    /// Section 7.3 — send a voice message, a video note or a GIF in the
    /// conversation's order. Returns the row's local id, or `nil` when the
    /// conversation is not held.
    @discardableResult
    func sendMedia(groupIdHex: String, media input: OutboxMediaInput, reply: ReplyRef?) -> Int64? {
        sendMessageWithUploads(groupIdHex: groupIdHex, text: "", parts: [.pending(input)], reply: reply)
    }

    /// Section 7.3 and `docs/ux/media-messages.md` section 11 — one message
    /// whose attachments are not all uploaded yet: its row (and outbox entry,
    /// with each unfinished attachment's prepared file) exists before
    /// anything is uploaded, and the uploads are the first part of its send,
    /// in the conversation's order. Returns the row's local id, or `nil` when
    /// the conversation is not held.
    @discardableResult
    func sendMessageWithUploads(
        groupIdHex: String, text: String, parts: [OutboxPart], reply: ReplyRef?
    ) -> Int64? {
        guard canonicalIdentity != nil, let record = groups[groupIdHex] else { return nil }
        let outbox = sendOutbox
        func file(_ bytes: Data?) -> String? {
            guard let bytes else { return nil }
            let id = SendOutbox.newFileId()
            // A file the outbox cannot write is kept while the app runs.
            if outbox?.writeFile(id: id, bytes: bytes) != true { outboxFallbackFiles[id] = bytes }
            return id
        }
        var attachments: [AttachmentRef] = []
        var pending: [SendOutbox.PreparedMedia] = []
        for (slot, part) in parts.enumerated() {
            switch part {
            case .ready(let ref):
                attachments.append(ref)
            case .pending(let input):
                let prepared = SendOutbox.PreparedMedia(
                    slot: slot, fileId: file(input.bytes) ?? "",
                    name: input.name, mime: input.mime, kind: input.meta.kind,
                    durationMs: input.meta.durationMs, waveform: input.meta.waveform,
                    width: input.meta.width, height: input.meta.height,
                    posterFileId: file(input.poster), posterWidth: input.posterWidth,
                    posterHeight: input.posterHeight, inlineThumb: input.inlineThumb,
                    title: input.title, artist: input.artist,
                    gifSourceURL: input.gifSourceURL?.absoluteString)
                pending.append(prepared)
                attachments.append(Self.outboxPlaceholder(prepared))
            }
        }
        let localId = record.nextLocalId()
        record.appendPending(
            localId: localId, text: text, attachments: attachments,
            createdAt: Self.isoTimestamp(ms: Self.nowMs()),
            authorKeyB64: AuthorIdentity.own(group: record.group),
            reply: reply, forward: nil)
        record.mutateLocalRow(localId: localId) { $0.outboxMedia = pending }
        state = state.with(threads: buildThreadList())
        if queueBehindWaiting(groupIdHex: groupIdHex, record: record, localId: localId) { return localId }
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await self.sendUnsentNow(groupIdHex: groupIdHex, localId: localId)
            } catch {
                self.settleManualFailure(groupIdHex: groupIdHex, localId: localId, error: error)
            }
        }
        return localId
    }

    /// The person's own send of an unsent row (a first send, or Retry): the
    /// upload stage when the row has one, then held or published. Throws
    /// with the row left "Sending..." for the caller to settle.
    func sendUnsentNow(groupIdHex: String, localId: Int64) async throws {
        let ready = try await uploadOutboxMediaIfNeeded(groupIdHex: groupIdHex, localId: localId)
        // ADR-0073 R.7 case 1 — a dead one-to-one holds it instead.
        if sendRoute(groupIdHex: groupIdHex) == .hold {
            guard holdMessage(
                groupIdHex: groupIdHex, text: ready.text, attachments: ready.attachments,
                reply: ready.reply, forward: ready.forward,
                content: ready.content.flatMap(ContentModel.content), existingLocalId: localId)
            else { throw ChatError.argument("the message could not be kept for sending") }
            return
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        try await publishUnsentRow(groupIdHex: groupIdHex, record: record, row: ready)
    }

    /// A send the person started did not go: waiting when it got no answer,
    /// `Not sent` otherwise.
    func settleManualFailure(groupIdHex: String, localId: Int64, error: Error) {
        guard let record = groups[groupIdHex], record.messages.contains(where: { $0.localId == localId })
        else { return }
        if WaitingSends.isNetworkFailure(error) {
            record.markWaiting(localId: localId, resetAttempts: true)
            state = state.with(threads: buildThreadList())
            recheckWaitingIfOnline(groupIdHex: groupIdHex)
            return
        }
        record.markFailed(localId: localId)
        state = state.with(threads: buildThreadList())
    }

    /// The row as it now stands, when it is still an unsent row of `groupIdHex`.
    private func liveUnsentRow(_ groupIdHex: String, _ localId: Int64) -> (GroupRecord, ChatMessage)? {
        guard let record = groups[groupIdHex],
              let row = record.messages.first(where: { $0.localId == localId && $0.messageId <= 0 })
        else { return nil }
        return (record, row)
    }

    /// Section 7.3 — the upload stage of a row whose attachments are not all
    /// uploaded: for each unfinished attachment in turn, fetch a GIF's bytes,
    /// upload the preview and the file, and put the reference in its slot
    /// (the outbox entry is written again each time, and the prepared files
    /// of the slot go). Returns the row as it now stands; a row with nothing
    /// to upload is returned as it is.
    func uploadOutboxMediaIfNeeded(groupIdHex: String, localId: Int64) async throws -> ChatMessage {
        guard case let (record, row)? = liveUnsentRow(groupIdHex, localId) else { throw OutboxMediaError.rowGone }
        guard !row.outboxMedia.isEmpty else { return row }
        record.markPending(localId: localId)
        state = state.with(threads: buildThreadList())
        let key = Self.outboxKey(groupIdHex: groupIdHex, localId: localId)
        let progress = outboxUploadProgress
        progress.begin(key)
        defer { progress.end(key) }
        do {
            while case let (_, current)? = liveUnsentRow(groupIdHex, localId), let next = current.outboxMedia.first {
                try await uploadSlot(next, groupIdHex: groupIdHex, localId: localId, progressKey: key)
                state = state.with(threads: buildThreadList())
            }
        } catch {
            progress.noteRefusal(error)
            throw error
        }
        guard case let (live, _)? = liveUnsentRow(groupIdHex, localId) else { throw OutboxMediaError.rowGone }
        // ADR-0047 section 6 — the micro thumbnail stays on the message's
        // first photo or video only.
        live.mutateLocalRow(localId: localId) {
            $0.attachments = ComposerTray.keepingInlineThumbOnFirstMedia($0.attachments)
        }
        guard case let (_, done)? = liveUnsentRow(groupIdHex, localId) else { throw OutboxMediaError.rowGone }
        return done
    }

    /// Upload one unfinished attachment and put its reference in its slot.
    private func uploadSlot(
        _ start: SendOutbox.PreparedMedia, groupIdHex: String, localId: Int64, progressKey key: String
    ) async throws {
        let outbox = sendOutbox
        let progress = outboxUploadProgress
        var media = start
        /// Rewrite this slot's prepared form on the row (and so the entry).
        func update(_ changed: SendOutbox.PreparedMedia, placeholder: Bool) {
            groups[groupIdHex]?.mutateLocalRow(localId: localId) { row in
                guard let at = row.outboxMedia.firstIndex(where: { $0.slot == changed.slot }) else { return }
                row.outboxMedia[at] = changed
                if placeholder, row.attachments.indices.contains(changed.slot) {
                    row.attachments[changed.slot] = Self.outboxPlaceholder(changed)
                }
            }
        }
        var bytes = preparedBytes(media.fileId, outbox: outbox)
        if bytes == nil, let source = media.gifSourceURL, let url = URL(string: source) {
            let fetched = try await outboxMediaPorts.fetchGif(url)
            let fileId = SendOutbox.newFileId()
            if outbox?.writeFile(id: fileId, bytes: fetched.bytes) != true {
                outboxFallbackFiles[fileId] = fetched.bytes
            }
            media.fileId = fileId
            media.mime = fetched.mime
            media.gifSourceURL = nil
            let ext = fetched.mime == "image/webp" ? "webp" : fetched.mime == "video/mp4" ? "mp4" : "gif"
            media.name = ((media.name as NSString).deletingPathExtension) + "." + ext
            update(media, placeholder: true)
            bytes = fetched.bytes
        }
        guard let bytes else { throw OutboxMediaError.fileMissing }
        var thumb = media.uploadedPoster?.toThumbRef()
        if thumb == nil, let posterId = media.posterFileId, let poster = preparedBytes(posterId, outbox: outbox) {
            do {
                let uploaded = try await outboxMediaPorts.uploadPoster(poster, media.posterWidth, media.posterHeight)
                thumb = uploaded
                // Kept with the entry: a later attempt (also after a
                // restart) reuses it, and a deletion deletes it.
                media.uploadedPoster = PersistedThumb(from: uploaded)
                update(media, placeholder: false)
                MediaSessionCache.shared.store(poster, key: ChatMediaController.thumbKey(for: uploaded))
            } catch {
                // No answer: the whole send waits. Any other failure sends
                // the attachment without its preview (ADR-0047).
                if WaitingSends.isNetworkFailure(error) { throw error }
                NSLog("[outbox] a preview was not uploaded; sending without it")
            }
        }
        let meta = MediaAttachmentMeta(
            kind: media.kind, durationMs: media.durationMs, waveform: media.waveform,
            width: media.width, height: media.height, thumb: thumb)
        var uploaded = try await outboxMediaPorts.upload(
            bytes, media.mime, media.name, meta,
            { fraction in Task { @MainActor in progress.set(key, fraction) } })
        uploaded.inlineThumb = InlineThumb.bounded(media.inlineThumb)
        if let title = media.title { uploaded.title = title }
        if let artist = media.artist { uploaded.artist = artist }
        guard case let (live, _)? = liveUnsentRow(groupIdHex, localId) else {
            // Deleted while uploading: the upload is nobody's.
            BlobLifecycle.shared.delete(MessageBlobs.ids(of: [uploaded]))
            throw OutboxMediaError.rowGone
        }
        let slot = media.slot
        live.mutateLocalRow(localId: localId) { row in
            if row.attachments.indices.contains(slot) { row.attachments[slot] = uploaded }
            row.outboxMedia.removeAll { $0.slot == slot }
        }
        removePreparedFiles(media, outbox: outbox)
        // The sent bubble shows its own bytes, never a download of them.
        MediaSessionCache.shared.store(bytes, key: ChatMediaController.key(for: uploaded))
    }

    // MARK: - Deleting an unsent row

    /// Section 7.3 — the person deletes a message that never went (waiting
    /// or `Not sent`): the row, its outbox entry, its prepared files, and
    /// any file already uploaded for it. Returns whether it did.
    @discardableResult
    func deleteUnsentRow(groupIdHex: String, localId: Int64) -> Bool {
        guard case let (record, row)? = liveUnsentRow(groupIdHex, localId),
              row.direction == .outgoing, row.ack == .failed || row.ack == .waiting
        else { return false }
        var uploaded = MessageBlobs.ids(of: row.attachments)
        for media in row.outboxMedia {
            if let poster = media.uploadedPoster?.toThumbRef() { uploaded.append(poster.blobId) }
            removePreparedFiles(media, outbox: sendOutbox)
        }
        record.removeLocalRow(localId: localId)
        state = state.with(threads: buildThreadList())
        // ADR-0065 section 6 — an upload whose message was never sent is
        // deleted at once.
        BlobLifecycle.shared.delete(uploaded)
        return true
    }
}
