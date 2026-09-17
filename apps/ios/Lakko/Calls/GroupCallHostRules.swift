// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The decisions the app-level ``GroupCallHost`` takes for a group (SFU) call,
/// kept pure so every branch is testable without WebRTC.
///
/// ## Why the group call moved
///
/// The group call used to be created and owned by `ChatViewModel` and presented
/// by `ChatScreen`. Three defects followed from that:
///
///   * **The call lived only as long as one view model.** `SessionRoot` builds a
///     fresh `ChatViewModel` on every body re-evaluation, and the chat is
///     replaced wholesale by a deep-link landing or a change of destination. A
///     view model that went away took the SFU session with it without hanging
///     it up or publishing the Leave, and the full-screen call was dismissed
///     with it.
///   * **The call key froze outside the chat.** The mid-call SFrame rekey
///     (G-18 / ADR-0029) ran only after the open chat's own long poll or a
///     roster refresh. An epoch advance applied by the foreground receive for a
///     chat that was not open was never noticed.
///   * **The mute button did not mute.** The in-call mic toggle only flipped
///     the screen's state: nothing bridged it onto the SFU session, so the
///     microphone kept sending while the button read muted.
///
/// Same contract as `apps/web/src/calls/group-call-host.ts`.
enum GroupCallHostRules {

    // MARK: - Leaving

    /// What leaving a group call has to tell the group.
    struct LeavePlan: Equatable {
        /// The reason carried by the Leave / End bodies.
        let reason: FfiCallEndReason
        /// Publish a `GroupCallLeave`.
        let publishLeave: Bool
        /// Publish the terminating `CallEnd`.
        let publishEnd: Bool
    }

    /// Decide what leaving announces.
    ///
    /// A Leave only if this device announced a Join. A `CallEnd` only when the
    /// chat's record shows no FRESH device other than this one (a device whose
    /// heartbeats stopped — its app was killed — no longer keeps the call open;
    /// another device of the same person still does), and only from a device that was
    /// actually in the call (joined) or that started it: a joiner whose
    /// connection failed before it ever joined must not end a call other members
    /// may be about to enter.
    ///
    /// A record for a DIFFERENT call means this call lost the start tie-break;
    /// this device then knows nothing about who is in its own room, so it does
    /// not declare that room over. No record at all means another member already
    /// ended it (or nothing was ever observed), so nobody is left.
    ///
    /// - Parameters:
    ///   - hungUp: the user ended it (`false` when the session failed).
    ///   - ownDeviceKey: this device's own leaf signature key (the key its own
    ///     joins are recorded under). `nil` excludes nothing, so a fresh own
    ///     entry then reads as someone else still in — the direction that never
    ///     ends a call wrongly.
    ///   - record: the chat's group-call record at leave time.
    ///   - nowMs: the instant freshness is judged at.
    static func planLeave(
        hungUp: Bool,
        joinPublished: Bool,
        initiator: Bool,
        callId: Data,
        ownDeviceKey: MemberIdentity?,
        record: OngoingGroupCall?,
        nowMs: Int64
    ) -> LeavePlan {
        let reason: FfiCallEndReason = hungUp ? .hungUp : .error
        var publishEnd = false
        if joinPublished || initiator {
            let othersJoined: Bool
            if let record {
                othersJoined = record.callId != callId
                    || !record.freshDevices(nowMs: nowMs, ignoringDevice: ownDeviceKey).isEmpty
            } else {
                othersJoined = false
            }
            publishEnd = !othersJoined
        }
        return LeavePlan(reason: reason, publishLeave: joinPublished, publishEnd: publishEnd)
    }

    // MARK: - Epoch changes

    /// What the host does when a group's MLS chain drains.
    enum EpochAction: Equatable {
        /// Not the call's group, nothing changed, or the epoch cannot be read.
        case none
        /// Re-derive the call seed at the current epoch and rekey the session.
        case rekey
        /// The device is no longer in the group: tear the call down locally.
        case endLocally
    }

    /// Decide whether a drained chain means the live group call must rekey.
    ///
    /// Fail-closed in both directions (invariant #4): an unreadable epoch keeps
    /// the current key rather than guessing, and a group this device has left
    /// ends the call rather than keep media flowing under a key no member will
    /// rotate again. `Group.isActive` going false is permanent (ADR-0045 UX
    /// §6.2), and every exporter call on such a group raises, so there would be
    /// nothing to rekey to.
    ///
    /// - Parameters:
    ///   - callGroupIdHex: the group of the live group call (one with a
    ///     session), or `nil`.
    ///   - settledGroupIdHex: the group whose chain drained.
    ///   - groupJoined: the chat is still in the registry.
    ///   - isActive: `Group.isActive`, `nil` when it cannot be read.
    ///   - keyedEpoch: the epoch the session's current seed was derived at.
    ///   - currentEpoch: the group's epoch now, `nil` when it cannot be read.
    static func decideEpochChange(
        callGroupIdHex: String?,
        settledGroupIdHex: String,
        groupJoined: Bool,
        isActive: Bool?,
        keyedEpoch: UInt64?,
        currentEpoch: UInt64?
    ) -> EpochAction {
        guard let callGroupIdHex, callGroupIdHex == settledGroupIdHex else { return .none }
        if !groupJoined || isActive == false { return .endLocally }
        guard let currentEpoch else { return .none }
        if let keyedEpoch, keyedEpoch == currentEpoch { return .none }
        return .rekey
    }

    // MARK: - The chat's call buttons

    /// What a chat's call buttons do.
    enum ChatCallAction: Equatable {
        /// Withdrawn: membership ended, or a call is already running anywhere.
        case unavailable
        /// Join the group call already in progress in this chat.
        case joinGroup(callId: Data)
        /// Start a new group call over the SFU.
        case startGroup
        /// Place a 1x1 P2P call.
        case startOneToOne
    }

    /// The chat header's call buttons.
    ///
    /// A call already running anywhere withdraws them: returning to the chat of a
    /// live group call shows that call (the root presentation), never offers to
    /// start a second one. A group call already in progress in this chat is
    /// JOINED, never shadowed by a new one; otherwise a group chat (more than two
    /// members) starts an SFU call and a two-member chat a 1x1 P2P call.
    static func chatCallAction(
        membershipEnded: Bool,
        callInProgress: Bool,
        affordance: GroupCallAffordance,
        memberCount: Int?
    ) -> ChatCallAction {
        if membershipEnded || callInProgress { return .unavailable }
        if case .joinable(let callId, _, _) = affordance { return .joinGroup(callId: callId) }
        return (memberCount ?? 0) > 2 ? .startGroup : .startOneToOne
    }
}
