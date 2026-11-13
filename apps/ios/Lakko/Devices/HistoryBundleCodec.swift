// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

extension Notification.Name {
    /// A pairing history bundle reached the message log. `userInfo` names the
    /// account and carries, per group, the bundle's own rows and floor.
    static let lakkoHistoryBundleFiled = Notification.Name("lakko.historyBundleFiled")
}

/// History catch-up bundle helpers on iOS (Phase 2a3d8-ui).
///
/// Mirror of `apps/web/src/devices/history-bundle.ts`. Two
/// pure functions wrap the structured UniFFI surface:
///
///   * [`buildFromMessageLog`] — scanner side. Reads the local
///     persistent message log, projects rows into
///     `StructuredHistoryGroup` / `StructuredHistoryMessage`
///     Records, seals the bundle. Returns `nil` when there's
///     nothing to send (no log on disk yet) so the caller
///     skips the mailbox PUT.
///
///   * [`applyToMessageLog`] — joining side. Takes the sealed
///     bytes, opens via the post-join exporter secret,
///     projects each row back into the persistent message
///     log. Idempotent on the `messageId` dedup.
///
/// `payload_bytes` are an opaque JSON projection of the
/// runtime `PersistedMessage` shape; the receiver's
/// `applyToMessageLog` decodes them back. A future sprint can
/// replace the placeholder with true MLS application-payload
/// bytes once the message log starts retaining them.
public enum HistoryBundleCodec {

    /// Hard cap on the total `HistoryMessage` count across
    /// every group in a single bundle — must match
    /// `lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES`.
    public static let maxBundleMessages: Int = 1000

    // MARK: - Scanner side

    /// Build the sealed history-bundle wire bytes from the
    /// scanner device's local message log.
    ///
    /// Returns `nil` when the log doesn't exist (fresh install
    /// — nothing to send) or decrypts to an empty groups map.
    /// Caller handles the `nil` case by skipping the mailbox
    /// PUT; the joining side's `pollForHistoryBundle` timeout
    /// fires harmlessly.
    static func buildFromMessageLog(
        accountIdB64: String,
        secret: Data,
        store: MessageLogStore = MessageLogStore(),
        cards: ChannelInviteCardStore = .shared
    ) throws -> Data? {
        guard let log = store.getLog(accountIdB64: accountIdB64) else {
            return nil
        }
        if log.groups.isEmpty { return nil }

        let groupHexes = Array(log.groups.keys)
        let perGroupCap = max(1, Self.maxBundleMessages / groupHexes.count)

        var groupInputs: [StructuredHistoryGroup] = []
        var maxHlc: UInt64 = 0

        for hex in groupHexes {
            guard let slot = log.groups[hex] else { continue }
            guard let groupIdBytes = Self.hexToBytes(hex) else { continue }
            // The MLS bookkeeping rows (a commit, an external-join request)
            // are never carried: this client hides them, and a receiver that
            // draws them as "Members changed" showed one per device commit.
            let carried = bundledMessages(slot.messages)
            let trimmed = Array(carried.suffix(perGroupCap))
            let capped = trimmed.count < carried.count
            var messages: [StructuredHistoryMessage] = []
            for m in trimmed {
                // G-6 step 2: encode the canonical cross-platform
                // placeholder via the shared Rust core. Web /
                // Android call the SAME codec, so the bytes agree
                // on the wire (replacing the iOS-only JSONEncoder
                // shape that did not interoperate with web).
                let placeholder: Data
                do {
                    // ADR-0087 section 14 — the invitation card lives beside
                    // the log, keyed on (conversation, message id); without it
                    // the new device draws the bare `/g/` link.
                    let card = m.deleted ? nil : cards.card(
                        conversationIdHex: hex, messageId: m.messageId, accountIdB64: accountIdB64)
                    placeholder = try Self.encodePlaceholder(m, channelInvite: card)
                } catch {
                    continue
                }
                let createdMs = Self.parseIsoMs(m.createdAt)
                // `messageId` is the HLC-derived Int64 the chat
                // pipeline assigns; serialise it big-endian for
                // the structured-history wire so the joining
                // device sees the same numeric value back.
                var idBE = m.messageId.bigEndian
                let idBytes = withUnsafeBytes(of: &idBE) { Data($0) }
                messages.append(
                    StructuredHistoryMessage(
                        payloadBytes: placeholder,
                        senderLeaf: 0,
                        messageId: idBytes,
                        receivedAtMs: createdMs
                    )
                )
                if m.messageId > 0, UInt64(m.messageId) > maxHlc {
                    maxHlc = UInt64(m.messageId)
                }
            }
            groupInputs.append(
                StructuredHistoryGroup(
                    groupId: groupIdBytes,
                    messages: messages,
                    countCapped: capped
                )
            )
        }
        if groupInputs.isEmpty { return nil }

        return try sealHistoryBundleStructured(
            secret: secret,
            groups: groupInputs,
            maxHlc: maxHlc
        )
    }

