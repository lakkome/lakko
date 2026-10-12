// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 R.7 case 1 - the DURABLE queue of held messages.
//
// iOS mirror of `apps/web/src/chat/held-send-queue.ts`. `HeldSend` holds the
// rules and the in-memory store type; this is what keeps a held message
// across a relaunch and lets the background flush it when no view of the
// conversation is on screen.
//
// Web mirrors the whole queue into the message log (IndexedDB, under the
// device key). Here it is one file per (owner account, conversation):
//
//   <base>/HeldSend/<hex of the owner account id>/<conversation hex>.held
//
// under Application Support, excluded from backup, written atomically. A held
// message is message PLAINTEXT, so the file body is never written as it is:
// every write goes through the `Sealer` the caller supplies (the device-key
// wrap the message log uses), and a file that does not open reads as empty.
// Nothing here is ever sent in its stored form.
//
// What a relaunch still loses: nothing of a held message itself (its text,
// its uploaded attachment references, its reply and forward attributions are
// all restored). Its give-up clock restarts with the process (`restored`),
// and a message that had already shown the failed state (which takes it out
// of the queue; its Retry lives in the row) is not kept. A media message
// whose bytes were still UPLOADING when the process went is lost, exactly as
// it is in a conversation that is not held: there is no attachment reference
// to keep yet.
public final class HeldSendQueue {

    /// One held message as stored.
    ///
    /// `attachments` and `forward` are OPAQUE to this module: one serialized
    /// payload per uploaded attachment reference, and one for the forward
    /// attribution, in whatever encoding the caller chose.
    public struct Entry: Equatable, Codable {
        /// The sender's id of the local row (unique within a conversation).
        public let localId: String
        /// When it was queued, Unix milliseconds.
        public let queuedAtMs: Int64
        public let text: String
        public let attachments: [Data]
        /// The reply reference and its quote snapshot, when it is a reply.
        public let reply: HeldSend.Reply?
        /// The group (standard base64) `reply.target` is a message id of: the
        /// group the reply was composed in. A flush into any other group
        /// keeps the quote and drops the jump target
        /// (`HeldSend.replyForSend`). Never stored without a reply.
        public let replyGroupIdB64: String?
        /// ADR-0052: set for a forward INTO the held conversation.
        public let forward: Data?

        public init(
            localId: String,
            queuedAtMs: Int64,
            text: String,
            attachments: [Data] = [],
            reply: HeldSend.Reply? = nil,
            replyGroupIdB64: String? = nil,
            forward: Data? = nil
        ) {
            self.localId = localId
            self.queuedAtMs = queuedAtMs
            self.text = text
            self.attachments = attachments
            self.reply = reply
            // A message with no reply stores no group; an empty one is none.
            if reply != nil, let replyGroupIdB64, !replyGroupIdB64.isEmpty {
                self.replyGroupIdB64 = replyGroupIdB64
            } else {
                self.replyGroupIdB64 = nil
            }
            self.forward = forward
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            self.init(
                localId: try c.decode(String.self, forKey: .localId),
                queuedAtMs: try c.decode(Int64.self, forKey: .queuedAtMs),
                text: try c.decode(String.self, forKey: .text),
                attachments: try c.decode([Data].self, forKey: .attachments),
                reply: try c.decodeIfPresent(HeldSend.Reply.self, forKey: .reply),
                replyGroupIdB64: try c.decodeIfPresent(String.self, forKey: .replyGroupIdB64),
                forward: try c.decodeIfPresent(Data.self, forKey: .forward))
        }

        /// Whether this is a message at all: junk is dropped, never sent.
        ///
        /// | local id | text  | attachments | valid |
        /// |----------|-------|-------------|-------|
        /// | empty    | any   | any         | no    |
        /// | set      | empty | none        | no    |
        /// | set      | set   | any         | yes   |
        /// | set      | any   | some        | yes   |
        public var isValid: Bool {
            !localId.isEmpty && !(text.isEmpty && attachments.isEmpty)
        }

