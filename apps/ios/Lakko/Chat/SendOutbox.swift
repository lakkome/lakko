// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `docs/ux/connection-state.md` section 7.3 — the account's OUTBOX on this
/// device: every unsent message (waiting or `Not sent`), with what is needed to
/// send it again after the conversation was left or the app restarted.
///
/// Laid out like the held-send queue (``HeldSendQueue``), one sealed file per
/// (account, conversation), plus one sealed file per prepared media file:
///
///   <base>/SendOutbox/<hex of the account id>/<group hex>.outbox
///   <base>/SendOutbox/<hex of the account id>/files/<file id>.bin
///
/// under Application Support, excluded from backup, written atomically. An
/// entry is message plaintext and a prepared file is the person's recording,
/// so neither is ever written as it is: every write goes through the
/// ``HeldSendQueue/Sealer`` the caller supplies (the device-key wrap the
/// message log uses) and is refused once the account is wiped (ADR-0077 store
/// gate). A file that does not open reads as empty. Never sent to a server,
/// never in the backup, the vault or the DeviceGroup.
public final class SendOutbox {

    /// What an entry is waiting for (section 7.1).
    public enum State: String, Codable, Equatable {
        /// It goes by itself when the servers answer again.
        case waiting
        /// `Not sent · Retry`: only the person's tap sends it again.
        case notSent
    }

    /// Section 7.3 — one attachment of a message whose upload has not
    /// finished: the file the upload sends (the bytes after any resizing or
    /// re-encoding, in ``fileId``'s sealed file), with its name, type and
    /// media metadata, and where it sits among the message's attachments.
    public struct PreparedMedia: Codable, Equatable {
        /// Its index in the message's attachments (the others are the
        /// references of uploads that finished).
        public var slot: Int
        /// The sealed file holding the bytes; empty while a GIF has not been
        /// fetched yet.
        public var fileId: String
        public var name: String
        public var mime: String
        public let kind: String?
        public let durationMs: UInt32?
        public let waveform: Data?
        public let width: UInt32?
        public let height: UInt32?
        /// The preview uploaded beside it (a video note's poster, a photo's
        /// or video's thumbnail, an audio file's cover), a JPEG in its own
        /// sealed file, and its size.
        public let posterFileId: String?
        public let posterWidth: UInt32?
        public let posterHeight: UInt32?
        /// The preview's reference once it went up, kept so a later attempt
        /// (also after a restart) neither uploads it again nor leaves it
        /// behind.
        public var uploadedPoster: PersistedThumb?
        /// ADR-0047 section 6 — the micro thumbnail the reference carries.
        public let inlineThumb: Data?
        /// ADR-0092 section 2 — an audio file's tags.
        public let title: String?
        public let artist: String?
        /// A GIF picked from the proxy: the proxy path its bytes are fetched
        /// from, as the first step of its upload (`fileId` empty until then).
        public var gifSourceURL: String?

        public init(
            slot: Int = 0, fileId: String, name: String, mime: String, kind: String?,
            durationMs: UInt32? = nil, waveform: Data? = nil,
            width: UInt32? = nil, height: UInt32? = nil,
            posterFileId: String? = nil, posterWidth: UInt32? = nil, posterHeight: UInt32? = nil,
            uploadedPoster: PersistedThumb? = nil, inlineThumb: Data? = nil,
            title: String? = nil, artist: String? = nil,
            gifSourceURL: String? = nil
        ) {
            self.slot = slot
            self.fileId = fileId
            self.name = name
            self.mime = mime
            self.kind = kind
            self.durationMs = durationMs
            self.waveform = waveform
            self.width = width
            self.height = height
            self.posterFileId = posterFileId
            self.posterWidth = posterWidth
            self.posterHeight = posterHeight
            self.uploadedPoster = uploadedPoster
            self.inlineThumb = inlineThumb
            self.title = title
            self.artist = artist
            self.gifSourceURL = gifSourceURL
        }
    }

