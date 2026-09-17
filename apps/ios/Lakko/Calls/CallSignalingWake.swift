// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which outbound call-signaling frames are worth a push wake.
///
/// ## What this closes
///
/// A 1x1 call offer is published as an ordinary MLS application message
/// through ``ChatRepository/publishApplicationMessage(groupIdHex:plaintextBytes:wake:)``,
/// which fired NO push wake at all — the deliberate ADR-0042 rule that a
/// control envelope (a routing-secret handshake, a liveness beacon) must not
/// wake devices. A call offer was swept up by that rule, so a callee whose app
/// was backgrounded or closed learned about the call only on its next
/// foreground poll, i.e. never: the phone did not ring.
/// `docs/ux/calls.md` §2.1 states the requirement plainly — "a call that only
/// rings inside an open chat is not a feature anyone can rely on" — and §7
/// names the missing piece as a call-kind wake that survives the background
/// filters.
///
/// ## The rule, and why it is this narrow
///
/// Exactly two of the seven signaling bodies wake anyone:
///
///   - **offer** — the ring itself. Nothing else can start one.
///   - **end** — stops a ring the callee may already be showing, on every
///     device of theirs, including the ones whose app is shut. Without it a
///     cancelled call keeps ringing a closed app's notification until the user
///     dismisses it by hand.
///
/// Everything else is deliberately silent:
///
///   - **answer** / **candidate** belong to a call whose peer is, by
///     construction, already awake and in the call — a wake buys nothing and
///     costs a push per ICE candidate, which is a burst of them;
///   - **groupStart** / **groupJoin** / **groupLeave** are group-call
///     announcements, not a 1x1 ring. `docs/ux/calls.md` §9 puts the group
///     call redesign out of scope, and waking for a join / leave would push
///     once per participant churn. `GroupCallHost`'s publish port therefore
///     never asks for a wake at all; this table refusing them is the second
///     layer, not the first.
///
/// ## Why a byte peek rather than a parse
///
/// The publisher sees the frame as already-composed prost bytes, and the
/// leading field tag of `CallSignalingPayload.body` names the variant with no
/// decode at all. The same tag table is already read by
/// ``ChatClient/looksLikeCallSignaling(_:)`` (the foreground receive
/// classifier) and by `nseLooksLikeCallSignaling` (the background mirror); the
/// group-body framing check is reused from `ChatClient` rather than copied, so
/// there is one definition of what makes a group tag a real group body.
///
/// A misread is bounded in both directions: a frame wrongly classified as
/// wakeable costs one extra push, and one wrongly classified as silent costs
/// the wake that the recipient's next poll would have made up anyway. Neither
/// can send a frame, drop a frame, or change what is published.
enum CallSignalingWake {

    /// The `CallSignalingPayload.body` variants, as the wire tags name them.
    enum Body: Equatable {
        case offer
        case answer
        case candidate
        case end
        case groupStart
        case groupJoin
        case groupLeave
    }

    /// Leading wire tag of an embedded `CallSignalingPayload.body` oneof field
    /// (wire type 2, so the tag byte is `(field << 3) | 2`):
    ///
    ///   * field 10 (offer)       -> 0x52
    ///   * field 11 (answer)      -> 0x5A
    ///   * field 12 (candidate)   -> 0x62
    ///   * field 13 (end)         -> 0x6A
    ///   * field 14 (group_start) -> 0x72
    ///   * field 15 (group_join)  -> 0x7A
    ///   * field 16 (group_leave) -> 0x82 0x01 (tag 130, a two-byte varint)
    ///
    /// A chat envelope is JSON and starts with `{` (0x7B); every `lakko/...`
    /// service envelope starts with `l` (0x6C). Neither overlaps this set.
    ///
    /// Returns `nil` when the bytes are not call signaling at all. Reads the
    /// leading tag only — it never decodes the body, so it cannot throw and
    /// costs the same on every frame.
    static func bodyKind(_ bytes: Data) -> Body? {
        guard let first = bytes.first else { return nil }
        switch first {
        case 0x52: return .offer
        case 0x5A: return .answer
        case 0x62: return .candidate
        case 0x6A: return .end
        // The group tags are ASCII `r` / `z`, so a first-byte rule would
        // swallow a legacy plain-text body starting with one. Matched on
        // their FRAMING, exactly as the receive classifier matches them.
        case 0x72:
            return ChatClient.isFramedGroupCallBody(bytes, tagLength: 1) ? .groupStart : nil
        case 0x7A:
            return ChatClient.isFramedGroupCallBody(bytes, tagLength: 1) ? .groupJoin : nil
        case 0x82:
            guard bytes.count >= 2, bytes[bytes.startIndex + 1] == 0x01,
                  ChatClient.isFramedGroupCallBody(bytes, tagLength: 2) else { return nil }
            return .groupLeave
        default: return nil
        }
    }

    /// Should publishing this signaling frame also fan a push wake out to the
    /// group's subscribed devices?
    ///
    /// `true` for an offer and an end, `false` for everything else —
    /// including anything that is not call signaling, so a caller that hands
    /// over the wrong bytes fails toward silence rather than toward a push.
    static func shouldWake(_ bytes: Data) -> Bool {
        switch bodyKind(bytes) {
        case .offer, .end: return true
        default: return false
        }
    }
}