        /// The same message with its give-up clock restarted.
        public func requeued(atMs nowMs: Int64) -> Entry {
            Entry(
                localId: localId, queuedAtMs: nowMs, text: text, attachments: attachments,
                reply: reply, replyGroupIdB64: replyGroupIdB64, forward: forward)
        }
    }

    /// How a file body is protected at rest. `seal` wraps the JSON before it
    /// is written; `open` unwraps what was read. Either may throw: a failed
    /// seal writes nothing, a failed open reads as "nothing held".
    public struct Sealer {
        public let seal: (Data) throws -> Data
        public let open: (Data) throws -> Data

        public init(seal: @escaping (Data) throws -> Data, open: @escaping (Data) throws -> Data) {
            self.seal = seal
            self.open = open
        }
    }

    /// A conversation id is the lowercase hex of the lineage root: the rule
    /// web applies to a stored row, and what makes it a safe file name.
    public static func isConversationId(_ conversationHex: String) -> Bool {
        !conversationHex.isEmpty && conversationHex.utf8.allSatisfy {
            ($0 >= 0x30 && $0 <= 0x39) || ($0 >= 0x61 && $0 <= 0x66)
        }
    }

    static let directoryName = "HeldSend"
    static let fileExtension = "held"
    static let fileVersion = 1

    private struct FileBody: Codable {
        let version: Int
        let entries: [Entry]
    }

    /// One lock for every instance: two queues over the same directory (the
    /// view's and the background's) must not interleave a read-modify-write.
    private static let lock = NSLock()

    private let ownerAccountId: String
    private let baseDirectory: URL?
    private let sealer: Sealer
    private let fileManager: FileManager

    /// - Parameters:
    ///   - ownerAccountId: the signed-in account the held messages belong to.
    ///   - sealer: the at-rest protection of the file body (the device-key
    ///     wrap). Required: there is no plaintext default.
    ///   - baseDirectory: where the `HeldSend` directory lives; `nil` is
    ///     Application Support. Injected by tests.
    public init(
        ownerAccountId: String,
        sealer: Sealer,
        baseDirectory: URL? = nil,
        fileManager: FileManager = .default
    ) {
        self.ownerAccountId = ownerAccountId
        self.sealer = sealer
        self.fileManager = fileManager
        self.baseDirectory = baseDirectory ?? Self.defaultBaseDirectory(fileManager: fileManager)
    }

    /// Application Support, or `nil` when it cannot be resolved (every
    /// operation then stores nothing and reads nothing).
    public static func defaultBaseDirectory(fileManager: FileManager = .default) -> URL? {
        try? fileManager.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
    }

    // MARK: - Operations

    /// Add one message at the END of the conversation's queue. Returns
    /// whether it is stored now. Refused (nothing written): an invalid entry,
    /// a conversation id that is not one, a local id the queue already holds
    /// (the first is kept, so a repeated enqueue is harmless), and a failed
    /// write.
    @discardableResult
    public func enqueue(conversationHex: String, entry: Entry) -> Bool {
        guard entry.isValid else { return false }
        return Self.lock.withLock {
            guard let url = fileURL(conversationHex) else { return false }
            var entries = read(url)
            if entries.contains(where: { $0.localId == entry.localId }) { return false }
            entries.append(entry)
            return write(entries, to: url)
        }
    }

    /// The conversation's held messages, in the order they were queued.
    public func list(conversationHex: String) -> [Entry] {
        Self.lock.withLock {
            guard let url = fileURL(conversationHex) else { return [] }
            return read(url)
        }
    }

    /// How many are held for the conversation.
    public func count(conversationHex: String) -> Int {
        list(conversationHex: conversationHex).count
    }

