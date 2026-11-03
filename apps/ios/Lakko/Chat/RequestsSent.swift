// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `docs/ux/group-and-channel-privacy.md` section 6.2 — the requests to join
/// this device sent and that have not led to a group yet. Per account, per
/// device, in `UserDefaults`: a group id, the name the link showed, when it
/// was sent, and what `Ask again` needs (the link's capability and ledger id;
/// no secret). Kept until the group appears; a request cannot be withdrawn.
public enum RequestsSentStore {

    public struct Entry: Codable, Equatable, Identifiable {
        public let groupIdHex: String
        public let name: String?
        public let handle: String?
        public var sentAtMs: Int64
        /// A link request: the capability and ledger id (empty for search).
        let capabilityB64: String
        let blobIdB64: String
        /// A request from search: the knock key and its version the public
        /// record named.
        var knockKeyB64: String? = nil
        var knockVersion: UInt16? = nil

        public var id: String { groupIdHex }
    }

    /// Section 6.2 — "No answer yet" and `Ask again` after this long.
    public static let askAgainAfterMs: Int64 = 7 * 86_400_000

    static func key(_ accountIdB64: String) -> String { "lakko.requests-sent.v1.\(accountIdB64)" }

    public static func load(_ accountIdB64: String, _ defaults: UserDefaults = .standard) -> [Entry] {
        defaults.data(forKey: key(accountIdB64)).flatMap { try? JSONDecoder().decode([Entry].self, from: $0) } ?? []
    }

    static func save(_ entries: [Entry], _ accountIdB64: String, _ defaults: UserDefaults = .standard) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(entries), forKey: key(accountIdB64))
    }

    public static func record(
        groupId: Data, name: String?, capabilityWire: Data, blobId: Data, accountIdB64: String, nowMs: Int64
    ) {
        let hex = AttachmentBridge.hex(groupId)
        var all = load(accountIdB64).filter { $0.groupIdHex != hex }
        all.append(Entry(
            groupIdHex: hex, name: name, handle: nil, sentAtMs: nowMs,
            capabilityB64: capabilityWire.base64EncodedString(), blobIdB64: blobId.base64EncodedString()))
        save(all, accountIdB64)
    }

    /// Section 6.1 — a request sent from a public group's card.
    public static func recordPublic(
        groupId: Data, name: String, handle: String, knockKey: Data, knockVersion: UInt16,
        accountIdB64: String, nowMs: Int64
    ) {
        let hex = AttachmentBridge.hex(groupId)
        var all = load(accountIdB64).filter { $0.groupIdHex != hex }
        all.append(Entry(
            groupIdHex: hex, name: name, handle: handle, sentAtMs: nowMs, capabilityB64: "", blobIdB64: "",
            knockKeyB64: knockKey.base64EncodedString(), knockVersion: knockVersion))
        save(all, accountIdB64)
    }

    /// Whether a request to this group is open (the card's `Request sent`).
    public static func isOpen(accountIdB64: String, groupIdHex: String) -> Bool {
        load(accountIdB64).contains { $0.groupIdHex == groupIdHex.lowercased() }
    }

    /// The open requests: those whose group this device does not hold. A
    /// joined group's entry is dropped here.
    public static func open(accountIdB64: String, heldGroups: Set<String>) -> [Entry] {
        let all = load(accountIdB64)
        let open = all.filter { !heldGroups.contains($0.groupIdHex) }
        if open.count != all.count { save(open, accountIdB64) }
        return open
    }

    /// Section 6.2 — over 7 days with no answer: `Ask again` is offered.
    public static func mayAskAgain(_ entry: Entry, nowMs: Int64) -> Bool {
        nowMs - entry.sentAtMs >= askAgainAfterMs
    }

    static func capability(_ entry: Entry) -> (wire: Data, blobId: Data)? {
        guard !entry.capabilityB64.isEmpty, let wire = Data(base64Encoded: entry.capabilityB64),
              let blob = Data(base64Encoded: entry.blobIdB64) else { return nil }
        return (wire, blob)
    }
}
