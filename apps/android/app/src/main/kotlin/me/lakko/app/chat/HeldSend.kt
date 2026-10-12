// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0065 R.7 case 1 / UX 16.4 - the HELD send of a one-to-one whose other
 * person reset their keys.
 *
 * Once the reset is accepted, a message is never sent into the old session
 * (only removed devices would get it). Until the successor exists it is
 * held: the bubble shows the ordinary sending state and the message goes out
 * when the session exists; after the give-up it shows the ordinary failed
 * state with its ordinary retry. No copy of its own (R.0).
 *
 * What is held: a text message, a media message (once its bytes are
 * uploaded) and a forward into such a one-to-one. What is NOT held is an op
 * on a message of the old session (an edit, a delete, a reaction): its target
 * exists only in the old session. It is applied on this device alone and
 * never sent ([opRoute]). Typing ticks and read receipts are not sent.
 *
 * A group is never held: a member sends where its device is (R.7 case 3).
 * Mirror of `apps/web/src/chat/held-send.ts`.
 */

/** Where a composed message goes now. */
public enum class SendRoute {
    /** Into the group the view has open. */
    SEND,

    /** Nowhere yet: the successor of this one-to-one does not exist here. */
    HOLD,
}

/**
 * | kind  | the open group is unrepairable for an accepted reset | route  |
 * |-------|------------------------------------------------------|--------|
 * | chat  | yes                                                  | `HOLD` |
 * | chat  | no                                                   | `SEND` |
 * | group | any                                                  | `SEND` |
 */
public fun sendRoute(kind: ConversationKind, unrepairableKeyB64: String?): SendRoute =
    if (kind == ConversationKind.Chat && unrepairableKeyB64 != null) SendRoute.HOLD else SendRoute.SEND

/** What an op on a message (edit, delete, reaction) does now. */
public enum class OpRoute {
    /** Publish it, then apply it locally. */
    SEND,

    /** Apply it on this device only; nothing is published. */
    LOCAL_ONLY,
}

/** The same condition as [sendRoute]'s `HOLD`: where a message would be held,
 *  an op is local. */
public fun opRoute(kind: ConversationKind, unrepairableKeyB64: String?): OpRoute =
    if (sendRoute(kind, unrepairableKeyB64) == SendRoute.HOLD) OpRoute.LOCAL_ONLY else OpRoute.SEND

/** Whether the background flushes a conversation's held messages now. */
public enum class FlushVerdict { FLUSH, WAIT }

/**
 * | held | a view of the conversation is mounted | head is live here | head route | verdict |
 * |------|---------------------------------------|-------------------|------------|---------|
 * | 0    | any                                   | any               | any        | `WAIT`  |
 * | > 0  | yes (the view resumes them itself)    | any               | any        | `WAIT`  |
 * | > 0  | no                                    | no                | any        | `WAIT`  |
 * | > 0  | no                                    | yes               | `HOLD`     | `WAIT`  |
 * | > 0  | no                                    | yes               | `SEND`     | `FLUSH` |
 *
 * On Android the flush is the repository's own (it owns every send), so the
 * caller passes `viewMounted = false`; the parameter is kept so the table is
 * the web one.
 */
public fun flushVerdict(held: Int, viewMounted: Boolean, headActive: Boolean, headRoute: SendRoute): FlushVerdict =
    if (held <= 0 || viewMounted || !headActive || headRoute != SendRoute.SEND) FlushVerdict.WAIT else FlushVerdict.FLUSH

/**
 * Whether a group is a DEAD SESSION: a one-to-one whose other person's reset
 * this device accepted. The predicate of the publish guard: into such a group
 * nothing is published but the successor claim. A GROUP conversation is never
 * a dead session, whoever reset.
 */
public fun isDeadSession(kind: ConversationKind, unrepairableKeyB64: String?): Boolean =
    sendRoute(kind, unrepairableKeyB64) == SendRoute.HOLD

/** What may be published into a group, as far as the dead-session guard is
 *  concerned. */
public enum class PublishKind {
    /** The successor claim (`group_closed`). */
    CLAIM,

    /** Anything else: a message, an op, a typing tick, a read receipt, a call
     *  frame, a service envelope, a commit. */
    OTHER,
}

/** The publish guard: nothing goes into a dead one-to-one except the claim. */
public fun publishAllowed(deadSession: Boolean, kind: PublishKind): Boolean =
    !deadSession || kind == PublishKind.CLAIM

/** How long a held message waits before it shows the ordinary failed state.
 *  Chosen, not measured. */
public const val HELD_SEND_GIVE_UP_MS: Long = 10L * 60 * 1000

/** What to do with a held message now. */
public enum class HeldVerdict { SEND, WAIT, GIVE_UP }

/**
 * | route  | held for            | verdict   |
 * |--------|---------------------|-----------|
 * | `SEND` | any                 | `SEND`    |
 * | `HOLD` | under the give-up   | `WAIT`    |
 * | `HOLD` | the give-up or more | `GIVE_UP` |
 */
public fun heldVerdict(route: SendRoute, heldAtMs: Long, nowMs: Long): HeldVerdict = when {
    route == SendRoute.SEND -> HeldVerdict.SEND
    nowMs - heldAtMs >= HELD_SEND_GIVE_UP_MS -> HeldVerdict.GIVE_UP
    else -> HeldVerdict.WAIT
}

/**
 * The reply target a flushed reply carries when its original belongs to
 * another segment: a DS message id no row has (ids start at 1), so the quote
 * is drawn from its snapshot and a tap on it finds nothing. The wire needs
 * SOME target for the snapshot to be carried.
 */
public const val REPLY_NO_JUMP_TARGET: Long = 0L

/**
 * ADR-0065 R.7 case 1 - the reply target a held message is SENT with.
 *
 * | reply | composed in group      | sent into group | result                     |
 * |-------|------------------------|-----------------|----------------------------|
 * | none  | -                      | -               | none                       |
 * | set   | the same               | the same        | unchanged                  |
 * | set   | another                | -               | quote kept, target dropped |
 * | set   | unknown (not recorded) | -               | quote kept, target dropped |
 *
 * The last row is the fail-safe direction: a held message whose compose group
 * was not recorded is never sent with a target that may be wrong.
 */
public fun replyTargetForSend(replyTarget: Long?, composedInGroupHex: String?, sendIntoGroupHex: String): Long? {
    if (replyTarget == null) return null
    if (composedInGroupHex != null && composedInGroupHex == sendIntoGroupHex) return replyTarget
    return REPLY_NO_JUMP_TARGET
}

/** How old the last identity check of a one-to-one's peer may be before a
 *  send (and an open) runs it again (R.2 "Triggers"): the re-admission arm's
 *  interval. */
public fun identityCheckDue(lastCheckedAtMs: Long?, nowMs: Long, intervalMs: Long): Boolean =
    lastCheckedAtMs == null || nowMs - lastCheckedAtMs >= intervalMs
