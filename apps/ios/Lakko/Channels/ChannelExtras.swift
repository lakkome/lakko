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
    /// ADR-0087 section 7 / UX 4.1 — this device's admission mark of a
    /// private channel: the service row number through which rows are
    /// readers (they came in before the channel went private, or a let-in
    /// pass let them in), the web access record's `admittedThroughSeq`;
    /// `nil` reads as 0. A rotation reseals per key to exactly these rows
    /// (``ChannelReseal/resealThrough(isPublic:mark:)``).
    /// Only ever raised.
    public var admittedThroughSeq: UInt64?
    /// This device set the mark itself (it made the channel private or
    /// minted its first admission seed), the web `admissionMarkSet`. A device
    /// that did not (a co-publisher, another device of the owner) cannot tell
    /// a reader from before the channel went private from a row with no
    /// ticket: it removes nobody and moves its mark only past rows it let in
    /// (``SubscriberAdmission/letInVerdict(seq:ticketLive:mark:markSet:isOwnKey:)``).
    public var admissionMarkSet: Bool?
    /// ADR-0090 section 6 — the highest mark this device sent on the
    /// PublisherGroup (`channel_admission_mark`) or received from a current
    /// publisher; a mark above it is sent at the next pass. `nil` reads 0.
    public var admissionMarkSentSeq: UInt64?
    /// ADR-0090 decision A — channel epoch minus PublisherGroup MLS epoch.
    /// A re-founded group restarts at MLS epoch 0 while the channels service
    /// goes on from N, so the two differ from then on; `nil` reads 0 (a group
    /// founded with its channel). Set by the re-founding device, learned from
    /// the service by a device admitted later (``epochOffsetToLearn``).
    public var epochOffset: Int64?
    /// ADR-0090 — this device was admitted and has not yet read the channel
    /// service's epoch to learn ``epochOffset``.
    public var epochOffsetToLearn: Bool?
    /// ADR-0090 contract 3a — a co-published channel: the owner's 32-byte
    /// account key, the issuer of the `/g/` capability this account verified
    /// when it joined (carried by the subscription register). A
    /// PublisherGroup Welcome is joined only from a device of this account;
    /// with none recorded, this device joins nothing.
    public var ownerAccountKey: Data?
    /// ADR-0090 security pass — the highest admission mark the channel OWNER's
    /// devices sent, held until this device has read that far
    /// (``subscriberRowsReadSeq``); only the part it has read counts.
    public var admissionMarkReceived: UInt64?
    /// The newest subscriber row number this device has itself read from the
    /// channels service.
    public var subscriberRowsReadSeq: UInt64?
    /// ADR-0090 security pass — the DS refused this device's signed reads of
    /// the channel's journal: a reset is suspected. The device stops posting
    /// and rotating under the old seed but KEEPS its group state; only an
    /// owner-signed Welcome for the same group replaces it.
    public var resetSuspected: Bool?
    /// ADR-0050 section 2.9 (amended 2026-11-13) — what the ACCOUNT is in the
    /// channel as its synced subscription register says: `"owner"` or
    /// `"coPublisher"`; `nil` for a subscriber or before a register landed.
    /// A device holding no PublisherGroup for a channel the account owns or
    /// co-publishes reads it as a reader (``ChannelsUiState/readerDeviceRole(_:)``).
    public var accountRole: String?
    /// 2026-11-13 (follow-up decision 1) — on the publishing device, the
    /// account's one subscriber key (the subscription register's) this device
    /// registered at the channels service, lower-case hex. Every rotation it
    /// makes reseals the new seed to it, whatever the admission mark says, so
    /// the account's reader devices keep reading a private channel
    /// (``ChannelAccountReaderKey``). The web `readerKeyRegisteredHex`.
    public var readerKeyRegisteredHex: String?
    /// UX 15.1 (live walk 2026-11-24) — the reader keys the channel's other
    /// publishing accounts announced on the PublisherGroup
    /// (``PublisherReaderKeys``): publisher account key (base64) to reader
    /// key (lower-case hex). Pruned to the current publishers each pass;
    /// left out of the subscriber list and count. The web
    /// `publisherReaderKeys`.
    public var publisherReaderKeys: [String: String]?
    /// What this device last announced (``PublisherReaderKeys/sendDue``).
    public var publisherReaderKeySent: String?
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

    /// The one Subscribe of a public channel found by its `@name` record, from
    /// a search result or a Discover card alike: keep the record's read key
    /// for the screen that subscribes, then open the channel subscribing under
    /// the record's name. With the read key the channel reads at once (its
    /// public seeds open, no waiting line; `group-and-channel-privacy.md` 7.2).
    static func subscribe(
        _ found: PublicChannelFound,
        open: (_ channelIdHex: String, _ handle: String, _ name: String) -> Void
    ) {
        if found.readKey.count == 32 { put(found.idHex, found.readKey) }
        open(found.idHex, found.handle, found.name)
    }
}

/// A public channel as its `@name` record names it (ADR-0087 sections 4 and
/// 6): what a Subscribe needs to read it at once.
struct PublicChannelFound: Equatable {
    /// The 32-char lower-case hex channel id.
    let idHex: String
    /// The canonical `@name`, without the `@`.
    let handle: String
    /// The channel's name from the record; may be empty.
    let name: String
    /// The channel's read key from the record (32 bytes).
    let readKey: Data
}

extension ConversationHandles.Found {
    /// A channel record as ``PublicChannelFound``.
    var asPublicChannel: PublicChannelFound {
        PublicChannelFound(idHex: idHex.lowercased(), handle: handle, name: name, readKey: key)
    }
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

