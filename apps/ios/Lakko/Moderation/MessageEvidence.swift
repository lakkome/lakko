// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What a report needs of one received chat message (ADR-0099 section 1):
/// the attestation verdict, the attested plaintext exactly as it was
/// decrypted, the sender leaf's signature key and the leaf's ADR-0050 device
/// binding. All of it is read at receipt, because a leaf can be handed to
/// someone else after the next commit (C-68).
public struct MessageEvidence: Codable, Equatable {
    /// The signature check of the plaintext against the sender leaf key.
    public enum Verdict: String, Codable, Equatable {
        case verified, failed, absent

        init(_ ffi: FfiAttestation) {
            switch ffi {
            case .verified: self = .verified
            case .failed: self = .failed
            case .absent: self = .absent
            }
        }
    }

    public let attestation: Verdict
    /// The decrypted plaintext, wrapper included.
    public let plaintext: Data
    /// The sender leaf's 32-byte signature key.
    public let leafKey: Data
    /// The leaf's binding bytes, empty when it carries none.
    public let binding: Data

    public init(attestation: Verdict, plaintext: Data, leafKey: Data, binding: Data) {
        self.attestation = attestation
        self.plaintext = plaintext
        self.leafKey = leafKey
        self.binding = binding
    }

    /// The report evidence item for this message in `groupId`.
    public func item(groupId: Data) -> FfiEvidenceItem {
        .message(groupId: groupId, plaintext: plaintext, leafKey: leafKey, binding: binding)
    }
}

/// A small per-account JSON value sealed under the message log's Keychain
/// wrap key and kept in this app's own Application Support, excluded from
/// the device backup. Never in the App Group: nothing outside the app reads
/// it.
struct DeviceLocalSealedFile<Value: Codable> {
    let directory: String
    let fallback: Value

    private func url(_ accountIdB64: String) -> URL? {
        guard let support = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true) else { return nil }
        return support.appendingPathComponent(directory)
            .appendingPathComponent(accountIdB64.replacingOccurrences(of: "/", with: "_") + ".bin")
    }

    func read(accountIdB64: String) -> Value {
        guard let url = url(accountIdB64), let sealed = try? Data(contentsOf: url),
              let plain = try? MessageLogStore.openUnderWrapKey(sealed),
              let value = try? JSONDecoder().decode(Value.self, from: plain) else { return fallback }
        return value
    }

    func write(accountIdB64: String, _ value: Value) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let url = url(accountIdB64),
              let plain = try? JSONEncoder().encode(value),
              let sealed = try? MessageLogStore.sealUnderWrapKey(plain) else { return }
        let dir = url.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        SharedStorage.excludeFromBackup(dir)
        try? sealed.write(to: url, options: .atomic)
        SharedStorage.excludeFromBackup(url)
    }

    func delete(accountIdB64: String) {
        if let url = url(accountIdB64) { try? FileManager.default.removeItem(at: url) }
    }
}

/// The evidence of received chat messages, per account, by conversation and
/// DS message id. Kept beside the message log rather than in its rows so no
/// copy of a ``ChatMessage`` can drop it. Bounded per conversation: the
/// newest ``maxPerConversation`` messages are kept, which is far more than a
/// report can carry (20).
@MainActor
public final class MessageEvidenceStore {
    public static let shared = MessageEvidenceStore()

    /// Chosen, not measured.
    public static let maxPerConversation = 400

    typealias Table = [String: [String: MessageEvidence]]
    private let file = DeviceLocalSealedFile<Table>(directory: "lakko-message-evidence", fallback: [:])
    private var cache: (account: String, table: Table)?
    private var writeScheduled = false

    init() {}

    private func table(_ accountIdB64: String) -> Table {
        if let cache, cache.account == accountIdB64 { return cache.table }
        let read = file.read(accountIdB64: accountIdB64)
        cache = (accountIdB64, read)
        return read
    }

    /// Keep `evidence` for the messages of `groupIdHex` it names.
    public func put(accountIdB64: String, groupIdHex: String, _ evidence: [Int64: MessageEvidence]) {
        guard !evidence.isEmpty else { return }
        var t = table(accountIdB64)
        var rows = t[groupIdHex] ?? [:]
        for (id, value) in evidence { rows[String(id)] = value }
        t[groupIdHex] = Self.bounded(rows)
        cache = (accountIdB64, t)
        scheduleWrite(accountIdB64)
    }

    /// The evidence of one message, or `nil` when it is no longer held.
    public func get(accountIdB64: String, groupIdHex: String, messageId: Int64) -> MessageEvidence? {
        table(accountIdB64)[groupIdHex]?[String(messageId)]
    }

    /// Forget a conversation (its transcript was deleted from this device).
    public func deleteConversation(accountIdB64: String, groupIdHex: String) {
        var t = table(accountIdB64)
        guard t.removeValue(forKey: groupIdHex) != nil else { return }
        cache = (accountIdB64, t)
        file.write(accountIdB64: accountIdB64, t)
    }

    /// Forget everything of the account (Forget device).
    public func deleteAll(accountIdB64: String) {
        if cache?.account == accountIdB64 { cache = nil }
        file.delete(accountIdB64: accountIdB64)
    }

    static func bounded(_ rows: [String: MessageEvidence]) -> [String: MessageEvidence] {
        guard rows.count > maxPerConversation else { return rows }
        let keep = rows.keys.compactMap(Int64.init).sorted(by: >).prefix(maxPerConversation)
        var out: [String: MessageEvidence] = [:]
        for id in keep { out[String(id)] = rows[String(id)] }
        return out
    }

    /// One write per run-loop turn, however many batches arrived in it.
    private func scheduleWrite(_ accountIdB64: String) {
        guard !writeScheduled else { return }
        writeScheduled = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.writeScheduled = false
            guard let cache = self.cache, cache.account == accountIdB64 else { return }
            self.file.write(accountIdB64: accountIdB64, cache.table)
        }
    }
}
