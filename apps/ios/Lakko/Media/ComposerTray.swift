// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import ImageIO
import LakkoCore
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

// `docs/ux/media-messages.md` section 11 — the composer tray: what is
// attached to the message being written, shown above the message field.
// Paste, the photo picker, the camera, the file picker and the audio picker
// all add to it; none opens a dialog. Each item uploads as soon as it is
// added. Send sends ONE message carrying the text and every item.

/// One item in the tray.
public struct ComposerTrayItem: Identifiable, Equatable {
    /// What the item is sent as. A photo or video is a thumbnail in the tray
    /// and a tile in the receiver's mosaic; a file is a chip.
    public enum Kind: Equatable {
        case photo
        case video
        case file
    }

    public enum Status: Equatable {
        /// Being read, converted or given its preview.
        case preparing
        /// The ciphertext is going up; the fraction sent so far.
        case uploading(Double)
        /// Uploaded: ``ComposerTrayItem/ref`` is what the message carries.
        case ready
        /// The server refused the upload (too large, over the limit, storage
        /// full, or anything else that is not a missing answer). Send stays
        /// off until it is retried or removed.
        case failed
        /// `docs/ux/connection-state.md` section 2 — the upload got no answer.
        /// In a chat it does not stop Send: the message takes it along and
        /// uploads it as part of its send (section 7.3). A tap retries.
        case noAnswer
    }

    public let id: UUID
    public var kind: Kind
    /// A file's own name; `nil` for a photo or video.
    public var name: String?
    /// Plaintext size in bytes, `0` until known.
    public var size: UInt64
    /// A video's length, when known (the tray's duration pill).
    public var durationMs: UInt32?
    /// A small JPEG the tray draws (the sent thumb, or a local preview).
    public var thumbnail: Data?
    public var status: Status
    /// The uploaded attachment, once ``status`` is ``Status/ready``.
    public var ref: AttachmentRef?

    public init(
        id: UUID = UUID(), kind: Kind, name: String? = nil, size: UInt64 = 0,
        durationMs: UInt32? = nil, thumbnail: Data? = nil, status: Status = .preparing,
        ref: AttachmentRef? = nil
    ) {
        self.id = id
        self.kind = kind
        self.name = name
        self.size = size
        self.durationMs = durationMs
        self.thumbnail = thumbnail
        self.status = status
        self.ref = ref
    }

    public var isMedia: Bool { kind != .file }

    public var isBusy: Bool {
        switch status {
        case .preparing, .uploading: return true
        case .ready, .failed, .noAnswer: return false
        }
    }

    /// A tap on it uploads it again.
    public var isRetryable: Bool { status == .failed || status == .noAnswer }
}

/// The tray's state and its rules (pure).
public struct ComposerTray: Equatable {
    /// At most this many photos and videos in one message (chosen, not
    /// measured; the mosaic's own cap).
    public static let maxMedia = 10
    /// At most this many files in one message (chosen, not measured).
    public static let maxFiles = 10

    /// Which limit an add ran into.
    public enum Limit: Equatable {
        case media
        case files
    }

    public private(set) var items: [ComposerTrayItem] = []

    public init(items: [ComposerTrayItem] = []) {
        self.items = items
    }

    public var isEmpty: Bool { items.isEmpty }
    public var mediaCount: Int { items.filter(\.isMedia).count }
    public var fileCount: Int { items.count - mediaCount }

    /// Add what fits, in order: a photo or video while there are fewer than
    /// ``maxMedia``, a file while there are fewer than ``maxFiles``. Returns
    /// the items added and the limit that left something out (photos and
    /// videos first when both did).
    @discardableResult
    public mutating func add(_ new: [ComposerTrayItem]) -> (added: [ComposerTrayItem], limit: Limit?) {
        var added: [ComposerTrayItem] = []
        var mediaHit = false
        var filesHit = false
        for item in new {
            if item.isMedia {
                guard mediaCount < Self.maxMedia else { mediaHit = true; continue }
            } else {
                guard fileCount < Self.maxFiles else { filesHit = true; continue }
            }
            items.append(item)
            added.append(item)
        }
        return (added, mediaHit ? .media : (filesHit ? .files : nil))
    }

