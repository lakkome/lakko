// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 R.7 case 1 - the RE-SEND MARKER.
//
// iOS mirror of `apps/web/src/chat/resend-marker.ts` and
// `apps/web/src/chat/resend-receive.ts`; the tables are the same and so are
// the cases in `ResendMarkerTests`.
//
// The founder of a successor one-to-one sends again, into it, its own
// account's messages that reached only removed devices
// (`ResetRepair.messagesToResend`). Each re-sent message carries, inside the
// sealed envelope, where it was originally sent: the ORIGINAL group id and the
// original DS message id there.
//
// The person who reset holds no history, so for them the re-sent message is
// simply a message. A device that DOES hold the original (another device of
// the founder's own account) would otherwise show it twice: once in the old
// segment, once in the successor. This rule is what makes it show once: the
// successor row is not drawn, not stored, not counted and not notified, and
// the history row stays where it is and is not replaced.
//
// The marker is a claim by the sender, so it is believed only against what
// this device already holds: the named row must exist, in an OLDER segment of
// the SAME conversation, and be by the SAME author account as the sender the
// MLS layer authenticated. A marker that matches nothing is ignored and the
// message renders as any other. Nothing is ever drawn for the marker itself.
public enum ResendMarker {

    /// Where a re-sent message was originally sent.
    public struct Origin: Equatable, Codable {
        /// The original conversation group's id, standard base64.
        public let groupIdB64: String
        /// The original's DS message id in that group.
        public let messageId: Int64

        public init(groupIdB64: String, messageId: Int64) {
            self.groupIdB64 = groupIdB64
            self.messageId = messageId
        }
    }

    public enum Verdict: Equatable {
        /// An ordinary message: stored, shown, counted.
        case draw
        /// This device already holds the original: the successor row is
        /// dropped before storage.
        case skip
    }

    /// | marker | origin group is an older segment of the receiving group's conversation | this device holds the named row | its author is the sender's account | verdict |
    /// |--------|-------------------------------------------------------------------------|---------------------------------|------------------------------------|---------|
    /// | none   | -                                                                       | -                               | -                                  | `draw`  |
    /// | set    | no (another conversation, an unknown group, the same or a later group)  | -                               | -                                  | `draw`  |
    /// | set    | yes                                                                     | no                              | -                                  | `draw`  |
    /// | set    | yes                                                                     | yes                             | no                                 | `draw`  |
    /// | set    | yes                                                                     | yes                             | yes                                | `skip`  |
    ///
    /// `heldAuthor` and `senderAuthor` are the same kind of token for one
    /// account (the caller resolves a key through its aliases first); `nil`
    /// means "not held" / "sender not attributable" and never matches.
    ///
    /// - Parameters:
    ///   - receivedConversation: the conversation the message was RECEIVED in.
    ///   - originConversation: the conversation the marker's group belongs
    ///     to, or `nil` when this device holds no such group.
    ///   - originIsOlderSegment: whether the marker's group comes BEFORE the
    ///     receiving group in that conversation's segments.
    ///   - heldAuthor: the author account of the row the marker names, or
    ///     `nil` when this device does not hold that row.
    ///   - senderAuthor: the account of the MLS-authenticated sender of the
    ///     received message.
    public static func resendVerdict(
        marker: Origin?,
        receivedConversation: String,
        originConversation: String?,
        originIsOlderSegment: Bool,
        heldAuthor: String?,
        senderAuthor: String?
    ) -> Verdict {
        guard marker != nil else { return .draw }
        guard let originConversation else { return .draw }
        if originConversation != receivedConversation { return .draw }
        if !originIsOlderSegment { return .draw }
        guard let heldAuthor, let senderAuthor else { return .draw }
        return heldAuthor == senderAuthor ? .skip : .draw
    }

    /// The direction of a stored row.
    public enum RowDirection: Equatable {
        case outgoing, incoming, system
    }

    /// The slice of a stored row the rule reads.
    public struct HeldRow: Equatable {
        /// The row's DS message id in its group.
        public let messageId: Int64
        public let direction: RowDirection
        /// The stored author's person key (standard base64) of a received
        /// row.
        public let authorKeyB64: String?

        public init(messageId: Int64, direction: RowDirection, authorKeyB64: String? = nil) {
            self.messageId = messageId
            self.direction = direction
            self.authorKeyB64 = authorKeyB64
        }
    }

    /// The author account token of a stored row: this account's own key for a
    /// row this device sent, the stored author key (through `canonical`) for
    /// a received one, `nil` for a system row, a missing row or a row with no
    /// author.
    public static func heldRowAuthor(
        _ row: HeldRow?, ownPersonKeyB64: String?, canonical: (String) -> String
    ) -> String? {
        guard let row, row.direction != .system else { return nil }
        if row.direction == .outgoing {
            return ownPersonKeyB64.map(canonical)
        }
        guard let key = row.authorKeyB64, !key.isEmpty else { return nil }
        return canonical(key)
    }

    // MARK: - the receive side

    /// The slice of a received application message the predicate reads.
    public struct Received: Equatable {
        /// The marker the sealed envelope carried, if any.
        public let resendOrigin: Origin?
        /// The MLS-authenticated sender's person key (standard base64), or
        /// `nil` when the sender is not attributable.
        public let senderKeyB64: String?

