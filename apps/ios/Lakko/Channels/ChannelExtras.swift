// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 sections 11, 13a, 16 and 18.2 — what a channel's metadata v3
/// record and its other per-channel facts add to the v2 row
/// (``ChannelHandle``): whether it is public, its `@name` as the record
/// carries it (with the certificate, so a re-issue carries it verbatim), its
/// photo, the reactions its owner allows, the comment ban list, and the read
/// key of a public channel.
///
/// Kept beside the channel log rather than inside it so the sealed log's
/// format does not move. Nothing here is secret beyond what holding the
/// channel already gives: the read key of a public channel is in its public
/// record, by design (section 6).
public struct ChannelExtras: Codable, Equatable, Sendable {
    /// The record's handle block (section 11), `nil` until a v3 record with
    /// one was read.
    public var handle: StoredConversationHandle?
    /// The channel photo (section 18.2), `nil` for none.
    public var avatar: ProfileAvatar?
    /// The reactions the owner allows (section 13a); `nil` = all.
    public var reactions: StoredAllowedReactions?
    /// The newest ban list (section 13a, "Banning"): its stamp and the comment
    /// author keys whose comments are hidden.
    public var banUpdatedAtMs: UInt64 = 0
    public var bannedAuthors: [Data] = []
    /// Section 6 — the 32-byte read key of a public channel, from its public
    /// record or an invitation, or derived by the owner.
    public var readKey: Data?
    /// ADR-0087 section 7 — this device subscribed through a private
    /// invitation and has not been admitted yet: the 7.2 waiting line.
    public var waitingForAdmission: Bool = false
    /// ADR-0051 section 4 — after a commit moved the channel's epoch (an
    /// approve, a removal, a rotation), the record and ban list must be
    /// re-posted under it; `true` until that landed, retried by every
    /// publisher pass. Optional so a row written before it decodes.
    public var recordOwed: Bool?
    /// ADR-0087 section 15 — a co-publisher removed a subscriber of this
    /// private channel and its rotate request to the owner's device has not
    /// gone out yet: `true` until it publishes, retried by every publisher
    /// pass, so the removed person does not keep reading until the owner's
    /// next scheduled rotation. Written before the publish.
    public var rotateRequestOwed: Bool?

    public init() {}

    /// The newest metadata record's own flag (`listed`, the PUBLIC flag
    /// since v3), `nil` until a record was read. Optional so a row written
    /// before it decodes.
    public var recordPublic: Bool?

    /// Whether the channel is public, as far as this device knows (UX
    /// section 12, the same order on web and Android): the signed `@name`'s
    /// flag in the newest record, else that record's own flag, else a read
    /// key held (only a public channel has one), else `nil` (unknown).
    public var isPublic: Bool? {
        if let handle { return handle.isPublic }
        if let recordPublic { return recordPublic }
        return readKey != nil ? true : nil
    }

    /// The allowed reactions in the core's shape (`all` when unset).
    public var allowed: FfiAllowedReactions { reactions?.ffi ?? .all }
}

/// The handle block of a channel's metadata record, stored as the record
/// carried it so a publisher's re-issue (after a rotation) sends it back
/// byte for byte.
public struct StoredConversationHandle: Codable, Equatable, Sendable {
    public let updatedAtMs: UInt64
    public let isPublic: Bool
    public let handle: String
    public let certificate: Data
    public let signature: Data

    public init(_ ffi: FfiConversationHandle) {
        updatedAtMs = ffi.updatedAtMs
        isPublic = ffi.public
        handle = ffi.handle
        certificate = ffi.certificate
        signature = ffi.signature
    }

    public var ffi: FfiConversationHandle {
        FfiConversationHandle(
            updatedAtMs: updatedAtMs, public: isPublic, handle: handle,
            certificate: certificate, signature: signature)
    }
}

/// ``FfiAllowedReactions`` in a codable shape.
public enum StoredAllowedReactions: Codable, Equatable, Sendable {
    case all
    case off
    case only([String])

    public init(_ ffi: FfiAllowedReactions) {
        switch ffi {
        case .all: self = .all
        case .off: self = .off
        case .only(let emoji): self = .only(emoji)
        }
    }

    public var ffi: FfiAllowedReactions {
        switch self {
        case .all: return .all
        case .off: return .off
        case .only(let emoji): return .only(emoji: emoji)
        }
    }
}