    @discardableResult
    public mutating func remove(_ id: UUID) -> ComposerTrayItem? {
        guard let index = items.firstIndex(where: { $0.id == id }) else { return nil }
        return items.remove(at: index)
    }

    public mutating func update(_ id: UUID, _ change: (inout ComposerTrayItem) -> Void) {
        guard let index = items.firstIndex(where: { $0.id == id }) else { return }
        change(&items[index])
    }

    public func item(_ id: UUID) -> ComposerTrayItem? { items.first { $0.id == id } }

    /// Something is still being prepared or uploaded.
    public var isBusy: Bool { items.contains(where: \.isBusy) }

    public var hasFailed: Bool { items.contains(where: \.isRetryable) }

    /// Send is on when there is text or at least one item, and no item is
    /// still uploading or failed.
    ///
    /// `sendsUnfinished` (a chat, `docs/ux/media-messages.md` section 11,
    /// 2026-11-24): an item still uploading, or whose upload got no answer,
    /// no longer stops Send; the message uploads it as part of its send.
    /// Only an item the server refused does, and one still being prepared
    /// (its file does not exist yet).
    public func canSend(hasText: Bool, sendsUnfinished: Bool = false) -> Bool {
        guard hasText || !items.isEmpty else { return false }
        guard sendsUnfinished else { return !isBusy && !hasFailed }
        return !items.contains { $0.status == .failed || $0.status == .preparing }
    }

    /// What the message carries, in the order the items were added. The
    /// micro thumbnail (ADR-0047 section 6) stays on the first photo or
    /// video only, as the core keeps it.
    public var refs: [AttachmentRef] { Self.keepingInlineThumbOnFirstMedia(items.compactMap(\.ref)) }

    /// ADR-0047 section 6 — `refs` with `inlineThumb` kept only on the first
    /// ref whose MIME is an inline image or video (the core's carrier rule,
    /// `lakko_attachments::inline_thumb_carrier`) and cleared on every other.
    static func keepingInlineThumbOnFirstMedia(
        _ refs: [AttachmentRef], allowlist: InlineMedia.Allowlist = InlineMedia.coreAllowlist
    ) -> [AttachmentRef] {
        let carrier = refs.firstIndex {
            switch InlineMedia.inlineType(mime: $0.mime, allowlist: allowlist) {
            case .image?, .video?: return true
            default: return false
            }
        }
        return refs.enumerated().map { index, ref in
            var out = ref
            out.inlineThumb = index == carrier ? InlineThumb.bounded(ref.inlineThumb) : nil
            return out
        }
    }

    /// The photos and videos, in order: what the viewer pages through.
    public var mediaItems: [ComposerTrayItem] { items.filter(\.isMedia) }

    public mutating func clear() { items = [] }
}

// MARK: - The store (one per conversation, kept while the app runs)

/// What the person attached, before it is prepared.
public enum ComposerTraySource {
    /// Photo bytes as picked, pasted or taken.
    case photo(Data)
    /// A video file the app owns (a temp copy).
    case video(URL)
    /// A file's bytes, sent unchanged.
    case file(Data, name: String, mime: String?)
    /// An audio file (attachments UX section 6): uploaded with its tags.
    case audio(URL)
}

/// What the worker reports while it runs.
public enum ComposerTrayEvent {
    /// The item's kind and preview are known.
    case prepared(kind: ComposerTrayItem.Kind, name: String?, size: UInt64, durationMs: UInt32?, thumbnail: Data?)
    /// Fraction of the upload sent.
    case progress(Double)
    /// Section 7.3 — the prepared form the upload is about to send, so a
    /// message sent before the upload finished can upload it itself.
    case payload(ComposerTrayPayload)
}

/// What an item's upload sends, prepared (`docs/ux/connection-state.md`
/// section 7.3): the bytes, or the prepared file they are in, and what the
/// reference carries.
public struct ComposerTrayPayload {
    public var input: OutboxMediaInput
    /// A prepared video the upload reads from; read into ``input`` when the
    /// message takes the item.
    public var fileURL: URL?
}

