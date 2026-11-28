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
/// Format `v` 4 (2026-11-13, flag day: `v` 3 is refused) — one more required
/// array, ``CarriedChannelSeeds``: per channel the account is subscribed to
/// (a live channel subscription register on the sending device), the newest
/// epoch seeds that device holds, at most ``maxSeedsPerChannel``, so the new
/// device reads the posts the old one can. The channels themselves, their
/// subscriber key, read key and role travel in the synced channel
/// subscription register (ADR-0050 section 2.9 as amended 2026-11-13), not
/// here; the seeds are kept until a register for the channel arrives, and
/// dropped if it is (or becomes) left. A seed held for an epoch is never
/// overwritten.
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

    /// ADR-0050 section 2.9 (bundle v4) — one channel's newest epoch seeds.
    public struct CarriedChannelSeeds: Codable, Equatable, Sendable {
        /// Standard base64 of the 16-byte channel id.
        public let channelIdB64: String
        /// 1 to ``maxSeedsPerChannel`` seeds, strictly ascending by epoch.
        public let seeds: [CarriedSeed]

        public init(channelIdB64: String, seeds: [CarriedSeed]) {
            self.channelIdB64 = channelIdB64
            self.seeds = seeds
        }
    }

    /// One epoch seed. Key material: the bundle is sealed under the
    /// DeviceGroup exporter, and this value is never logged.
    public struct CarriedSeed: Codable, Equatable, Sendable {
        public let epoch: UInt32
        /// Standard base64 of the 32-byte seed.
        public let seedB64: String

        public init(epoch: UInt32, seedB64: String) {
            self.epoch = epoch
            self.seedB64 = seedB64
        }
    }

    /// ADR-0080 amendment (2026-11-23) — one device card the confirming
    /// device holds, for the DeviceGroup leaf it is keyed on. Display-only;
    /// attributed by the confirming device, an own device of the account.
    public struct CarriedDeviceCard: Codable, Equatable, Sendable {
        /// Standard base64 of the 32-byte leaf signature key.
        public let leafKeyB64: String
        /// Standard base64 of the device-card envelope (44 to 270 bytes).
        public let envelopeB64: String

        public init(leafKeyB64: String, envelopeB64: String) {
            self.leafKeyB64 = leafKeyB64
            self.envelopeB64 = envelopeB64
        }
    }

    /// Alignment item F (ADR-0101 sections 10 and 11) — the highest seal
    /// announcement `seq` this device keeps for a contact, so a newly paired
    /// device does not start at floor 0. Additive inside v4, absent when
    /// empty; the same JSON on web, iOS and Android.
    public struct CarriedSealFloor: Codable, Equatable, Sendable {
        /// 32 lower-case hex characters of the 16-byte account id.
        public let accountIdHex: String
        public let seq: UInt64

        public init(accountIdHex: String, seq: UInt64) {
            self.accountIdHex = accountIdHex
            self.seq = seq
        }
    }

    /// Alignment item F (UX 16.11) — a contact's key an unsealed reset left
    /// behind (the one-to-one with it is closed). Additive inside v4.
    public struct CarriedStartedOver: Codable, Equatable, Sendable {
        /// 32 lower-case hex characters of the 16-byte account id.
        public let accountIdHex: String
        /// Standard base64 of the 32-byte key of before the reset.
        public let oldKeyB64: String
        /// When it started over, UNIX milliseconds (0 when not recorded).
        public let atMs: Int64

        public init(accountIdHex: String, oldKeyB64: String, atMs: Int64) {
            self.accountIdHex = accountIdHex
            self.oldKeyB64 = oldKeyB64
            self.atMs = atMs
        }
    }

    /// At most this many of each (chosen, not measured; the pins cap).
    public static let maxSealFloors = 2000
    public static let maxStartedOver = 2000

    /// At most this many device cards (`MAX_LEAVES_PER_ACCOUNT`), and the
    /// envelope bounds (`lakko-contacts::device_card`).
    public static let maxDeviceCards = 8
    static let deviceCardEnvelopeMinLength = 44
    static let deviceCardEnvelopeMaxLength = 270

    /// The bundle's format version. A bundle with any other `v` is refused.
    public static let version = 4
    /// At most this many channels carry seeds, and this many seeds each.
    public static let maxChannelSeedEntries = 1000
    public static let maxSeedsPerChannel = 32
    /// The per-channel seed counts a build steps down through while the
    /// encoded bundle is over the size cap; seeds never push out another
    /// array.
    static let seedLadder = [32, 16, 8, 4, 0]
    static let channelIdLength = 16
    static let channelSeedLength = 32
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

    public struct V4: Codable, Equatable, Sendable {
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
        /// Bundle v4 — the newest epoch seeds of the subscribed channels,
        /// ascending by channel id bytes.
        public let channelSeeds: [CarriedChannelSeeds]
        /// ADR-0080 amendment (2026-11-23), additive inside v4: absent (nil)
        /// when empty; at most ``StateBundle/maxDeviceCards``, one per leaf
        /// key, ascending by key bytes.
        public var deviceCards: [CarriedDeviceCard]?
        /// Alignment item F — absent (nil) when empty.
        public var sealFloors: [CarriedSealFloor]?
        /// Alignment item F — absent (nil) when empty.
        public var startedOver: [CarriedStartedOver]?

        public init(
            contacts: [CarriedContact], pins: [CarriedPin], knownGroupIdsB64: [String],
            ownPredecessors: [OwnPredecessor] = [],
            groupNames: [CarriedGroupName] = [],
            profiles: [CarriedProfile] = [],
            pendingContacts: [CarriedPendingContact] = [],
            channelSeeds: [CarriedChannelSeeds] = [],
            deviceCards: [CarriedDeviceCard]? = nil
        ) {
            self.v = StateBundle.version
            self.contacts = contacts
            self.pins = pins
            self.knownGroupIdsB64 = knownGroupIdsB64
            self.ownPredecessors = ownPredecessors
            self.groupNames = groupNames
            self.profiles = profiles
            self.pendingContacts = pendingContacts
            self.channelSeeds = channelSeeds
            self.deviceCards = deviceCards
        }
    }

    // MARK: - Codec

    public static func encode(_ bundle: V4) -> Data {
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
    public static func decode(_ bytes: Data) -> V4? {
        guard !bytes.isEmpty, bytes.count <= maxBundleBytes,
              let bundle = try? JSONDecoder().decode(V4.self, from: bytes),
              bundle.v == version,
              bundle.ownPredecessors.count <= maxPredecessors,
              bundle.groupNames.count <= maxGroupNames,
              bundle.profiles.count <= maxProfiles,
              bundle.pendingContacts.count <= maxPendingContacts,
              validChannelSeeds(bundle.channelSeeds),
              validDeviceCards(bundle.deviceCards ?? []),
              validSealFloors(bundle.sealFloors ?? []),
              validStartedOver(bundle.startedOver ?? [])
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
    ///
    /// `arrivedAtMs` (2026-11-23): on this device the person is a contact only
    /// since the bundle arrived, so the row's `createdAt` (its "contact
    /// since") is the later of the carried acceptance and that time. A request
    /// that was already in the account's mailbox then predates the contact
    /// here (``KnownContactRequest/requestPredatesContact(requestCreatedAt:contactSince:)``):
    /// the device that held the contact handled it, and this one never asks
    /// again. Mirror of Android `carriedContactSince`, web `carriedContactSinceMs`.
    public static func accepted(
        _ carried: CarriedContact,
        arrivedAtMs: Double = Date().timeIntervalSince1970 * 1000
    ) -> AcceptedContact? {
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
            from: Date(timeIntervalSince1970: max(0, carried.acceptedAtMs, arrivedAtMs) / 1000))
        let request = IncomingContactRequest(
            messageId: -Int64(id) - 1,
            fromPubkey: pubkey,
            fromAccountId: accountId,
            fromPseudonym: pseudonym,
            validAfterMs: 0,
            intro: nil,
            createdAt: createdAt
        )
        // The carried chat link is not taken (2026-11-05): another client may
        // have linked a group the person is in. This device links the
        // one-to-one it joins with them by its own roster
        // (`ContactsViewModel.adoptOneToOnePeers`).
        return AcceptedContact(
            request: request,
            groupIdHex: nil,
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

    /// ADR-0048 / ADR-0101 section 8 — every stored profile but this
    /// account's own keys, each as the ENVELOPE IT WAS RECEIVED AS (profile
    /// v6 is signed by its author's account key, and no key this device holds
    /// may sign in the author's place), sorted by key (a stable order), at
    /// most ``maxProfiles``. A row held without its envelope (a name found by
    /// search, a row filed before the envelope was kept) is not carried.
    static func carriedProfiles(
        _ profiles: [String: ProfileClaim], ownKeysB64: Set<String>
    ) -> [CarriedProfile] {
        var out: [CarriedProfile] = []
        for key in profiles.keys.sorted() where out.count < maxProfiles {
            guard !ownKeysB64.contains(key),
                  Data(base64Encoded: key)?.count == signatureKeyLength,
                  let envelope = profiles[key]?.envelope else { continue }
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
        channelSeeds: [CarriedChannelSeeds] = [],
        deviceCards: [CarriedDeviceCard] = [],
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) -> V4 {
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
        let fitted = fittingCap(V4(
            contacts: contacts.map(carried), pins: pins, knownGroupIdsB64: groupIds,
            ownPredecessors: own,
            groupNames: names,
            profiles: carriedProfiles(profiles, ownKeysB64: ownKeysB64),
            pendingContacts: pending))
        var bundle = withChannelSeeds(fitted, channelSeeds)
        let cards = carriedDeviceCards(deviceCards)
        bundle.deviceCards = cards.isEmpty ? nil : cards
        let floors = carriedSealFloors(pinStore)
        bundle.sealFloors = floors.isEmpty ? nil : floors
        let closed = carriedStartedOver(pinStore)
        bundle.startedOver = closed.isEmpty ? nil : closed
        return bundle
    }

    /// Alignment item F — the floors this device keeps, by account id hex.
    static func carriedSealFloors(_ pins: PeerPinStore) -> [CarriedSealFloor] {
        pins.allPins()
            .compactMap { accountIdB64, pin -> CarriedSealFloor? in
                guard let seq = pin.sealSeqFloor, seq > 0,
                      let id = Data(base64Encoded: accountIdB64), id.count == accountIdLength else { return nil }
                return CarriedSealFloor(accountIdHex: hexEncode(id), seq: seq)
            }
            .sorted { $0.accountIdHex < $1.accountIdHex }
            .prefix(maxSealFloors)
            .map { $0 }
    }

    /// Alignment item F — the closed keys this device keeps.
    static func carriedStartedOver(_ pins: PeerPinStore) -> [CarriedStartedOver] {
        pins.startedOverRecords()
            .compactMap { record -> CarriedStartedOver? in
                guard let id = Data(base64Encoded: record.accountIdB64), id.count == accountIdLength,
                      Data(base64Encoded: record.keyB64)?.count == signatureKeyLength else { return nil }
                return CarriedStartedOver(accountIdHex: hexEncode(id), oldKeyB64: record.keyB64, atMs: record.atMs)
            }
            .prefix(maxStartedOver)
            .map { $0 }
    }

    /// Alignment item F — the decoder's rule for `sealFloors` (the whole
    /// bundle is refused on any violation, as for `deviceCards`): at most
    /// ``maxSealFloors`` entries, each a 32-character lower-case hex account
    /// id of 16 bytes, no account twice.
    static func validSealFloors(_ entries: [CarriedSealFloor]) -> Bool {
        guard entries.count <= maxSealFloors else { return false }
        var seen = Set<String>()
        for entry in entries {
            guard validAccountIdHex(entry.accountIdHex), seen.insert(entry.accountIdHex).inserted else {
                return false
            }
        }
        return true
    }

    /// Alignment item F — the decoder's rule for `startedOver`: at most
    /// ``maxStartedOver`` entries, each an account id as above, a 32-byte
    /// key in standard base64 and a time not below 0; no (account, key)
    /// twice.
    static func validStartedOver(_ entries: [CarriedStartedOver]) -> Bool {
        guard entries.count <= maxStartedOver else { return false }
        var seen = Set<String>()
        for entry in entries {
            guard validAccountIdHex(entry.accountIdHex),
                  Data(base64Encoded: entry.oldKeyB64)?.count == signatureKeyLength,
                  entry.atMs >= 0,
                  seen.insert(entry.accountIdHex + "/" + entry.oldKeyB64).inserted else { return false }
        }
        return true
    }

    private static func validAccountIdHex(_ hex: String) -> Bool {
        hex.count == 32 && hex == hex.lowercased() && hexDecode(hex)?.count == accountIdLength
    }

    /// Alignment item F — keep the higher `seq` per contact and add a
    /// started-over record this device does not have. A bundle whose entries
    /// are malformed never gets here (``decode(_:)`` refuses it). Returns
    /// how many changed.
    @discardableResult
    static func applySealFacts(_ bundle: V4, pins: PeerPinStore) -> Int {
        var changed = 0
        for floor in (bundle.sealFloors ?? []).prefix(maxSealFloors) {
            guard floor.accountIdHex.count == 32, let id = hexDecode(floor.accountIdHex),
                  id.count == accountIdLength else { continue }
            if pins.raiseSealSeqFloor(accountIdB64: base64Encode(id), seq: floor.seq) { changed += 1 }
        }
        for closed in (bundle.startedOver ?? []).prefix(maxStartedOver) {
            guard closed.accountIdHex.count == 32, let id = hexDecode(closed.accountIdHex),
                  id.count == accountIdLength,
                  Data(base64Encoded: closed.oldKeyB64)?.count == signatureKeyLength else { continue }
            if pins.addStartedOverKey(accountIdB64: base64Encode(id), keyB64: closed.oldKeyB64, atMs: closed.atMs) {
                changed += 1
            }
        }
        return changed
    }

    /// ADR-0080 amendment — whether one carried device-card entry is
    /// well-formed: a 32-byte key, an envelope of 44 to 270 bytes.
    static func deviceCardEntryValid(_ entry: CarriedDeviceCard) -> Bool {
        guard Data(base64Encoded: entry.leafKeyB64)?.count == signatureKeyLength,
              let envelope = Data(base64Encoded: entry.envelopeB64) else { return false }
        return (deviceCardEnvelopeMinLength...deviceCardEnvelopeMaxLength).contains(envelope.count)
    }

    /// The sender's list: well-formed entries only, the first per leaf key,
    /// ascending by key bytes, at most ``maxDeviceCards``.
    static func carriedDeviceCards(_ entries: [CarriedDeviceCard]) -> [CarriedDeviceCard] {
        var byKey: [Data: CarriedDeviceCard] = [:]
        for entry in entries where deviceCardEntryValid(entry) {
            guard let key = Data(base64Encoded: entry.leafKeyB64), byKey[key] == nil else { continue }
            byKey[key] = entry
        }
        return byKey
            .sorted { $0.key.lexicographicallyPrecedes($1.key) }
            .prefix(maxDeviceCards)
            .map(\.value)
    }

    /// The decoder's rule for `deviceCards` (the whole bundle is refused on
    /// any violation): at most ``maxDeviceCards`` well-formed entries, no
    /// leaf key twice.
    static func validDeviceCards(_ entries: [CarriedDeviceCard]) -> Bool {
        guard entries.count <= maxDeviceCards else { return false }
        var keys = Set<Data>()
        for entry in entries {
            guard deviceCardEntryValid(entry), let key = Data(base64Encoded: entry.leafKeyB64),
                  keys.insert(key).inserted else { return false }
        }
        return true
    }

    /// The carried entries that decode with the core codec, keyed by
    /// lower-case hex.
    static func decodedDeviceCards(_ entries: [CarriedDeviceCard]) -> [(keyHex: String, card: StoredDeviceCard)] {
        entries.compactMap { entry in
            guard let key = Data(base64Encoded: entry.leafKeyB64),
                  let envelope = Data(base64Encoded: entry.envelopeB64),
                  let ffi = try? decodeDeviceCardEnvelope(plaintext: envelope) else { return nil }
            return (hexEncode(key), DeviceCardCenter.stored(ffi))
        }
    }

    /// The roster's leaf keys (lower-case hex) and this device's own, or
    /// `nil` when the handle cannot be read or does not hold this device.
    static func roster(of group: DeviceGroup) -> (keysHex: [String], ownHex: String)? {
        guard let own = try? group.ownLeafIndex(), let members = try? group.devices(),
              let mine = members.first(where: { $0.leafIndex == own }) else { return nil }
        return (members.map { hexEncode($0.signaturePublicKey) }, hexEncode(mine.signaturePublicKey))
    }

    /// ADR-0080 amendment (2026-11-23), new device — store the device cards
    /// the confirming device handed over: decoded here, kept on the main
    /// actor by ``DeviceCardCenter``. Returns how many were kept.
    static func applyDeviceCards(_ bundle: V4, accountIdB64: String, group: DeviceGroup) async -> Int {
        guard let carried = bundle.deviceCards, !carried.isEmpty,
              let roster = roster(of: group) else { return 0 }
        let entries = decodedDeviceCards(carried)
        guard !entries.isEmpty else { return 0 }
        return await MainActor.run {
            DeviceCardCenter.shared.storeCarried(
                accountIdB64: accountIdB64, entries: entries,
                rosterKeysHex: roster.keysHex, ownKeyHex: roster.ownHex)
        }
    }

    /// Bundle v4 — `bundle` (already fitted) with `seeds` added at
    /// ``maxSeedsPerChannel`` per channel, the per-channel count stepping
    /// down for every channel at once (``seedLadder``) while the encoding is
    /// over `cap`. Seeds never push out another array; at 0 none ride.
    static func withChannelSeeds(_ bundle: V4, _ seeds: [CarriedChannelSeeds], cap: Int = maxBundleBytes) -> V4 {
        let entries = Array(seeds.prefix(maxChannelSeedEntries))
        for perChannel in seedLadder {
            let kept: [CarriedChannelSeeds] = perChannel == 0 ? [] : entries.compactMap { entry in
                let newest = Array(entry.seeds.suffix(perChannel))
                return newest.isEmpty ? nil : CarriedChannelSeeds(channelIdB64: entry.channelIdB64, seeds: newest)
            }
            let candidate = V4(
                contacts: bundle.contacts, pins: bundle.pins, knownGroupIdsB64: bundle.knownGroupIdsB64,
                ownPredecessors: bundle.ownPredecessors, groupNames: bundle.groupNames,
                profiles: bundle.profiles, pendingContacts: bundle.pendingContacts, channelSeeds: kept)
            if perChannel == 0 || encode(candidate).count <= cap {
                if perChannel < maxSeedsPerChannel, !entries.isEmpty {
                    NSLog("state bundle: channel seeds trimmed to %d per channel to fit the size cap", perChannel)
                }
                return candidate
            }
        }
        return bundle
    }

    /// The bundle entries for `held` (lower-case hex channel id -> epoch ->
    /// seed): one per channel holding a 16-byte id and at least one 32-byte
    /// seed, ascending by channel id bytes, each its newest `perChannel`
    /// seeds ascending by epoch; at most ``maxChannelSeedEntries``.
    static func carriedChannelSeeds(_ held: [String: [UInt32: Data]], perChannel: Int) -> [CarriedChannelSeeds] {
        var out: [(id: Data, entry: CarriedChannelSeeds)] = []
        for (hex, byEpoch) in held {
            guard let id = hexDecode(hex.lowercased()), id.count == channelIdLength else { continue }
            let seeds = byEpoch.filter { $0.value.count == channelSeedLength }
                .sorted { $0.key < $1.key }
                .suffix(perChannel)
                .map { CarriedSeed(epoch: $0.key, seedB64: $0.value.base64EncodedString()) }
            guard !seeds.isEmpty else { continue }
            out.append((id, CarriedChannelSeeds(channelIdB64: id.base64EncodedString(), seeds: Array(seeds))))
        }
        return out
            .sorted { $0.id.lexicographicallyPrecedes($1.id) }
            .prefix(maxChannelSeedEntries)
            .map(\.entry)
    }

    /// The decoder's rule for the `channelSeeds` array (the whole bundle is
    /// refused on any violation): at most ``maxChannelSeedEntries`` entries,
    /// no channel id twice, each id 16 bytes; 1 to ``maxSeedsPerChannel``
    /// seeds per entry, epochs strictly ascending, each seed 32 bytes.
    static func validChannelSeeds(_ entries: [CarriedChannelSeeds]) -> Bool {
        guard entries.count <= maxChannelSeedEntries else { return false }
        var ids = Set<Data>()
        for entry in entries {
            guard let id = Data(base64Encoded: entry.channelIdB64), id.count == channelIdLength,
                  ids.insert(id).inserted,
                  (1...maxSeedsPerChannel).contains(entry.seeds.count) else { return false }
            var previous: UInt32?
            for seed in entry.seeds {
                if let previous, seed.epoch <= previous { return false }
                previous = seed.epoch
                guard var bytes = Data(base64Encoded: seed.seedB64) else { return false }
                let length = bytes.count
                bytes.resetBytes(in: 0..<bytes.count)
                guard length == channelSeedLength else { return false }
            }
        }
        return true
    }

    /// The carried seeds as lower-case hex channel id -> epoch -> seed (a
    /// decoded bundle has passed ``validChannelSeeds(_:)``).
    static func decodedChannelSeeds(_ entries: [CarriedChannelSeeds]) -> [String: [UInt32: Data]] {
        var out: [String: [UInt32: Data]] = [:]
        for entry in entries {
            guard let id = Data(base64Encoded: entry.channelIdB64), id.count == channelIdLength else { continue }
            for seed in entry.seeds {
                guard let bytes = Data(base64Encoded: seed.seedB64), bytes.count == channelSeedLength else { continue }
                out[hexEncode(id), default: [:]][seed.epoch] = bytes
            }
        }
        return out
    }

    /// `held` with every carried seed added unless one is held for that
    /// channel and epoch (never overwritten).
    static func mergedChannelSeeds(
        held: [String: [UInt32: Data]], carried: [String: [UInt32: Data]]
    ) -> [String: [UInt32: Data]] {
        var out = held
        for (id, seeds) in carried {
            for (epoch, seed) in seeds where out[id]?[epoch] == nil {
                out[id, default: [:]][epoch] = seed
            }
        }
        return out
    }

    /// `bundle` trimmed until its encoding fits `cap` (the receiver refuses
    /// anything larger, whole): profiles from the end first, then group
    /// names from the end, then held messages from the end (the last pending
    /// contact's last message first). Contacts, pins, known group ids, own
    /// predecessors and the pending contacts themselves are never trimmed. A
    /// bundle that still does not fit is returned as it is.
    static func fittingCap(_ bundle: V4, cap: Int = maxBundleBytes) -> V4 {
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
        let trimmed = V4(
            contacts: bundle.contacts, pins: bundle.pins, knownGroupIdsB64: bundle.knownGroupIdsB64,
            ownPredecessors: bundle.ownPredecessors, groupNames: names, profiles: profiles,
            pendingContacts: pending, channelSeeds: bundle.channelSeeds)
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
                avatar: ProfileAvatar(decoded.avatar),
                cardShareable: decoded.cardShareable,
                // ADR-0101 section 8 — kept verbatim, so it can be carried on.
                envelope: envelope)
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
        _ bundle: V4,
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
        _ bundle: V4,
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
        // Alignment item F — the contacts' announcement floors and the keys
        // their unsealed resets left behind, after the pins they sit under.
        if applySealFacts(bundle, pins: pinStore) > 0 {
            NotificationCenter.default.post(name: StartedOverEvents.changed, object: nil)
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
        // Bundle v4 — the channel seeds, kept for the channels whose
        // subscription register arrives (``ChannelSubscriptionSync``).
        if applyNamesOnMain, !bundle.channelSeeds.isEmpty {
            let seeds = bundle.channelSeeds
            Task { @MainActor in
                ChannelSubscriptionSync.shared.storeCarriedSeeds(accountIdB64: accountIdB64, carried: seeds)
            }
        }
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

    /// A group the bundle named ended for this account (ADR-0087 section 9):
    /// this device will not join it, and "Synchronizing..." does not wait for
    /// it.
    static func forgetKnownGroup(
        groupIdHex: String, accountIdB64: String, defaults: UserDefaults = .standard
    ) {
        guard let id = hexDecode(groupIdHex) else { return }
        let known = loadKnownGroupIds(accountIdB64: accountIdB64, defaults: defaults)
        let kept = known.filter { $0 != base64Encode(id) }
        if kept.count != known.count {
            saveKnownGroupIds(kept, accountIdB64: accountIdB64, defaults: defaults)
        }
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
        bundle: V4,
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
    ) async -> V4? {
        var key: Data
        do {
            key = try group.exportSecret(
                identity: identity,
                label: exporterLabel,
                context: rendezvousPseudonym,
                length: UInt32(exporterKeyLength)
            )
        } catch {
            NSLog("state bundle key not derived: %@", error.localizedDescription)
            return nil
        }
        defer { key.resetBytes(in: 0..<key.count) }
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        var cursor = afterCursor
        while true {
            if Task.isCancelled { return nil }
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { return nil }
            let waitSecs = min(perRequestLongPollSeconds, max(1, Int(remaining)))
            let result: FetchResult
            do {
                result = try await ds.fetchRendezvous(
                    id: rendezvousPseudonym, after: cursor, limit: RendezvousRead.pageSize, waitSecs: waitSecs)
            } catch {
                // 404 ("no rows yet") and network blips: keep polling until
                // the deadline, as the history and recovery polls do.
                try? await Task.sleep(nanoseconds: 500_000_000)
                continue
            }
            if let bundle = firstOpening(result.messages, after: cursor, key: key) {
                return bundle
            }
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
    }

    /// The first state row among `rows` past `after` that opens under `key`
    /// and decodes. Anyone who learned the rendezvous can post to it: a
    /// tagged row that does not open or decode is passed over.
    static func firstOpening(_ rows: [MailboxMessage], after: Int64, key: Data) -> V4? {
        for row in rows where row.id > after {
            guard let body = unwrapRow(row.payload) else { continue }
            guard var plain = try? openWithKey(key: key, wire: body) else {
                NSLog("state bundle row did not open; passed over")
                continue
            }
            defer { plain.resetBytes(in: 0..<plain.count) }
            if let bundle = decode(plain) { return bundle }
            NSLog("state bundle row did not decode; passed over")
        }
        return nil
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
