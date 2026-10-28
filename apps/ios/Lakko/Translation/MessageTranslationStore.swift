// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import os

/// ADR-0082 section 5 — the translations, transcripts and per-conversation
/// choices one ACCOUNT keeps on this device.
///
/// What is stored is the text of messages a second time (a translation is
/// what a message says), so it sits under the same protection as the message
/// log: one file per account, AES-256-GCM under the message log's Keychain
/// wrap key (``MessageLogStore/sealUnderWrapKey(_:)``), excluded from the
/// device backup. Unlike the message log it is written only into the app's
/// own Application Support, never into the App Group container: nothing in
/// the notification extension reads it, and ADR-0082 section 7 says nothing
/// in the background translates (C-59).
///
/// Every write goes through ``AccountStoreGate`` (ADR-0077), so a write in
/// flight when the account is forgotten finishes before the wipe and a later
/// one is refused. Never in the backup, the conversation vault or the
/// DeviceGroup synced state (ADR-0082 section 5, UX open question 1).
///
/// A result is keyed on `(conversation, message id, kind)` and carries the
/// op id and the target language it was made for; the shared rule
/// `storedResultIsCurrent` decides whether it still describes the message.
public final class MessageTranslationStore {

    /// What one stored result is.
    public enum Kind: String, Codable, Sendable {
        case translation
        case transcript
        /// A translation of a transcript (UX 4.2: a transcript is text like any other).
        case transcriptTranslation
    }

    public struct Result: Codable, Equatable, Sendable {
        public var text: String
        /// The language the result is in (the target of a translation, the
        /// spoken language of a transcript), canonical.
        public var language: String
        /// The source language of a translation; `nil` for a transcript.
        public var source: String?
        /// The message's last op id when the result was made
        /// (``ChatMessage/lastOpId``, 0 when never edited).
        public var opId: Int64
        /// Whether a transcript is the whole recording.
        public var complete: Bool
    }

    /// What one conversation's person chose on this device.
    public struct ConversationPrefs: Codable, Equatable, Sendable {
        public var autoTranslate = false
        /// The source chosen in the picker for translation (UX 3.4), canonical.
        public var translationSource: String?
        /// The source chosen in the picker for transcription (UX 4.4).
        public var transcriptSource: String?
        /// Languages whose bar was dismissed here (UX 3.6), canonical.
        public var dismissed: [String] = []
        /// Per message: the source chosen for that message alone.
        public var messageSources: [String: String] = [:]

        public init() {}
    }

    struct Payload: Codable {
        var version = 1
        var results: [String: Result] = [:]
        var conversations: [String: ConversationPrefs] = [:]
    }

    private static let subdir = "lakko-message-translations"
    private static let log = Logger(subsystem: "me.lakko", category: "translation-store")

    private let directory: URL?
    private let lock = NSLock()
    /// Decrypted payloads read so far, per account. In memory only.
    private var cache: [String: Payload] = [:]

    public static let shared = MessageTranslationStore()

    init(directory: URL? = MessageTranslationStore.defaultDirectory()) {
        self.directory = directory
    }

    static func defaultDirectory() -> URL? {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent(subdir, isDirectory: true)
    }

    static func resultKey(conversation: String, messageId: Int64, kind: Kind) -> String {
        "\(conversation.lowercased())|\(messageId)|\(kind.rawValue)"
    }

    // MARK: - Reads

    public func result(
        accountIdB64: String, conversation: String, messageId: Int64, kind: Kind
    ) -> Result? {
        lock.lock()
        defer { lock.unlock() }
        return payload(accountIdB64).results[Self.resultKey(conversation: conversation, messageId: messageId, kind: kind)]
    }

    public func prefs(accountIdB64: String, conversation: String) -> ConversationPrefs {
        lock.lock()
        defer { lock.unlock() }
        return payload(accountIdB64).conversations[conversation.lowercased()] ?? ConversationPrefs()
    }

    /// How many results this account keeps (the count the delete-all
    /// confirmation states, UX section 6).
    public func count(accountIdB64: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return payload(accountIdB64).results.count
    }

    // MARK: - Writes

    public func putResult(
        accountIdB64: String, conversation: String, messageId: Int64, kind: Kind, result: Result
    ) {
        mutate(accountIdB64) {
            $0.results[Self.resultKey(conversation: conversation, messageId: messageId, kind: kind)] = result
        }
    }