/// A problem that leaves an item out of the tray (said in the composer).
public struct ComposerTrayRefusal: Error {
    public let text: UiText
}

/// The tray of one conversation and the uploads behind it.
///
/// Held by ``ComposerTrays`` for the life of the app process, so leaving a
/// conversation and coming back keeps the tray, and an upload still running
/// continues (section 11, Draft).
@MainActor
public final class ComposerTrayStore: ObservableObject {
    /// Prepares and uploads one source; reports its kind and progress.
    public typealias Worker = @MainActor (
        _ source: ComposerTraySource, _ idleDays: UInt8?, _ report: @escaping @MainActor (ComposerTrayEvent) -> Void
    ) async throws -> AttachmentRef

    @Published public private(set) var tray = ComposerTray()
    /// Section 11 — `Up to 10 ...` under the strip, for 4 s.
    @Published public private(set) var limitNotice: ComposerTray.Limit?
    /// A picked item that could not be added (too big, unreadable).
    @Published public private(set) var problem: UiText?

    /// ADR-0087 section 13 — the idle life of a channel file; `nil` in a chat.
    let idleDays: UInt8?
    private let worker: Worker
    private var sources: [UUID: ComposerTraySource] = [:]
    private var tasks: [UUID: Task<Void, Never>] = [:]
    private var payloads: [UUID: ComposerTrayPayload] = [:]
    private var noticeTask: Task<Void, Never>?

    public init(idleDays: UInt8? = nil, worker: Worker? = nil) {
        self.idleDays = idleDays
        self.worker = worker ?? ComposerTrayUpload.run
    }

    /// Add sources in order; each starts uploading at once.
    public func add(_ new: [ComposerTraySource]) {
        guard !new.isEmpty else { return }
        problem = nil
        let pairs = new.map { source in (ComposerTrayItem(kind: Self.presumedKind(source), name: Self.presumedName(source)), source) }
        let (added, limit) = tray.add(pairs.map(\.0))
        let addedIds = Set(added.map(\.id))
        for (item, source) in pairs {
            if addedIds.contains(item.id) {
                sources[item.id] = source
                start(item.id)
            } else {
                Self.discard(source)
            }
        }
        if let limit { showLimit(limit) }
    }

    public func remove(_ id: UUID) {
        tasks[id]?.cancel()
        tasks[id] = nil
        payloads[id] = nil
        if let source = sources.removeValue(forKey: id) { Self.discard(source) }
        if let removed = tray.remove(id), let ref = removed.ref {
            // ADR-0065 section 6 — uploaded, never sent: deleted at once.
            BlobLifecycle.shared.delete(MessageBlobs.ids(of: [ref]))
        }
    }

    public func retry(_ id: UUID) {
        guard tray.item(id)?.isRetryable == true, sources[id] != nil else { return }
        start(id)
    }

    /// Everything the message carries, in order; the tray clears. Only when
    /// ``ComposerTray/canSend(hasText:)`` holds.
    public func takeForSend() -> [AttachmentRef] {
        let refs = tray.refs
        for source in sources.values { Self.discard(source) }
        sources = [:]
        tasks = [:]
        payloads = [:]
        tray.clear()
        problem = nil
        return refs
    }

