// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
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
    ///
    /// ADR-0041 LOW-1 (defence in depth) — the fallback is on **open**
    /// failure, not just presence: if the preferred (shared) copy exists
    /// but fails to AES-GCM-open (e.g. a truncated file left by an
    /// interrupted pre-atomic-migration copy) the intact old copy is
    /// opened rather than shadowed by a `nil` return. `nil` still results
    /// only when every copy is absent or unopenable (fail-closed).
    public func getLog(accountIdB64: String) -> MessageLogPayload? {
        let candidates = readCandidates(accountIdB64: accountIdB64)
        guard !candidates.isEmpty else { return nil }
        guard let keyBytes = KeychainWrapKey.read(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount) else {
            return nil
        }
        let wrapKey = SymmetricKey(data: keyBytes)
        return SharedStorage.firstReadable(candidates) { url -> MessageLogPayload? in
            guard let combined = try? Data(contentsOf: url) else { return nil }
            let plaintext: Data
            do {
                let sealed = try AES.GCM.SealedBox(combined: combined)
                plaintext = try AES.GCM.open(sealed, using: wrapKey)
            } catch {
                return nil
            }
            return try? JSONDecoder().decode(MessageLogPayload.self, from: plaintext)
        }
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

    /// ADR-0045 UX §6.2.4 — drop ONE group's transcript from this device.
    ///
    /// Returns `false` when there was nothing to drop (no row, an
    /// undecryptable row, or no entry for this group), so the caller can tell
    /// "deleted" from "there was nothing there" without a second read.
    ///
    /// **What this does NOT remove, stated because the confirmation copy is
    /// exact rather than sweeping:** the inactive MLS group stays in the
    /// provider snapshot as inert bytes. `Group` exposes no delete, so a
    /// `Group::delete` is a real (small) core follow-up rather than something
    /// this rides free. The remnant is inert in the strong sense — it cannot
    /// send, decrypt, export or process — but it is not nothing.
    public func deleteGroup(accountIdB64: String, groupIdHex: String) -> Bool {
        guard let payload = getLog(accountIdB64: accountIdB64) else { return false }
        guard let next = withoutGroup(payload, groupIdHex: groupIdHex) else { return false }
        putLog(accountIdB64: accountIdB64, payload: next)
        return true
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

    /// Ordered read candidates (preferred first): the App Group container
    /// copy, then the old copy — each included only when it exists on
    /// disk. Feeds ``SharedStorage/firstReadable(_:open:)`` so an
    /// unopenable preferred copy falls through to the old copy (ADR-0041
    /// LOW-1) rather than shadowing it.
    private func readCandidates(accountIdB64: String) -> [URL] {
        let fm = FileManager.default
        var urls: [URL] = []
        if let n = newURL(accountIdB64: accountIdB64), fm.fileExists(atPath: n.path) {
            urls.append(n)
        }
        if let o = oldURL(accountIdB64: accountIdB64), fm.fileExists(atPath: o.path) {
            urls.append(o)
        }
        return urls
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

/// ADR-0045 UX §6.2.4 — the pure half of a per-group history delete.
///
/// No client had one before: ``MessageLogStore/deleteLog(accountIdB64:)`` and
/// its snapshot sibling are whole-ACCOUNT operations. But the log is a
/// per-account blob keyed `{ groupIdHex -> PersistedGroup }` internally, so
/// dropping one group's entry is a read-modify-write on a store that already
/// exists rather than a new one.
///
/// Kept separate from the IO so it is testable: the store reads and writes an
/// AES-wrapped file under a Keychain key, and this is where the rule that could
/// actually be wrong lives.
///
/// Returns `nil` when the group was **absent**, so the caller can skip the
/// re-encrypt + write entirely — not merely an optimisation, since a rewrite
/// re-encrypts and rewrites the whole per-account envelope.
///
/// Never mutates its input.
public func withoutGroup(
    _ payload: MessageLogPayload,
    groupIdHex: String
) -> MessageLogPayload? {
    guard payload.groups[groupIdHex] != nil else { return nil }
    var groups = payload.groups
    groups.removeValue(forKey: groupIdHex)
    return MessageLogPayload(version: payload.version, groups: groups)
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
    /// ADR-0023 — the author's IDENTITY: their Ed25519 signature public key,
    /// base64, resolved from the MLS sender leaf at receipt. `nil` when the
    /// client could not resolve it, and **absent entirely on a row written
    /// before this field existed** — both read back as "unknown author", which
    /// fails the edit / delete gate closed. That is the same posture every
    /// reloaded message had before the field existed; it never misattributes,
    /// so there is no migration and none is owed.
    ///
    /// Persisting it is what makes edit / delete survive a reload at all. The
    /// leaf it replaces could not be persisted — a leaf stored now names a
    /// different member after the next commit, and iOS storing it is what made
    /// the impersonation permanent rather than transient. The field it replaces
    /// is GONE rather than kept alongside: a stored leaf that still round-trips
    /// is what a later reader reaches for. No secret is added to the blob —
    /// the key is public.
    public let authorKeyB64: String?
    /// ADR-0023 — edited / deleted markers. Default `false` for
    /// older blobs without the field.
    public let edited: Bool
    public let deleted: Bool
    /// ADR-0023 — folded reactions. Empty for older blobs.
    public let reactions: [PersistedReaction]
    /// ADR-0052 — the reply reference and its snapshot, when this message is
    /// a reply. Absent on an ordinary message and on a row written before the
    /// field existed; both read back as `nil`.
    public let reply: PersistedReply?
    /// ADR-0052 decision 2 — the forward attribution and origin, when this
    /// message was forwarded. Absent on an ordinary message and on a row
    /// written before the field existed; both read back as `nil`.
    public let forward: PersistedForward?

    public init(
        messageId: Int64,
        direction: String,
        text: String,
        attachments: [PersistedAttachment],
        createdAt: String,
        authorKeyB64: String? = nil,
        edited: Bool = false,
        deleted: Bool = false,
        reactions: [PersistedReaction] = [],
        reply: PersistedReply? = nil,
        forward: PersistedForward? = nil
    ) {
        self.messageId = messageId
        self.direction = direction
        self.text = text
        self.attachments = attachments
        self.createdAt = createdAt
        self.authorKeyB64 = authorKeyB64
        self.edited = edited
        self.deleted = deleted
        self.reactions = reactions
        self.reply = reply
        self.forward = forward
    }

    // ADR-0023 — custom decoder so a blob written before any of these fields
    // existed decodes without throwing. `decodeIfPresent` + defaults keep older
    // envelopes loadable.
    enum CodingKeys: String, CodingKey {
        case messageId, direction, text, attachments, createdAt
        case authorKeyB64, edited, deleted, reactions, reply, forward
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.messageId = try c.decode(Int64.self, forKey: .messageId)
        self.direction = try c.decode(String.self, forKey: .direction)
        self.text = try c.decode(String.self, forKey: .text)
        self.attachments = try c.decode([PersistedAttachment].self, forKey: .attachments)
        self.createdAt = try c.decode(String.self, forKey: .createdAt)
        self.authorKeyB64 = try c.decodeIfPresent(String.self, forKey: .authorKeyB64)
        self.edited = try c.decodeIfPresent(Bool.self, forKey: .edited) ?? false
        self.deleted = try c.decodeIfPresent(Bool.self, forKey: .deleted) ?? false
        // `try?` rather than `decodeIfPresent`: a row written before the
        // reactor became an identity carries `reactorLeaf` instead, which does
        // not decode. Those reactions are unattributable to a PERSON — the leaf
        // they name may since have been handed to someone else — so they are
        // dropped rather than resurrected under a stranger's name, and dropping
        // them must not cost the message, the group or the whole transcript.
        self.reactions = (try? c.decode([PersistedReaction].self, forKey: .reactions)) ?? []
        self.reply = try c.decodeIfPresent(PersistedReply.self, forKey: .reply)
        self.forward = try c.decodeIfPresent(PersistedForward.self, forKey: .forward)
    }
}

/// ADR-0052 decision 2 — JSON projection of a ``ForwardRef``: the original
/// author's name and the origin reference exactly as the envelope carried
/// them, so the "Forwarded from" line and its tap target survive a relaunch.
public struct PersistedForward: Codable, Equatable {
    public let author: String?
    public let originGroupIdHex: String?
    public let originMessageId: Int64?

    public init(author: String?, originGroupIdHex: String?, originMessageId: Int64?) {
        self.author = author
        self.originGroupIdHex = originGroupIdHex
        self.originMessageId = originMessageId
    }
}

/// ADR-0052 — JSON projection of a ``ReplyRef``: the target's DS `messageId`
/// and the snapshot exactly as the envelope carried it. Stored verbatim so the
/// quote survives a relaunch drawn the same way it was drawn on arrival.
public struct PersistedReply: Codable, Equatable {
    public let target: Int64
    public let author: String?
    public let excerpt: String?

    public init(target: Int64, author: String?, excerpt: String?) {
        self.target = target
        self.author = author
        self.excerpt = excerpt
    }
}

/// ADR-0023 — JSON projection of a folded ``Reaction``.
///
/// `reactorKeyB64` is the reactor's signature key, never their MLS leaf: a
/// stored leaf is what let a member added into a vacated leaf toggle the
/// departed member's reaction off — and, since the last reactor's removal
/// prunes the emoji entirely, make the chip disappear.
public struct PersistedReaction: Codable, Equatable {
    public let reactorKeyB64: String
    public let emoji: String

    public init(reactorKeyB64: String, emoji: String) {
        self.reactorKeyB64 = reactorKeyB64
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
    /// ADR-0047 — `voice` / `video_note` / `gif` / another (unknown) kind, or
    /// `nil` for a plain file. Absent on a row written before the field
    /// existed, which restores as a plain file chip.
    public let kind: String?
    public let durationMs: UInt32?
    /// Base64 of at most 128 amplitude bytes.
    public let waveformB64: String?
    public let width: UInt32?
    public let height: UInt32?
    /// The video-note poster blob.
    public let thumb: PersistedThumb?

    public init(
        blobIdB64: String,
        keyB64: String,
        nonceB64: String,
        mime: String?,
        size: UInt64,
        name: String?,
        kind: String? = nil,
        durationMs: UInt32? = nil,
        waveformB64: String? = nil,
        width: UInt32? = nil,
        height: UInt32? = nil,
        thumb: PersistedThumb? = nil
    ) {
        self.blobIdB64 = blobIdB64
        self.keyB64 = keyB64
        self.nonceB64 = nonceB64
        self.mime = mime
        self.size = size
        self.name = name
        self.kind = kind
        self.durationMs = durationMs
        self.waveformB64 = waveformB64
        self.width = width
        self.height = height
        self.thumb = thumb
    }

    enum CodingKeys: String, CodingKey {
        case blobIdB64, keyB64, nonceB64, mime, size, name
        case kind, durationMs, waveformB64, width, height, thumb
    }

    // ADR-0047 — every media field is optional and read with `try?`: a row
    // written before the fields existed, or one whose optional metadata no
    // longer decodes, keeps the attachment itself (the envelope rule: bad
    // metadata is dropped, the attachment survives).
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.blobIdB64 = try c.decode(String.self, forKey: .blobIdB64)
        self.keyB64 = try c.decode(String.self, forKey: .keyB64)
        self.nonceB64 = try c.decode(String.self, forKey: .nonceB64)
        self.mime = try c.decodeIfPresent(String.self, forKey: .mime)
        self.size = try c.decode(UInt64.self, forKey: .size)
        self.name = try c.decodeIfPresent(String.self, forKey: .name)
        self.kind = (try? c.decodeIfPresent(String.self, forKey: .kind)) ?? nil
        self.durationMs = (try? c.decodeIfPresent(UInt32.self, forKey: .durationMs)) ?? nil
        self.waveformB64 = (try? c.decodeIfPresent(String.self, forKey: .waveformB64)) ?? nil
        self.width = (try? c.decodeIfPresent(UInt32.self, forKey: .width)) ?? nil
        self.height = (try? c.decodeIfPresent(UInt32.self, forKey: .height)) ?? nil
        self.thumb = (try? c.decodeIfPresent(PersistedThumb.self, forKey: .thumb)) ?? nil
    }
}

/// ADR-0047 — JSON projection of a ``ThumbRef`` (the video-note poster blob).
public struct PersistedThumb: Codable, Equatable {
    public let blobIdB64: String
    public let keyB64: String
    public let nonceB64: String
    public let mime: String?
    public let size: UInt64
    public let width: UInt32?
    public let height: UInt32?

    public init(
        blobIdB64: String,
        keyB64: String,
        nonceB64: String,
        mime: String?,
        size: UInt64,
        width: UInt32?,
        height: UInt32?
    ) {
        self.blobIdB64 = blobIdB64
        self.keyB64 = keyB64
        self.nonceB64 = nonceB64
        self.mime = mime
        self.size = size
        self.width = width
        self.height = height
    }
}

// The `ChatMessage` ↔ `PersistedMessage` / `AttachmentRef` ↔
// `PersistedAttachment` bridge helpers depend on the full `LakkoCore`
// UniFFI surface and live in `MessageLogStore+ChatMessage.swift` (compiled
// only into the main app target). Keeping them out of THIS file is what
// makes `MessageLogStore` FFI-clean so the NSE — which links the slim
// `LakkoCoreExtension`, not the full `LakkoCore` — can read the per-group
// cursor (ADR-0041 Phase 3).
