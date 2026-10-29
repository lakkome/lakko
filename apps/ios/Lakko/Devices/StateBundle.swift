// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

extension Notification.Name {
    /// Posted after a pairing state bundle (or a backup restore) merged
    /// contacts into ``AcceptedContactsStore``, so a live
    /// ``ContactsRepository`` reloads them. The object is the merged
    /// `[AcceptedContact]`; `userInfo[StateBundle.appliedAccountKey]` names
    /// the account they were filed under, and a repository bound to any other
    /// account ignores them.
    static let lakkoStateBundleApplied = Notification.Name("me.lakko.state-bundle-applied")
    /// Posted on the main actor after a pairing state bundle stored group
    /// names or profiles; `userInfo[StateBundle.appliedAccountKey]` names the
    /// account. A live chat repository of that account re-titles.
    static let lakkoStateBundleNamesApplied = Notification.Name("me.lakko.state-bundle-names-applied")
}

/// ADR-0050 section 2.9 — the state bundle a pairing hands to the new device
/// beside the history bundle: accepted contacts (with their local renames),
/// the per-contact identity pins and rotation markers, and the ids of the
/// groups the old device knows.
///
/// ADR-0074 part 3 (format `v` 2, flag day: any other `v` is refused) — the
/// bundle also carries what a device paired AFTER a rotation of the account
/// keys cannot learn from the current key alone: the account's OWN
/// predecessor seeds with the time each was retired (``OwnPredecessor``), and
/// for each pinned contact the keys that contact rotated away from by proof.
/// Without the first the new device would bind its conversation leaves under
/// the new account key and read as another account in every existing chat.
///
/// Format `v` 3 (2026-10-28, flag day: `v` 2 is refused) — three more
/// arrays, each required and possibly empty, because a new device of an
/// existing member is not membership growth and nothing else hands it what
/// the confirming device already knows:
///
/// - ``CarriedGroupName``: the shared group names (ADR-0051), each as the
///   group-name envelope composed from the stored name and its ORIGINAL
///   stamp, so a later rename still wins;
/// - ``CarriedProfile``: every profile the device stores for another person
///   (ADR-0048), as the profile envelope with its original stamp;
/// - ``CarriedPendingContact``: the people added through their invite link
///   whose app has not answered yet (ADR-0079 section 5), with display-only
///   copies of what was written to them (``CarriedHeldMessage``).
///
/// iOS mirror of `apps/web/src/devices/state-bundle.ts`. The JSON shape, the
/// row magic and the exporter label are identical so a web scanner and an iOS
/// joiner (and the reverse) interoperate: one more row on the pairing
/// rendezvous mailbox, tagged by a plaintext magic so the history / recovery /
/// state polls each skip the rows that are not theirs, sealed under the
/// DeviceGroup exporter at the post-add epoch.
public enum StateBundle {

    /// `userInfo` key of ``Notification/Name/lakkoStateBundleApplied``: the
    /// base64 account id the merged contacts belong to.
    public static let appliedAccountKey = "accountIdB64"

    /// `userInfo` key of ``Notification/Name/lakkoStateBundleApplied``:
    /// `true` when the bundle added pending contacts (a live contacts
    /// repository re-reads them).
    public static let pendingAddedKey = "pendingAdded"

    /// Exporter label; the context is the 32-byte rendezvous pseudonym.
    public static let exporterLabel = "lakko/state-bundle/v1"
    /// Exporter output length — the ChaCha20-Poly1305 key size.
    public static let exporterKeyLength = 32

    static let rowMagic = Data("lakko/state-bundle-row/v1\u{0}".utf8)
    public static let pollTimeoutSeconds: TimeInterval = 20
    static let perRequestLongPollSeconds = 5
    static let maxBundleBytes = 1 << 20
    /// Per-account prefix of the known group ids; the account id follows.
    static let knownGroupsKeyPrefix = "lakko.pairing.known-groups.v2."
    /// The device-wide key the known group ids used before they were per
    /// account. Never read; removed by ``clearKnownGroupIds(accountIdB64:defaults:)``.
    static let legacyKnownGroupsKey = "lakko.pairing.known-groups.v1"

    static func knownGroupsKey(accountIdB64: String) -> String {
        knownGroupsKeyPrefix + accountIdB64
    }

    // MARK: - Wire shape (field names match the web bundle)

    public struct CarriedContact: Codable, Equatable, Sendable {
        public let signaturePubkeyB64: String
        public let displayName: String
        public let acceptedAtMs: Double
        public let accountIdHex: String?
        public let x25519PubkeyB64: String?
        public let groupIdB64: String?

        public init(
            signaturePubkeyB64: String,
            displayName: String,
            acceptedAtMs: Double,
            accountIdHex: String? = nil,
            x25519PubkeyB64: String? = nil,
            groupIdB64: String? = nil
        ) {
            self.signaturePubkeyB64 = signaturePubkeyB64
            self.displayName = displayName
            self.acceptedAtMs = acceptedAtMs
            self.accountIdHex = accountIdHex
            self.x25519PubkeyB64 = x25519PubkeyB64
            self.groupIdB64 = groupIdB64
        }
    }

    public struct CarriedPin: Codable, Equatable, Sendable {
        public let accountIdB64: String
        public let pinnedSignaturePubkeyB64: String
        public let rotationMarker: Int64
        /// The contact's proven predecessors as the sending device holds
        /// them: standard base64 of 32-byte signature keys, OLDEST FIRST, at
        /// most ``maxPredecessors``.
        public let provenPredecessorsB64: [String]

        public init(
            accountIdB64: String, pinnedSignaturePubkeyB64: String, rotationMarker: Int64,
            provenPredecessorsB64: [String] = []
        ) {
            self.accountIdB64 = accountIdB64
            self.pinnedSignaturePubkeyB64 = pinnedSignaturePubkeyB64
            self.rotationMarker = rotationMarker
            self.provenPredecessorsB64 = provenPredecessorsB64
        }
    }

    /// One earlier key of the account being paired (ADR-0074 part 3).
    public struct OwnPredecessor: Codable, Equatable, Sendable {
        /// Standard base64 of the 64-byte account phrase seed of that key.
        /// Key material: the bundle is sealed under the DeviceGroup exporter,
        /// and this value is never logged.
        public let seedB64: String
        /// When the SENDING device adopted the key that replaced it (unix
        /// ms): the predecessor mailbox's 30-day read window counts from it.
        public let adoptedAtMs: Int64