    /// Section 11 / `docs/ux/connection-state.md` section 7.3 — everything
    /// the message carries, in order, when some uploads have not finished:
    /// an uploaded item as its reference, any other as its prepared form,
    /// which the message uploads as part of its send. The item's own upload
    /// is cancelled (a result that still arrives is deleted, never sent).
    /// The tray clears. `nil`, and nothing changes, when an item has no
    /// prepared form (still being prepared or refused): Send is off then.
    public func takeForSendWithUploads() -> [OutboxPart]? {
        var parts: [OutboxPart] = []
        for item in tray.items {
            if let ref = item.ref {
                parts.append(.ready(ref))
                continue
            }
            guard item.status != .failed, var payload = payloads[item.id] else { return nil }
            if payload.input.bytes == nil, let file = payload.fileURL {
                // Read now, before the cancelled upload removes its file.
                guard let bytes = try? Data(contentsOf: file, options: .mappedIfSafe) else { return nil }
                payload.input.bytes = Data(bytes)
            }
            parts.append(.pending(payload.input))
        }
        for task in tasks.values { task.cancel() }
        tasks = [:]
        payloads = [:]
        for source in sources.values { Self.discard(source) }
        sources = [:]
        tray.clear()
        problem = nil
        // ADR-0047 section 6 — the micro thumbnail on the first photo or
        // video only, as ``ComposerTray/refs`` keeps it.
        let firstMedia = parts.firstIndex { part in
            let mime: String?
            switch part {
            case .ready(let ref): mime = ref.mime
            case .pending(let input): mime = input.mime
            }
            switch InlineMedia.inlineType(mime: mime) {
            case .image?, .video?: return true
            default: return false
            }
        }
        return parts.enumerated().map { index, part in
            switch part {
            case .ready(var ref):
                ref.inlineThumb = index == firstMedia ? InlineThumb.bounded(ref.inlineThumb) : nil
                return .ready(ref)
            case .pending(var input):
                input.inlineThumb = index == firstMedia ? InlineThumb.bounded(input.inlineThumb) : nil
                return .pending(input)
            }
        }
    }

    /// Forget device / Delete account: drop everything, deleting nothing on
    /// the server (the account's files go with it).
    public func reset() {
        for task in tasks.values { task.cancel() }
        tasks = [:]
        payloads = [:]
        for source in sources.values { Self.discard(source) }
        sources = [:]
        tray.clear()
        problem = nil
        limitNotice = nil
    }

    /// Section 11 — what the viewer pages through from the tray: an uploaded
    /// item as it was sent, one still going up from its local bytes.
    func galleryItems() -> [MediaGalleryItem] {
        tray.mediaItems.compactMap { item in
            if let ref = item.ref { return MediaGalleryItem(ref: ref) }
            switch sources[item.id] {
            case .photo(let data)?:
                return MediaGalleryItem(id: item.id.uuidString, source: .localPhoto(data))
            case .video(let url)?:
                return MediaGalleryItem(id: item.id.uuidString, source: .localVideo(url, poster: item.thumbnail))
            default:
                return nil
            }
        }
    }

    public func report(_ refusal: UiText) { problem = refusal }

    public func clearProblem() { problem = nil }

    private func start(_ id: UUID) {
        guard let source = sources[id] else { return }
        tray.update(id) { $0.status = .preparing }
        let worker = worker
        let idleDays = idleDays
        tasks[id] = Task { @MainActor [weak self] in
            do {
                let ref = try await worker(source, idleDays) { event in
                    guard let self, !Task.isCancelled else { return }
                    switch event {
                    case .prepared(let kind, let name, let size, let durationMs, let thumbnail):
                        self.tray.update(id) {
                            $0.kind = kind
                            $0.name = name
                            $0.size = size
                            $0.durationMs = durationMs
                            $0.thumbnail = thumbnail ?? $0.thumbnail
                            $0.status = .uploading(0)
                        }
                    case .progress(let fraction):
                        self.tray.update(id) {
                            if case .uploading = $0.status { $0.status = .uploading(fraction) }
                        }
                    case .payload(let payload):
                        self.payloads[id] = payload
                    }
                }
                guard let self else { return }
                guard !Task.isCancelled, self.tray.item(id) != nil else {
                    // Removed while it went up: the blob is an orphan.
                    BlobLifecycle.shared.delete(MessageBlobs.ids(of: [ref]))
                    return
                }
                self.tray.update(id) {
                    $0.ref = ref
                    $0.size = ref.size
                    $0.status = .ready
                }
                self.tasks[id] = nil
                self.payloads[id] = nil
                // Uploaded: the source is no longer needed for a retry.
                if let source = self.sources.removeValue(forKey: id) { Self.discard(source) }
            } catch let refusal as ComposerTrayRefusal {
                guard let self, !Task.isCancelled else { return }
                // The item cannot be sent at all: it leaves the tray, and the
                // composer says why.
                self.tasks[id] = nil
                if let source = self.sources.removeValue(forKey: id) { Self.discard(source) }
                self.tray.remove(id)
                self.problem = refusal.text
            } catch {
                guard let self, !Task.isCancelled else { return }
                self.tasks[id] = nil
                // `docs/ux/connection-state.md` section 2 — no answer is not
                // a refusal: a chat's Send stays on.
                let noAnswer = WaitingSends.isNetworkFailure(error)
                self.tray.update(id) { $0.status = noAnswer ? .noAnswer : .failed }
                if let bridge = error as? AttachmentBridgeError, case .uploadLimited = bridge {
                    self.problem = bridge.userText(fileName: nil)
                }
                NSLog("[tray] upload failed")
            }
        }
    }

