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

    // MARK: - Alone in the call

    /// Chosen: long enough for a peer whose app blinked to come back, short
    /// enough that nobody sits in an empty room — the same five seconds the
    /// SFU gives an empty room before it calls the call over
    /// (`docs/ux/calls.md` §4.3).
    static let aloneEndMs: Int64 = 5_000

    enum AloneAction: Equatable {
        case none
        /// Start the alone-end timer.
        case armEnd
        /// Someone came back: cancel it.
        case disarm
    }

    /// Whether the call should end by itself because everyone else has left.
    ///
    /// Only a call that HAD other people qualifies: the person who started a
    /// call and is still waiting for the first joiner is not in this case, since
    /// nobody left.
    ///
    /// - Parameters:
    ///   - remoteCount: other participants on the roster now.
    ///   - sawRemote: this call has had at least one other participant.
    ///   - armed: the alone-end timer is currently running.
    static func aloneAction(remoteCount: Int, sawRemote: Bool, armed: Bool) -> AloneAction {
        if remoteCount == 0, sawRemote, !armed { return .armEnd }
        if remoteCount > 0, armed { return .disarm }
        return .none
    }

    // MARK: - The conversation row

    /// The row a finished group call leaves in the conversation
    /// (`docs/ux/calls.md` §6), or `nil` when there is nothing to write.
    ///
    /// Direction is not who rang whom — nobody rings in a group call — but whose
    /// call it was from this device's side: one it started is outgoing, one it
    /// joined is incoming. Only a device whose media actually flowed writes a
    /// row: a join that never connected rang nowhere and missed nothing, and the
    /// group-call banner is what announces a live call, so there is no missed
    /// row either.
    static func transcriptRow(
        initiator: Bool, connectedAtMs: Int64?, endedAtMs: Int64, video: Bool = false
    ) -> CallTranscriptRow? {
        guard let connectedAtMs else { return nil }
        return CallTranscriptRow(
            direction: initiator ? .outgoing : .incoming,
            outcome: .answered(durationMs: max(0, endedAtMs &- connectedAtMs)),
            video: video)
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

    /// `docs/ux/group-and-channel-privacy.md` section 10 — a group of one has
    /// no call controls: there is nobody to call. They appear with a second
    /// person. `people` counts persons, not leaves (ADR-0050), so one person
    /// on two phones is still alone; a roster that cannot be read names nobody
    /// to call. A one-to-one always has someone.
    static func hasSomeoneToCall(isGroup: Bool, people: Int?) -> Bool {
        guard isGroup else { return true }
        return (people ?? 0) > 1
    }

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
    /// JOINED, never shadowed by a new one; otherwise a group (ADR-0062 kind,
    /// any size — a two-person group included) starts an SFU call and a
    /// one-to-one a 1x1 P2P call. A group with nobody else in it has nothing
    /// to do here (``hasSomeoneToCall(isGroup:people:)``).
    static func chatCallAction(
        membershipEnded: Bool,
        callInProgress: Bool,
        affordance: GroupCallAffordance,
        isGroup: Bool,
        someoneToCall: Bool = true
    ) -> ChatCallAction {
        if membershipEnded || callInProgress || !someoneToCall { return .unavailable }
        if case .joinable(let callId, _, _) = affordance { return .joinGroup(callId: callId) }
        return isGroup ? .startGroup : .startOneToOne
    }
}
