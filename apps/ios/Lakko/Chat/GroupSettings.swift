// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 sections 5, 11 and 18 — the group-setting service envelopes the
/// receive loop routes out of the timeline: the group's photo, the reactions
/// it takes, its @name, and an owner's knock seed for an admin.
public enum GroupSettingEnvelope: Equatable {
    case photo(FfiGroupPhoto)
    case reactions(FfiGroupReactions)
    case handle(FfiConversationHandle)
    /// The whole envelope; opened only by the addressee (sealed to its key).
    case knockSeed(Data)
}

/// Reactions-and-emoji spec section 9 — the reactions a group takes, as this
/// device holds them, with the setter's stamp (re-sent verbatim on growth).
public struct StoredGroupReactions: Codable, Equatable {
    /// `all`, `off` or `only`.
    public let kind: String
    /// The list for `only`, in the setter's order.
    public let emoji: [String]
    public let updatedAtMs: UInt64

    public init(_ value: FfiAllowedReactions, updatedAtMs: UInt64) {
        switch value {
        case .all: kind = "all"; emoji = []
        case .off: kind = "off"; emoji = []
        case .only(let list): kind = "only"; emoji = list
        }
        self.updatedAtMs = updatedAtMs
    }

    public var allowed: FfiAllowedReactions {
        switch kind {
        case "off": return .off
        case "only": return emoji.isEmpty ? .all : .only(emoji: emoji)
        default: return .all
        }
    }
}

/// ADR-0087 section 18.1 — the group's photo as this device holds it.
/// `avatar == nil` with a stamp is a removal, kept so the stamp orders.
public struct StoredGroupPhoto: Codable, Equatable {
    public let avatar: ProfileAvatar?
    public let updatedAtMs: UInt64

    public init(avatar: ProfileAvatar?, updatedAtMs: UInt64) {
        self.avatar = avatar
        self.updatedAtMs = updatedAtMs
    }

    var ffi: FfiGroupPhoto { FfiGroupPhoto(avatar: avatar?.ffi, updatedAtMs: updatedAtMs) }
}

/// ADR-0087 section 11 — the group's @name and flag, verbatim as signed by
/// the handle key (any member relays it unchanged).
public struct StoredGroupHandle: Codable, Equatable {
    public let handle: String
    public let isPublic: Bool
    public let certificate: Data
    public let signature: Data
    public let updatedAtMs: UInt64

    public init(_ value: FfiConversationHandle) {
        handle = value.handle
        isPublic = value.public
        certificate = value.certificate
        signature = value.signature
        updatedAtMs = value.updatedAtMs
    }

    var ffi: FfiConversationHandle {
        FfiConversationHandle(
            updatedAtMs: updatedAtMs, public: isPublic, handle: handle,
            certificate: certificate, signature: signature)
    }
}

/// ADR-0087 section 18.5 — a photo this device uploaded and has not yet sent
/// into the group (`docs/ux/group-and-channel-privacy.md` section 4.5.2,
/// "Uploaded, the device went offline"): sent when the device is back, also
/// after a restart.
public struct OwedGroupPhoto: Codable, Equatable {
    public let photo: StoredGroupPhoto
}