    // MARK: - Joining side

    /// Open the sealed bytes + project rows into the joining
    /// device's persistent message log. Returns the number of
    /// rows applied (across all groups). Idempotent on
    /// `messageId` — collisions take the on-disk value.
    static func applyToMessageLog(
        accountIdB64: String,
        secret: Data,
        sealedBytes: Data,
        store: MessageLogStore = MessageLogStore(),
        cards: ChannelInviteCardStore = .shared
    ) throws -> Int {
        let opened = try openHistoryBundleStructured(
            secret: secret,
            sealedBytes: sealedBytes
        )
        let existing = store.getLog(accountIdB64: accountIdB64)
            ?? MessageLogPayload(version: 1, groups: [:])

        var decoded: [String: [PersistedMessage]] = [:]
        for g in opened.groups {
            // G-6 step 2: decode the canonical cross-platform placeholder via
            // the shared Rust core. Bytes from a web or Android scanner decode
            // identically here. A bookkeeping row from a sender that still
            // carries them is dropped: it is nothing the person reads.
            let hex = Self.bytesToHex(g.groupId)
            for opened in g.messages.compactMap({ Self.decodeOpenedPlaceholder($0.payloadBytes) }) {
                if Self.isBookkeepingSystemRow(opened.message) { continue }
                decoded[hex, default: []].append(opened.message)
                // ADR-0087 section 14 — the card goes where this client keeps
                // cards, so the bubble and the Chats row draw it. A card this
                // device already holds for the row stays.
                if let card = opened.channelInvite,
                   cards.card(conversationIdHex: hex, messageId: opened.message.messageId,
                              accountIdB64: accountIdB64) == nil {
                    cards.put(
                        card: card, conversationIdHex: hex, messageId: opened.message.messageId,
                        accountIdB64: accountIdB64)
                }
            }
        }
        let merged = Self.mergeBundle(held: existing.groups, decoded: decoded)

        store.putLog(
            accountIdB64: accountIdB64,
            payload: MessageLogPayload(
                version: existing.version,
                groups: merged.groups
            )
        )
        // A repository already running on this account keeps the whole log in
        // memory and rewrites it from there, so it is told what just landed.
        NotificationCenter.default.post(
            name: .lakkoHistoryBundleFiled, object: nil,
            userInfo: [accountUserInfoKey: accountIdB64, slotsUserInfoKey: merged.opened])
        return merged.applied
    }

    static let accountUserInfoKey = "accountIdB64"
    static let slotsUserInfoKey = "slots"

    /// What ``mergeBundle(held:decoded:)`` made of one bundle.
    struct MergedBundle: Equatable {
        /// The whole log after the merge.
        let groups: [String: PersistedGroup]
        /// How many rows the merge added across all groups.
        let applied: Int
        /// Per group, the bundle's own rows and floor: what a running
        /// repository takes in.
        let opened: [String: PersistedGroup]
    }

