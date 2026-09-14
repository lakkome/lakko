// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The decisions the app-level [GroupCallHost] takes for a group (SFU) call,
 * kept pure so they can be unit-tested. The Kotlin twin of
 * `apps/web/src/calls/group-call-host.ts`.
 *
 * ## Why the group call moved
 *
 * It used to be created and owned by `ChatViewModel` and presented by
 * `ChatScreen` in a dialog. Three defects followed:
 *
 *  - **Leaving the chat hid the call but kept it running.** The view model is
 *    activity-scoped, so a push tap, an invite or a `/g/` link that changed the
 *    destination took the in-call dialog away while the SFU session kept
 *    sending audio, with no screen left to hang it up; a `/c/` link replaced the
 *    whole session UI and could start a second SFU session on top.
 *  - **The call key followed a view model collector.** The mid-call SFrame rekey
 *    (G-18 / ADR-0029) ran on every `ChatRepository.state` emission, reading the
 *    epoch while another operation on the same group could be half-way through
 *    (an eager-merged commit not yet published, ADR-0040).
 *  - **Nothing made the calls exclusive.** Only a 1x1 offer was answered busy
 *    during a group call; a link call made nobody busy.
 */

/** Why this device is leaving a group call. */
public enum class GroupLeaveReason { HUNG_UP, ERROR }

/** What leaving a group call has to tell the group. */
public data class GroupCallLeavePlan(
    /** The reason carried by the Leave / End bodies. */
    val leaveReason: GroupLeaveReason,
    /** Publish a `GroupCallLeave`. */
    val publishLeave: Boolean,
    /** Publish the terminating `CallEnd`. */
    val publishEnd: Boolean,
)

/**
 * What leaving a group call has to tell the group.
 *
 * A Leave only if this device announced a Join. A `CallEnd` only when the local
 * record shows nobody else joined, and only from a device that was actually in
 * the call (joined) or that started it: a joiner whose connection failed before
 * it ever joined must not end a call other members may be about to enter.
 *
 * @param sessionFailed the SFU session itself failed — overrides [reason].
 * @param recordJoinedLeaves joined leaves of the call's record at leave time,
 *   `null` when the record is gone or names another call.
 */
public fun planGroupCallLeave(
    reason: GroupLeaveReason,
    sessionFailed: Boolean,
    joinPublished: Boolean,
    initiator: Boolean,
    ownLeaf: UInt,
    recordJoinedLeaves: Set<UInt>?,
): GroupCallLeavePlan {
    val leaveReason = if (sessionFailed) GroupLeaveReason.ERROR else reason
    var publishEnd = false
    if (joinPublished || initiator) {
        val othersJoined = recordJoinedLeaves != null && recordJoinedLeaves.any { it != ownLeaf }
        publishEnd = !othersJoined
    }
    return GroupCallLeavePlan(leaveReason = leaveReason, publishLeave = joinPublished, publishEnd = publishEnd)
}

/** What the host does when a group's MLS work settles. */
public enum class GroupCallEpochAction {
    /** Not the call's group, nothing changed, or the state cannot be read now. */
    NONE,

    /** Re-derive the call seed at the current epoch and rekey the session. */
    REKEY,

    /** The device is no longer in the group: tear the call down locally. */
    END_LOCALLY,
}

/**
 * Decide whether a settled group means the live group call must rekey.
 *
 * Fail-closed in both directions (invariant #4): an unreadable or unsettled
 * epoch keeps the current key rather than guessing, and a group this device has
 * left ends the call rather than keeping media flowing under a key no member
 * will rotate again. `Group.isActive` going false is permanent (ADR-0045 UX
 * §6.2), and every exporter call on such a group raises.
 *
 * @param callGroupIdHex the group of the connected group call, or `null`.
 * @param settledGroupIdHex the group whose per-group lock queue drained.
 * @param groupJoined the group is still in the repository.
 * @param isActive `Group.isActive`, `null` when it cannot be read.
 * @param settled no operation on the group was queued or running when the state
 *   was read; `false` means a later drain will report again.
 * @param keyedEpoch the epoch the session's current seed was derived at.
 * @param currentEpoch the group's epoch now, `null` when it cannot be read.
 */
public fun decideGroupCallEpochChange(
    callGroupIdHex: String?,
    settledGroupIdHex: String,
    groupJoined: Boolean,
    isActive: Boolean?,
    settled: Boolean,
    keyedEpoch: ULong?,
    currentEpoch: ULong?,
): GroupCallEpochAction {
    if (callGroupIdHex == null || callGroupIdHex != settledGroupIdHex) return GroupCallEpochAction.NONE
    if (!groupJoined || isActive == false) return GroupCallEpochAction.END_LOCALLY
    if (!settled || currentEpoch == null) return GroupCallEpochAction.NONE
    if (keyedEpoch != null && keyedEpoch == currentEpoch) return GroupCallEpochAction.NONE
    return GroupCallEpochAction.REKEY
}

/** Whether a group call may start (or be joined) now. */
public sealed interface GroupCallStartDecision {
    public data object Start : GroupCallStartDecision

    /** This very call is already starting or live here: show it, never a
     *  second session for it. */
    public data object AlreadyInThisCall : GroupCallStartDecision

    public data class Busy(val message: String) : GroupCallStartDecision
}

/**
 * One call at a time, across all kinds.
 *
 * @param occupant the call occupying the slot, or `null`.
 * @param activeGroupCall the group call in the host's slot as `(groupIdHex,
 *   callIdHex)`, or `null`.
 * @param requestedGroupIdHex the chat the request came from.
 * @param requestedCallIdHex the call to join, or `null` to start a new one.
 */
public fun decideGroupCallStart(
    occupant: CallKind?,
    activeGroupCall: Pair<String, String>?,
    requestedGroupIdHex: String,
    requestedCallIdHex: String?,
): GroupCallStartDecision {
    if (occupant == null) return GroupCallStartDecision.Start
    val verb = if (requestedCallIdHex == null) CallVerb.START else CallVerb.JOIN
    if (occupant == CallKind.GROUP && activeGroupCall != null &&
        activeGroupCall.first == requestedGroupIdHex &&
        (requestedCallIdHex == null || requestedCallIdHex == activeGroupCall.second)
    ) {
        return GroupCallStartDecision.AlreadyInThisCall
    }
    return GroupCallStartDecision.Busy(callBusyMessage(occupant, verb))
}