    /// Remove one message after its send was CONFIRMED (or after it showed
    /// the failed state). The others keep their order. Returns whether the
    /// queue no longer holds it on disk: `true` also when it never did,
    /// `false` only when the write failed (the caller must not treat the
    /// message as gone from disk).
    @discardableResult
    public func remove(conversationHex: String, localId: String) -> Bool {
        Self.lock.withLock {
            guard let url = fileURL(conversationHex) else { return false }
            let entries = read(url)
            let kept = entries.filter { $0.localId != localId }
            if kept.count == entries.count { return true }
            return write(kept, to: url)
        }
    }

    /// Drop everything held for one conversation.
    @discardableResult
    public func clear(conversationHex: String) -> Bool {
        Self.lock.withLock {
            guard let url = fileURL(conversationHex) else { return false }
            return removeFile(url)
        }
    }

    /// Every conversation with something held, with its messages in order.
    /// Conversations are ordered by their OLDEST held message (then by id),
    /// so the background flushes the longest-waiting conversation first.
    public func all() -> [(conversationHex: String, entries: [Entry])] {
        Self.lock.withLock {
            guard let dir = accountDirectory(),
                  let names = try? fileManager.contentsOfDirectory(atPath: dir.path) else { return [] }
            var out: [(conversationHex: String, entries: [Entry])] = []
            for name in names {
                let url = dir.appendingPathComponent(name)
                guard url.pathExtension == Self.fileExtension else { continue }
                let conversationHex = url.deletingPathExtension().lastPathComponent
                guard Self.isConversationId(conversationHex) else { continue }
                let entries = read(url)
                if !entries.isEmpty { out.append((conversationHex: conversationHex, entries: entries)) }
            }
            return out.sorted { a, b in
                let ta = a.entries.first?.queuedAtMs ?? 0
                let tb = b.entries.first?.queuedAtMs ?? 0
                return ta == tb ? a.conversationHex < b.conversationHex : ta < tb
            }
        }
    }

    /// What a relaunch reads back: `all()` with every give-up clock restarted
    /// at `nowMs` (a message read back is in the ordinary sending state
    /// again, not failed on arrival). The disk is not rewritten.
    public func restored(nowMs: Int64) -> [(conversationHex: String, entries: [Entry])] {
        all().map { (conversationHex: $0.conversationHex, entries: $0.entries.map { $0.requeued(atMs: nowMs) }) }
    }

    /// Drop everything held for this queue's account.
    @discardableResult
    public func clearAccount() -> Bool {
        Self.clearAccount(
            ownerAccountId: ownerAccountId, baseDirectory: baseDirectory, fileManager: fileManager)
    }

    /// Drop everything held for an account (its data is wiped). Needs no
    /// sealer: nothing is read.
    @discardableResult
    public static func clearAccount(
        ownerAccountId: String, baseDirectory: URL? = nil, fileManager: FileManager = .default
    ) -> Bool {
        lock.withLock {
            guard let base = baseDirectory ?? defaultBaseDirectory(fileManager: fileManager) else { return false }
            let dir = accountDirectory(base: base, ownerAccountId: ownerAccountId)
            guard fileManager.fileExists(atPath: dir.path) else { return true }
            return (try? fileManager.removeItem(at: dir)) != nil
        }
    }

    // MARK: - Layout

    /// The account's directory name: the hex of the account id's UTF-8 bytes
    /// (a base64 id holds `/` and `+`; hex holds neither and cannot collide).
    static func accountDirectoryName(_ ownerAccountId: String) -> String {
        let digits = Array("0123456789abcdef".utf8)
        var out = [UInt8]()
        out.reserveCapacity(ownerAccountId.utf8.count * 2)
        for byte in ownerAccountId.utf8 {
            out.append(digits[Int(byte >> 4)])
            out.append(digits[Int(byte & 0x0f)])
        }
        return String(decoding: out, as: UTF8.self)
    }

    static func accountDirectory(base: URL, ownerAccountId: String) -> URL {
        base.appendingPathComponent(directoryName, isDirectory: true)
            .appendingPathComponent(accountDirectoryName(ownerAccountId), isDirectory: true)
    }