/// The per-account store of ``ChannelExtras``, in `UserDefaults` behind the
/// account store gate (ADR-0077): a sealed account takes no late write.
enum ChannelExtrasStore {
    static func key(_ accountIdB64: String) -> String { "lakko.channel-extras.v1.\(accountIdB64)" }

    static func load(_ accountIdB64: String, _ defaults: UserDefaults = .standard) -> [String: ChannelExtras] {
        defaults.data(forKey: key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: ChannelExtras].self, from: $0) } ?? [:]
    }

    static func save(_ all: [String: ChannelExtras], _ accountIdB64: String, _ defaults: UserDefaults = .standard) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(all), forKey: key(accountIdB64))
    }

    static func delete(_ accountIdB64: String, _ defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }
}

/// ADR-0087 section 18 / UX 4.5.4 — the photos of the channels this device
/// holds, drawn by the avatar component like a group's (``AvatarPhoto/group``
/// with the channel id). A map of its own so the group refresh, which replaces
/// the group map wholesale, never drops a channel's.
@MainActor
final class ChannelPhotoDirectory {
    static let shared = ChannelPhotoDirectory()
    private(set) var photos: [String: ProfileAvatar] = [:]

    func replace(_ next: [String: ProfileAvatar]) {
        let lowered = Dictionary(next.map { ($0.key.lowercased(), $0.value) }, uniquingKeysWith: { a, _ in a })
        guard lowered != photos else { return }
        photos = lowered
        GroupPhotoDirectory.shared.channelPhotosChanged()
    }
}

/// ADR-0087 section 6 — the read key a search record or an invitation carried
/// for a public channel, handed to the conversation screen that subscribes.
/// In memory, taken once.
@MainActor
enum FoundChannelReadKeys {
    private static var keys: [String: Data] = [:]

    static func put(_ channelIdHex: String, _ key: Data) { keys[channelIdHex.lowercased()] = key }

    static func take(_ channelIdHex: String) -> Data? { keys.removeValue(forKey: channelIdHex.lowercased()) }
}

/// ADR-0087 section 7.2 — a private channel invitation the link page handed
/// to the conversation screen that subscribes with it. In memory, taken once.
@MainActor
enum PendingChannelInvites {
    struct Invite: Equatable {
        let inviteSeed: Data
        let admissionPublicKey: Data
        let admissionVersion: UInt16
    }

    private static var invites: [String: Invite] = [:]

    static func put(_ channelIdHex: String, _ invite: Invite) { invites[channelIdHex.lowercased()] = invite }

    static func take(_ channelIdHex: String) -> Invite? { invites.removeValue(forKey: channelIdHex.lowercased()) }
}

/// ADR-0087 section 13.1 — reaction adds that found no `channel-react` token,
/// kept per account behind the store gate until they leave with the next
/// refill, across restarts. A reaction taken off drops its entry.
enum PendingChannelReactionStore {
    struct Entry: Codable, Equatable {
        let channelIdHex: String
        let cursor: UInt64
        let emoji: String
        let epoch: UInt32
    }

    /// The most kept per account (chosen).
    static let cap = 200

    static func key(_ accountIdB64: String) -> String { "lakko.channel-reaction-outbox.v1.\(accountIdB64)" }

    static func load(_ accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64) -> [Entry] {
        guard let accountIdB64 else { return [] }
        return UserDefaults.standard.data(forKey: key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([Entry].self, from: $0) } ?? []
    }

    private static func save(_ entries: [Entry], _ accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        UserDefaults.standard.set(try? JSONEncoder().encode(entries), forKey: key(accountIdB64))
    }

    static func add(_ entry: Entry, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64) {
        guard let accountIdB64 else { return }
        var all = load(accountIdB64).filter { $0 != entry }
        all.append(entry)
        save(Array(all.suffix(cap)), accountIdB64)
    }

    static func remove(
        channelId: String, cursor: UInt64, emoji: String,
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let accountIdB64 else { return }
        let all = load(accountIdB64)
        let kept = all.filter { !($0.channelIdHex == channelId && $0.cursor == cursor && $0.emoji == emoji) }
        if kept.count != all.count { save(kept, accountIdB64) }
    }

    static func delete(_ accountIdB64: String) {
        UserDefaults.standard.removeObject(forKey: key(accountIdB64))
    }
}

