// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * ADR-0065 R.12 / UX 16.4 - a live ONE-TO-ONE call ends when its conversation
 * moves.
 *
 * A one-to-one call takes its keys and its signalling from the one-to-one's
 * group. Once the other person's reset is accepted that group's other leaves
 * are removed devices: the media is going to a device that is no longer the
 * person's. The call ends the way a call ends when the other side drops, with
 * no copy of its own (R.0); the next call uses the conversation's successor.
 * A group call is not ended here.
 *
 * | a one-to-one call is live in group G | G is unrepairable for an accepted reset | the conversation's head is G | ends |
 * |--------------------------------------|------------------------------------------|------------------------------|------|
 * | no                                   | -                                        | -                            | no   |
 * | yes                                  | yes                                      | any                          | yes  |
 * | yes                                  | no                                       | no (it moved)                | yes  |
 * | yes                                  | no                                       | yes, or unknown              | no   |
 *
 * Mirror of `apps/web/src/calls/call-continuity.ts`.
 */
public fun oneToOneCallEndsOnMove(
    /** The group the live one-to-one call runs in; `null` when none is live. */
    callGroupIdHex: String?,
    /** `unrepairableBy` over that group. */
    unrepairableKeyB64: String?,
    /** The head group of that group's conversation, or `null` when unknown. */
    headGroupIdHex: String?,
): Boolean {
    if (callGroupIdHex == null) return false
    if (unrepairableKeyB64 != null) return true
    return headGroupIdHex != null && headGroupIdHex != callGroupIdHex
}

/** Where a one-to-one call asked for in one group is placed. */
public sealed interface OneToOneCallTarget {
    public data class Dial(val groupIdHex: String) : OneToOneCallTarget

    /** The call does not start: it fails before any signalling, with the
     *  client's existing "could not start the call" copy. */
    public data object Unavailable : OneToOneCallTarget
}

/**
 * A call takes its keys and its signalling from the group it is dialled in.
 * In a one-to-one whose other person's reset was accepted an offer rings
 * nobody the person is. So the call is placed in the conversation's successor
 * when this device holds one, and otherwise it does not start.
 *
 * | the asked group is unrepairable | the conversation's head | the head is unrepairable | target        |
 * |---------------------------------|-------------------------|--------------------------|---------------|
 * | no                              | the asked group/unknown | -                        | dial asked    |
 * | no                              | another group           | no                       | dial the head |
 * | no                              | another group           | yes                      | unavailable   |
 * | yes                             | the asked group/unknown | -                        | unavailable   |
 * | yes                             | another group           | no                       | dial the head |
 * | yes                             | another group           | yes                      | unavailable   |
 */
public fun oneToOneCallTarget(
    /** The group the call was asked for. */
    groupIdHex: String,
    /** `unrepairableBy` over that group. */
    unrepairableKeyB64: String?,
    /** The head group of that group's conversation, or `null` when unknown. */
    headGroupIdHex: String?,
    /** `unrepairableBy` over the head (`null` when the head is the asked
     *  group, is unknown, or is repairable). */
    headUnrepairableKeyB64: String?,
): OneToOneCallTarget {
    val moved = headGroupIdHex != null && headGroupIdHex != groupIdHex
    if (moved) {
        return if (headUnrepairableKeyB64 == null) OneToOneCallTarget.Dial(headGroupIdHex!!) else OneToOneCallTarget.Unavailable
    }
    return if (unrepairableKeyB64 == null) OneToOneCallTarget.Dial(groupIdHex) else OneToOneCallTarget.Unavailable
}