    /// One unsent message.
    public struct Entry: Codable, Equatable {
        /// The client's local id of the row (unique within the conversation).
        public let localId: Int64
        /// When it was written, Unix milliseconds: the order.
        public let writtenAtMs: Int64
        public var state: State
        public var autoSendAttempts: Int
        /// The row as the message log stores a message: text, attachments
        /// (the references of a finished upload), reply and forward
        /// snapshots, content fields. `messageId` is 0.
        public var row: PersistedMessage
        /// The attachments whose upload has not finished; empty once every
        /// reference exists.
        public var media: [PreparedMedia]

        public init(
            localId: Int64, writtenAtMs: Int64, state: State, autoSendAttempts: Int,
            row: PersistedMessage, media: [PreparedMedia] = []
        ) {
            self.localId = localId
            self.writtenAtMs = writtenAtMs
            self.state = state
            self.autoSendAttempts = autoSendAttempts
            self.row = row
            self.media = media
        }
    }

    static let directoryName = "SendOutbox"
    static let filesDirectoryName = "files"
    static let fileExtension = "outbox"
    static let fileVersion = 1

    private struct FileBody: Codable {
        let version: Int
        let entries: [Entry]
    }

    /// One lock for every instance over the same directory.
    private static let lock = NSLock()

    private let ownerAccountId: String
    private let baseDirectory: URL?
    private let sealer: HeldSendQueue.Sealer
    private let fileManager: FileManager

    public init(
        ownerAccountId: String,
        sealer: HeldSendQueue.Sealer,
        baseDirectory: URL? = nil,
        fileManager: FileManager = .default
    ) {
        self.ownerAccountId = ownerAccountId
        self.sealer = sealer
        self.fileManager = fileManager
        self.baseDirectory = baseDirectory ?? HeldSendQueue.defaultBaseDirectory(fileManager: fileManager)
    }

    // MARK: - Entries

    /// Replace the conversation's entries with `entries` (in written order).
    /// An empty list removes the file. Returns whether the disk now holds it.
    @discardableResult
    public func save(conversationHex: String, entries: [Entry]) -> Bool {
        Self.lock.withLock {
            guard let url = entriesURL(conversationHex) else { return false }
            if entries.isEmpty { return removeItem(url) }
            return writeSealed(
                try? JSONEncoder().encode(FileBody(version: Self.fileVersion, entries: entries)),
                to: url)
        }
    }

    /// The conversation's entries, in written order.
    public func list(conversationHex: String) -> [Entry] {
        Self.lock.withLock {
            guard let url = entriesURL(conversationHex) else { return [] }
            return readEntries(url)
        }
    }

    /// Every conversation with entries.
    public func all() -> [(conversationHex: String, entries: [Entry])] {
        Self.lock.withLock {
            guard let dir = accountDirectory(),
                  let names = try? fileManager.contentsOfDirectory(atPath: dir.path) else { return [] }
            var out: [(conversationHex: String, entries: [Entry])] = []
            for name in names.sorted() {
                let url = dir.appendingPathComponent(name)
                guard url.pathExtension == Self.fileExtension else { continue }
                let hex = url.deletingPathExtension().lastPathComponent
                guard HeldSendQueue.isConversationId(hex) else { continue }
                let entries = readEntries(url)
                if !entries.isEmpty { out.append((conversationHex: hex, entries: entries)) }
            }
            return out
        }
    }

    // MARK: - Prepared files

    /// A fresh file id.
    public static func newFileId() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        for i in bytes.indices { bytes[i] = UInt8.random(in: 0...255) }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// Store a prepared file's bytes under `id`, sealed.
    @discardableResult
    public func writeFile(id: String, bytes: Data) -> Bool {
        Self.lock.withLock {
            guard let url = fileURL(id) else { return false }
            return writeSealed(bytes, to: url)
        }
    }

    /// The bytes of a prepared file, or `nil` when missing or unopenable.
    public func readFile(id: String) -> Data? {
        Self.lock.withLock {
            guard let url = fileURL(id), let sealed = try? Data(contentsOf: url) else { return nil }
            return try? sealer.open(sealed)
        }
    }

    public func hasFile(id: String) -> Bool {
        Self.lock.withLock {
            guard let url = fileURL(id) else { return false }
            return fileManager.fileExists(atPath: url.path)
        }
    }

