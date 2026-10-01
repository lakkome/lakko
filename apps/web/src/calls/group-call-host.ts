// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The decisions the app-level call host takes for a GROUP (SFU) call, kept pure
 * so they can be tested — vitest runs in plain node, without jsdom and without
 * the Solid transform, so nothing reachable only from `CallHostContext.tsx` can
 * be asserted at all.
 *
 * ## Why the group call moved
 *
 * The 1x1 call had already moved to the call host; the group call was still
 * created and owned by `routes/Chat.tsx`. Three defects followed from that:
 *
 *   - **Leaving the chat left the call.** The view's cleanup ran
 *     `leaveGroupCall`, so navigating to Home, Contacts or another chat hung the
 *     SFU session up and published the Leave (a confirmation prompt only asked
 *     the user to choose between one screen and the call).
 *   - **The call key froze outside the chat.** The mid-call SFrame rekey
 *     (G-18 / ADR-0029) was an effect over the chat view's `membersVersion`, a
 *     counter only that view bumps. Nothing else noticed an epoch advance.
 *   - **A call started in a chat that was not open surfaced nowhere.** The
 *     group-call record already heard the start from the background receive,
 *     but only the chat view rendered it.
 *
 * The host now owns the session, its UI adapter, the in-call overlay and the
 * hang-up; the rekey is driven by the per-group MLS lock draining
 * (`mls/group-lock.ts` `onGroupMlsIdle`), which every epoch-moving region on
 * every receive path runs under; and the call lane of the background receive
 * long-polls the call's group while the chat is closed.
 */

/** Why this device is leaving a group call. */
export type GroupLeaveReason = "hungUp" | "error";

export interface GroupCallLeaveInput {
  /** What the caller asked for. */
  readonly reason: GroupLeaveReason;
  /** The SFU session itself reported `failed` — overrides `reason`. */
  readonly sessionFailed: boolean;
  /** This device published its `GroupCallJoin`. */
  readonly joinPublished: boolean;
  /** This device published the `GroupCallStart`. */
  readonly initiator: boolean;
  /** This device's signature key (base64), as the record keys members. */
  readonly ownMemberKey: string;
  /** FRESH members of the call's record at leave time (`freshMemberKeys`),
   *  `null` when the record is gone (another member already ended it). A
   *  member whose heartbeat stopped does not keep the call alive. */
  readonly recordFreshMemberKeys: ReadonlySet<string> | null;
}

export interface GroupCallLeavePlan {
  /** The reason carried by the Leave / End bodies. */
  readonly leaveReason: GroupLeaveReason;
  /** Publish a `GroupCallLeave`. */
  readonly publishLeave: boolean;
  /** Publish the terminating `CallEnd`. */
  readonly publishEnd: boolean;
}

/**
 * What leaving a group call has to tell the group.
 *
 * A Leave only if this device announced a Join. A `CallEnd` only when the local
 * record shows nobody else freshly joined, and only from a device that was actually in
 * the call (joined) or that started it: a joiner whose connection failed before
 * it ever joined must not end a call other members may be about to enter.
 */
export function planGroupCallLeave(input: GroupCallLeaveInput): GroupCallLeavePlan {
  const leaveReason: GroupLeaveReason = input.sessionFailed ? "error" : input.reason;
  const publishLeave = input.joinPublished;
  let publishEnd = false;
  if (input.joinPublished || input.initiator) {
    const othersJoined =
      input.recordFreshMemberKeys !== null &&
      [...input.recordFreshMemberKeys].some((key) => key !== input.ownMemberKey);
    publishEnd = !othersJoined;
  }
  return { leaveReason, publishLeave, publishEnd };
}

/** What the host does when a group's MLS lock queue drains. */
export type GroupCallEpochAction =
  /** Not the call's group, nothing changed, or the epoch cannot be read. */
  | "none"
  /** Re-derive the call seed at the current epoch and rekey the session. */
  | "rekey"
  /** The device is no longer in the group: tear the call down locally. */
  | "end-locally";

export interface GroupCallEpochInput {
  /** The group of the live group call (one with a session), or `null`. */
  readonly callGroupIdB64: string | null;
  /** The group whose lock queue drained. */
  readonly settledGroupIdB64: string;
  /** The chat is still in the registry. */
  readonly groupJoined: boolean;
  /** `Group.isActive`, `undefined` when it cannot be read. */
  readonly isActive: boolean | undefined;
  /** The epoch the session's current seed was derived at. */
  readonly keyedEpoch: bigint | null;
  /** The group's epoch now, `undefined` when it cannot be read. */
  readonly currentEpoch: bigint | undefined;
}

/**
 * Decide whether a drained lock queue means the live group call must rekey.
 *
 * Fail-closed in both directions (invariant #4): an unreadable epoch keeps the
 * current key rather than guessing, and a group this device has left ends the
 * call rather than keeping media flowing under a key no member will rotate
 * again. `Group.isActive` going false is permanent (ADR-0045 UX §6.2), and every
 * exporter call on such a group raises, so there would be nothing to rekey to.
 */