    private func showLimit(_ limit: ComposerTray.Limit) {
        limitNotice = limit
        noticeTask?.cancel()
        noticeTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 4_000_000_000)
            guard !Task.isCancelled else { return }
            self?.limitNotice = nil
        }
    }

    static func presumedKind(_ source: ComposerTraySource) -> ComposerTrayItem.Kind {
        switch source {
        case .photo: return .photo
        case .video: return .video
        case .file, .audio: return .file
        }
    }

    static func presumedName(_ source: ComposerTraySource) -> String? {
        switch source {
        case .photo, .video: return nil
        case .file(_, let name, _): return name
        case .audio(let url): return url.lastPathComponent
        }
    }

    /// A video file the app copied for the tray goes when the item does.
    static func discard(_ source: ComposerTraySource) {
        if case .video(let url) = source, url.path.hasPrefix(PhotoVideoPrep.pickedVideoDirectory.path) {
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
        }
    }
}

/// The trays of this app process, one per conversation.
@MainActor
public final class ComposerTrays {
    public static let shared = ComposerTrays()

    private var stores: [String: ComposerTrayStore] = [:]

    /// The tray of `conversation` (a chat's conversation id, or
    /// `channel:<id>`), made on first use.
    public func store(for conversation: String, idleDays: UInt8? = nil) -> ComposerTrayStore {
        if let existing = stores[conversation] { return existing }
        let made = ComposerTrayStore(idleDays: idleDays)
        stores[conversation] = made
        return made
    }

    /// Forget device / Delete account.
    public func removeAll() {
        for store in stores.values { store.reset() }
        stores = [:]
    }
}

// MARK: - Ways in

extension ComposerTraySource {
    /// Attachments UX section 5 — what the camera took. A video is moved
    /// under the picked-video directory, which the tray owns and clears.
    static func captured(_ media: CapturedMedia) -> ComposerTraySource {
        switch media {
        case .photo(let data):
            return .photo(data)
        case .video(let url):
            let dir = PhotoVideoPrep.pickedVideoDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
            let ext = url.pathExtension.isEmpty ? "mov" : url.pathExtension
            let target = dir.appendingPathComponent("captured.\(ext)")
            do {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                try FileManager.default.moveItem(at: url, to: target)
                return .video(target)
            } catch {
                return .video(url)
            }
        }
    }
}

/// Section 11 — the photo picker's items join `tray` in the order picked,
/// each as soon as it is read.
@MainActor
func addPicked(_ items: [PhotosPickerItem], to tray: ComposerTrayStore) async {
    for item in items {
        if item.supportedContentTypes.contains(where: { $0.conforms(to: .movie) }) {
            if let movie = try? await item.loadTransferable(type: PickedMovie.self) {
                tray.add([.video(movie.url)])
            } else {
                tray.report(.key("chat.attachment.error.open_video"))
            }
        } else if let data = try? await item.loadTransferable(type: Data.self), !data.isEmpty {
            tray.add([.photo(data)])
        } else {
            tray.report(.key("chat.attachment.error.open_photo"))
        }
    }
}

// MARK: - Preparing and uploading one item