    @discardableResult
    public func removeFile(id: String) -> Bool {
        Self.lock.withLock {
            guard let url = fileURL(id) else { return false }
            return removeItem(url)
        }
    }

    /// Remove every prepared file no entry names (a file left by a write that
    /// did not complete, or one whose entry was deleted).
    public func removeOrphanFiles() {
        removeFiles(notNamedIn: Set(all().flatMap { $0.entries }.flatMap { $0.media }.flatMap {
            [$0.fileId, $0.posterFileId].compactMap { $0 }
        }))
    }

    /// Remove every prepared file whose id is not in `named`.
    public func removeFiles(notNamedIn named: Set<String>) {
        Self.lock.withLock {
            guard let dir = filesDirectory(),
                  let names = try? fileManager.contentsOfDirectory(atPath: dir.path) else { return }
            for name in names {
                let id = (name as NSString).deletingPathExtension
                if !named.contains(id) { _ = removeItem(dir.appendingPathComponent(name)) }
            }
        }
    }

    // MARK: - Wipe

    /// Drop the account's whole outbox (its data is wiped). Needs no sealer.
    @discardableResult
    public static func clearAccount(
        ownerAccountId: String, baseDirectory: URL? = nil, fileManager: FileManager = .default
    ) -> Bool {
        lock.withLock {
            guard let base = baseDirectory ?? HeldSendQueue.defaultBaseDirectory(fileManager: fileManager)
            else { return false }
            let dir = base.appendingPathComponent(directoryName, isDirectory: true)
                .appendingPathComponent(HeldSendQueue.accountDirectoryName(ownerAccountId), isDirectory: true)
            guard fileManager.fileExists(atPath: dir.path) else { return true }
            return (try? fileManager.removeItem(at: dir)) != nil
        }
    }

    // MARK: - Layout

    private func accountDirectory() -> URL? {
        guard let baseDirectory, !ownerAccountId.isEmpty else { return nil }
        return baseDirectory.appendingPathComponent(Self.directoryName, isDirectory: true)
            .appendingPathComponent(HeldSendQueue.accountDirectoryName(ownerAccountId), isDirectory: true)
    }

    private func filesDirectory() -> URL? {
        accountDirectory()?.appendingPathComponent(Self.filesDirectoryName, isDirectory: true)
    }

    private func entriesURL(_ conversationHex: String) -> URL? {
        guard HeldSendQueue.isConversationId(conversationHex), let dir = accountDirectory() else { return nil }
        return dir.appendingPathComponent(conversationHex).appendingPathExtension(Self.fileExtension)
    }

    private func fileURL(_ id: String) -> URL? {
        guard HeldSendQueue.isConversationId(id), let dir = filesDirectory() else { return nil }
        return dir.appendingPathComponent(id).appendingPathExtension("bin")
    }

    // MARK: - Disk

    private func readEntries(_ url: URL) -> [Entry] {
        guard let sealed = try? Data(contentsOf: url),
              let raw = try? sealer.open(sealed),
              let body = try? JSONDecoder().decode(FileBody.self, from: raw),
              body.version == Self.fileVersion else { return [] }
        var seen = Set<Int64>()
        return body.entries.filter { $0.localId != 0 && seen.insert($0.localId).inserted }
    }

    private func writeSealed(_ raw: Data?, to url: URL) -> Bool {
        guard let raw else { return false }
        // Nothing is written back for a wiped account.
        guard let gate = AccountStoreGate.enterUnlessWiped(ownerAccountId) else { return false }
        defer { gate.leave() }
        do {
            try fileManager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            if let baseDirectory {
                Self.excludeFromBackup(baseDirectory.appendingPathComponent(Self.directoryName, isDirectory: true))
            }
            let sealed = try sealer.seal(raw)
            try sealed.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            Self.excludeFromBackup(url)
            return true
        } catch {
            return false
        }
    }

    private func removeItem(_ url: URL) -> Bool {
        guard fileManager.fileExists(atPath: url.path) else { return true }
        return (try? fileManager.removeItem(at: url)) != nil
    }

    private static func excludeFromBackup(_ url: URL) {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }
}