        public init(seedB64: String, adoptedAtMs: Int64) {
            self.seedB64 = seedB64
            self.adoptedAtMs = adoptedAtMs
        }
    }

    /// ADR-0051 — one group's shared name: the group-name envelope composed
    /// from the stored name AND the stored stamp (never a fresh one).
    public struct CarriedGroupName: Codable, Equatable, Sendable {
        /// Standard base64 of the group id.
        public let groupIdB64: String
        /// Standard base64 of the envelope.
        public let envelopeB64: String

        public init(groupIdB64: String, envelopeB64: String) {
            self.groupIdB64 = groupIdB64
            self.envelopeB64 = envelopeB64
        }
    }

    /// ADR-0048 — the profile this device stores for one other person, as the
    /// profile envelope with its original stamp.
    public struct CarriedProfile: Codable, Equatable, Sendable {
        /// Standard base64 of the 32-byte person (account) key it is stored under.
        public let personKeyB64: String
        /// Standard base64 of the envelope.
        public let envelopeB64: String

        public init(personKeyB64: String, envelopeB64: String) {
            self.personKeyB64 = personKeyB64
            self.envelopeB64 = envelopeB64
        }
    }

    /// One message written to a pending contact. On the receiving device it is
    /// a DISPLAY-ONLY copy: shown as an own waiting row, never queued, never
    /// sent (the device it was typed on sends it).
    public struct CarriedHeldMessage: Codable, Equatable, Sendable {
        public let text: String
        /// The sending client's own JSON form of the held attachment refs;
        /// opaque here (only their number is used, to match the real message).
        public let attachments: [JSONValue]
        /// The message id it replies to, or `nil`.
        public let replyTo: Int64?
        /// When it was written (Unix ms): a copy unmatched 7 days later goes.
        public let heldAtMs: Int64

        public init(text: String, attachments: [JSONValue] = [], replyTo: Int64? = nil, heldAtMs: Int64) {
            self.text = text
            self.attachments = attachments
            self.replyTo = replyTo
            self.heldAtMs = heldAtMs
        }

