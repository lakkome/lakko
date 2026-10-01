// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0059 section 6 — the channels group members share inside a group, and
/// the switch that shares this person's own. Mirror of web
/// `discover/shared-channels.ts`.
///
/// Pure: the row this device keeps (the newest list per member per group,
/// keyed on the member's ACCOUNT key resolved at receipt, C-68, replaced only
/// by a strictly newer stamp; the groups the switch is on in; and what this
/// device last sent to each, so a growth re-send carries the original stamp),
/// and the rule of what is shared (only subscribed channels whose own
/// metadata says `listed`, with a handle, at most ``SharedChannels/maxChannels``).
public enum SharedChannels {
    /// Most channels one list carries (`lakko-contacts::shared_channels`).
    public static let maxChannels = 32

    /// One shared channel.
    public struct Channel: Equatable, Hashable, Codable, Sendable {
        /// 32-char lowercase hex.
        public let channelIdHex: String
        public let handle: String
        public let name: String

        public init(channelIdHex: String, handle: String, name: String) {
            self.channelIdHex = channelIdHex
            self.handle = handle
            self.name = name
        }
    }

    /// One member's list.
    public struct List: Equatable, Sendable {
        public let updatedAtMs: UInt64
        public let channels: [Channel]

        public init(updatedAtMs: UInt64, channels: [Channel]) {
            self.updatedAtMs = updatedAtMs
            self.channels = channels
        }
    }

    /// The row: received lists, what was sent, and the switches.
    public struct Row: Equatable, Sendable {
        /// groupIdHex -> member account key (base64) -> newest list.
        public var received: [String: [String: List]]
        /// groupIdHex -> the last list this device sent there.
        public var sent: [String: List]
        /// Groups the "Share my channels" switch is on in.
        public var shareOn: Set<String>

        public init(
            received: [String: [String: List]] = [:],
            sent: [String: List] = [:],
            shareOn: Set<String> = []
        ) {
            self.received = received
            self.sent = sent
            self.shareOn = shareOn
        }
    }

    /// The verdict on an incoming list.
    public enum Verdict: Equatable, Sendable {
        case accept
        case rejectStale
        case rejectUnattributed
    }

    /// File `incoming` from `memberKeyB64` in `groupIdHex` when it is strictly
    /// newer than what is held for that member (the core's
    /// `sharedChannelsIsNewer`, injected). An unattributed sender (no account
    /// key resolved at receipt) is refused: a list speaks only for its sender.
    public static func applyIncoming(
        row: Row,
        groupIdHex: String,
        memberKeyB64: String?,
        incoming: List,
        isNewer: (_ incoming: UInt64, _ stored: UInt64) -> Bool
    ) -> (row: Row, verdict: Verdict) {
        guard let memberKeyB64, !memberKeyB64.isEmpty else { return (row, .rejectUnattributed) }
        if let held = row.received[groupIdHex]?[memberKeyB64],
           !isNewer(incoming.updatedAtMs, held.updatedAtMs) {
            return (row, .rejectStale)
        }
        var next = row
        next.received[groupIdHex, default: [:]][memberKeyB64] = incoming
        return (next, .accept)
    }

    /// The group was deleted from this device: forget everything about it.
    public static func forgetGroup(row: Row, groupIdHex: String) -> Row {
        var next = row
        next.received.removeValue(forKey: groupIdHex)
        next.sent.removeValue(forKey: groupIdHex)
        next.shareOn.remove(groupIdHex)
        return next
    }

    /// A stamp strictly after `previous` (a clock that went backwards still
    /// produces a list every member takes as newer).
    public static func freshStamp(nowMs: Int64, previous: List?) -> UInt64 {
        let now = UInt64(max(0, nowMs))
        guard let previous, now <= previous.updatedAtMs else { return now }
        return previous.updatedAtMs &+ 1
    }

    /// What the share decision reads about one channel on this device.
    public struct Shareable: Equatable, Sendable {
        public let channelIdHex: String
        /// This device follows it as a subscriber.
        public let subscribed: Bool
        /// The channel's own metadata v2 says `listed`.
        public let listed: Bool
        /// The handle it was found under; empty when none.
        public let handle: String
        /// The name the channel's metadata carries (or the local name).
        public let name: String

        public init(channelIdHex: String, subscribed: Bool, listed: Bool, handle: String, name: String) {
            self.channelIdHex = channelIdHex
            self.subscribed = subscribed
            self.listed = listed
            self.handle = handle
            self.name = name
        }
    }

    /// `[a-z0-9_]{3,32}` — the canonical handle rule.
    static func isCanonicalHandle(_ handle: String) -> Bool {
        let bytes = Array(handle.utf8)
        guard (3...32).contains(bytes.count) else { return false }
        return bytes.allSatisfy { (0x61...0x7a).contains($0) || (0x30...0x39).contains($0) || $0 == 0x5f }
    }

