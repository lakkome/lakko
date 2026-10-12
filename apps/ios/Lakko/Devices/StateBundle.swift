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
}

/// ADR-0050 section 2.9 — the state bundle a pairing hands to the new device
/// beside the history bundle: accepted contacts (with their local renames),
/// the per-contact identity pins and rotation markers, and the ids of the
/// groups the old device knows.
///
/// ADR-0066 part 3 (format `v` 2, flag day: any other `v` is refused) — the
/// bundle also carries what a device paired AFTER a rotation of the account
/// keys cannot learn from the current key alone: the account's OWN
/// predecessor seeds with the time each was retired (``OwnPredecessor``), and
/// for each pinned contact the keys that contact rotated away from by proof.
/// Without the first the new device would bind its conversation leaves under
/// the new account key and read as another account in every existing chat.
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

    public struct CarriedContact: Codable, Equatable {
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

    public struct CarriedPin: Codable, Equatable {
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

    /// One earlier key of the account being paired (ADR-0066 part 3).
    public struct OwnPredecessor: Codable, Equatable {
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

    /// The bundle's format version. A bundle with any other `v` is refused.
    public static let version = 2
    /// At most this many own predecessors, and this many proven predecessors
    /// per pin; a bundle carrying more is refused whole.
    public static let maxPredecessors = 8
    /// The length of an account phrase seed.
    public static let seedLength = 64
    static let signatureKeyLength = 32

    public struct V2: Codable, Equatable {
        public let v: Int
        public let contacts: [CarriedContact]
        public let pins: [CarriedPin]
        /// base64 group ids the old device holds.
        public let knownGroupIdsB64: [String]
        /// The account's own earlier keys, OLDEST FIRST; empty when the
        /// account never rotated.
        public let ownPredecessors: [OwnPredecessor]

        public init(
            contacts: [CarriedContact], pins: [CarriedPin], knownGroupIdsB64: [String],
            ownPredecessors: [OwnPredecessor] = []
        ) {
            self.v = StateBundle.version
            self.contacts = contacts
            self.pins = pins
            self.knownGroupIdsB64 = knownGroupIdsB64
            self.ownPredecessors = ownPredecessors
        }
    }

    // MARK: - Codec

    public static func encode(_ bundle: V2) -> Data {
        (try? JSONEncoder().encode(bundle)) ?? Data()
    }

    /// Decode a bundle, refusing anything that is not the shape above: another
    /// `v` (version 1 included, no dual accept), a missing field, more than
    /// ``maxPredecessors`` own predecessors or proven predecessors of one
    /// pin, a seed that is not 64 bytes, a proven key that is not 32 bytes,
    /// a time that is not an integer.
    public static func decode(_ bytes: Data) -> V2? {
        guard !bytes.isEmpty, bytes.count <= maxBundleBytes,
              let bundle = try? JSONDecoder().decode(V2.self, from: bytes),
              bundle.v == version,
              bundle.ownPredecessors.count <= maxPredecessors
        else { return nil }
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
        let pseudonym = (try? IdentityPseudonym.fromIdentity(pubkey: pubkey).asBytes()) ?? Data()
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

    /// Read this device's contacts, pins, known group ids, the account's own
    /// predecessor seeds and each pinned contact's proven predecessors into a
    /// bundle.
    static func build(
        accountIdB64: String,
        pinStore: PeerPinStore? = nil,
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        adoptions: PredecessorMailbox.AdoptionStore = PredecessorMailbox.AdoptionStore(),
        provenStore: ProvenPredecessorStore? = nil,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) -> V2 {
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
        let groupIds = KnownGroupsStore().load(accountIdB64: accountIdB64)
            .sorted()
            .compactMap { hexDecode($0) }
            .map { base64Encode($0) }
        return V2(
            contacts: contacts.map(carried), pins: pins, knownGroupIdsB64: groupIds,
            ownPredecessors: own)
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

    /// ADR-0066 part 3 — store the account's own predecessors exactly as a
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
    /// ADR-0066 part 3 — the account's own predecessors are stored FIRST
    /// (the device binding minted at the first chat bootstrap reads them),
    /// and each pinned contact's proven predecessors are recorded in the
    /// proven-predecessor store, on the sending device's word: it is a device
    /// of this account, and the bundle is sealed under the DeviceGroup
    /// exporter. A pin this device refuses records nothing for that contact.
    @discardableResult
    static func apply(
        _ bundle: V2,
        accountIdB64: String,
        pinStore: PeerPinStore? = nil,
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        adoptions: PredecessorMailbox.AdoptionStore = PredecessorMailbox.AdoptionStore(),
        provenStore: ProvenPredecessorStore? = nil
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
        // The contacts ride in the notification too: a live repository merges
        // them into its state (whose next save would otherwise overwrite the
        // store write above).
        NotificationCenter.default.post(
            name: .lakkoStateBundleApplied, object: fresh,
            userInfo: [appliedAccountKey: accountIdB64])
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
        bundle: V2,
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
    ) async -> V2? {
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