    private func accountDirectory() -> URL? {
        guard let baseDirectory, !ownerAccountId.isEmpty else { return nil }
        return Self.accountDirectory(base: baseDirectory, ownerAccountId: ownerAccountId)
    }

    private func fileURL(_ conversationHex: String) -> URL? {
        guard Self.isConversationId(conversationHex), let dir = accountDirectory() else { return nil }
        return dir.appendingPathComponent(conversationHex).appendingPathExtension(Self.fileExtension)
    }

    // MARK: - Disk

    /// A missing, unopenable or unreadable file is "nothing held"; an entry
    /// that is not a message is dropped.
    private func read(_ url: URL) -> [Entry] {
        guard let sealed = try? Data(contentsOf: url),
              let raw = try? sealer.open(sealed),
              let body = try? JSONDecoder().decode(FileBody.self, from: raw),
              body.version == Self.fileVersion else { return [] }
        var seen = Set<String>()
        return body.entries.filter { $0.isValid && seen.insert($0.localId).inserted }
    }

    private func write(_ entries: [Entry], to url: URL) -> Bool {
        if entries.isEmpty { return removeFile(url) }
        do {
            let dir = url.deletingLastPathComponent()
            try fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
            if let baseDirectory {
                Self.excludeFromBackup(baseDirectory.appendingPathComponent(Self.directoryName, isDirectory: true))
            }
            let raw = try JSONEncoder().encode(FileBody(version: Self.fileVersion, entries: entries))
            let sealed = try sealer.seal(raw)
            try sealed.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            Self.excludeFromBackup(url)
            return true
        } catch {
            return false
        }
    }

    private func removeFile(_ url: URL) -> Bool {
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

// MARK: - What the background flush sent

/// One background flush of one conversation: the group the messages went into
/// and what they became, in send order.
public struct HeldFlushReport: Equatable {

    /// One sent message: the queue entry it was and the row it became.
    public struct Message: Equatable {
        /// The held entry's local id.
        public let localId: String
        /// The DS message id it was stored under in the group.
        public let messageId: Int64

        public init(localId: String, messageId: Int64) {
            self.localId = localId
            self.messageId = messageId
        }
    }

    /// The group (standard base64) the messages were sent into.
    public let groupIdB64: String
    public let messages: [Message]

    public init(groupIdB64: String, messages: [Message]) {
        self.groupIdB64 = groupIdB64
        self.messages = messages
    }
}

/// Hear every background flush. A chat view subscribes BEFORE it reads the
/// log, so a flush that raced its appearance is either in what it read (the
/// flush reports after its log write) or reported to it here;
/// `HeldSend.flushedRowsToShow` removes the overlap.
public final class HeldFlushHub {

    /// The process's hub.
    public static let shared = HeldFlushHub()

    private let lock = NSLock()
    private var nextToken = 0
    private var listeners: [(token: Int, listener: (HeldFlushReport) throws -> Void)] = []

    public init() {}

    /// Subscribe. The returned closure unsubscribes (calling it twice is
    /// harmless). Listeners are called on the thread that reports, in the
    /// order they subscribed.
    public func subscribe(_ listener: @escaping (HeldFlushReport) throws -> Void) -> () -> Void {
        let token: Int = lock.withLock {
            nextToken += 1
            listeners.append((token: nextToken, listener: listener))
            return nextToken
        }
        return { [weak self] in
            guard let self else { return }
            self.lock.withLock { self.listeners.removeAll { $0.token == token } }
        }
    }

    /// Report one flush. Called by the background flush after its log write.
    /// An empty flush reports nothing; one listener's failure is its own and
    /// does not stop the next.
    public func notify(_ report: HeldFlushReport) {
        if report.messages.isEmpty { return }
        let snapshot = lock.withLock { listeners }
        for entry in snapshot {
            try? entry.listener(report)
        }
    }
}