        public init(resendOrigin: Origin?, senderKeyB64: String?) {
            self.resendOrigin = resendOrigin
            self.senderKeyB64 = senderKeyB64
        }
    }

    /// The stored message log as the predicate reads it.
    public struct HeldLog {
        /// The stored rows of a group (by lowercase hex id), or `nil` when
        /// this device holds no stored slot for that group.
        public let rows: (_ groupIdHex: String) -> [HeldRow]?

        public init(rows: @escaping (_ groupIdHex: String) -> [HeldRow]?) {
            self.rows = rows
        }

        /// A log held in memory, by lowercase hex group id.
        public init(rowsByGroupHex: [String: [HeldRow]]) {
            self.rows = { rowsByGroupHex[$0] }
        }
    }

    /// The facts the receive side gathers, for one batch of one group. Group
    /// ids are lowercase hex, as the lineage and the log key them.
    public struct ReceivePort {
        /// The group the batch was received in.
        public let receivedGroupIdHex: String
        /// The lineage: a group's conversation.
        public let conversationOf: (_ groupIdHex: String) throws -> String
        /// The lineage: a group's segment index in its conversation.
        public let segmentIndexOf: (_ groupIdHex: String) throws -> Int
        /// This account's person key, standard base64, or `nil`.
        public let ownPersonKeyB64: String?
        /// A key through its aliases (a proven rotation names the same
        /// person).
        public let canonical: (String) -> String
        /// The stored message log, or `nil`.
        public let loadLog: () throws -> HeldLog?

        public init(
            receivedGroupIdHex: String,
            conversationOf: @escaping (_ groupIdHex: String) throws -> String,
            segmentIndexOf: @escaping (_ groupIdHex: String) throws -> Int,
            ownPersonKeyB64: String?,
            canonical: @escaping (String) -> String,
            loadLog: @escaping () throws -> HeldLog?
        ) {
            self.receivedGroupIdHex = receivedGroupIdHex
            self.conversationOf = conversationOf
            self.segmentIndexOf = segmentIndexOf
            self.ownPersonKeyB64 = ownPersonKeyB64
            self.canonical = canonical
            self.loadLog = loadLog
        }
    }

    /// The lowercase hex of a standard-base64 group id, or `nil` for junk.
    public static func groupIdHex(fromB64 groupIdB64: String) -> String? {
        guard let bytes = Data(base64Encoded: groupIdB64), !bytes.isEmpty else { return nil }
        let digits = Array("0123456789abcdef".utf8)
        var out = [UInt8]()
        out.reserveCapacity(bytes.count * 2)
        for byte in bytes {
            out.append(digits[Int(byte >> 4)])
            out.append(digits[Int(byte & 0x0f)])
        }
        return String(decoding: out, as: UTF8.self)
    }

    /// The predicate for one batch: "is this message a re-send whose original
    /// this device already holds".
    ///
    /// One function for every receiver (the background receive, the open
    /// chat's own poll), so they cannot disagree on what a marker matches.
    /// The message log is read at most once per batch, and only for a batch
    /// that carries a marker at all.
    ///
    /// Never throws: a batch whose facts cannot be read gets "nothing is
    /// held", so every message renders (the marker is ignored, which is its
    /// fail-safe direction).
    ///
    /// | the batch carries a marker | the log and the lineage read | the message's own verdict | predicate |
    /// |----------------------------|------------------------------|---------------------------|-----------|
    /// | no                         | not read                     | -                         | false     |
    /// | yes                        | no (threw, or no log)        | -                         | false     |
    /// | yes                        | yes                          | `draw`, or its facts threw | false    |
    /// | yes                        | yes                          | `skip`                    | true      |
    public static func heldResendPredicate(
        batch: [Received], port: ReceivePort
    ) -> (Received) -> Bool {
        let never: (Received) -> Bool = { _ in false }
        guard batch.contains(where: { $0.resendOrigin != nil }) else { return never }
        let held: HeldLog
        let receivedConversation: String
        let receivedIndex: Int
        do {
            guard let log = try port.loadLog() else { return never }
            held = log
            receivedConversation = try port.conversationOf(port.receivedGroupIdHex)
            receivedIndex = try port.segmentIndexOf(port.receivedGroupIdHex)
        } catch {
            return never
        }
        return { message in
            guard let marker = message.resendOrigin,
                  let originHex = groupIdHex(fromB64: marker.groupIdB64) else { return false }
            do {
                let slot = held.rows(originHex)
                let row = slot?.first { $0.messageId == marker.messageId }
                // A group this device holds no stored slot for is not a
                // segment it can hold the original in.
                let originConversation = slot == nil ? nil : try port.conversationOf(originHex)
                let verdict = resendVerdict(
                    marker: marker,
                    receivedConversation: receivedConversation,
                    originConversation: originConversation,
                    originIsOlderSegment: try port.segmentIndexOf(originHex) < receivedIndex,
                    heldAuthor: heldRowAuthor(
                        row, ownPersonKeyB64: port.ownPersonKeyB64, canonical: port.canonical),
                    senderAuthor: message.senderKeyB64.map(port.canonical))
                return verdict == .skip
            } catch {
                return false
            }
        }
    }
}