        enum CodingKeys: String, CodingKey { case text, attachments, replyTo, heldAtMs }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(text, forKey: .text)
            try c.encode(attachments, forKey: .attachments)
            // Written as `null`, not left out: the field is part of the shape.
            if let replyTo { try c.encode(replyTo, forKey: .replyTo) } else { try c.encodeNil(forKey: .replyTo) }
            try c.encode(heldAtMs, forKey: .heldAtMs)
        }
    }

    /// ADR-0079 section 5 — a person added through their invite link whose
    /// app has not answered yet. Field names follow ``PendingContact``.
    public struct CarriedPendingContact: Codable, Equatable, Sendable {
        /// 32 lower-case hex characters (the 16-byte account id).
        public let accountIdHex: String
        public let signaturePubkeyB64: String
        public let x25519PubkeyB64: String
        public let displayName: String
        public let username: String
        public let addedAtMs: Int64
        /// Standard base64 of the 16-byte id of the link the request answered.
        public let linkIdB64: String
        /// What was written to the person, in queue order.
        public let heldMessages: [CarriedHeldMessage]

        public init(
            accountIdHex: String, signaturePubkeyB64: String, x25519PubkeyB64: String,
            displayName: String, username: String, addedAtMs: Int64, linkIdB64: String,
            heldMessages: [CarriedHeldMessage] = []
        ) {
            self.accountIdHex = accountIdHex
            self.signaturePubkeyB64 = signaturePubkeyB64
            self.x25519PubkeyB64 = x25519PubkeyB64
            self.displayName = displayName
            self.username = username
            self.addedAtMs = addedAtMs
            self.linkIdB64 = linkIdB64
            self.heldMessages = heldMessages
        }
    }

    /// The bundle's format version. A bundle with any other `v` is refused.
    public static let version = 3
    /// At most this many group names, profiles and pending contacts, and this
    /// many held messages per pending contact (chosen, not measured). A bundle
    /// carrying more is refused whole; a build keeps the first ones in a
    /// stable order.
    public static let maxGroupNames = 1000
    public static let maxProfiles = 2000
    public static let maxPendingContacts = 500
    public static let maxHeldMessages = 200
    static let accountIdLength = 16
    static let linkIdLength = 16
    /// At most this many own predecessors, and this many proven predecessors
    /// per pin; a bundle carrying more is refused whole.
    public static let maxPredecessors = 8
    /// The length of an account phrase seed.
    public static let seedLength = 64
    static let signatureKeyLength = 32

    public struct V3: Codable, Equatable, Sendable {
        public let v: Int
        public let contacts: [CarriedContact]
        public let pins: [CarriedPin]
        /// base64 group ids the old device holds.
        public let knownGroupIdsB64: [String]
        /// The account's own earlier keys, OLDEST FIRST; empty when the
        /// account never rotated.
        public let ownPredecessors: [OwnPredecessor]
        /// The shared names of the groups in ``knownGroupIdsB64``.
        public let groupNames: [CarriedGroupName]
        /// The profiles of other people, never this account's own.
        public let profiles: [CarriedProfile]
        /// The pending contacts, with what was written to them.
        public let pendingContacts: [CarriedPendingContact]

        public init(
            contacts: [CarriedContact], pins: [CarriedPin], knownGroupIdsB64: [String],
            ownPredecessors: [OwnPredecessor] = [],
            groupNames: [CarriedGroupName] = [],
            profiles: [CarriedProfile] = [],
            pendingContacts: [CarriedPendingContact] = []
        ) {
            self.v = StateBundle.version
            self.contacts = contacts
            self.pins = pins
            self.knownGroupIdsB64 = knownGroupIdsB64
            self.ownPredecessors = ownPredecessors
            self.groupNames = groupNames
            self.profiles = profiles
            self.pendingContacts = pendingContacts
        }
    }

    // MARK: - Codec

    public static func encode(_ bundle: V3) -> Data {
        (try? JSONEncoder().encode(bundle)) ?? Data()
    }

    /// Decode a bundle, refusing anything that is not the shape above: another
    /// `v` (version 1 included, no dual accept), a missing field, more than
    /// ``maxPredecessors`` own predecessors or proven predecessors of one
    /// pin, a seed that is not 64 bytes, a proven key that is not 32 bytes,
    /// a time that is not an integer; more than ``maxGroupNames`` group
    /// names, ``maxProfiles`` profiles, ``maxPendingContacts`` pending
    /// contacts or ``maxHeldMessages`` held messages of one; a malformed or
    /// repeated entry of any of the three.
    public static func decode(_ bytes: Data) -> V3? {
        guard !bytes.isEmpty, bytes.count <= maxBundleBytes,
              let bundle = try? JSONDecoder().decode(V3.self, from: bytes),
              bundle.v == version,
              bundle.ownPredecessors.count <= maxPredecessors,
              bundle.groupNames.count <= maxGroupNames,
              bundle.profiles.count <= maxProfiles,
              bundle.pendingContacts.count <= maxPendingContacts
        else { return nil }
        var groupIds = Set<String>()
        for entry in bundle.groupNames {
            guard let id = Data(base64Encoded: entry.groupIdB64), !id.isEmpty,
                  let envelope = Data(base64Encoded: entry.envelopeB64), !envelope.isEmpty,
                  groupIds.insert(entry.groupIdB64).inserted else { return nil }
        }
        var personKeys = Set<String>()
        for entry in bundle.profiles {
            guard Data(base64Encoded: entry.personKeyB64)?.count == signatureKeyLength,
                  let envelope = Data(base64Encoded: entry.envelopeB64), !envelope.isEmpty,
                  personKeys.insert(entry.personKeyB64).inserted else { return nil }
        }
        var pendingIds = Set<String>()
        for entry in bundle.pendingContacts {
            guard isAccountIdHex(entry.accountIdHex),
                  Data(base64Encoded: entry.signaturePubkeyB64)?.count == signatureKeyLength,
                  Data(base64Encoded: entry.x25519PubkeyB64)?.count == signatureKeyLength,
                  Data(base64Encoded: entry.linkIdB64)?.count == linkIdLength,
                  entry.heldMessages.count <= maxHeldMessages,
                  pendingIds.insert(entry.accountIdHex).inserted else { return nil }
        }
        for own in bundle.ownPredecessors {
            guard var seed = Data(base64Encoded: own.seedB64) else { return nil }
            let length = seed.count
            seed.resetBytes(in: 0..<seed.count)
            guard length == seedLength else { return nil }
        }
        for pin in bundle.pins {
            guard pin.provenPredecessorsB64.count <= maxPredecessors else { return nil }
            for key in pin.provenPredecessorsB64 {
                guard Data(base64Encoded: key)?.count == signatureKeyLength else { return nil }
            }
        }
        return bundle
    }

    // MARK: - Mapping to and from this client's stores

    /// Map an iOS accepted contact to the carried shape.
    public static func carried(_ contact: AcceptedContact) -> CarriedContact {
        CarriedContact(
            signaturePubkeyB64: base64Encode(contact.request.fromPubkey),
            displayName: contact.displayName ?? "",
            acceptedAtMs: Double(StateBundle.acceptedAtMs(contact)),
            accountIdHex: contact.request.fromAccountId.map { hexEncode($0) },
            x25519PubkeyB64: nil,
            groupIdB64: contact.groupIdHex.flatMap { hexDecode($0) }.map { base64Encode($0) }
        )
    }

    /// Map a carried contact back to an iOS accepted contact. `nil` when the
    /// signature key does not decode. The synthesised request carries a stable
    /// negative `messageId` derived from the key, so it never collides with a
    /// real mailbox row id.
    public static func accepted(_ carried: CarriedContact) -> AcceptedContact? {
        guard let pubkey = base64Decode(carried.signaturePubkeyB64), pubkey.count == 32 else {
            return nil
        }
        let accountId = carried.accountIdHex.flatMap { hexDecode($0) }.flatMap { $0.count == 16 ? $0 : nil }
        // A carried contact is already accepted: its request routes nothing
        // (ADR-0078: no mailbox is derived from an identity key), so the
        // pseudonym slot carries zeros, as a restored contact's does.
        let pseudonym = Data(count: 32)
        var id: UInt64 = 0
        for byte in pubkey.prefix(7) { id = (id << 8) | UInt64(byte) }
        let createdAt = ISO8601DateFormatter().string(
            from: Date(timeIntervalSince1970: max(0, carried.acceptedAtMs) / 1000))
        let request = IncomingContactRequest(
            messageId: -Int64(id) - 1,
            fromPubkey: pubkey,
            fromAccountId: accountId,
            fromPseudonym: pseudonym,
            validAfterMs: 0,
            intro: nil,
            createdAt: createdAt
        )
        let groupIdHex = carried.groupIdB64.flatMap { base64Decode($0) }.map { hexEncode($0) }
        return AcceptedContact(
            request: request,
            groupIdHex: groupIdHex,
            displayName: carried.displayName.isEmpty ? nil : carried.displayName
        )
    }

    private static func acceptedAtMs(_ contact: AcceptedContact) -> Int64 {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: contact.request.createdAt)
            ?? ISO8601DateFormatter().date(from: contact.request.createdAt)
        return Int64((date?.timeIntervalSince1970 ?? 0) * 1000)
    }

    /// The own predecessors a bundle carries for `seeds` (oldest first, as
    /// the predecessor slot holds them), each with the time its key was
    /// retired on this device. More than ``maxPredecessors`` keeps the OLDEST
    /// (the key the conversation leaves are bound to) and the newest ones.
    static func carriedOwnPredecessors(
        seeds: [Data], adoptedAtMs: (_ keyB64: String) -> Int64
    ) -> [OwnPredecessor] {
        let valid = seeds.filter { $0.count == seedLength }
        let kept = valid.count <= maxPredecessors
            ? valid
            : [valid[0]] + valid.suffix(maxPredecessors - 1)
        return kept.compactMap { seed in
            guard let key = try? Identity.fromSeed(seed: seed).signaturePublicKey() else { return nil }
            return OwnPredecessor(
                seedB64: seed.base64EncodedString(), adoptedAtMs: adoptedAtMs(base64Encode(key)))
        }
    }

    /// A contact's proven predecessors for the wire: oldest first, the
    /// nearest ``maxPredecessors`` when there are more.
    static func carriedProvenPredecessors(of pinnedKeyB64: String, in proven: ProvenPredecessors) -> [String] {
        // The resolver lists the nearest first.
        let nearestFirst = proven.predecessors(of: pinnedKeyB64)
            .filter { Data(base64Encoded: $0)?.count == signatureKeyLength }
        return Array(nearestFirst.prefix(maxPredecessors).reversed())
    }

    // MARK: - Bundle v3: group names, profiles, pending contacts

    /// ADR-0051 — the shared names this device holds for `groupIdHexes`, in
    /// that order, each composed from the stored name and the STORED stamp
    /// (a fresh stamp would out-order a later rename). A cleared name is
    /// carried too: its stamp still orders. At most ``maxGroupNames``.
    static func carriedGroupNames(
        groupIdHexes: [String], load: (_ groupIdHex: String) -> StoredGroupName?
    ) -> [CarriedGroupName] {
        var out: [CarriedGroupName] = []
        var seen = Set<String>()
        for hex in groupIdHexes where out.count < maxGroupNames {
            guard let id = hexDecode(hex), !id.isEmpty, seen.insert(hex).inserted,
                  let stored = load(hex),
                  let envelope = try? composeGroupNameEnvelope(
                    groupName: FfiGroupName(name: stored.name, updatedAtMs: stored.updatedAtMs))
            else { continue }
            out.append(CarriedGroupName(
                groupIdB64: base64Encode(id), envelopeB64: base64Encode(envelope)))
        }
        return out
    }

    /// ADR-0048 — `claim` as the profile envelope with its original stamp.
    /// A stored username the core refuses to compose (one with no
    /// certificate it can carry) is left out rather than losing the name.
    static func profileEnvelope(_ claim: ProfileClaim) -> Data? {
        let full = FfiProfile(
            displayName: claim.displayName,
            username: claim.username,
            updatedAtMs: claim.updatedAtMs,
            accountId: claim.accountId,
            usernameCertificate: claim.username == nil ? nil : claim.usernameCertificate,
            grants: [],
            avatar: claim.avatar?.ffi)
        if let envelope = try? composeProfileEnvelope(profile: full) { return envelope }
        let nameOnly = FfiProfile(
            displayName: claim.displayName,
            username: nil,
            updatedAtMs: claim.updatedAtMs,
            accountId: claim.accountId,
            usernameCertificate: nil,
            grants: [],
            avatar: claim.avatar?.ffi)
        return try? composeProfileEnvelope(profile: nameOnly)
    }

    /// ADR-0048 — every stored profile but this account's own keys, sorted by
    /// key (a stable order), at most ``maxProfiles``.
    static func carriedProfiles(
        _ profiles: [String: ProfileClaim], ownKeysB64: Set<String>
    ) -> [CarriedProfile] {
        var out: [CarriedProfile] = []
        for key in profiles.keys.sorted() where out.count < maxProfiles {
            guard !ownKeysB64.contains(key),
                  Data(base64Encoded: key)?.count == signatureKeyLength,
                  let claim = profiles[key],
                  let envelope = profileEnvelope(claim) else { continue }
            out.append(CarriedProfile(personKeyB64: key, envelopeB64: base64Encode(envelope)))
        }
        return out
    }

    /// ADR-0079 section 5 — the pending contacts in the order they are kept,
    /// each with what was written to it (`held`), at most
    /// ``maxPendingContacts``. A record whose keys or link id do not have
    /// their lengths is left out (the receiver would refuse the whole bundle).
    static func carriedPendingContacts(
        _ list: [PendingContact], held: (PendingContact) -> [CarriedHeldMessage]
    ) -> [CarriedPendingContact] {
        var out: [CarriedPendingContact] = []
        var seen = Set<String>()
        for contact in list where out.count < maxPendingContacts {
            guard isAccountIdHex(contact.accountIdHex),
                  contact.signaturePubkey.count == signatureKeyLength,
                  contact.x25519Pubkey.count == signatureKeyLength,
                  contact.linkId.count == linkIdLength,
                  seen.insert(contact.accountIdHex).inserted else { continue }
            out.append(CarriedPendingContact(
                accountIdHex: contact.accountIdHex,
                signaturePubkeyB64: base64Encode(contact.signaturePubkey),
                x25519PubkeyB64: base64Encode(contact.x25519Pubkey),
                displayName: contact.displayName,
                username: contact.username,
                addedAtMs: contact.addedAtMs,
                linkIdB64: base64Encode(contact.linkId),
                heldMessages: Array(held(contact).prefix(maxHeldMessages))))
        }
        return out
    }

    /// What was written to a pending contact, as the bundle carries it: this
    /// device's own held messages (text only: a message to a pending contact
    /// is never written with attachments on this client) and the display
    /// copies it holds from another device (their attachment refs as they
    /// came), oldest first.
    static func carriedHeldMessages(
        own: [HeldSendQueue.Entry], copies: [HeldSendQueue.Entry]
    ) -> [CarriedHeldMessage] {
        var out: [(Int, CarriedHeldMessage)] = []
        for entry in own {
            out.append((out.count, CarriedHeldMessage(text: entry.text, heldAtMs: entry.queuedAtMs)))
        }
        for entry in copies {
            let attachments = entry.attachments.compactMap { try? JSONDecoder().decode(JSONValue.self, from: $0) }
            out.append((out.count, CarriedHeldMessage(
                text: entry.text, attachments: attachments, heldAtMs: entry.queuedAtMs)))
        }
        return out
            .sorted { $0.1.heldAtMs == $1.1.heldAtMs ? $0.0 < $1.0 : $0.1.heldAtMs < $1.1.heldAtMs }
            .map(\.1)
    }

    /// The held messages of `contact` read from this device's stores.
    static func heldMessagesFromStores(_ contact: PendingContact, accountIdB64: String) -> [CarriedHeldMessage] {
        let sealer = HeldSendQueue.Sealer(
            seal: { try MessageLogStore.sealUnderWrapKey($0) },
            open: { try MessageLogStore.openUnderWrapKey($0) })
        let own = HeldSendQueue(ownerAccountId: accountIdB64, sealer: sealer)
            .list(conversationHex: contact.queueKey)
        let copies = PendingDisplayCopies(ownerAccountIdB64: accountIdB64, sealer: sealer)
            .list(key: contact.queueKey)
        return carriedHeldMessages(own: own, copies: copies)
    }

    /// Read this device's contacts, pins, known group ids, the account's own
    /// predecessor seeds and each pinned contact's proven predecessors into a
    /// bundle.
    ///
    /// Bundle v3 — also the shared group names of the known groups, the
    /// `profiles` this device stores for other people (``ProfileDirectory``,
    /// main actor, so the caller reads them) minus `ownKeysB64`, and the
    /// pending contacts with what was written to them (`heldMessages`).
    static func build(
        accountIdB64: String,
        pinStore: PeerPinStore? = nil,
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        adoptions: PredecessorMailbox.AdoptionStore = PredecessorMailbox.AdoptionStore(),
        provenStore: ProvenPredecessorStore? = nil,
        groupNameStore: GroupNameStore = GroupNameStore(),
        knownGroupIdHexes: [String]? = nil,
        profiles: [String: ProfileClaim] = [:],
        ownKeysB64: Set<String> = [],
        pendingDefaults: UserDefaults = .standard,
        heldMessages: ((PendingContact) -> [CarriedHeldMessage])? = nil,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) -> V3 {
        let pinStore = pinStore ?? PeerPinStore(ownerAccountIdB64: accountIdB64)
        let proven = (provenStore ?? ProvenPredecessorStore(ownerAccountIdB64: accountIdB64)).load()
        let contacts = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        var pins: [CarriedPin] = []
        for contact in contacts {
            guard let accountId = contact.request.fromAccountId else { continue }
            let accountIdB64 = base64Encode(accountId)
            guard let pin = pinStore.readPin(accountIdB64: accountIdB64) else { continue }
            pins.append(CarriedPin(
                accountIdB64: accountIdB64,
                pinnedSignaturePubkeyB64: pin.pinnedSignaturePubkeyB64,
                rotationMarker: pin.rotationMarker,
                provenPredecessorsB64: carriedProvenPredecessors(
                    of: pin.pinnedSignaturePubkeyB64, in: proven)))
        }
        // A key with no recorded adoption (adopted before the time was kept)
        // is stamped now, exactly as the mailbox poll stamps it.
        let own = carriedOwnPredecessors(seeds: ownPredecessors.load(accountIdB64: accountIdB64)) {
            adoptions.recordIfAbsent(accountIdB64: accountIdB64, keyB64: $0, nowMs: nowMs)
        }
        let groupIdHexes = (knownGroupIdHexes ?? Array(KnownGroupsStore().load(accountIdB64: accountIdB64))).sorted()
        let groupIds = groupIdHexes
            .compactMap { hexDecode($0) }
            .map { base64Encode($0) }
        let names = carriedGroupNames(groupIdHexes: groupIdHexes) {
            groupNameStore.load(accountIdB64: accountIdB64, groupIdHex: $0)
        }
        let pending = carriedPendingContacts(
            PendingContactStore.load(accountIdB64: accountIdB64, defaults: pendingDefaults),
            held: heldMessages ?? { heldMessagesFromStores($0, accountIdB64: accountIdB64) })
        return fittingCap(V3(
            contacts: contacts.map(carried), pins: pins, knownGroupIdsB64: groupIds,
            ownPredecessors: own,
            groupNames: names,
            profiles: carriedProfiles(profiles, ownKeysB64: ownKeysB64),
            pendingContacts: pending))
    }

    /// `bundle` trimmed until its encoding fits `cap` (the receiver refuses
    /// anything larger, whole): profiles from the end first, then group
    /// names from the end, then held messages from the end (the last pending
    /// contact's last message first). Contacts, pins, known group ids, own
    /// predecessors and the pending contacts themselves are never trimmed. A
    /// bundle that still does not fit is returned as it is.
    static func fittingCap(_ bundle: V3, cap: Int = maxBundleBytes) -> V3 {
        var total = encode(bundle).count
        guard total > cap else { return bundle }
        var profiles = bundle.profiles
        var names = bundle.groupNames
        var pending = bundle.pendingContacts
        // Each removal shrinks the encoding by the entry and, while others
        // remain, its separating comma; the exact size is checked at the end.
        func size<T: Encodable>(_ value: T, alone: Bool) -> Int {
            ((try? JSONEncoder().encode(value))?.count ?? 0) + (alone ? 0 : 1)
        }
        while total > cap, let last = profiles.last {
            total -= size(last, alone: profiles.count == 1)
            profiles.removeLast()
        }
        while total > cap, let last = names.last {
            total -= size(last, alone: names.count == 1)
            names.removeLast()
        }
        while total > cap, let index = pending.lastIndex(where: { !$0.heldMessages.isEmpty }) {
            let contact = pending[index]
            total -= size(contact.heldMessages[contact.heldMessages.count - 1], alone: contact.heldMessages.count == 1)
            pending[index] = CarriedPendingContact(
                accountIdHex: contact.accountIdHex, signaturePubkeyB64: contact.signaturePubkeyB64,
                x25519PubkeyB64: contact.x25519PubkeyB64, displayName: contact.displayName,
                username: contact.username, addedAtMs: contact.addedAtMs, linkIdB64: contact.linkIdB64,
                heldMessages: Array(contact.heldMessages.dropLast()))
        }
        let trimmed = V3(
            contacts: bundle.contacts, pins: bundle.pins, knownGroupIdsB64: bundle.knownGroupIdsB64,
            ownPredecessors: bundle.ownPredecessors, groupNames: names, profiles: profiles,
            pendingContacts: pending)
        let exact = encode(trimmed).count
        if exact > cap, exact < encode(bundle).count, !(profiles.isEmpty && names.isEmpty
            && pending.allSatisfy { $0.heldMessages.isEmpty }) {
            // The estimate was short by a few bytes: one more step.
            return fittingCap(trimmed, cap: cap)
        }
        NSLog("state bundle: trimmed to fit the size cap (%d profiles, %d group names left)",
              profiles.count, names.count)
        return trimmed
    }

    /// ADR-0051 — store the carried group names through the store a received
    /// name lands in, keyed on the group id, under the strictly-newer rule
    /// (`isNewer` is the core's `groupNameIsNewer`): a name this device holds
    /// at a stamp as new or newer is kept. No authority check: the bundle
    /// comes from a device of this account, sealed under the DeviceGroup
    /// exporter. A carried entry that does not decode is skipped alone.
    /// Nothing is posted to any group. Returns how many names were stored.
    @discardableResult
    static func storeGroupNames(
        _ carried: [CarriedGroupName],
        accountIdB64: String,
        store: GroupNameStore,
        isNewer: (_ incoming: UInt64, _ stored: UInt64) -> Bool = {
            groupNameIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1)
        }
    ) -> Int {
        var stored = 0
        for entry in carried {
            guard let groupId = Data(base64Encoded: entry.groupIdB64), !groupId.isEmpty,
                  let envelope = Data(base64Encoded: entry.envelopeB64),
                  let decoded = try? decodeGroupNameEnvelope(plaintext: envelope) else { continue }
            let groupIdHex = hexEncode(groupId)
            if let held = store.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
               !isNewer(decoded.updatedAtMs, held.updatedAtMs) { continue }
            store.record(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                value: StoredGroupName(name: decoded.name, updatedAtMs: decoded.updatedAtMs))
            stored += 1
        }
        return stored
    }

    /// ADR-0048 — the carried profiles as claims to store, each under its
    /// person key; an entry that does not decode, or that names one of
    /// `ownKeysB64`, is skipped alone. Grants are never taken from a bundle.
    static func carriedProfileClaims(
        _ carried: [CarriedProfile], ownKeysB64: Set<String>
    ) -> [(personKeyB64: String, claim: ProfileClaim)] {
        carried.compactMap { entry in
            guard !ownKeysB64.contains(entry.personKeyB64),
                  Data(base64Encoded: entry.personKeyB64)?.count == signatureKeyLength,
                  let envelope = Data(base64Encoded: entry.envelopeB64),
                  let decoded = try? decodeProfileEnvelope(plaintext: envelope) else { return nil }
            let claim = ProfileClaim(
                displayName: decoded.displayName,
                username: decoded.username,
                updatedAtMs: decoded.updatedAtMs,
                accountId: decoded.accountId,
                usernameCertificate: decoded.usernameCertificate,
                avatar: ProfileAvatar(decoded.avatar))
            return (personKeyB64: entry.personKeyB64, claim: claim)
        }
    }

    /// ADR-0079 section 5 — add the carried pending contacts this device
    /// holds no record of: neither an accepted contact nor a pending contact
    /// with that account (by signature key or account id). Written with the
    /// store's add rule (``PendingContacts/adding(_:to:)``). Returns the
    /// contacts added, each with its carried held messages.
    static func storePendingContacts(
        _ carried: [CarriedPendingContact],
        accountIdB64: String,
        accepted: [AcceptedContact],
        defaults: UserDefaults = .standard
    ) -> [(contact: PendingContact, held: [CarriedHeldMessage])] {
        var list = PendingContactStore.load(accountIdB64: accountIdB64, defaults: defaults)
        var added: [(contact: PendingContact, held: [CarriedHeldMessage])] = []
        for entry in carried {
            guard let signature = Data(base64Encoded: entry.signaturePubkeyB64),
                  signature.count == signatureKeyLength,
                  let x25519 = Data(base64Encoded: entry.x25519PubkeyB64), x25519.count == signatureKeyLength,
                  let linkId = Data(base64Encoded: entry.linkIdB64), linkId.count == linkIdLength,
                  let accountId = hexDecode(entry.accountIdHex), accountId.count == accountIdLength
            else { continue }
            let heldAsContact = accepted.contains {
                $0.request.fromPubkey == signature || $0.request.fromAccountId == accountId
            }
            let heldAsPending = list.contains {
                $0.signaturePubkey == signature || $0.accountIdHex == entry.accountIdHex
            }
            if heldAsContact || heldAsPending { continue }
            let contact = PendingContact(
                accountIdHex: entry.accountIdHex, signaturePubkey: signature, x25519Pubkey: x25519,
                displayName: entry.displayName, username: entry.username,
                addedAtMs: entry.addedAtMs, linkId: linkId)
            list = PendingContacts.adding(contact, to: list)
            added.append((contact: contact, held: entry.heldMessages))
        }
        if !added.isEmpty {
            PendingContactStore.save(list, accountIdB64: accountIdB64, defaults: defaults)
        }
        return added
    }

    /// The keys this account is known under on this device: the current
    /// account key (when a session put one in ``ReachCenter``) and its own
    /// predecessors. A bundle never stores a profile under one of them.
    @MainActor
    static func ownKeysB64(accountIdB64: String) -> Set<String> {
        var keys = Set(OwnAccountKeys.predecessorPublicKeys(accountIdB64: accountIdB64).map { base64Encode($0) })
        if let current = ReachCenter.shared.ownAccountKey { keys.insert(base64Encode(current)) }
        return keys
    }

    /// The main-actor half of ``apply(_:accountIdB64:pinStore:ownPredecessors:adoptions:provenStore:)``:
    /// the group names and the profiles, so they cannot interleave with the
    /// receive path's read-decide-write of the same stores (both run on the
    /// main actor). Posts ``Notification/Name/lakkoStateBundleNamesApplied``
    /// so a live chat list re-titles.
    @MainActor
    @discardableResult
    static func applyNames(
        _ bundle: V3,
        accountIdB64: String,
        groupNameStore: GroupNameStore = GroupNameStore(),
        directory: ProfileDirectory = .shared,
        ownKeysB64: Set<String>? = nil
    ) -> (groupNames: Int, profiles: Int) {
        let names = storeGroupNames(bundle.groupNames, accountIdB64: accountIdB64, store: groupNameStore)
        let own = (ownKeysB64 ?? Self.ownKeysB64(accountIdB64: accountIdB64))
            .union(bundle.ownPredecessors.compactMap { own -> String? in
                guard let seed = Data(base64Encoded: own.seedB64),
                      let key = try? Identity.fromSeed(seed: seed).signaturePublicKey() else { return nil }
                return base64Encode(key)
            })
        let profiles = directory.adoptCarriedProfiles(
            carriedProfileClaims(bundle.profiles, ownKeysB64: own), accountIdB64: accountIdB64)
        if names > 0 || profiles > 0 {
            NotificationCenter.default.post(
                name: .lakkoStateBundleNamesApplied, object: nil,
                userInfo: [appliedAccountKey: accountIdB64])
        }
        return (names, profiles)
    }

    /// The predecessor slot after a bundle: the bundle's seeds in their order
    /// (oldest first), then whatever this device already held that the bundle
    /// does not name (a rotation it adopted itself, later than the bundle).
    static func mergedOwnPredecessors(held: [Data], carried: [Data]) -> [Data] {
        var out: [Data] = []
        for seed in carried + held where !out.contains(seed) { out.append(seed) }
        return out
    }

    /// The proven edges one carried pin stands for: each predecessor is
    /// superseded by the next, the last by the pinned key.
    static func provenEdges(of pin: CarriedPin) -> [(oldKeyB64: String, newKeyB64: String)] {
        let chain = pin.provenPredecessorsB64 + [pin.pinnedSignaturePubkeyB64]
        return zip(chain, chain.dropFirst()).map { (oldKeyB64: $0, newKeyB64: $1) }
    }

    /// ADR-0074 part 3 — store the account's own predecessors exactly as a
    /// kept device that adopted each rotation holds them: the seeds in the
    /// predecessor slot (oldest first), and each key's adoption time as the
    /// SENDING device recorded it, so the mailbox of each is read for its
    /// own 30 days from then. An adoption time this device already holds is
    /// never moved. Returns how many seeds are held afterwards; throws when
    /// the slot could not be written (nothing else of the bundle is then
    /// applied by ``apply(_:accountIdB64:pinStore:ownPredecessors:adoptions:provenStore:)``).
    @discardableResult
    static func storeOwnPredecessors(
        _ carried: [OwnPredecessor],
        accountIdB64: String,
        store: OwnPredecessorStoring,
        adoptions: PredecessorMailbox.AdoptionStore
    ) throws -> Int {
        var seeds: [Data] = []
        for own in carried {
            guard let seed = Data(base64Encoded: own.seedB64), seed.count == seedLength,
                  let key = try? Identity.fromSeed(seed: seed).signaturePublicKey() else { continue }
            seeds.append(seed)
            adoptions.recordIfAbsent(
                accountIdB64: accountIdB64, keyB64: base64Encode(key), nowMs: own.adoptedAtMs)
        }
        guard !seeds.isEmpty else { return store.load(accountIdB64: accountIdB64).count }
        let held = store.load(accountIdB64: accountIdB64)
        let merged = mergedOwnPredecessors(held: held, carried: seeds)
        if merged != held {
            try store.replace(merged, accountIdB64: accountIdB64)
        }
        return merged.count
    }

    /// Merge a bundle into this device: contacts not already present (by
    /// signature key), pins through the monotone setter, the known group ids
    /// for the chat list's "Synchronizing..." label. `accountIdB64` is the
    /// account the bundle belongs to (the one being paired): its contacts and
    /// pins are filed under it, never under whoever else is signed in.
    ///
    /// ADR-0074 part 3 — the account's own predecessors are stored FIRST
    /// (the device binding minted at the first chat bootstrap reads them),
    /// and each pinned contact's proven predecessors are recorded in the
    /// proven-predecessor store, on the sending device's word: it is a device
    /// of this account, and the bundle is sealed under the DeviceGroup
    /// exporter. A pin this device refuses records nothing for that contact.
    ///
    /// Bundle v3 — the pending contacts this device holds no record of are
    /// added with display-only copies of what was written to them
    /// (``PendingDisplayCopies``); the group names and profiles are stored on
    /// the main actor (``applyNames(_:accountIdB64:groupNameStore:directory:ownKeysB64:)``),
    /// after this returns, unless `applyNamesOnMain` is `false` (tests).
    @discardableResult
    static func apply(
        _ bundle: V3,
        accountIdB64: String,
        pinStore: PeerPinStore? = nil,
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        adoptions: PredecessorMailbox.AdoptionStore = PredecessorMailbox.AdoptionStore(),
        provenStore: ProvenPredecessorStore? = nil,
        pendingDefaults: UserDefaults = .standard,
        displayCopies: PendingDisplayCopies? = nil,
        applyNamesOnMain: Bool = true
    ) -> (contacts: Int, pins: Int) {
        do {
            try storeOwnPredecessors(
                bundle.ownPredecessors, accountIdB64: accountIdB64,
                store: ownPredecessors, adoptions: adoptions)
        } catch {
            // Without the predecessor seeds this device would bind its
            // conversation leaves under the wrong key; the binding rule
            // re-reads the slot at every bootstrap, so a later bundle (a
            // second pairing) still repairs a device that holds no chat.
            NSLog("state bundle: the account's own predecessors were not stored")
        }
        let pinStore = pinStore ?? PeerPinStore(ownerAccountIdB64: accountIdB64)
        let provenStore = provenStore ?? ProvenPredecessorStore(ownerAccountIdB64: accountIdB64)
        var current = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        let have = Set(current.map { $0.request.fromPubkey })
        var fresh: [AcceptedContact] = []
        for carried in bundle.contacts {
            guard let contact = accepted(carried), !have.contains(contact.request.fromPubkey) else {
                continue
            }
            fresh.append(contact)
        }
        let added = fresh.count
        if added > 0 {
            current.append(contentsOf: fresh)
            AcceptedContactsStore.save(current, accountIdB64: accountIdB64)
        }
        var pins = 0
        for pin in bundle.pins {
            // A pin the setter refuses (a lower marker than one already held)
            // is not ours to force.
            if pinStore.setPin(
                accountIdB64: pin.accountIdB64,
                pinnedSignaturePubkeyB64: pin.pinnedSignaturePubkeyB64,
                rotationMarker: pin.rotationMarker) == .set {
                pins += 1
            }
            // The predecessors lead to the key the bundle pinned: recorded
            // only when that is the key this device holds for the contact.
            guard pinStore.readPin(accountIdB64: pin.accountIdB64)?.pinnedSignaturePubkeyB64
                    == pin.pinnedSignaturePubkeyB64 else { continue }
            for edge in provenEdges(of: pin) {
                provenStore.record(
                    oldKeyB64: edge.oldKeyB64, newKeyB64: edge.newKeyB64, evidence: .verifiedProof)
            }
        }
        saveKnownGroupIds(bundle.knownGroupIdsB64, accountIdB64: accountIdB64)
        // ADR-0079 section 5 — the pending contacts, after the accepted ones
        // so a person already accepted here is not also pending. What was
        // written to each is kept as display-only copies, never in this
        // device's own held-send queue: the device it was typed on sends it.
        let pendingAdded = storePendingContacts(
            bundle.pendingContacts, accountIdB64: accountIdB64,
            accepted: AcceptedContactsStore.load(accountIdB64: accountIdB64),
            defaults: pendingDefaults)
        if !pendingAdded.isEmpty {
            let copies = displayCopies ?? PendingDisplayCopies(ownerAccountIdB64: accountIdB64)
            for added in pendingAdded where !added.held.isEmpty {
                copies.store(added.held, key: added.contact.queueKey)
            }
        }
        // The contacts ride in the notification too: a live repository merges
        // them into its state (whose next save would otherwise overwrite the
        // store write above).
        NotificationCenter.default.post(
            name: .lakkoStateBundleApplied, object: fresh,
            userInfo: [appliedAccountKey: accountIdB64, pendingAddedKey: !pendingAdded.isEmpty])
        if applyNamesOnMain, !bundle.groupNames.isEmpty || !bundle.profiles.isEmpty {
            Task { @MainActor in
                let applied = applyNames(bundle, accountIdB64: accountIdB64)
                NSLog("state bundle: %d group names, %d profiles", applied.groupNames, applied.profiles)
            }
        }
        return (added, pins)
    }

    // MARK: - Known group ids (drives the one "Synchronizing..." label)
    //
    // Per account (2026-09-30): the ids name the groups of the account that was
    // paired, so they are filed under it, the way ``AcceptedContactsStore``
    // files the contacts. They used to sit under one device-wide key, so a
    // second account signed in on the same phone showed "Synchronizing..." for
    // the first account's groups. A caller that does not name the account acts
    // on the bound one (``AcceptedContactsStore/boundAccountIdB64``); with none,
    // reads are empty and writes do nothing.

    static func saveKnownGroupIds(
        _ idsB64: [String],
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64,
        defaults: UserDefaults = .standard
    ) {
        guard let accountIdB64 else { return }
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let key = knownGroupsKey(accountIdB64: accountIdB64)
        if idsB64.isEmpty {
            defaults.removeObject(forKey: key)
        } else {
            defaults.set(idsB64, forKey: key)
        }
    }

    static func loadKnownGroupIds(
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64,
        defaults: UserDefaults = .standard
    ) -> [String] {
        guard let accountIdB64 else { return [] }
        return defaults.stringArray(forKey: knownGroupsKey(accountIdB64: accountIdB64)) ?? []
    }

    /// Forget-device: drop one account's known group ids (and the old
    /// device-wide row, which nothing reads any more).
    static func clearKnownGroupIds(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: knownGroupsKey(accountIdB64: accountIdB64))
        defaults.removeObject(forKey: legacyKnownGroupsKey)
    }

    /// How many known groups this device has not joined yet. Clears the stored
    /// list once every known group is held, so the label cannot outlive its
    /// cause. `joinedGroupIdHexes` are this device's hex group ids.
    static func pendingKnownGroups(
        joinedGroupIdHexes: [String],
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64,
        defaults: UserDefaults = .standard
    ) -> Int {
        guard let accountIdB64 else { return 0 }
        let known = loadKnownGroupIds(accountIdB64: accountIdB64, defaults: defaults)
        guard !known.isEmpty else { return 0 }
        let joined = Set(joinedGroupIdHexes.compactMap { hexDecode($0) }.map { base64Encode($0) })
        let pending = known.filter { !joined.contains($0) }
        if pending.isEmpty { saveKnownGroupIds([], accountIdB64: accountIdB64, defaults: defaults) }
        return pending.count
    }

    // MARK: - The tagged mailbox row

    public static func wrapRow(_ sealed: Data) -> Data {
        rowMagic + sealed
    }

    public static func isStateRow(_ row: Data) -> Bool {
        row.count > rowMagic.count && row.prefix(rowMagic.count) == rowMagic
    }

    public static func unwrapRow(_ row: Data) -> Data? {
        guard isStateRow(row) else { return nil }
        return row.subdata(in: row.startIndex + rowMagic.count..<row.endIndex)
    }

    // MARK: - Transport

    /// Scanner side: seal `bundle` under the per-pairing exporter key of
    /// `group` and publish it as a tagged row.
    public static func publish(
        bundle: V3,
        group: DeviceGroup,
        identity: Identity,
        rendezvousPseudonym: Data,
        ds: DsClient = DsClient()
    ) async throws {
        var key = try group.exportSecret(
            identity: identity,
            label: exporterLabel,
            context: rendezvousPseudonym,
            length: UInt32(exporterKeyLength)
        )
        defer { key.resetBytes(in: 0..<key.count) }
        let sealed = try sealWithKey(key: key, plaintext: encode(bundle))
        _ = try await ds.publishRendezvous(id: rendezvousPseudonym, payload: wrapRow(sealed))
    }

    /// Joiner side: poll the state row past the Welcome cursor (skipping the
    /// history and recovery rows), open it under the per-pairing exporter key,
    /// decode. `nil` when none arrived within the window or it did not decode.
    public static func receive(
        group: DeviceGroup,
        identity: Identity,
        rendezvousPseudonym: Data,
        afterCursor: Int64,
        timeoutSeconds: TimeInterval = pollTimeoutSeconds,
        ds: DsClient = DsClient()
    ) async -> V3? {
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        var cursor = afterCursor
        var sealed: Data?
        while sealed == nil {
            if Task.isCancelled { return nil }
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { return nil }
            let waitSecs = min(perRequestLongPollSeconds, max(1, Int(remaining)))
            let result: FetchResult
            do {
                result = try await ds.fetchRendezvous(id: rendezvousPseudonym, after: cursor, limit: 1, waitSecs: waitSecs)
            } catch {
                // 404 ("no rows yet") and network blips: keep polling until
                // the deadline, as the history and recovery polls do.
                try? await Task.sleep(nanoseconds: 500_000_000)
                continue
            }
            if let first = result.messages.first {
                if let body = unwrapRow(first.payload) {
                    sealed = body
                } else {
                    cursor = first.id
                }
                continue
            }
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
        guard let sealed else { return nil }
        do {
            var key = try group.exportSecret(
                identity: identity,
                label: exporterLabel,
                context: rendezvousPseudonym,
                length: UInt32(exporterKeyLength)
            )
            defer { key.resetBytes(in: 0..<key.count) }
            return decode(try openWithKey(key: key, wire: sealed))
        } catch {
            NSLog("state bundle open failed: %@", error.localizedDescription)
            return nil
        }
    }

    /// 32 lower-case hex characters: a 16-byte account id as the pending
    /// contact names it.
    static func isAccountIdHex(_ hex: String) -> Bool {
        hex.utf8.count == accountIdLength * 2 && hex.utf8.allSatisfy {
            ($0 >= 0x30 && $0 <= 0x39) || ($0 >= 0x61 && $0 <= 0x66)
        }
    }

    // MARK: - Hex

    static func hexEncode(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    static func hexDecode(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        return out
    }
}

/// Any JSON value, kept as it was read: the held attachment refs of another
/// client travel in that client's own form and are never interpreted here.
public indirect enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let value = try? c.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? c.decode(Double.self) {
            self = .number(value)
        } else if let value = try? c.decode(String.self) {
            self = .string(value)
        } else if let value = try? c.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try c.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let value): try c.encode(value)
        case .number(let value): try c.encode(value)
        case .string(let value): try c.encode(value)
        case .array(let value): try c.encode(value)
        case .object(let value): try c.encode(value)
        }
    }
}
