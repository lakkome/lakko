// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

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
/// At most two of the signaling bodies wake anyone, and each only once in a
/// call's life:
///
///   - **the dial** — the FIRST offer of a call, the ring itself. A later
///     offer for the same call id is a renegotiation (a camera turned on or
///     off, the peer's screen share) between two people already in the call.
///   - **an end of a call that never connected here** — the caller
///     cancelling, the callee declining or missing it. It stops a ring the
///     other side may still be showing, on every device of theirs, including
///     the ones whose app is shut. Once the call has connected nobody is
///     ringing.
///
/// A wake for anything else is not merely wasted: on iOS a call wake is a
/// VoIP push, which must be reported to CallKit as an incoming call, so a
/// renegotiation offer or the end of a connected call rang the other phone as
/// a new audio call. Which offer is the dial and whether the call connected
/// are not in the bytes, so the session decides
/// (``sessionAsks(_:everConnected:)``) and the publisher combines that with
/// the byte peek (``wakeKind(for:sessionAsks:)``).
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
/// ## Why a byte peek as well
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
        case screenShare
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
    ///   * field 17 (screen_share) -> 0x8A 0x01 (tag 138, ADR-0083)
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
        case 0x8A:
            guard bytes.count >= 2, bytes[bytes.startIndex + 1] == 0x01,
                  ChatClient.isFramedGroupCallBody(bytes, tagLength: 2) else { return nil }
            return .screenShare
        default: return nil
        }
    }

    /// Should publishing this signaling frame also fan a push wake out to the
    /// group's subscribed devices? A `true` becomes ``DsWake/Kind/call`` on the
    /// publish's `x-lakko-wake` header (ADR-0054 decision 1).
    ///
    /// `true` for an offer and an end, `false` for everything else —
    /// including anything that is not call signaling, so a caller that hands
    /// over the wrong bytes fails toward silence rather than toward a push.
    ///
    /// An upper bound only: the session narrows it to the dial and to the end
    /// of a call that never connected (``wakeKind(for:sessionAsks:)``).
    static func shouldWake(_ bytes: Data) -> Bool {
        switch bodyKind(bytes) {
        case .offer, .end: return true
        default: return false
        }
    }

    /// The frames a 1x1 ``CallSession`` puts on the wire, as the session
    /// knows them. "Dial" and "renegotiation" are the same body on the wire.
    enum OutboundFrame: Equatable {
        /// The first offer of a call this device places.
        case dialOffer
        /// Any later offer for the same call id.
        case renegotiationOffer
        case answer
        case candidate
        case end
    }

    /// Does the session ask for a call wake for this frame? The dial does,
    /// and an end does while the call has never connected on this device.
    static func sessionAsks(_ frame: OutboundFrame, everConnected: Bool) -> Bool {
        switch frame {
        case .dialOffer: return true
        case .end: return !everConnected
        case .renegotiationOffer, .answer, .candidate: return false
        }
    }

    /// The wake a publish carries: ``DsWake/Kind/call`` when the session asks
    /// for one AND the bytes are an offer or an end, else none. Either side
    /// alone can only remove the wake, so wrong bytes or a wrong flag fail
    /// toward silence.
    static func wakeKind(for bytes: Data, sessionAsks: Bool) -> DsWake.Kind? {
        sessionAsks && shouldWake(bytes) ? .call : nil
    }
}

/// The receive half: a call wake for a conversation in which this device is
/// already in a 1x1 call, or has just left one, rings nothing.
///
/// A VoIP push must be reported to CallKit as an incoming call
/// (``VoipPushRegistrar``), so every call wake that reaches this device rings
/// it unless the decision says otherwise. Three kinds of wake reach a device
/// that is in the call the wake is about:
///
///   - this device's own publish, echoed back (the DS wakes every subscribed
///     device of the group, the sender's included): the dial of a call it
///     placed, the end of a call it cancelled;
///   - a sender that still wakes for every offer and end (a renegotiation
///     offer, the end of a connected call);
///   - the other person's end for a ring this device is still showing.
///
/// None of them is a new call. ``VoipPushRegistrar/decide(_:binding:routes:defaults:callWakeSuppressed:)``
/// treats them as it treats a muted conversation: reported, ended at once,
/// and still fetched, so the row behind the wake is applied to the call it
/// belongs to.
///
/// Main-actor state of ``CallHost``; read synchronously at report time.
struct CallWakeSuppression {

    /// How long after a call ended here a wake for its conversation still
    /// rings nothing. Chosen, not measured: long enough for a push sent at the
    /// hang-up to arrive after this device already applied the end (the push
    /// and the journal poll race), short enough that a call placed again a
    /// moment later rings normally. A call placed again inside the window
    /// still arrives through the fetched row, without the VoIP placeholder.
    static let recentEndWindowMs: Int64 = 10_000

    /// Lowercase group hex -> when the last 1x1 call in it ended here.
    private var endedAtMs: [String: Int64] = [:]

    /// Record that the 1x1 call in `groupIdHex` reached `.done`.
    mutating func noteEnded(groupIdHex: String, atMs: Int64) {
        endedAtMs = endedAtMs.filter { atMs - $0.value < Self.recentEndWindowMs }
        endedAtMs[groupIdHex.lowercased()] = atMs
    }

    mutating func reset() {
        endedAtMs.removeAll()
    }

    /// The VoIP report's whole answer for one conversation: the one-to-one
    /// host's, the group-call host's, and the conversation's kind. A call
    /// wake for a conversation known to be a GROUP is never a one-to-one ring
    /// (ADR-0062: a group of any size calls over the SFU, and a group call
    /// rings no closed device). An unknown kind (the group not loaded yet, a
    /// cold launch) decides nothing.
    static func anySource(oneToOne: Bool, groupCall: Bool, kind: GroupKind?) -> Bool {
        oneToOne || groupCall || kind == .group
    }

    /// Does a call wake for `groupIdHex` ring nothing?
    ///
    /// `true` when the call in the slot is in that conversation and is
    /// ringing, dialling, connecting, connected or ending — a wake about it
    /// is about a call already on this device — or when the conversation's
    /// last call ended less than ``recentEndWindowMs`` ago. An `.idle`
    /// session (the pre-join check, nothing sent yet) suppresses nothing.
    func suppresses(
        groupIdHex: String,
        activeGroupIdHex: String?,
        activeState: CallSession.WebCallState?,
        nowMs: Int64
    ) -> Bool {
        let group = groupIdHex.lowercased()
        if let activeState, let activeGroupIdHex, activeGroupIdHex.lowercased() == group {
            switch activeState {
            case .ringing, .outgoing, .connecting, .connected, .ending:
                return true
            case .idle, .done:
                break
            }
        }
        guard let ended = endedAtMs[group] else { return false }
        // A clock that stepped back reads as "just ended": silence is the
        // safe side for a call this device already took part in.
        return nowMs - ended < Self.recentEndWindowMs
    }
}
