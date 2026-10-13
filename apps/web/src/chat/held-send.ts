// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 / UX 16.4 - the HELD send of a one-to-one whose other
 * person reset their keys.
 *
 * Once the reset is accepted, a message is never sent into the old session
 * (only removed devices would get it). Until the successor exists it is
 * held: the bubble shows the ordinary sending state and the message goes
 * out when the session exists; after the give-up it shows the ordinary
 * failed state with its ordinary retry. No copy of its own (R.0).
 *
 * What is held: a text message, a media message (once its bytes are
 * uploaded) and a forward into such a one-to-one. What is NOT held is an op
 * on a message of the old session (an edit, a delete, a reaction): its
 * target exists only in the old session, so there is nothing a successor
 * could apply it to. It is applied on this device alone and never sent
 * ({@link opRoute}); the founder's re-send of its own messages reads the
 * stored text, which is the edited text, and skips a deleted one.
 *
 * The store is keyed on the CONVERSATION id, so a held message survives the
 * view remounting onto the successor group (the moment it can finally go
 * out) and a change of route. `held-send-queue.ts` mirrors it into the
 * message log (under the device key), so it also survives a reload, and
 * flushes it from the background when no view of the conversation is
 * mounted.
 *
 * The refusal itself is enforced where every publish goes through
 * (`chat/client.ts` `installDeadSessionGuard`): nothing this module forgets
 * to route can reach the old session.
 *
 * A group is never held: a member sends where its device is (R.7 case 3).
 */
import type { ConversationKind } from "./conversation-kind";

/** Where a composed message goes now. */
export type SendRoute =
  /** Into the group the view has open. */
  | "send"
  /** Nowhere yet: the successor of this one-to-one does not exist here. */
  | "hold";

/**
 * | kind  | the open group is unrepairable for an accepted reset | route  |
 * |-------|------------------------------------------------------|--------|
 * | chat  | yes                                                  | `hold` |
 * | chat  | no                                                   | `send` |
 * | group | any                                                  | `send` |
 *
 * `unrepairableKeyB64` is `successor-claim.ts` `unrepairableBy` over the
 * OPEN group: once the view is on the successor (whose roster holds the new
 * identity) it is `null` and the message goes out.
 */
export function sendRoute(input: {
  readonly kind: ConversationKind;
  readonly unrepairableKeyB64: string | null;
}): SendRoute {
  return input.kind === "chat" && input.unrepairableKeyB64 !== null ? "hold" : "send";
}

/** What an op on a message (edit, delete, reaction) does now. */
export type OpRoute =
  /** Publish it, then apply it locally. */
  | "send"
  /** Apply it on this device only; nothing is published. */
  | "local-only";

/**
 * | kind  | the open group is unrepairable for an accepted reset | op route     |
 * |-------|------------------------------------------------------|--------------|
 * | chat  | yes                                                  | `local-only` |
 * | chat  | no                                                   | `send`       |
 * | group | any                                                  | `send`       |
 *
 * The same condition as {@link sendRoute}'s `hold`: where a message would be
 * held, an op is local.
 */
export function opRoute(input: {
  readonly kind: ConversationKind;
  readonly unrepairableKeyB64: string | null;
}): OpRoute {
  return sendRoute(input) === "hold" ? "local-only" : "send";
}

/** Whether the background flushes a conversation's held messages now. */
export type FlushVerdict =
  /** Send them into the conversation's current group. */
  | "flush"
  /** Leave them held. */
  | "wait";

/**
 * | held | a view of the conversation is mounted | head is live here | head route | verdict |
 * |------|---------------------------------------|-------------------|------------|---------|
 * | 0    | any                                   | any               | any        | `wait`  |
 * | > 0  | yes (the view resumes them itself)    | any               | any        | `wait`  |
 * | > 0  | no                                    | no                | any        | `wait`  |
 * | > 0  | no                                    | yes               | `hold`     | `wait`  |
 * | > 0  | no                                    | yes               | `send`     | `flush` |
 *
 * `headRoute` is {@link sendRoute} over the conversation's CURRENT group as
 * this device holds it: while that is still the superseded one-to-one the
 * route is `hold`; once the successor is the head it is `send`. There is no
 * give-up here: a message nobody is looking at is not failed, it waits.
 */
