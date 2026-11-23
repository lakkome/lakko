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
            guard let projected = projectGroup(
                hex: hex, rows: bundledMessages(slot.messages), cap: perGroupCap,
                accountIdB64: accountIdB64, cards: cards) else { continue }
            groupInputs.append(projected.group)
            maxHlc = max(maxHlc, projected.maxHlc)
        }
        if groupInputs.isEmpty { return nil }

        return try sealHistoryBundleStructured(
            secret: secret,
            groups: groupInputs,
            maxHlc: maxHlc
        )
    }

    /// ADR-0091 phase 2 section 7 — one conversation's `rows` (already the
    /// gap's, bookkeeping rows left out) sealed to the request's public
    /// `gapKey` and signed by `readmitter` (this device's identity in that
    /// conversation, `sealGapHistoryStructured`), or `nil` when none of them
    /// projects. Only the holder of the gap secret opens it; no member can.
    ///
    /// `heldBeforeGap`: whether this device holds a row older than the gap's
    /// start. Without one it cannot vouch that it saw the start of the gap,
    /// and the group is marked `countCapped` ("not everything"), the web rule;
    /// a receiver whose bundle is not capped drops its gap line.
    static func buildGapForGroup(
        hex: String,
        rows: [PersistedMessage],
        accountIdB64: String,
        readmitter: Identity,
        gapKey: Data,
        groupId: Data,
        heldBeforeGap: Bool = true,
        ownKey: Data? = nil,
        cards: ChannelInviteCardStore = .shared
    ) throws -> Data? {
        guard let projected = projectGroup(
            hex: hex, rows: rows, cap: Self.maxBundleMessages,
            accountIdB64: accountIdB64, cards: cards, notEverything: !heldBeforeGap, ownKey: ownKey),
              !projected.group.messages.isEmpty else { return nil }
        return try sealGapHistoryStructured(
            readmitter: readmitter, gapKey: gapKey, groupId: groupId,
            groups: [projected.group], maxHlc: projected.maxHlc)
    }

    /// One group's rows projected onto the structured history wire: the
    /// newest `cap` of `carried`, each as the canonical placeholder.
    private static func projectGroup(
        hex: String,
        rows carried: [PersistedMessage],
        cap: Int,
        accountIdB64: String,
        cards: ChannelInviteCardStore,
        notEverything: Bool = false,
        ownKey: Data? = nil
    ) -> (group: StructuredHistoryGroup, maxHlc: UInt64)? {
        guard let groupIdBytes = Self.hexToBytes(hex) else { return nil }
        // The MLS bookkeeping rows (a commit, an external-join request)
        // are never carried: this client hides them, and a receiver that
        // draws them as "Members changed" showed one per device commit.
        let trimmed = Array(carried.suffix(cap))
        let capped = trimmed.count < carried.count || notEverything
        var maxHlc: UInt64 = 0
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
                placeholder = try Self.encodePlaceholder(
                    m, channelInvite: card, authorKey: Self.placeholderAuthorKey(m, ownKey: ownKey))
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
        return (StructuredHistoryGroup(groupId: groupIdBytes, messages: messages, countCapped: capped), maxHlc)
    }

    // MARK: - Joining side

    /// Open the sealed bytes + project rows into the joining
    /// device's persistent message log. Returns the number of
    /// rows applied (across all groups). Idempotent on
    /// `messageId` — collisions take the on-disk value.
    /// `true` when `sealedBytes` opens as a history bundle under `secret`.
    /// The joining device's history poll passes over a row that does not.
    static func opens(secret: Data, sealedBytes: Data) -> Bool {
        (try? openHistoryBundleStructured(secret: secret, sealedBytes: sealedBytes)) != nil
    }

    static func applyToMessageLog(
        accountIdB64: String,
        secret: Data,
        sealedBytes: Data,
        store: MessageLogStore = MessageLogStore(),
        cards: ChannelInviteCardStore = .shared,
        callIndex: CallHistoryBacking = SealedCallHistoryBacking()
    ) throws -> Int {
        let opened = try openHistoryBundleStructured(
            secret: secret,
            sealedBytes: sealedBytes
        )
        return applyOpened(
            opened, accountIdB64: accountIdB64, store: store, cards: cards, callIndex: callIndex, gap: nil)
    }

    /// ADR-0091 section 7 — what a filed gap history tells the gap line.
    struct GapApplied: Equatable {
        /// Rows the merge added.
        let applied: Int
        /// No group was marked `countCapped`: the bundle carries everything
        /// from the gap's start.
        let complete: Bool
        /// The earliest row the bundle carried, in milliseconds.
        let earliestMs: Int64?
    }

    /// File an opened gap history (`openGapHistoryStructured`, its signer
    /// already accepted by the caller) like ``applyToMessageLog(accountIdB64:secret:sealedBytes:store:cards:)``
    /// and report what it carried (the web `applyGapHistoryBundle`).
    ///
    /// Walk-3 rule W3-R3: `ownKey` is this account's own key in the
    /// conversation (the person key peers resolve this device's leaf to) and
    /// `route` the route of the request that brought this device back; each
    /// row takes the author and direction ``gapRow(_:author:ownKey:route:)``
    /// gives it.
    static func applyOpenedGapBundle(
        _ opened: StructuredHistoryBundle,
        accountIdB64: String,
        ownKey: Data? = nil,
        route: String? = nil,
        store: MessageLogStore = MessageLogStore(),
        cards: ChannelInviteCardStore = .shared,
        callIndex: CallHistoryBacking = SealedCallHistoryBacking()
    ) -> GapApplied {
        let summary = gapSummary(opened)
        let applied = applyOpened(
            opened, accountIdB64: accountIdB64, store: store, cards: cards, callIndex: callIndex,
            gap: GapContext(ownKey: ownKey, route: route))
        return GapApplied(applied: applied, complete: summary.complete, earliestMs: summary.earliestMs)
    }

    /// Whether `opened` carries everything, and its earliest row.
    static func gapSummary(_ opened: StructuredHistoryBundle) -> (complete: Bool, earliestMs: Int64?) {
        var complete = true
        var earliest: Int64?
        for g in opened.groups {
            if g.countCapped { complete = false }
            for m in g.messages {
                guard let row = Self.decodeOpenedPlaceholder(m.payloadBytes)?.message,
                      let ms = ChatRepository.parseDsTimestampMs(row.createdAt) else { continue }
                if earliest.map({ ms < $0 }) ?? true { earliest = ms }
            }
        }
        return (complete, earliest)
    }

    /// What a gap bundle's receiver knows to place each row (W3-R3).
    struct GapContext {
        let ownKey: Data?
        let route: String?
    }

    private static func applyOpened(
        _ opened: StructuredHistoryBundle,
        accountIdB64: String,
        store: MessageLogStore,
        cards: ChannelInviteCardStore,
        callIndex: CallHistoryBacking,
        gap: GapContext?
    ) -> Int {
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
                var bundled = opened.message
                if let gap, let call = CallTranscriptRow.parse(bundled.text), bundled.direction == "system" {
                    guard let mine = Self.gapCallRow(call, route: gap.route) else { continue }
                    bundled = Self.withText(bundled, mine.marker)
                }
                let row = gap.map {
                    Self.gapRow(bundled, author: opened.authorKey, ownKey: $0.ownKey, route: $0.route)
                } ?? Self.pairingRow(bundled, author: opened.authorKey)
                decoded[hex, default: []].append(row)
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
        // The call rows just written go into the Calls index in the same step,
        // whether or not the chat repository runs (`docs/ux/navigation.md`
        // section 6.2). The kind is not known here: a group until the index or
        // the conversation says otherwise.
        let calls = decoded.flatMap { hex, rows in
            rows.filter { $0.direction == "system" }.compactMap {
                CallHistoryEntry.fromLogRow(
                    conversationId: hex, kind: .group, text: $0.text, createdAt: $0.createdAt)
            }
        }
        let filed = CallHistoryStore.fileFromLog(calls, accountIdB64: accountIdB64, backing: callIndex)
        if !filed.isEmpty {
            Task { @MainActor in CallHistoryStore.shared.absorbFiled(filed, accountIdB64: accountIdB64) }
        }
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
                syncedThroughMessageId: PairingHistory.higherFloor(slot.syncedThroughMessageId, floor),
                cursorEpoch: slot.cursorEpoch
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
    static func encodePlaceholder(
        _ m: PersistedMessage, channelInvite: Data? = nil, authorKey: Data? = nil
    ) throws -> Data {
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
                thumb: a.thumb.flatMap(Self.historyThumb(from:)),
                title: a.title,
                artist: a.artist,
                // ADR-0047 section 6 — the micro thumbnail travels too; the
                // core keeps it only on the first photo or video.
                inlineThumb: a.inlineThumbB64.flatMap { Data(base64Encoded: $0) }
            )
        }
        // ADR-0092 section 3.11 — a content message travels with its content
        // and the state this device folded.
        let content = m.toChatMessage().flatMap { ContentModel.bundleContent($0, nowMs: ChatRepository.nowMs()) }
        let forward = m.forward.map { f in
            let origin = f.originGroupIdHex.flatMap(Self.hexToBytes)
            return HistoryForward(
                author: f.author,
                originGroupId: f.originMessageId == nil ? nil : origin,
                originMessageId: origin == nil ? nil : f.originMessageId
            )
        }
        // Placeholder version 7 — a call row travels as its call record, not
        // as this client's stored marker, which no other client reads. A row
        // that claims to be a call and does not parse is not drawn here and is
        // not carried.
        let call: HistoryCall?
        if m.direction == "system", CallTranscriptRow.isCallRow(m.text) {
            guard let row = CallTranscriptRow.parse(m.text) else {
                throw HistoryBundleCodecError.unreadableCallRow
            }
            call = Self.historyCall(from: row)
        } else {
            call = nil
        }
        let placeholder = HistoryPlaceholder(
            messageId: UInt64(bitPattern: m.messageId),
            direction: Self.directionFromString(m.direction),
            createdAt: m.createdAt,
            text: call == nil ? m.text : "",
            attachments: attachments,
            content: content,
            channelInvite: channelInvite,
            reply: m.reply.map { HistoryReply(target: $0.target, author: $0.author, excerpt: $0.excerpt) },
            forward: forward,
            authorKey: authorKey,
            call: call
        )
        return try encodeHistoryPlaceholder(placeholder: placeholder)
    }

    /// Why a row cannot be carried.
    enum HistoryBundleCodecError: Error {
        /// A call row whose marker this build cannot read.
        case unreadableCallRow
    }

    // MARK: - Call rows (placeholder version 7)

    /// The wire record of a call row. A duration travels in whole seconds and
    /// only when media flowed; an outgoing call nobody answered is an
    /// `outgoing` record with no duration. A declined or failed call carries
    /// no direction.
    static func historyCall(from row: CallTranscriptRow) -> HistoryCall {
        switch (row.direction, row.outcome) {
        case (.outgoing, .answered(let durationMs)):
            return HistoryCall(outcome: .outgoing, video: row.video, durationSecs: Self.wholeSeconds(durationMs))
        case (.incoming, .answered(let durationMs)):
            return HistoryCall(outcome: .incoming, video: row.video, durationSecs: Self.wholeSeconds(durationMs))
        case (.incoming, .missed):
            return HistoryCall(outcome: .missed, video: row.video)
        case (.outgoing, .missed):
            return HistoryCall(outcome: .outgoing, video: row.video)
        case (_, .declined):
            return HistoryCall(outcome: .declined, video: row.video)
        case (_, .failed):
            return HistoryCall(outcome: .failed, video: row.video)
        }
    }

    /// The call row a wire record names, drawn exactly as the live row is.
    ///
    /// A declined or failed call carries no direction; it reads as incoming,
    /// which renders the same row: the conversation's row, its glyph and its
    /// spoken label, and the Calls tab's glyph and label, read the direction
    /// only of an answered call. An incoming record with no duration (a
    /// sender whose clock moved backwards; this client always writes one)
    /// reads as an answered call of no length.
    static func transcriptRow(from call: HistoryCall) -> CallTranscriptRow {
        switch call.outcome {
        case .outgoing:
            if let secs = call.durationSecs {
                return CallTranscriptRow(
                    direction: .outgoing, outcome: .answered(durationMs: Int64(secs) * 1000), video: call.video)
            }
            return CallTranscriptRow(direction: .outgoing, outcome: .missed, video: call.video)
        case .incoming:
            return CallTranscriptRow(
                direction: .incoming, outcome: .answered(durationMs: Int64(call.durationSecs ?? 0) * 1000),
                video: call.video)
        case .missed:
            return CallTranscriptRow(direction: .incoming, outcome: .missed, video: call.video)
        case .declined:
            return CallTranscriptRow(direction: .incoming, outcome: .declined, video: call.video)
        case .failed:
            return CallTranscriptRow(direction: .incoming, outcome: .failed, video: call.video)
        }
    }

    private static func wholeSeconds(_ ms: Int64) -> UInt32 {
        UInt32(clamping: max(0, ms) / 1000)
    }

    /// A call row of a gap bundle (ADR-0091) on the device that came back. A
    /// call row is the call from the seat of the account that recorded it
    /// (`Outgoing call` / `Incoming call`), so it is filed only when the
    /// request was answered by one of this account's own devices
    /// (`own-device`); from anyone else it is that person's call and is
    /// dropped. The web client applies the same rule.
    static func gapCallRow(_ row: CallTranscriptRow, route: String?) -> CallTranscriptRow? {
        route == "own-device" ? row : nil
    }

    /// A decoded placeholder: the log row and the invitation card, which this
    /// client keeps beside the log rather than in the row.
    struct OpenedPlaceholder {
        let message: PersistedMessage
        let channelInvite: Data?
        /// The author's key the sender stored (W3-R3), 32 bytes, or `nil`.
        let authorKey: Data?
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
                },
                title: a.title,
                artist: a.artist,
                inlineThumbB64: a.inlineThumb.flatMap { $0.isEmpty ? nil : $0.base64EncodedString() }
            )
        }
        let restored = placeholder.content.flatMap { content in
            ContentModel.restored(content, attachments: attachments.compactMap { $0.toAttachmentRef() })
        }
        // Placeholder version 7 — a call record becomes this client's call
        // row: a system row whose text is the marker the live row stores.
        let callRow = placeholder.call.map(Self.transcriptRow(from:))
        let message = PersistedMessage(
            messageId: Int64(bitPattern: placeholder.messageId),
            direction: callRow == nil ? Self.directionToString(placeholder.direction) : "system",
            text: callRow?.marker ?? placeholder.text,
            attachments: attachments,
            createdAt: placeholder.createdAt,
            reply: placeholder.reply.map { PersistedReply(target: $0.target, author: $0.author, excerpt: $0.excerpt) },
            forward: placeholder.forward.map {
                PersistedForward(
                    author: $0.author,
                    originGroupIdHex: $0.originGroupId.map(Self.bytesToHex),
                    originMessageId: $0.originMessageId
                )
            },
            content: restored
        )
        return OpenedPlaceholder(
            message: message,
            channelInvite: placeholder.channelInvite.flatMap { $0.isEmpty ? nil : $0 },
            authorKey: placeholder.authorKey.flatMap { $0.count == 32 ? $0 : nil }
        )
    }

    // MARK: - The author of a bundled row (walk-3 rule W3-R3)

    /// The author key a bundled row carries: an incoming row the key its live
    /// row was attributed to; an outgoing row this account's own key in the
    /// conversation (`ownKey`, else the key stamped on the row when it was
    /// sent); a system row none. Only a 32-byte key is carried.
    static func placeholderAuthorKey(_ m: PersistedMessage, ownKey: Data?) -> Data? {
        let key: Data?
        switch m.direction {
        case "incoming": key = m.authorKeyB64.flatMap { Data(base64Encoded: $0) }
        case "outgoing": key = ownKey ?? m.authorKeyB64.flatMap { Data(base64Encoded: $0) }
        default: key = nil
        }
        return key.flatMap { $0.count == 32 ? $0 : nil }
    }

    /// A gap bundle row on the device that came back. With an author: the
    /// row is attributed to it, outgoing when it is this account's own key in
    /// the conversation, incoming otherwise. Without one: the old route rule
    /// (``ConversationReentryRules/bundleDirection(route:senderDirection:)``).
    /// A system row stays one.
    static func gapRow(_ m: PersistedMessage, author: Data?, ownKey: Data?, route: String?) -> PersistedMessage {
        guard m.direction != "system" else { return m }
        guard let author else {
            return reauthored(
                m, authorKeyB64: m.authorKeyB64,
                direction: ConversationReentryRules.bundleDirection(route: route, senderDirection: m.direction))
        }
        let direction = (ownKey.map { $0 == author } ?? false) ? "outgoing" : "incoming"
        return reauthored(m, authorKeyB64: base64Encode(author), direction: direction)
    }

    /// A pairing bundle row (the same account): an incoming row takes the
    /// author it carries; the direction is as sent.
    static func pairingRow(_ m: PersistedMessage, author: Data?) -> PersistedMessage {
        guard m.direction == "incoming", let author else { return m }
        return reauthored(m, authorKeyB64: base64Encode(author), direction: m.direction)
    }

    private static func withText(_ m: PersistedMessage, _ text: String) -> PersistedMessage {
        PersistedMessage(
            messageId: m.messageId, direction: m.direction, text: text, attachments: m.attachments,
            createdAt: m.createdAt, authorKeyB64: m.authorKeyB64, edited: m.edited, deleted: m.deleted,
            reactions: m.reactions, reply: m.reply, forward: m.forward,
            unsupportedVersion: m.unsupportedVersion, lastOpId: m.lastOpId, content: m.content)
    }

    private static func reauthored(_ m: PersistedMessage, authorKeyB64: String?, direction: String) -> PersistedMessage {
        PersistedMessage(
            messageId: m.messageId, direction: direction, text: m.text, attachments: m.attachments,
            createdAt: m.createdAt, authorKeyB64: authorKeyB64, edited: m.edited, deleted: m.deleted,
            reactions: m.reactions, reply: m.reply, forward: m.forward,
            unsupportedVersion: m.unsupportedVersion, lastOpId: m.lastOpId, content: m.content)
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
