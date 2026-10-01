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

        public init(accountIdB64: String, pinnedSignaturePubkeyB64: String, rotationMarker: Int64) {
            self.accountIdB64 = accountIdB64
            self.pinnedSignaturePubkeyB64 = pinnedSignaturePubkeyB64
            self.rotationMarker = rotationMarker
        }
    }

    public struct V1: Codable, Equatable {
        public let v: Int
        public let contacts: [CarriedContact]
        public let pins: [CarriedPin]
        /// base64 group ids the old device holds.
        public let knownGroupIdsB64: [String]

        public init(contacts: [CarriedContact], pins: [CarriedPin], knownGroupIdsB64: [String]) {
            self.v = 1
            self.contacts = contacts
            self.pins = pins
            self.knownGroupIdsB64 = knownGroupIdsB64
        }
    }

    // MARK: - Codec

    public static func encode(_ bundle: V1) -> Data {
        (try? JSONEncoder().encode(bundle)) ?? Data()
    }

    /// Decode a bundle, refusing anything that is not the shape above.
    public static func decode(_ bytes: Data) -> V1? {
        guard !bytes.isEmpty, bytes.count <= maxBundleBytes,
              let bundle = try? JSONDecoder().decode(V1.self, from: bytes),
              bundle.v == 1
        else { return nil }
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

    /// Read this device's contacts, pins and known group ids into a bundle.
    public static func build(accountIdB64: String, pinStore: PeerPinStore? = nil) -> V1 {
        let pinStore = pinStore ?? PeerPinStore(ownerAccountIdB64: accountIdB64)
        let contacts = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        var pins: [CarriedPin] = []
        for contact in contacts {
            guard let accountId = contact.request.fromAccountId else { continue }
            let accountIdB64 = base64Encode(accountId)
            guard let pin = pinStore.readPin(accountIdB64: accountIdB64) else { continue }
            pins.append(CarriedPin(
                accountIdB64: accountIdB64,
                pinnedSignaturePubkeyB64: pin.pinnedSignaturePubkeyB64,
                rotationMarker: pin.rotationMarker))
        }
        let groupIds = KnownGroupsStore().load(accountIdB64: accountIdB64)
            .sorted()
            .compactMap { hexDecode($0) }
            .map { base64Encode($0) }
        return V1(contacts: contacts.map(carried), pins: pins, knownGroupIdsB64: groupIds)
    }

    /// Merge a bundle into this device: contacts not already present (by
    /// signature key), pins through the monotone setter, the known group ids
    /// for the chat list's "Synchronizing..." label. `accountIdB64` is the
    /// account the bundle belongs to (the one being paired): its contacts and
    /// pins are filed under it, never under whoever else is signed in.
    @discardableResult
    public static func apply(
        _ bundle: V1,
        accountIdB64: String,
        pinStore: PeerPinStore? = nil
    ) -> (contacts: Int, pins: Int) {
        let pinStore = pinStore ?? PeerPinStore(ownerAccountIdB64: accountIdB64)
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
        bundle: V1,
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
    ) async -> V1? {
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