export function flushVerdict(input: {
  readonly held: number;
  readonly viewMounted: boolean;
  readonly headActive: boolean;
  readonly headRoute: SendRoute;
}): FlushVerdict {
  if (input.held <= 0 || input.viewMounted || !input.headActive) return "wait";
  return input.headRoute === "send" ? "flush" : "wait";
}

/**
 * Whether a group is a DEAD SESSION: a one-to-one whose other person's reset
 * this device accepted. The predicate of the publish guard
 * (`chat/client.ts` `installDeadSessionGuard`): into such a group nothing is
 * published but the successor claim.
 *
 * `unrepairableKeyB64` is `successor-claim.ts` `unrepairableBy` over the
 * group. A GROUP conversation is never a dead session, whoever reset: its
 * members send where their device is (R.7 cases 2 and 3).
 */
export function isDeadSession(input: {
  readonly kind: ConversationKind;
  readonly unrepairableKeyB64: string | null;
}): boolean {
  return sendRoute(input) === "hold";
}

/** How long a held message waits before it shows the ordinary failed state.
 *  Chosen, not measured: long enough for the other person's new device to
 *  publish what is needed to add it, short enough that "sending" does not
 *  sit on screen for an evening. */
export const HELD_SEND_GIVE_UP_MS = 10 * 60 * 1000;

/** What to do with a held message now. */
export type HeldVerdict = "send" | "wait" | "give-up";

/**
 * | route  | held for             | verdict   |
 * |--------|----------------------|-----------|
 * | `send` | any                  | `send`    |
 * | `hold` | under the give-up    | `wait`    |
 * | `hold` | the give-up or more  | `give-up` |
 */
export function heldVerdict(input: {
  readonly route: SendRoute;
  readonly heldAtMs: number;
  readonly nowMs: number;
}): HeldVerdict {
  if (input.route === "send") return "send";
  return input.nowMs - input.heldAtMs >= HELD_SEND_GIVE_UP_MS ? "give-up" : "wait";
}

/**
 * The reply target a flushed reply carries when its original belongs to
 * another segment: a DS message id no row has (ids start at 1), so the
 * quote is drawn from its snapshot and a tap on it finds nothing. The wire
 * needs SOME target for the snapshot to be carried (the core drops a
 * snapshot without one), so "no jump target" is this value, not an absent
 * field.
 */
export const REPLY_NO_JUMP_TARGET = 0;

/**
 * ADR-0073 R.7 case 1 - the reply reference a held message is SENT with.
 *
 * A reply names its original by that message's id in the journal it was
 * composed in. A held reply is composed in the old one-to-one and flushed
 * into the successor, whose ids restart: the old id there names nothing, or
 * worse, an unrelated message. So the quote (author and excerpt, a snapshot
 * taken at compose time) is kept and the jump target is dropped.
 *
 * | reply | composed in group       | sent into group | result                          |
 * |-------|-------------------------|-----------------|---------------------------------|
 * | none  | -                       | -               | none                            |
 * | set   | the same                | the same        | unchanged                       |
 * | set   | another                 | -               | quote kept, target dropped      |
 * | set   | unknown (not recorded)  | -               | quote kept, target dropped      |
 *
 * The last row is the fail-safe direction: a held message whose compose
 * group was not recorded is never sent with a target that may be wrong.
 */