    /// The list to share: subscribed, listed, with a canonical handle and a
    /// name, in channel-id order (so an unchanged set is an unchanged list),
    /// at most ``maxChannels``. A channel reached through a private handle is
    /// never listed, so never shared.
    public static func toShare(_ channels: [Shareable]) -> [Channel] {
        let picked = channels
            .filter { $0.subscribed && $0.listed && isCanonicalHandle($0.handle) }
            .compactMap { c -> Channel? in
                let name = c.name.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty else { return nil }
                return Channel(channelIdHex: c.channelIdHex, handle: c.handle, name: name)
            }
            .sorted { $0.channelIdHex < $1.channelIdHex }
        return Array(picked.prefix(maxChannels))
    }

    /// One channel as the members of the person's groups share it.
    public struct GroupShared: Equatable, Sendable {
        public let channelIdHex: String
        public let handle: String
        public let name: String
        /// groupIdHex -> the member keys sharing it there.
        public let byGroup: [String: [String]]
    }

    /// Every channel shared with this person, across groups. The person's own
    /// key is skipped (their own list is not a suggestion to themselves), and
    /// so is a group this device no longer holds (`liveGroups`). When members
    /// disagree on a channel's handle or name, the most common claim wins,
    /// then the smallest.
    public static func aggregate(row: Row, ownKeyB64: String?, liveGroups: Set<String>) -> [GroupShared] {
        var claims: [String: [String: Int]] = [:]
        var byGroup: [String: [String: [String]]] = [:]
        for (groupIdHex, members) in row.received where liveGroups.contains(groupIdHex) {
            for (memberKey, list) in members where memberKey != ownKeyB64 {
                for c in list.channels {
                    let claim = "\(c.handle)\n\(c.name)"
                    claims[c.channelIdHex, default: [:]][claim, default: 0] += 1
                    var keys = byGroup[c.channelIdHex, default: [:]][groupIdHex, default: []]
                    if !keys.contains(memberKey) { keys.append(memberKey) }
                    byGroup[c.channelIdHex, default: [:]][groupIdHex] = keys
                }
            }
        }
        return claims.keys.sorted().map { id in
            let best = (claims[id] ?? [:]).sorted { a, b in
                a.value != b.value ? a.value > b.value : a.key < b.key
            }.first?.key ?? "\n"
            let parts = best.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            let handle = parts.first.map(String.init) ?? ""
            let name = parts.count > 1 ? String(parts[1]) : ""
            return GroupShared(channelIdHex: id, handle: handle, name: name, byGroup: byGroup[id] ?? [:])
        }
    }

    // MARK: - Row codec (JSON, sealed by the store)

    private struct WireList: Codable {
        let u: String
        let c: [[String]]
    }

    private struct WireRow: Codable {
        let version: Int?
        let received: [String: [String: WireList]]?
        let sent: [String: WireList]?
        let shareOn: [String]?
    }

    private static func encodeList(_ l: List) -> WireList {
        WireList(u: String(l.updatedAtMs), c: l.channels.map { [$0.channelIdHex, $0.handle, $0.name] })
    }

    private static func decodeList(_ w: WireList) -> List? {
        guard w.u.count <= 20, w.u.allSatisfy(\.isASCII), let stamp = UInt64(w.u) else { return nil }
        var channels: [Channel] = []
        for c in w.c {
            guard c.count == 3, isHexId(c[0]) else { continue }
            channels.append(Channel(channelIdHex: c[0], handle: c[1], name: c[2]))
            if channels.count >= maxChannels { break }
        }
        return List(updatedAtMs: stamp, channels: channels)
    }

    static func isHexId(_ s: String) -> Bool {
        s.utf8.count == 32 && s.utf8.allSatisfy { (0x30...0x39).contains($0) || (0x61...0x66).contains($0) }
    }

    public static func encode(_ row: Row) -> Data {
        let wire = WireRow(
            version: 1,
            received: row.received.mapValues { $0.mapValues(encodeList) },
            sent: row.sent.mapValues(encodeList),
            shareOn: row.shareOn.sorted())
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return (try? encoder.encode(wire)) ?? Data()
    }

    /// Lenient per entry; anything unreadable decodes to an empty row.
    public static func decode(_ data: Data) -> Row {
        guard let wire = try? JSONDecoder().decode(WireRow.self, from: data), wire.version == 1 else {
            return Row()
        }
        var row = Row()
        for (g, members) in wire.received ?? [:] {
            var m: [String: List] = [:]
            for (k, l) in members { if let list = decodeList(l) { m[k] = list } }
            row.received[g] = m
        }
        for (g, l) in wire.sent ?? [:] { if let list = decodeList(l) { row.sent[g] = list } }
        row.shareOn = Set(wire.shareOn ?? [])
        return row
    }
}