/// `docs/ux/attachments.md` sections 3, 4 and 6: a photo is re-encoded as a
/// JPEG with its size and thumbnail; a video that is H.264 is sent as MP4
/// with its size and poster; anything that cannot be sent that way goes as a
/// file. A file over the attachment cap is not added.
enum ComposerTrayUpload {
    @MainActor
    static func run(
        _ source: ComposerTraySource, idleDays: UInt8?,
        report: @escaping @MainActor (ComposerTrayEvent) -> Void
    ) async throws -> AttachmentRef {
        let stamp = String(UUID().uuidString.prefix(8)).lowercased()
        let progress: @Sendable (Double) -> Void = { fraction in
            Task { @MainActor in report(.progress(fraction)) }
        }
        switch source {
        case .photo(let data):
            let prepared = await Task.detached(priority: .userInitiated) {
                PhotoVideoPrep.preparePhoto(data, stamp: stamp)
            }.value
            guard let prepared, prepared.bytes != nil else {
                // Section 3 — a photo that does not decode goes as a file.
                return try await uploadFile(
                    data, name: "file-\(stamp)", mime: PhotoVideoPrep.sniffInlineImageMime(data),
                    idleDays: idleDays, report: report, progress: progress)
            }
            return try await uploadPrepared(prepared, kind: .photo, idleDays: idleDays, report: report, progress: progress)
        case .video(let url):
            if await videoAcceptable(url),
               let prepared = await PhotoVideoPrep.prepareVideo(url, stamp: stamp, allowReencode: false) {
                return try await uploadPrepared(
                    prepared, kind: .video, durationMs: await durationMs(url), idleDays: idleDays,
                    report: report, progress: progress)
            }
            // Another format or over the cap: sent as the file it is.
            guard let data = try? Data(contentsOf: url, options: .mappedIfSafe) else {
                throw ComposerTrayRefusal(text: .key("chat.attachment.error.open_video"))
            }
            return try await uploadFile(
                data, name: url.lastPathComponent, mime: UTType(filenameExtension: url.pathExtension)?.preferredMIMEType,
                fileURL: url, idleDays: idleDays, report: report, progress: progress)
        case .file(let data, let name, let mime):
            return try await uploadFile(data, name: name, mime: mime, idleDays: idleDays, report: report, progress: progress)
        case .audio(let url):
            return try await uploadAudio(url, idleDays: idleDays, report: report, progress: progress)
        }
    }

    /// Section 3 — MP4 or QuickTime with H.264, at most the cap.
    static func videoAcceptable(isMP4: Bool, isH264: Bool, bytes: Int) -> Bool {
        isMP4 && isH264 && bytes <= AttachmentBridge.maxAttachmentBytes
    }

    static func videoAcceptable(_ url: URL) async -> Bool {
        // An H.264 QuickTime file (what the iPhone camera and library give)
        // counts: it is remuxed into MP4 without re-encoding.
        let type = UTType(filenameExtension: url.pathExtension)
        let isMP4 = type?.conforms(to: .mpeg4Movie) == true || type?.conforms(to: .quickTimeMovie) == true
        let size = ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.intValue ?? .max
        var isH264 = false
        if let track = try? await AVURLAsset(url: url).loadTracks(withMediaType: .video).first,
           let formats = try? await track.load(.formatDescriptions) {
            isH264 = formats.contains { CMFormatDescriptionGetMediaSubType($0) == kCMVideoCodecType_H264 }
        }
        return videoAcceptable(isMP4: isMP4, isH264: isH264, bytes: size)
    }

    private static func durationMs(_ url: URL) async -> UInt32? {
        guard let duration = try? await AVURLAsset(url: url).load(.duration), duration.isNumeric else { return nil }
        let ms = duration.seconds * 1000
        return ms >= 1 && ms < Double(UInt32.max) ? UInt32(ms) : nil
    }