/// Per-account, per-group store of the group settings (UserDefaults, the
/// ``GroupNameStore`` shape: small values the members already share).
/// App-private; the notification extension only recognises the envelopes.
public final class GroupSettingsStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    // MARK: Reactions

    public func reactions(accountIdB64: String, groupIdHex: String) -> StoredGroupReactions? {
        decode(StoredGroupReactions.self, accountIdB64, "reactions", groupIdHex)
    }

    public func setReactions(accountIdB64: String, groupIdHex: String, _ value: StoredGroupReactions) {
        encode(value, accountIdB64, "reactions", groupIdHex)
    }

    // MARK: Photo

    public func photo(accountIdB64: String, groupIdHex: String) -> StoredGroupPhoto? {
        decode(StoredGroupPhoto.self, accountIdB64, "photo", groupIdHex)
    }

    public func setPhoto(accountIdB64: String, groupIdHex: String, _ value: StoredGroupPhoto) {
        encode(value, accountIdB64, "photo", groupIdHex)
    }

    public func owedPhoto(accountIdB64: String, groupIdHex: String) -> OwedGroupPhoto? {
        decode(OwedGroupPhoto.self, accountIdB64, "photo_owed", groupIdHex)
    }

    public func setOwedPhoto(accountIdB64: String, groupIdHex: String, _ value: OwedGroupPhoto?) {
        encode(value, accountIdB64, "photo_owed", groupIdHex)
    }

    /// Every group with an owed photo, for the send-when-back sweep.
    public func groupsWithOwedPhoto(accountIdB64: String) -> [String] {
        Array(map(accountIdB64, "photo_owed").keys)
    }

    /// ADR-0087 section 18.5 — blob ids this device uploaded as a group photo
    /// and that a later photo replaced: deleted by this device, retried until
    /// the storage answers. Durable so a crash between the replace and the
    /// delete does not leave the file to the one-year expiry.
    public func replacedBlobs(accountIdB64: String) -> [Data] {
        let raw = defaults.array(forKey: Self.key(accountIdB64, "photo_replaced")) as? [String] ?? []
        return raw.compactMap { Data(base64Encoded: $0) }
    }

    public func setReplacedBlobs(accountIdB64: String, _ ids: [Data]) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(ids.map { $0.base64EncodedString() }, forKey: Self.key(accountIdB64, "photo_replaced"))
    }

    // MARK: Photo re-key (ADR-0087 section 18.3)

    public func photoRekeyOwed(accountIdB64: String, groupIdHex: String) -> Bool {
        (map(accountIdB64, "photo_rekey")[groupIdHex] as? Bool) == true
    }

    public func setPhotoRekeyOwed(accountIdB64: String, groupIdHex: String, _ owed: Bool) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = map(accountIdB64, "photo_rekey")
        all[groupIdHex] = owed ? true : nil
        defaults.set(all, forKey: Self.key(accountIdB64, "photo_rekey"))
    }

    // MARK: Names join requests carry (UX section 8)

    /// The name a request to join carried, by the joiner's leaf signature key
    /// (base64): what the person claims, unverified.
    public func requestNames(accountIdB64: String, groupIdHex: String) -> [String: String] {
        map(accountIdB64, "request_names")[groupIdHex] as? [String: String] ?? [:]
    }

    public func keepRequestName(accountIdB64: String, groupIdHex: String, joinerKeyB64: String, name: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = map(accountIdB64, "request_names")
        var row = all[groupIdHex] as? [String: String] ?? [:]
        row[joinerKeyB64] = name
        all[groupIdHex] = row
        defaults.set(all, forKey: Self.key(accountIdB64, "request_names"))
    }

    /// UX section 8 — when a request to join reached this device and, for a
    /// request that came by a link, the account key of the link's issuer.
    public struct RequestMeta: Equatable {
        public let askedAtMs: Int64
        public let inviterKey: Data?
    }

    /// The meta of each request, by the joiner's leaf signature key (base64).
    public func requestMeta(accountIdB64: String, groupIdHex: String) -> [String: RequestMeta] {
        let row = map(accountIdB64, "request_meta")[groupIdHex] as? [String: [String: Any]] ?? [:]
        var out: [String: RequestMeta] = [:]
        for (key, value) in row {
            guard let at = (value["at"] as? NSNumber)?.int64Value else { continue }
            out[key] = RequestMeta(
                askedAtMs: at, inviterKey: (value["inviter"] as? String).flatMap { Data(base64Encoded: $0) })
        }
        return out
    }

    /// Keeps the first sighting of a request; a request seen again keeps its
    /// time.
    public func keepRequestMeta(
        accountIdB64: String, groupIdHex: String, joinerKeyB64: String, askedAtMs: Int64, inviterKey: Data?
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = map(accountIdB64, "request_meta")
        var row = all[groupIdHex] as? [String: Any] ?? [:]
        guard row[joinerKeyB64] == nil else { return }
        var entry: [String: Any] = ["at": NSNumber(value: askedAtMs)]
        if let inviterKey { entry["inviter"] = inviterKey.base64EncodedString() }
        row[joinerKeyB64] = entry
        all[groupIdHex] = row
        defaults.set(all, forKey: Self.key(accountIdB64, "request_meta"))
    }

    // MARK: Created here (UX section 10)

    /// This device's account created the group: the timeline opens with
    /// "You created this group".
    public func createdHere(accountIdB64: String, groupIdHex: String) -> Bool {
        (map(accountIdB64, "created")[groupIdHex] as? Bool) == true
    }

    public func markCreatedHere(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = map(accountIdB64, "created")
        all[groupIdHex] = true
        defaults.set(all, forKey: Self.key(accountIdB64, "created"))
    }

    // MARK: Handle

    public func handle(accountIdB64: String, groupIdHex: String) -> StoredGroupHandle? {
        decode(StoredGroupHandle.self, accountIdB64, "handle", groupIdHex)
    }

    public func setHandle(accountIdB64: String, groupIdHex: String, _ value: StoredGroupHandle?) {
        encode(value, accountIdB64, "handle", groupIdHex)
    }

    // MARK: Knock seeds (ADR-0087 section 5)

    /// The knock seeds this device holds as an admin, by version.
    public func knockSeeds(accountIdB64: String, groupIdHex: String) -> [UInt16: Data] {
        guard let raw = map(accountIdB64, "knock")[groupIdHex] as? [String: String] else { return [:] }
        var out: [UInt16: Data] = [:]
        for (v, b64) in raw {
            if let version = UInt16(v), let seed = Data(base64Encoded: b64), seed.count == 32 {
                out[version] = seed
            }
        }
        return out
    }

    public func keepKnockSeed(accountIdB64: String, groupIdHex: String, version: UInt16, seed: Data) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = map(accountIdB64, "knock")
        var row = all[groupIdHex] as? [String: String] ?? [:]
        row[String(version)] = seed.base64EncodedString()
        all[groupIdHex] = row
        defaults.set(all, forKey: Self.key(accountIdB64, "knock"))
    }

    // MARK: Whole group

    /// ADR-0087 section 9 step 4 — the membership ended: everything of the
    /// group goes.
    public func removeGroup(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        for part in ["reactions", "photo", "photo_owed", "handle", "knock", "photo_rekey", "created", "request_names", "request_meta"] {
            var all = map(accountIdB64, part)
            if all.removeValue(forKey: groupIdHex) != nil {
                defaults.set(all, forKey: Self.key(accountIdB64, part))
            }
        }
    }

    public func clear(accountIdB64: String) {
        for part in ["reactions", "photo", "photo_owed", "handle", "knock", "photo_replaced", "photo_rekey", "created", "request_names", "request_meta"] {
            defaults.removeObject(forKey: Self.key(accountIdB64, part))
        }
    }

    // MARK: Plumbing

    private func map(_ accountIdB64: String, _ part: String) -> [String: Any] {
        defaults.dictionary(forKey: Self.key(accountIdB64, part)) ?? [:]
    }

    private func decode<T: Decodable>(_: T.Type, _ accountIdB64: String, _ part: String, _ groupIdHex: String) -> T? {
        guard let raw = map(accountIdB64, part)[groupIdHex] as? Data else { return nil }
        return try? JSONDecoder().decode(T.self, from: raw)
    }

    private func encode<T: Encodable>(_ value: T?, _ accountIdB64: String, _ part: String, _ groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = map(accountIdB64, part)
        if let value, let data = try? JSONEncoder().encode(value) {
            all[groupIdHex] = data
        } else {
            all[groupIdHex] = nil
        }
        defaults.set(all, forKey: Self.key(accountIdB64, part))
    }

    static func key(_ accountIdB64: String, _ part: String) -> String {
        "group_setting.\(part).\(accountIdB64)"
    }
}

