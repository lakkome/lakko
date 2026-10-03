// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Ending a group call for everyone (`docs/ux/calls.md` section 4.1.2). iOS
/// mirror of `apps/web/src/calls/end-for-everyone.ts`.
///
/// The person who started a group or link call gets a choice behind `End`:
/// leave, or end the call for every person in it. The end travels as an
/// ordinary `CallEnd` with the reason `endedForAll`, inside the conversation's
/// encryption (the call's own group for a link call), and a device takes itself
/// out of the call only when the MLS-authenticated sender is that call's
/// starter. The same message from anyone else is an ordinary end notice and
/// takes nobody out. The server is never asked.
///
/// Pure apart from the catalog lookup and the core's own signaling parser.
enum EndForEveryone {

    /// How long "{name} ended the call for everyone" stays on screen.
    static let noticeMs: Int64 = 6_000

    /// How long the owner of a link call keeps its session up after sending
    /// the end for everyone. The end travels over the same socket the hang-up
    /// closes, and the send is fire-and-forget: nothing shows that a pending
    /// send is flushed before the close. Chosen, not measured; the same value
    /// as the Android client.
    static let hangUpDelayMs: Int64 = 300

    /// Whether pressing `End` opens the choice rather than ending the call.
    ///
    /// Only for the starter, and only while someone else is in the call: alone,
    /// both rows would do the same thing.
    static func endOpensMenu(isStarter: Bool, othersInCall: Int) -> Bool {
        isStarter && othersInCall > 0
    }

    /// What a received end says about the call this device is in.
    struct ReceivedEnd: Equatable {
        /// The reason the `CallEnd` body carries.
        let reason: FfiCallEndReason
        /// The sender's person key, resolved from the MLS sender at receipt
        /// (C-68); `nil` when it could not be resolved.
        let senderPersonKey: MemberIdentity?
        /// The person who started the call, as this device recorded it; `nil`
        /// when it does not know.
        let starterPersonKey: MemberIdentity?
    }

    /// Whether a received end takes this device out of the call: the reason
    /// says "for everyone" AND it comes from the call's starter. Anything less
    /// is not enough: an unknown starter, an unresolved sender, or anyone else.
    static func endsTheCallForMe(_ end: ReceivedEnd) -> Bool {
        guard end.reason == .endedForAll,
              let sender = end.senderPersonKey, !sender.isEmpty,
              let starter = end.starterPersonKey
        else { return false }
        return sender == starter
    }

    /// The line that tells the others why their call closed.
    static func endedText(starterName: String?) -> UiText {
        let name = starterName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty
            ? .key("call.ended.for_all")
            : .key("call.ended.for_all_by", texts: ["name": name])
    }

    // MARK: - Link calls

    /// Whether a sealed application message of a link call is "this call has
    /// been ended for everyone". States an intent only; the caller checks who
    /// sent it (``sentByOwner(senderLeaf:members:ownerKeyHex:)``).
    static func isEndForEveryone(plaintext: Data, callId: Data) -> Bool {
        guard case .end(let named, let reason)? = try? parseCallSignaling(plaintext: plaintext)
        else { return false }
        return reason == .endedForAll && named == callId
    }

    /// Whether the MLS-authenticated sender of a link call's application
    /// message is the link's owner: the key the SIGNED capability names,
    /// matched against the call group's roster the way the post-join
    /// owner-membership check does. An unresolved sender is nobody.
    static func sentByOwner(
        senderLeaf: UInt32?, members: [ElectionMember], ownerKeyHex: String
    ) -> Bool {
        guard let senderLeaf, !ownerKeyHex.isEmpty else { return false }
        return members.contains { $0.leafIndex == senderLeaf && $0.signatureKeyHex == ownerKeyHex }
    }
}

/// A `CallEnd` this device received on a group's journal, as the repository
/// hands it to ``GroupCallHost``: which call it names, and what
/// ``EndForEveryone/endsTheCallForMe(_:)`` needs to judge it.
struct ReceivedGroupCallEnd: Equatable {
    let groupIdHex: String
    let callId: Data
    let end: EndForEveryone.ReceivedEnd
}