    @MainActor
    private static func uploadPrepared(
        _ prepared: PhotoVideoPrep.Prepared, kind: ComposerTrayItem.Kind, durationMs: UInt32? = nil,
        idleDays: UInt8?, report: @MainActor (ComposerTrayEvent) -> Void, progress: @escaping @Sendable (Double) -> Void
    ) async throws -> AttachmentRef {
        defer {
            if let file = prepared.fileURL {
                try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
            }
        }
        let size: UInt64
        if let bytes = prepared.bytes {
            size = UInt64(bytes.count)
        } else if let file = prepared.fileURL {
            size = ((try? FileManager.default.attributesOfItem(atPath: file.path))?[.size] as? NSNumber)
                .map { UInt64($0.intValue) } ?? 0
        } else {
            size = 0
        }
        // Section 7.3 — what a message sent before this upload finishes
        // uploads itself.
        report(.payload(ComposerTrayPayload(
            input: OutboxMediaInput(
                bytes: prepared.bytes, name: prepared.name, mime: prepared.mime,
                meta: MediaAttachmentMeta(kind: nil, width: prepared.width, height: prepared.height),
                poster: prepared.thumb, posterWidth: prepared.thumbWidth, posterHeight: prepared.thumbHeight,
                inlineThumb: InlineThumb.bounded(prepared.inlineThumb)),
            fileURL: prepared.bytes == nil ? prepared.fileURL : nil)))
        report(.prepared(kind: kind, name: nil, size: size, durationMs: durationMs, thumbnail: prepared.thumb))
        var thumbRef: ThumbRef?
        if let thumb = prepared.thumb {
            // A preview that does not go up is dropped, never the send.
            thumbRef = try? await AttachmentBridge.encryptAndUploadThumb(
                bytes: thumb, mime: "image/jpeg", width: prepared.thumbWidth, height: prepared.thumbHeight)
        }
        let meta = MediaAttachmentMeta(kind: nil, width: prepared.width, height: prepared.height, thumb: thumbRef)
        do {
            var ref: AttachmentRef
            if let bytes = prepared.bytes {
                ref = try await AttachmentBridge.encryptAndUpload(
                    bytes: bytes, mime: prepared.mime, name: prepared.name, media: meta, idleDays: idleDays,
                    onProgress: progress)
                // Own bytes: the sent bubble shows them without a download.
                MediaSessionCache.shared.store(bytes, key: MediaSessionCache.key(for: ref))
            } else if let file = prepared.fileURL {
                ref = try await AttachmentBridge.encryptAndUpload(
                    fileURL: file, mime: prepared.mime, name: prepared.name, media: meta, idleDays: idleDays,
                    onProgress: progress)
                MediaSessionCache.shared.adoptVideoFile(file, key: MediaSessionCache.key(for: ref))
            } else {
                throw ComposerTrayRefusal(text: .key("attach.preview.photo_unreadable"))
            }
            if let thumbRef, let thumb = prepared.thumb {
                MediaSessionCache.shared.store(thumb, key: MediaSessionCache.key(for: thumbRef))
            }
            // ADR-0047 section 6 — every photo and video keeps its micro
            // thumbnail here; ``ComposerTray/refs`` leaves it only on the
            // message's first photo or video, so removing the first item
            // hands the role to the next one.
            ref.inlineThumb = InlineThumb.bounded(prepared.inlineThumb)
            return ref
        } catch {
            // The preview went up, the file did not: the preview is an orphan.
            if let thumbRef { BlobLifecycle.shared.delete([thumbRef.blobId]) }
            throw error
        }
    }

    @MainActor
    private static func uploadFile(
        _ data: Data, name: String, mime: String?, fileURL: URL? = nil, idleDays: UInt8?,
        report: @MainActor (ComposerTrayEvent) -> Void, progress: @escaping @Sendable (Double) -> Void
    ) async throws -> AttachmentRef {
        // Section 4 — a file over the cap is not added.
        guard data.count <= AttachmentBridge.maxAttachmentBytes else {
            throw ComposerTrayRefusal(text: .key("attach.preview.file_too_big"))
        }
        // ADR-0047 section 6 — the core keeps the micro thumbnail on the
        // first ref whose MIME is an inline image or video, however it was
        // added: a JPEG or MP4 picked through File, pasted or dropped can
        // be that ref, so it gets one too.
        let inline = await inlineThumb(forFile: data, mime: mime, fileURL: fileURL)
        report(.payload(ComposerTrayPayload(
            input: OutboxMediaInput(
                bytes: data, name: name, mime: mime ?? "application/octet-stream",
                meta: MediaAttachmentMeta(kind: nil), inlineThumb: inline),
            fileURL: nil)))
        report(.prepared(kind: .file, name: name, size: UInt64(data.count), durationMs: nil, thumbnail: nil))
        var ref = try await AttachmentBridge.encryptAndUpload(
            bytes: data, mime: mime, name: name, idleDays: idleDays, onProgress: progress)
        if InlineMedia.decision(for: ref).bubble != .file {
            MediaSessionCache.shared.store(data, key: MediaSessionCache.key(for: ref))
        }
        ref.inlineThumb = inline
        return ref
    }