export function decideGroupCallEpochChange(input: GroupCallEpochInput): GroupCallEpochAction {
  if (input.callGroupIdB64 === null || input.callGroupIdB64 !== input.settledGroupIdB64) {
    return "none";
  }
  if (!input.groupJoined || input.isActive === false) return "end-locally";
  if (input.currentEpoch === undefined) return "none";
  if (input.keyedEpoch !== null && input.keyedEpoch === input.currentEpoch) return "none";
  return "rekey";
}

/** A live group call some chat has recorded. */
export interface LiveGroupCallRef {
  readonly groupIdB64: string;
  readonly callIdHex: string;
}

export interface GroupCallBannerInput {
  /** Every chat with a live group call, in registry order. */
  readonly live: readonly LiveGroupCallRef[];
  /** The chat view for this group is mounted (it shows its own banner). */
  readonly isChatOpen: (groupIdB64: string) => boolean;
  /** Whether this device is still a member (`Group.isActive !== false`). */
  readonly isMember: (groupIdB64: string) => boolean;
  /** Any call — 1x1, group or call link — is ringing, starting, waiting or
   *  live on this device. */
  readonly inAnyCall: boolean;
  /** Calls the user dismissed, or left, this session. */
  readonly dismissedCallIds: ReadonlySet<string>;
}

/**
 * The group call to announce on screens other than its chat, or `null`.
 *
 * Nothing while this device is in a call (one call at a time), nothing for a
 * chat that is open (its own banner already offers Join), nothing for a group
 * this device has left (a call control asserts a live E2EE session — UX
 * §6.2.11), and nothing for a call the user dismissed or has just left.
 */
export function selectGroupCallBanner(input: GroupCallBannerInput): LiveGroupCallRef | null {
  if (input.inAnyCall) return null;
  for (const ref of input.live) {
    if (input.dismissedCallIds.has(ref.callIdHex)) continue;
    if (input.isChatOpen(ref.groupIdB64)) continue;
    if (!input.isMember(ref.groupIdB64)) continue;
    return ref;
  }
  return null;
}

/** Chosen: long enough for a peer whose app blinked to come back, short enough
 *  that nobody sits in an empty room. The same five seconds the SFU gives an
 *  empty room before it calls the call over (`docs/ux/calls.md` section 4.3). */
export const GROUP_CALL_ALONE_END_MS = 5_000;

export type GroupCallAloneAction = "none" | "arm-end" | "disarm";

/**
 * Whether a group call left with nobody else in it should end by itself
 * (`docs/ux/calls.md` §4.3): when the last other person leaves a call that had
 * other people in it, the call ends as a hang-up `GROUP_CALL_ALONE_END_MS`
 * later, and someone rejoining inside that window cancels it. The person who
 * started a call and is still waiting for the first joiner is NOT in this
 * case — nobody left.
 *
 * @param remoteCount other participants on the roster now.
 * @param sawRemote this call has had at least one other participant.
 * @param armed the alone-end timer is currently running.
 */
export function groupCallAloneAction(
  remoteCount: number,
  sawRemote: boolean,
  armed: boolean,
): GroupCallAloneAction {
  if (remoteCount === 0 && sawRemote && !armed) return "arm-end";
  if (remoteCount > 0 && armed) return "disarm";
  return "none";
}

/** What a chat's call buttons do. */
export type ChatCallAffordance =
  /** No call buttons: membership ended, or a call is already running. */
  | "hidden"
  /** Join the group call already in progress in this chat. */
  | "join-group"
  /** Start a new group call over the SFU. */
  | "start-group"
  /** Place a 1x1 P2P call. */
  | "start-one-to-one";

export interface ChatCallAffordanceInput {
  readonly membershipEnded: boolean;
  /** Any call — 1x1, group or call link — is ringing, starting, waiting or
   *  live anywhere (`call-slot.ts`). */
  readonly callInProgress: boolean;
  /** This chat has a live group-call record. */
  readonly liveCallInThisChat: boolean;
  /** Set when the conversation was created as a group (ADR-0060), whatever
   *  its people count; a two-person group calls over the SFU too. */
  readonly isGroup: boolean;
}

/**
 * The chat header's call buttons.
 *
 * A call already running anywhere hides them: returning to the chat of a live
 * group call must show that call (the host's overlay), never offer to start a
 * second one. A group call already in progress in this chat is JOINED, never
 * shadowed by a new one; otherwise a group starts an SFU call and a one-to-one
 * a 1x1 P2P call. The kind is the conversation's own (ADR-0060), never the
 * people count, so a group of two still calls as a group.
 */
export function chatCallAffordance(input: ChatCallAffordanceInput): ChatCallAffordance {
  if (input.membershipEnded || input.callInProgress) return "hidden";
  if (input.liveCallInThisChat) return "join-group";
  return input.isGroup ? "start-group" : "start-one-to-one";
}
