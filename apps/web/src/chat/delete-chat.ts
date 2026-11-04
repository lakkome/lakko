// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Delete this chat`, the removal itself.
 *
 * Reachable only once the membership has ended, so this never removes a live
 * conversation, and since ADR-0087 section 9 only on a device that still holds
 * the conversation after that: an ended one-to-one, closed from the contact
 * card, and a group whose own leaf was retired while the account stays in it.
 * Everyone else's group goes by itself (`MembershipEndContext.tsx`). The
 * confirmation dialog and the busy / error state stay with each caller; what
 * is here is the part that must not drift between them: which stores are
 * touched, and in which order.
 *
 * **What it removes:** everything on `membership-wipe.ts`'s list, then the MLS
 * group (`deleteGroup`, `Group.deleteLocally`), then the row in the in-memory
 * registry.
 *
 * The push subscription is already gone by this point: the §6.2.8 teardown
 * retires it at the moment of the eviction, not at the moment of the delete,
 * so a user who never presses this button still stops being woken.
 *
 * **The order is the rule.** The registry row is dropped LAST, so a failure
 * anywhere above leaves the chat reachable rather than orphaning a transcript
 * behind a row the user can no longer open. `delete-chat.test.ts` pins that.
 */

import {
  groupWipeSteps,
  runWipeSteps,
  type GroupWipeContexts,
} from "./membership-wipe";

/** The one thing this needs from the group registry. */
export interface ChatRegistry {
  /** Drop the row; the caller navigates away afterwards. */
  remove(groupIdB64: string): void;
}

export interface DeleteChatInput {
  readonly accountIdB64: string;
  /** Standard base64 of the group id — the key of every per-group store. */
  readonly groupIdB64: string;
  /** Hex of the same id — the key the message log is filed under. */
  readonly groupIdHex: string;
  /** Hex of the conversation's id, which what is about the conversation is
   *  filed under. */
  readonly conversationHex: string;
  readonly chats: ChatRegistry;
  readonly contexts: GroupWipeContexts;
  /** Delete the MLS group and leave that durable. */
  readonly deleteGroup: (groupIdB64: string) => Promise<void>;
}

/**
 * Delete an ended chat from this device.
 *
 * Throws when a store could not be cleared or the MLS group not deleted; the
 * caller shows the error and the chat stays reachable, because the registry
 * row is only dropped once everything above it succeeded. Every step can be
 * run again.
 */
export async function deleteChat(input: DeleteChatInput): Promise<void> {
  const { accountIdB64, groupIdB64, groupIdHex, conversationHex, chats, contexts, deleteGroup } = input;
  const failed = await runWipeSteps(
    groupWipeSteps({ accountIdB64, groupIdB64, groupIdHex, conversationHex }, contexts),
  );
  if (failed.length > 0) throw new Error(`the chat was not deleted: ${failed.join(",")}`);
  await deleteGroup(groupIdB64);
  // Drop the row from the registry LAST, so a failure above leaves the chat
  // reachable rather than orphaning a transcript behind a row the user can no
  // longer open.
  chats.remove(groupIdB64);
}

/** One group of a conversation, as the delete reads it. */
export interface ConversationSegment {
  readonly groupIdB64: string;
  readonly groupIdHex: string;
}

/**
 * ADR-0073 R.5 — the order in which a conversation's groups are deleted:
 * every held segment that is not the head first (in the order given), the
 * HEAD last. The head's registry row is what keeps the conversation's row
 * on screen, so a failure at any earlier segment leaves the conversation
 * reachable, exactly as {@link deleteChat} does for one group. A segment
 * named twice is deleted once.
 */
export function conversationDeleteOrder(
  held: readonly ConversationSegment[],
  headGroupIdB64: string,
): ConversationSegment[] {
  const seen = new Set<string>();
  const rest: ConversationSegment[] = [];
  let head: ConversationSegment | null = null;
  for (const segment of held) {
    if (seen.has(segment.groupIdB64)) continue;
    seen.add(segment.groupIdB64);
    if (segment.groupIdB64 === headGroupIdB64) head = segment;
    else rest.push(segment);
  }
  return head === null ? rest : [...rest, head];
}

/**
 * Delete a whole CONVERSATION from this device: every group of its lineage
 * this device holds (ADR-0073 R.5: one conversation over several groups).
 * A conversation that never moved has one segment and this is
 * {@link deleteChat}.
 *
 * Throws at the first segment that fails; the segments before it are gone
 * and the head is still there.
 */
export async function deleteConversation(input: {
  readonly accountIdB64: string;
  /** Every segment this device holds, the head among them. */
  readonly segments: readonly ConversationSegment[];
  readonly headGroupIdB64: string;
  readonly conversationHex: string;
  readonly chats: ChatRegistry;
  readonly contexts: GroupWipeContexts;
  readonly deleteGroup: (groupIdB64: string) => Promise<void>;
}): Promise<void> {
  for (const segment of conversationDeleteOrder(input.segments, input.headGroupIdB64)) {
    await deleteChat({
      accountIdB64: input.accountIdB64,
      groupIdB64: segment.groupIdB64,
      groupIdHex: segment.groupIdHex,
      conversationHex: input.conversationHex,
      chats: input.chats,
      contexts: input.contexts,
      deleteGroup: input.deleteGroup,
    });
  }
}