export function replyForSend<R extends { readonly target: number }>(
  reply: R | undefined,
  composedInGroupIdB64: string | undefined,
  sendIntoGroupIdB64: string,
): R | undefined {
  if (reply === undefined) return undefined;
  if (composedInGroupIdB64 !== undefined && composedInGroupIdB64 === sendIntoGroupIdB64) return reply;
  return reply.target === REPLY_NO_JUMP_TARGET ? reply : { ...reply, target: REPLY_NO_JUMP_TARGET };
}

/**
 * ADR-0073 R.7 case 1 - which of the messages a background flush just sent
 * the open view still has to show.
 *
 * The background flushes a conversation's held messages only while no view
 * of it is attached; a view that is MOUNTING at that moment has already
 * read the log (before the flush wrote to it) and finds the queue empty, so
 * without this it would show the messages only after a later re-read. The
 * flush reports what it sent; the view adds the rows it does not hold.
 *
 * | the view holds a row with that message id | shown now |
 * |-------------------------------------------|-----------|
 * | yes (the log read came after the write)   | no        |
 * | no                                        | yes       |
 *
 * Order is the flush's (send order); a message id repeated in the report is
 * shown once.
 */
export function flushedRowsToShow<M extends { readonly messageId: string }>(
  shownMessageIds: ReadonlySet<number>,
  flushed: readonly M[],
): M[] {
  const seen = new Set<number>(shownMessageIds);
  const out: M[] = [];
  for (const m of flushed) {
    const id = Number(m.messageId);
    if (!Number.isFinite(id) || seen.has(id)) continue;
    seen.add(id);
    out.push(m);
  }
  return out;
}

/** One held message. `payload` is the view's own (text, attachments, reply). */
export interface HeldSend<T> {
  readonly heldAtMs: number;
  readonly payload: T;
}

/** The per-session store of held messages, by conversation id. */
export class HeldSendStore<T> {
  private readonly byConversation = new Map<string, HeldSend<T>[]>();

  /** Hold one more message of `conversationId`, in send order. */
  hold(conversationId: string, payload: T, heldAtMs: number): void {
    const list = this.byConversation.get(conversationId) ?? [];
    list.push({ heldAtMs, payload });
    this.byConversation.set(conversationId, list);
  }

  /** Take every held message of `conversationId`, in send order. The store
   *  no longer holds them: whoever took them owns sending them. */
  take(conversationId: string): HeldSend<T>[] {
    const list = this.byConversation.get(conversationId) ?? [];
    this.byConversation.delete(conversationId);
    return list;
  }

  /** Put messages back at the FRONT, in the order given (a flush that
   *  stopped part-way returns what it did not send). */
  putBack(conversationId: string, items: readonly HeldSend<T>[]): void {
    if (items.length === 0) return;
    const list = this.byConversation.get(conversationId) ?? [];
    this.byConversation.set(conversationId, [...items, ...list]);
  }

  /** Every conversation with something held, with its messages in order. */
  all(): [string, readonly HeldSend<T>[]][] {
    return [...this.byConversation.entries()].map(([k, v]) => [k, [...v]]);
  }

  /** Replace everything (a restore from the log at start). */
  replaceAll(entries: readonly (readonly [string, HeldSend<T>])[]): void {
    this.byConversation.clear();
    for (const [conversationId, item] of entries) {
      const list = this.byConversation.get(conversationId) ?? [];
      list.push(item);
      this.byConversation.set(conversationId, list);
    }
  }

  /** How many are held for `conversationId`. */
  count(conversationId: string): number {
    return this.byConversation.get(conversationId)?.length ?? 0;
  }

  /** Sign-out: nothing held survives into the next account. */
  clear(): void {
    this.byConversation.clear();
  }
}

/** How old the last identity check of a one-to-one's peer may be before a
 *  send (and an open) runs it again (R.2 "Triggers"): the re-admission arm's
 *  interval. */
export function identityCheckDue(lastCheckedAtMs: number | undefined, nowMs: number, intervalMs: number): boolean {
  return lastCheckedAtMs === undefined || nowMs - lastCheckedAtMs >= intervalMs;
}