    /// Merge a bundle's `decoded` rows, group by group, into the log `held` on
    /// this device. Idempotent on `messageId`: a collision takes the held
    /// row. The cursor and the seen state are this device's, never the
    /// bundle's (ADR-0063), and each group's floor moves up to the highest id
    /// the bundle carried for it, and never down ("History is not news",
    /// `docs/ux/second-device.md` section 2).
    static func mergeBundle(
        held: [String: PersistedGroup],
        decoded: [String: [PersistedMessage]]
    ) -> MergedBundle {
        var next = held
        var opened: [String: PersistedGroup] = [:]
        var applied = 0
        for (hex, rows) in decoded {
            let slot = next[hex] ?? PersistedGroup(cursor: 0, messages: [])
            let merged = PairingHistory.mergeLogRows(slot.messages, rows)
            applied += merged.added
            let floor = PairingHistory.bundleFloor(rows)
            next[hex] = PersistedGroup(
                cursor: slot.cursor,
                messages: merged.rows,
                lastViewedMessageId: slot.lastViewedMessageId,
                feedSeen: slot.feedSeen,
                syncedThroughMessageId: PairingHistory.higherFloor(slot.syncedThroughMessageId, floor)
            )
            opened[hex] = PersistedGroup(
                cursor: 0,
                messages: PairingHistory.mergeLogRows([], rows).rows,
                syncedThroughMessageId: floor
            )
        }
        return MergedBundle(groups: next, applied: applied, opened: opened)
    }

    // MARK: - Bookkeeping rows

    /// A system row the receive loop files for an MLS commit or an
    /// external-join request (`[mls commit]`, `[mls external join]`, the
    /// same `[mls <kind>]` text on every client). A call row is a system
    /// row too and is not one of these.
    static func isBookkeepingSystemRow(_ m: PersistedMessage) -> Bool {
        m.direction == "system" && m.text.hasPrefix("[mls ") && m.text.hasSuffix("]")
    }

    /// The rows of a group a history bundle carries: every row but the
    /// bookkeeping ones.
    static func bundledMessages(_ messages: [PersistedMessage]) -> [PersistedMessage] {
        messages.filter { !isBookkeepingSystemRow($0) }
    }

    // MARK: - Placeholder codec (G-6 step 2)

    /// Encode a local-store message row into the canonical
    /// cross-platform placeholder bytes via the shared Rust core. Version 3
    /// also carries `channelInvite` (the ADR-0087 card this client keeps
    /// beside the log) and the row's ADR-0052 reply and forward.
    static func encodePlaceholder(_ m: PersistedMessage, channelInvite: Data? = nil) throws -> Data {
        // ADR-0047 §5 — placeholder version 2 carries the media metadata, so a
        // voice message, video note or GIF restores as itself on the new
        // device. The core applies the envelope's bounds on encode and decode.
        // The local `played` marker is never transferred.
        let attachments = m.attachments.map { a in
            HistoryAttachment(
                blobId: Data(base64Encoded: a.blobIdB64) ?? Data(),
                key: Data(base64Encoded: a.keyB64) ?? Data(),
                nonce: Data(base64Encoded: a.nonceB64) ?? Data(),
                size: a.size,
                mime: a.mime,
                name: a.name,
                kind: a.kind,
                durationMs: a.durationMs,
                waveform: a.waveformB64.flatMap { Data(base64Encoded: $0) },
                width: a.width,
                height: a.height,
                thumb: a.thumb.flatMap(Self.historyThumb(from:))
            )
        }
        let forward = m.forward.map { f in
            let origin = f.originGroupIdHex.flatMap(Self.hexToBytes)
            return HistoryForward(
                author: f.author,
                originGroupId: f.originMessageId == nil ? nil : origin,
                originMessageId: origin == nil ? nil : f.originMessageId
            )
        }
        let placeholder = HistoryPlaceholder(
            messageId: UInt64(bitPattern: m.messageId),
            direction: Self.directionFromString(m.direction),
            createdAt: m.createdAt,
            text: m.text,
            attachments: attachments,
            channelInvite: channelInvite,
            reply: m.reply.map { HistoryReply(target: $0.target, author: $0.author, excerpt: $0.excerpt) },
            forward: forward
        )
        return try encodeHistoryPlaceholder(placeholder: placeholder)
    }

    /// A decoded placeholder: the log row and the invitation card, which this
    /// client keeps beside the log rather than in the row.
    struct OpenedPlaceholder {
        let message: PersistedMessage
        let channelInvite: Data?
    }

