// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import LakkoCore
import Security

/// Persistent plaintext-message log on iOS (Phase 2a3f1).
///
/// Mirror of (lands alongside):
///
///   * `apps/android/.../chat/MessageLogStore.kt`
///   * `apps/web/src/chat/message-log-store.ts`
///
/// Persists the per-account `{ groupIdHex → (cursor, messages) }`
/// map the in-memory ``ChatRepository`` keeps in
/// ``GroupRecord/messages``. Without this layer the timeline
/// vanishes on every app launch (MLS state survives via
/// ``IdentitySnapshotStore`` but cannot decrypt already-
/// consumed wire bytes — forward secrecy intentionally wipes
/// the chain keys).
///
/// ## Threat model
///
/// The stored bytes ARE plaintext messages — text + attachment
/// refs. They MUST be wrapped under a Keychain-resident key
/// with the same posture as
/// ``DeviceGroupSnapshotStore``: a dedicated alias the OS
/// keeps in the Secure Enclave when the device supports it,
/// AES-256-GCM around the inner serialised JSON.
///
/// ## On-disk format
///
/// ```text
///   ApplicationSupport/lakko-message-logs/
///     <accountIdB64>.bin     12-byte IV || ciphertext || 16-byte tag
/// ```
///
/// Plaintext is the JSON-encoded ``MessageLogPayload``. Format
/// version field (`version: 1`) lets a future migration bump
/// the schema while keeping older blobs decryptable.
///
/// ## Mutation cadence
///
/// Callers (`ChatRepository`) snapshot + write on every
/// mutation that updates `messages` / `cursor` — same hook
/// as the existing `persistSnapshotBestEffort`. The blob is
/// small (a few hundred KB even for power users) so atomic-
/// replace I/O is fine without batching.
public final class MessageLogStore {

    /// Keychain service tag for the AES wrap key. Versioned so
    /// a future rotation has somewhere to go without colliding
    /// with v1 entries on upgrade.
    public static let wrapKeyService: String = "me.lakko.message-log-wrap.v1"

    /// Keychain account marker — one wrap key per app
    /// installation (not per Lakko account). Per-account
    /// isolation comes from the file path, not the wrap key.
    private static let wrapKeyAccount: String = "wrap-key"

    /// Container subdirectory holding the per-account envelopes.
    private static let subdir: String = "lakko-message-logs"

    /// Old app-private location (`Application Support/lakko-message-logs`).
    /// Retained as the read-fallback + migration source per ADR-0041 —
    /// never deleted by the migration.
    private let oldEnvelopeDir: URL?

    public init() {
        self.oldEnvelopeDir = Self.oldEnvelopeDir()
    }

    // MARK: - Public API

    /// Persist the full message log for `accountIdB64`.
    /// Overwrites any prior envelope. Best-effort —
    /// Keychain / FS failures surface as logged-and-swallowed
    /// no-ops at the caller (`ChatRepository` calls this on
    /// every mutation, so the next successful call recovers).
    ///
    /// ADR-0041 — the authoritative copy is written into the App Group
    /// container (NSE-readable), falling back to the old location when
    /// the App Group is unavailable.
    public func putLog(accountIdB64: String, payload: MessageLogPayload) {
        guard let target = writeTarget(accountIdB64: accountIdB64) else { return }
        let plaintext: Data
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            plaintext = try encoder.encode(payload)
        } catch {
            return
        }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let combined = try? AES.GCM.seal(plaintext, using: wrapKey).combined else { return }