/// The receive-side decision for a role-gated group setting (photo, allowed
/// reactions): the ``GroupNameGate`` table with a caller-supplied "identical"
/// test and order.
///
/// | Role table | Sender | Verdict |
/// |---|---|---|
/// | present | owner or admin | accept if strictly newer |
/// | present | member | `rejectNotAuthorized` |
/// | present | unresolvable / table unreadable | `rejectUnverifiable` |
/// | absent | any member | accept if strictly newer |
public enum GroupSettingGate {

    public static func classify(
        identical: Bool,
        incomingUpdatedAtMs: UInt64,
        storedUpdatedAtMs: UInt64?,
        senderKey: Data?,
        authority: PushGroupRouting.SecretAuthority,
        isNewer: (_ incoming: UInt64, _ stored: UInt64) -> Bool
    ) -> GroupNameVerdict {
        if identical { return .acceptIdentical }
        switch authority {
        case .noRoleTable:
            break
        case .unreadable:
            return .rejectUnverifiable
        case .roleTable(let ownerKey, let admins):
            guard let senderKey, !senderKey.isEmpty else { return .rejectUnverifiable }
            guard senderKey == ownerKey || admins.contains(senderKey) else { return .rejectNotAuthorized }
        }
        guard let storedUpdatedAtMs else { return .accept }
        return isNewer(incomingUpdatedAtMs, storedUpdatedAtMs) ? .accept : .rejectStale
    }
}