    public func removeResult(accountIdB64: String, conversation: String, messageId: Int64, kind: Kind) {
        mutate(accountIdB64) {
            $0.results[Self.resultKey(conversation: conversation, messageId: messageId, kind: kind)] = nil
        }
    }

    /// Delete for everyone removed the message: its results go with it
    /// (ADR-0082 section 5).
    public func removeMessage(accountIdB64: String, conversation: String, messageId: Int64) {
        mutate(accountIdB64) { payload in
            for kind in [Kind.translation, .transcript, .transcriptTranslation] {
                payload.results[Self.resultKey(conversation: conversation, messageId: messageId, kind: kind)] = nil
            }
        }
    }

    public func setPrefs(accountIdB64: String, conversation: String, prefs: ConversationPrefs) {
        mutate(accountIdB64) { $0.conversations[conversation.lowercased()] = prefs }
    }

    /// Settings > Delete all translations and transcripts. Returns how many
    /// went. The per-conversation choices stay: they are not translations.
    @discardableResult
    public func deleteAllResults(accountIdB64: String) -> Int {
        var removed = 0
        mutate(accountIdB64) { payload in
            removed = payload.results.count
            payload.results.removeAll()
        }
        return removed
    }

    /// Forget device / Delete account: the file and the cache go
    /// (``AccountLocalDataWipe``, which runs inside the account's seal).
    public func clear(accountIdB64: String) throws {
        lock.lock()
        cache[accountIdB64] = nil
        lock.unlock()
        guard let url = fileURL(accountIdB64) else { return }
        if FileManager.default.fileExists(atPath: url.path) {
            try FileManager.default.removeItem(at: url)
        }
    }

    /// The file holding `accountIdB64`'s results, for the wipe's inventory.
    func fileURL(_ accountIdB64: String) -> URL? {
        directory?.appendingPathComponent(Self.fileName(accountIdB64))
    }

    /// The account id is base64 (it may carry `/`), so the file name is its
    /// URL-safe spelling.
    static func fileName(_ accountIdB64: String) -> String {
        accountIdB64
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "+", with: "-") + ".bin"
    }

    // MARK: - Internals

    private func mutate(_ accountIdB64: String, _ change: (inout Payload) -> Void) {
        guard !accountIdB64.isEmpty, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        lock.lock()
        defer { lock.unlock() }
        var current = payload(accountIdB64)
        change(&current)
        cache[accountIdB64] = current
        write(current, accountIdB64: accountIdB64)
    }

    /// Caller holds `lock`.
    private func payload(_ accountIdB64: String) -> Payload {
        if let cached = cache[accountIdB64] { return cached }
        let loaded = read(accountIdB64) ?? Payload()
        cache[accountIdB64] = loaded
        return loaded
    }

    private func read(_ accountIdB64: String) -> Payload? {
        guard let url = fileURL(accountIdB64),
              let sealed = try? Data(contentsOf: url),
              let plaintext = try? MessageLogStore.openUnderWrapKey(sealed)
        else { return nil }
        return try? JSONDecoder().decode(Payload.self, from: plaintext)
    }

    private func write(_ payload: Payload, accountIdB64: String) {
        guard let directory, let url = fileURL(accountIdB64) else { return }
        do {
            let plaintext = try JSONEncoder().encode(payload)
            let sealed = try MessageLogStore.sealUnderWrapKey(plaintext)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            SharedStorage.excludeFromBackup(directory)
            try sealed.write(to: url, options: [.atomic, .completeFileProtection])
            SharedStorage.excludeFromBackup(url)
        } catch {
            // No text, no language: only that a write failed.
            Self.log.error("translation store write failed")
        }
    }
}

/// The delete-for-everyone hook: ``GroupRecord`` folds a delete op without
/// knowing the account, so the account the chat repository is bound to is
/// kept here (set by ``ChatRepository``) and the purge uses it.
enum MessageTranslationPurge {
    private static let lock = NSLock()
    private static var _boundAccount: String?

    static var boundAccountIdB64: String? {
        get { lock.lock(); defer { lock.unlock() }; return _boundAccount }
        set { lock.lock(); _boundAccount = newValue; lock.unlock() }
    }

    static func messageDeleted(groupId: Data, messageId: Int64) {
        guard let account = boundAccountIdB64 else { return }
        let hex = groupId.map { String(format: "%02x", $0) }.joined()
        MessageTranslationStore.shared.removeMessage(
            accountIdB64: account, conversation: hex, messageId: messageId)
        Task { @MainActor in
            MessageTranslationCenter.shared.forgetMessage(conversation: hex, messageId: messageId)
        }
    }
}
