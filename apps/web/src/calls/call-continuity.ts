// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 R.12 / UX 16.4 - a live ONE-TO-ONE call ends when its
 * conversation moves.
 *
 * A one-to-one call takes its keys and its signalling from the one-to-one's
 * group. Once the other person's reset is accepted that group's other
 * leaves are removed devices: the media is going to a device that is no
 * longer the person's. The call ends the way a call ends when the other
 * side drops, with no copy of its own (R.0); the next call uses the
 * conversation's successor.
 *
 * A group call is not ended here: its members send where their devices are
 * (R.7 case 3), and a group call's roster is the SFU's.
 */

/**
 * | a one-to-one call is live in group G | G is unrepairable for an accepted reset | the conversation's head is G | ends |
 * |--------------------------------------|------------------------------------------|------------------------------|------|
 * | no                                   | -                                        | -                            | no   |
 * | yes                                  | yes                                      | any                          | yes  |
 * | yes                                  | no                                       | no (it moved)                | yes  |
 * | yes                                  | no                                       | yes, or unknown              | no   |
 */
export function oneToOneCallEndsOnMove(input: {
  /** The group the live one-to-one call runs in; `null` when none is live. */
  readonly callGroupIdB64: string | null;
  /** `successor-claim.ts` `unrepairableBy` over that group. */
  readonly unrepairableKeyB64: string | null;
  /** The head group of that group's conversation, or `null` when unknown. */
  readonly headGroupIdB64: string | null;
}): boolean {
  if (input.callGroupIdB64 === null) return false;
  if (input.unrepairableKeyB64 !== null) return true;
  return input.headGroupIdB64 !== null && input.headGroupIdB64 !== input.callGroupIdB64;
}

/** Where a one-to-one call the person starts is placed. */
export type OneToOneCallTarget =
  /** Dial in this group. */
  | { readonly kind: "dial"; readonly groupIdB64: string }
  /** No session the other person's devices are in exists on this device
   *  yet: the call does not start, exactly as a call that cannot start
   *  anywhere else (no copy of its own, R.0). */
  | { readonly kind: "unavailable" };

/**
 * ADR-0065 R.12 - a one-to-one call is never STARTED in a dead session.
 *
 * A call takes its keys and its signalling from the group it is dialled in.
 * In a one-to-one whose other person's reset was accepted, that group's
 * other leaves are removed devices: an offer there rings nobody the person
 * is. So the call is placed in the conversation's successor when this
 * device holds one, and otherwise it does not start.
 *
 * | the asked group is unrepairable | the conversation's head | the head is unrepairable | target        |
 * |---------------------------------|-------------------------|--------------------------|---------------|
 * | no                              | the asked group/unknown | -                        | dial asked    |
 * | no                              | another group           | no                       | dial the head |
 * | no                              | another group           | yes                      | unavailable   |
 * | yes                             | the asked group/unknown | -                        | unavailable   |
 * | yes                             | another group           | no                       | dial the head |
 * | yes                             | another group           | yes                      | unavailable   |
 *
 * Rows 2 and 3: a view still open on an older segment after the
 * conversation moved dials where the conversation is now.
 */
export function oneToOneCallTarget(input: {
  /** The group the call was asked for. */
  readonly groupIdB64: string;
  /** `successor-claim.ts` `unrepairableBy` over that group. */
  readonly unrepairableKeyB64: string | null;
  /** The head group of that group's conversation, or `null` when unknown. */
  readonly headGroupIdB64: string | null;
  /** `unrepairableBy` over the head (`null` when the head is the asked
   *  group, is unknown, or is repairable). */
  readonly headUnrepairableKeyB64: string | null;
}): OneToOneCallTarget {
  const moved = input.headGroupIdB64 !== null && input.headGroupIdB64 !== input.groupIdB64;
  if (moved) {
    return input.headUnrepairableKeyB64 === null
      ? { kind: "dial", groupIdB64: input.headGroupIdB64 as string }
      : { kind: "unavailable" };
  }
  return input.unrepairableKeyB64 === null
    ? { kind: "dial", groupIdB64: input.groupIdB64 }
    : { kind: "unavailable" };
}

/** Thrown by the call host's `dial` for {@link OneToOneCallTarget}
 *  `unavailable`: nothing was signalled anywhere. Callers treat it as any
 *  other call that could not start. */
export class CallUnavailableInSession extends Error {
  constructor() {
    super("call unavailable in this session");
    this.name = "CallUnavailableInSession";
  }
}