/// ADR-0087 section 5 — per group, on the owner's devices: the knock version
/// new links carry, the admin set it was handed to, and which admins have
/// received the seed of which version. Per account, in `UserDefaults`; no
/// secret (the seed is derived from the journal root when needed).
public enum KnockVersionStore {

    struct Row: Codable, Equatable {
        var version: UInt16
        var admins: [Data]
        var sent: [String]
        /// ADR-0087 section 5 (rotation rule) — versions up to `through` are
        /// derived from the root of the predecessor account key named here
        /// (its seed is kept with the account's own predecessors); later
        /// ones from the next boundary's root, or the current root.
        var rootBoundaries: [RootBoundary]? = nil
    }

    struct RootBoundary: Codable, Equatable {
        let through: UInt16
        /// Base64 of the predecessor's account signature key.
        let predecessorKeyB64: String
    }

    /// The predecessor whose root derives `version`, or `nil` for the
    /// current root (pure).
    static func predecessorKey(for version: UInt16, row: Row?) -> String? {
        guard let boundaries = row?.rootBoundaries else { return nil }
        return boundaries.sorted { $0.through < $1.through }.first { version <= $0.through }?.predecessorKeyB64
    }

    /// A rotation moved the root: versions up to the current one stay with
    /// the predecessor, the next one starts on the new root (pure).
    static func rotated(_ row: Row, predecessorKeyB64: String) -> Row {
        var next = row
        next.rootBoundaries = (row.rootBoundaries ?? []) + [
            RootBoundary(through: row.version, predecessorKeyB64: predecessorKeyB64)]
        next.version = row.version &+ 1
        next.sent = []
        return next
    }

    static func key(_ accountIdB64: String) -> String { "lakko.knock-versions.v1.\(accountIdB64)" }

    static func sentTag(_ admin: Data, _ version: UInt16) -> String {
        "\(admin.base64EncodedString())|\(version)"
    }

    private static func all(_ accountIdB64: String) -> [String: Row] {
        UserDefaults.standard.data(forKey: key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: Row].self, from: $0) } ?? [:]
    }

    static func load(_ accountIdB64: String, _ groupIdHex: String) -> Row? {
        all(accountIdB64)[groupIdHex]
    }

    static func save(_ row: Row, _ accountIdB64: String, _ groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64)
        map[groupIdHex] = row
        UserDefaults.standard.set(try? JSONEncoder().encode(map), forKey: key(accountIdB64))
    }

    /// Pure: an admin that left the set bumps the version (and forgets who
    /// received the old one); a new admin only joins the set.
    static func reconciled(_ row: Row, currentAdmins: [Data]) -> Row {
        let removed = row.admins.contains { !currentAdmins.contains($0) }
        guard removed else {
            var next = row
            next.admins = currentAdmins
            return next
        }
        return Row(version: row.version &+ 1, admins: currentAdmins, sent: [], rootBoundaries: row.rootBoundaries)
    }
}