        do {
            try FileManager.default.createDirectory(
                at: target.dir,
                withIntermediateDirectories: true
            )
            SharedStorage.excludeFromBackup(target.dir)
        } catch {
            // best-effort
        }
        do {
            try combined.write(to: target.url, options: .atomic)
            SharedStorage.excludeFromBackup(target.url)
        } catch {
            // best-effort
        }
    }

    /// Read + unwrap the log for `accountIdB64`. Returns `nil`
    /// when:
    ///   * no envelope on disk for this account,
    ///   * Keychain access denied / wrap key rotated since
    ///     the envelope was written,
    ///   * JSON parse failure (corrupt blob — caller starts
    ///     with an empty timeline + re-bootstraps from the DS
    ///     journal cursor).
    ///
    /// ADR-0041 — prefers the App Group container copy, falls back to the
    /// old location so an un-migrated log still reads.
    public func getLog(accountIdB64: String) -> MessageLogPayload? {
        guard let url = readableURL(accountIdB64: accountIdB64) else { return nil }
        guard let combined = try? Data(contentsOf: url) else { return nil }
        guard let keyBytes = KeychainWrapKey.read(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount) else {
            return nil
        }
        let wrapKey = SymmetricKey(data: keyBytes)
        let plaintext: Data
        do {
            let sealed = try AES.GCM.SealedBox(combined: combined)
            plaintext = try AES.GCM.open(sealed, using: wrapKey)
        } catch {
            return nil
        }
        return try? JSONDecoder().decode(MessageLogPayload.self, from: plaintext)
    }

    /// Erase the log for `accountIdB64` from **both** the App Group
    /// container and the old location. No-op when absent. Called from
    /// `forgetDevice`.
    public func deleteLog(accountIdB64: String) {
        if let n = newURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: n)
        }
        if let o = oldURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: o)
        }
    }

    /// ADR-0041 Phase 1 — one-time, idempotent, **copy-not-move**
    /// migration of the log into the shared storage the NSE can read: the
    /// AES wrap key into the shared Keychain access group, and the
    /// envelope file into the App Group container. Never deletes the old
    /// copies.
    public func migrateToSharedStorage(accountIdB64: String) {
        KeychainWrapKey.migrate(service: Self.wrapKeyService, account: Self.wrapKeyAccount)
        guard let old = oldURL(accountIdB64: accountIdB64),
              let shared = newURL(accountIdB64: accountIdB64) else { return }
        if SharedStorage.migrateFileIfNeeded(old: old, shared: shared) == shared {
            SharedStorage.excludeFromBackup(shared)
        }
    }

    // MARK: - File layout (dual-location, ADR-0041)

    private func fileName(_ accountIdB64: String) -> String {
        "\(accountIdB64.replacingOccurrences(of: "/", with: "_")).bin"
    }

    private func newEnvelopeDir() -> URL? {
        SharedStorage.containerURL()?.appendingPathComponent(Self.subdir)
    }

    private func newURL(accountIdB64: String) -> URL? {
        newEnvelopeDir()?.appendingPathComponent(fileName(accountIdB64))
    }

    private func oldURL(accountIdB64: String) -> URL? {
        oldEnvelopeDir?.appendingPathComponent(fileName(accountIdB64))
    }

    private func readableURL(accountIdB64: String) -> URL? {
        let fm = FileManager.default
        if let n = newURL(accountIdB64: accountIdB64), fm.fileExists(atPath: n.path) { return n }
        if let o = oldURL(accountIdB64: accountIdB64), fm.fileExists(atPath: o.path) { return o }
        return nil
    }

    private func writeTarget(accountIdB64: String) -> (dir: URL, url: URL)? {
        if let dir = newEnvelopeDir() {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        if let dir = oldEnvelopeDir {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        return nil
    }

    private static func oldEnvelopeDir() -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        return appSupport.appendingPathComponent(subdir)
    }
}

public enum MessageLogStoreError: Error {
    case keychain(status: OSStatus, operation: String)
}

// MARK: - JSON payload

/// On-disk shape — versioned so a future migration knows what
/// it's reading. Mirror of the equivalent Android / web JSON
/// schema so a backup envelope migrated across platforms can
/// round-trip (this isn't a goal in 2a3f1 but the schema is
/// trivial enough to align).
public struct MessageLogPayload: Codable, Equatable {
    public let version: Int
    public let groups: [String: PersistedGroup]

    public init(version: Int = 1, groups: [String: PersistedGroup]) {
        self.version = version
        self.groups = groups
    }
}

/// Per-group log slot. `cursor` is the DS journal cursor —
/// the next poll starts at this id + 1.
public struct PersistedGroup: Codable, Equatable {
    public let cursor: Int64
    public let messages: [PersistedMessage]

    public init(cursor: Int64, messages: [PersistedMessage]) {
        self.cursor = cursor
        self.messages = messages
    }
}

/// JSON projection of [`ChatMessage`] + [`AttachmentRef`] — the
/// UniFFI types aren't `Codable` directly (they live in a
/// generated module we don't own), so we hand-roll a parallel
/// shape with the same fields.
public struct PersistedMessage: Codable, Equatable {
    public let messageId: Int64
    public let direction: String
    public let text: String
    public let attachments: [PersistedAttachment]
    public let createdAt: String
    /// ADR-0023 — original author's MLS leaf index. Optional so
    /// a pre-ADR-0023 envelope (no field) decodes cleanly.
    public let authorLeaf: UInt32?
    /// ADR-0023 — edited / deleted markers. Default `false` for
    /// older blobs without the field.
    public let edited: Bool
    public let deleted: Bool
    /// ADR-0023 — folded reactions. Empty for older blobs.
    public let reactions: [PersistedReaction]