    /// The micro thumbnail of a file whose MIME is an inline image or video;
    /// `nil` for anything else, or when none can be made or fits.
    static func inlineThumb(forFile data: Data, mime: String?, fileURL: URL? = nil) async -> Data? {
        switch InlineMedia.inlineType(mime: mime) {
        case .image?:
            return await Task.detached(priority: .utility) { InlineThumb.make(fromEncoded: data) }.value
        case .video?:
            if let fileURL { return await InlineThumb.make(fromVideo: fileURL) }
            // The bytes only: a temporary copy for the frame reader, removed
            // at once (under the media temp parent the launch sweep clears).
            let dir = MediaTempFiles.defaultParent
                .appendingPathComponent("inline-thumb-\(UUID().uuidString)", isDirectory: true)
            defer { try? FileManager.default.removeItem(at: dir) }
            let file = dir.appendingPathComponent("video.mp4")
            do {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                try data.write(to: file)
            } catch {
                return nil
            }
            return await InlineThumb.make(fromVideo: file)
        default:
            return nil
        }
    }

    /// Attachments UX section 6 — an audio file's own tags (title, artist,
    /// duration, cover art as a 320-px thumbnail), uploaded as `audio`.
    @MainActor
    private static func uploadAudio(
        _ url: URL, idleDays: UInt8?,
        report: @MainActor (ComposerTrayEvent) -> Void, progress: @escaping @Sendable (Double) -> Void
    ) async throws -> AttachmentRef {
        let didStart = url.startAccessingSecurityScopedResource()
        defer { if didStart { url.stopAccessingSecurityScopedResource() } }
        guard let bytes = try? Data(contentsOf: url) else {
            throw ComposerTrayRefusal(text: .key("audio.unplayable"))
        }
        guard bytes.count <= AttachmentBridge.maxAttachmentBytes else {
            throw ComposerTrayRefusal(text: .key("attach.preview.file_too_big"))
        }
        let tags = await AudioTags.read(url: url)
        let limits = contentLimits()
        let durationMs = tags.durationMs.map { min($0, limits.maxAudioDurationMs) }
        let title = AudioTags.bounded(tags.title ?? url.deletingPathExtension().lastPathComponent, limits)
        let artist = tags.artist.flatMap { AudioTags.bounded($0, limits) }
        report(.payload(ComposerTrayPayload(
            input: OutboxMediaInput(
                bytes: bytes, name: url.lastPathComponent,
                mime: UTTypeMime.of(url) ?? "application/octet-stream",
                meta: MediaAttachmentMeta(kind: "audio", durationMs: durationMs),
                poster: tags.cover?.data, posterWidth: tags.cover?.width, posterHeight: tags.cover?.height,
                title: title, artist: artist),
            fileURL: nil)))
        report(.prepared(kind: .file, name: url.lastPathComponent, size: UInt64(bytes.count), durationMs: nil, thumbnail: nil))
        var thumb: ThumbRef?
        if let cover = tags.cover {
            thumb = try? await AttachmentBridge.encryptAndUploadThumb(
                bytes: cover.data, mime: "image/jpeg", width: cover.width, height: cover.height)
        }
        var ref = try await AttachmentBridge.encryptAndUpload(
            bytes: bytes, mime: UTTypeMime.of(url), name: url.lastPathComponent,
            media: MediaAttachmentMeta(
                kind: "audio",
                durationMs: durationMs,
                width: nil, height: nil, thumb: thumb),
            idleDays: idleDays, onProgress: progress)
        ref.title = title
        ref.artist = artist
        MediaSessionCache.shared.store(bytes, key: MediaSessionCache.key(for: ref))
        return ref
    }
}