    /// Decode canonical placeholder bytes (from any platform)
    /// back into a local-store message row; `nil` on malformed
    /// input.
    static func decodePlaceholder(_ bytes: Data) -> PersistedMessage? {
        decodeOpenedPlaceholder(bytes)?.message
    }

    /// ``decodePlaceholder(_:)`` with the invitation card.
    static func decodeOpenedPlaceholder(_ bytes: Data) -> OpenedPlaceholder? {
        guard let placeholder = try? decodeHistoryPlaceholder(bytes: bytes) else {
            return nil
        }
        let attachments = placeholder.attachments.map { a in
            PersistedAttachment(
                blobIdB64: a.blobId.base64EncodedString(),
                keyB64: a.key.base64EncodedString(),
                nonceB64: a.nonce.base64EncodedString(),
                mime: a.mime,
                size: a.size,
                name: a.name,
                kind: a.kind,
                durationMs: a.durationMs,
                waveformB64: a.waveform.flatMap { $0.isEmpty ? nil : $0.base64EncodedString() },
                width: a.width,
                height: a.height,
                thumb: a.thumb.map { t in
                    PersistedThumb(
                        blobIdB64: t.blobId.base64EncodedString(),
                        keyB64: t.key.base64EncodedString(),
                        nonceB64: t.nonce.base64EncodedString(),
                        mime: t.mime,
                        size: t.size,
                        width: t.width,
                        height: t.height
                    )
                }
            )
        }
        let message = PersistedMessage(
            messageId: Int64(bitPattern: placeholder.messageId),
            direction: Self.directionToString(placeholder.direction),
            text: placeholder.text,
            attachments: attachments,
            createdAt: placeholder.createdAt,
            reply: placeholder.reply.map { PersistedReply(target: $0.target, author: $0.author, excerpt: $0.excerpt) },
            forward: placeholder.forward.map {
                PersistedForward(
                    author: $0.author,
                    originGroupIdHex: $0.originGroupId.map(Self.bytesToHex),
                    originMessageId: $0.originMessageId
                )
            }
        )
        return OpenedPlaceholder(
            message: message,
            channelInvite: placeholder.channelInvite.flatMap { $0.isEmpty ? nil : $0 }
        )
    }

    /// A stored poster frame as the placeholder's thumb; `nil` when a byte
    /// field no longer decodes (the attachment itself survives).
    private static func historyThumb(from t: PersistedThumb) -> HistoryThumb? {
        guard let blobId = Data(base64Encoded: t.blobIdB64),
              let key = Data(base64Encoded: t.keyB64),
              let nonce = Data(base64Encoded: t.nonceB64),
              blobId.count == 32, key.count == 32, nonce.count == 12 else {
            return nil
        }
        return HistoryThumb(
            blobId: blobId,
            key: key,
            nonce: nonce,
            size: t.size,
            mime: t.mime,
            width: t.width,
            height: t.height
        )
    }

    private static func directionFromString(_ s: String) -> HistoryMessageDirection {
        switch s {
        case "outgoing": return .outgoing
        case "incoming": return .incoming
        default: return .system
        }
    }

    private static func directionToString(_ d: HistoryMessageDirection) -> String {
        switch d {
        case .outgoing: return "outgoing"
        case .incoming: return "incoming"
        case .system: return "system"
        }
    }

    // MARK: - Helpers

    private static func parseIsoMs(_ iso: String) -> UInt64 {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: iso) {
            return UInt64(d.timeIntervalSince1970 * 1000)
        }
        let f2 = ISO8601DateFormatter()
        f2.formatOptions = [.withInternetDateTime]
        if let d = f2.date(from: iso) {
            return UInt64(d.timeIntervalSince1970 * 1000)
        }
        return 0
    }

    private static func hexToBytes(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data()
        out.reserveCapacity(hex.count / 2)
        var i = hex.startIndex
        while i < hex.endIndex {
            let next = hex.index(i, offsetBy: 2)
            guard let b = UInt8(hex[i..<next], radix: 16) else { return nil }
            out.append(b)
            i = next
        }
        return out
    }

    private static func bytesToHex(_ data: Data) -> String {
        return data.map { String(format: "%02x", $0) }.joined()
    }
}