    public init(
        messageId: Int64,
        direction: String,
        text: String,
        attachments: [PersistedAttachment],
        createdAt: String,
        authorLeaf: UInt32? = nil,
        edited: Bool = false,
        deleted: Bool = false,
        reactions: [PersistedReaction] = []
    ) {
        self.messageId = messageId
        self.direction = direction
        self.text = text
        self.attachments = attachments
        self.createdAt = createdAt
        self.authorLeaf = authorLeaf
        self.edited = edited
        self.deleted = deleted
        self.reactions = reactions
    }

    // ADR-0023 — custom decoder so a pre-ADR-0023 blob (which has
    // no authorLeaf / edited / deleted / reactions keys) decodes
    // without throwing. `decodeIfPresent` + defaults keep older
    // envelopes loadable.
    enum CodingKeys: String, CodingKey {
        case messageId, direction, text, attachments, createdAt
        case authorLeaf, edited, deleted, reactions
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.messageId = try c.decode(Int64.self, forKey: .messageId)
        self.direction = try c.decode(String.self, forKey: .direction)
        self.text = try c.decode(String.self, forKey: .text)
        self.attachments = try c.decode([PersistedAttachment].self, forKey: .attachments)
        self.createdAt = try c.decode(String.self, forKey: .createdAt)
        self.authorLeaf = try c.decodeIfPresent(UInt32.self, forKey: .authorLeaf)
        self.edited = try c.decodeIfPresent(Bool.self, forKey: .edited) ?? false
        self.deleted = try c.decodeIfPresent(Bool.self, forKey: .deleted) ?? false
        self.reactions = try c.decodeIfPresent(
            [PersistedReaction].self,
            forKey: .reactions
        ) ?? []
    }
}

/// ADR-0023 — JSON projection of a folded ``Reaction``.
public struct PersistedReaction: Codable, Equatable {
    public let reactorLeaf: UInt32
    public let emoji: String

    public init(reactorLeaf: UInt32, emoji: String) {
        self.reactorLeaf = reactorLeaf
        self.emoji = emoji
    }
}

public struct PersistedAttachment: Codable, Equatable {
    /// Base64-encoded 32-byte SHA-256 blob id.
    public let blobIdB64: String
    /// Base64-encoded 32-byte CEK.
    public let keyB64: String
    /// Base64-encoded 12-byte AEAD nonce.
    public let nonceB64: String
    public let mime: String?
    public let size: UInt64
    public let name: String?

    public init(
        blobIdB64: String,
        keyB64: String,
        nonceB64: String,
        mime: String?,
        size: UInt64,
        name: String?
    ) {
        self.blobIdB64 = blobIdB64
        self.keyB64 = keyB64
        self.nonceB64 = nonceB64
        self.mime = mime
        self.size = size
        self.name = name
    }
}

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
        self.authorLeaf = msg.authorLeaf
        self.edited = msg.edited
        self.deleted = msg.deleted
        self.reactions = msg.reactions.map {
            PersistedReaction(reactorLeaf: $0.reactorLeaf, emoji: $0.emoji)
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
            authorLeaf: authorLeaf,
            edited: edited,
            deleted: deleted,
            reactions: reactions.map {
                Reaction(reactorLeaf: $0.reactorLeaf, emoji: $0.emoji)
            }
        )
    }
}

extension PersistedAttachment {
    public init(from ref: AttachmentRef) {
        self.blobIdB64 = ref.blobId.base64EncodedString()
        self.keyB64 = ref.key.base64EncodedString()
        self.nonceB64 = ref.nonce.base64EncodedString()
        self.mime = ref.mime
        self.size = ref.size
        self.name = ref.name
    }

    public func toAttachmentRef() -> AttachmentRef? {
        guard let blobId = Data(base64Encoded: blobIdB64),
              let key = Data(base64Encoded: keyB64),
              let nonce = Data(base64Encoded: nonceB64) else {
            return nil
        }
        return AttachmentRef(
            blobId: blobId,
            key: key,
            nonce: nonce,
            mime: mime,
            size: size,
            name: name
        )
    }
}
