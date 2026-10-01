// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6.2.4 — `Delete this chat`, the removal itself.
 *
 * Reachable only once the membership has ended, so this never removes a live
 * conversation. Two screens reach it: the conversation's own exit block
 * (`routes/Chat.tsx`, the group case) and the contact card
 * (`routes/Contacts.tsx`, the one-to-one case — `docs/ux/conversation-info.md`
 * section 3 sends a person's header to the card, so the card is the only
 * place an ENDED one-to-one can be closed from on web, as it already is on
 * iOS and Android). The confirmation dialog and the busy / error state stay
 * with each caller; what is here is the part that must not drift between
 * them: which stores are touched, and in which order.
 *
 * **What it removes:** this group's entry in the per-account message log
 * (a read-modify-write on a store that already exists — the log is keyed
 * `{ groupIdHex -> PersistedGroup }` internally, so no new store is needed),
 * its persisted conversation kind, its pending-request observations, its
 * unapplied-request records, its durable leave record, its armed re-sends,
 * and its row in the in-memory group registry.
 *
 * **What it does NOT remove, which is why the confirmation body is exact
 * rather than sweeping:** the inactive MLS group stays in the provider
 * snapshot as inert bytes. `Group` exposes no delete, so a `Group::delete`
 * is a real (small) core follow-up rather than something this rides free.
 * The remnant is inert in the strong sense — it cannot send, decrypt, export
 * or process anything — but it is not nothing.
 *
 * The push subscription is already gone by this point: the §6.2.8 teardown
 * retires it at the moment of the eviction, not at the moment of the delete,
 * so a user who never presses this button still stops being woken.
 *
 * **The order is the rule.** The registry row is dropped LAST, so a failure
 * anywhere above leaves the chat reachable rather than orphaning a transcript
 * behind a row the user can no longer open. `delete-chat.test.ts` pins that.
 */

import { getDeviceKey } from "../auth/key-store";
import { forgetConversationKind } from "./conversation-kind-store";
import { clearLeaveRequested } from "./membership-ended";
import { clearArmedRetries, clearOwnRequestsForGroup, retryScopeKey } from "./membership-retry";
import { deleteGroupFromMessageLog } from "./message-log-store";
import { pruneObservations } from "./pending-requests";
import { clearUnappliedForGroup } from "./unapplied-requests";

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
  readonly chats: ChatRegistry;
}

/**
 * Delete an ended chat from this device.
 *
 * Throws when the message-log removal fails; the caller shows the error and
 * the chat stays reachable, because the registry row is only dropped once
 * everything above it succeeded.
 */
export async function deleteChat(input: DeleteChatInput): Promise<void> {
  const { accountIdB64, groupIdB64, groupIdHex, chats } = input;
  const deviceKey = await getDeviceKey(accountIdB64);
  if (deviceKey !== null) {
    await deleteGroupFromMessageLog(accountIdB64, deviceKey, groupIdHex);
  }
  // Local bookkeeping for a group that no longer exists here. All are
  // best-effort by construction and none can fail the delete.
  forgetConversationKind(accountIdB64, groupIdB64);
  pruneObservations(accountIdB64, groupIdB64, []);
  clearUnappliedForGroup(accountIdB64, groupIdB64);
  clearOwnRequestsForGroup(accountIdB64, groupIdB64);
  clearLeaveRequested(accountIdB64, groupIdB64);
  // ADR-0045 UX §7 — an armed re-send names a group that is gone from
  // here. The verdict would drop it anyway on `membershipEnded`, so this
  // is tidiness rather than a gate; it is here because leaving a record
  // behind for a deleted chat is the kind of residue that later reads as
  // a bug.
  clearArmedRetries(retryScopeKey(accountIdB64, groupIdB64));
  // Drop the row from the registry LAST, so a failure above leaves the
  // chat reachable rather than orphaning a transcript behind a row the
  // user can no longer open.
  chats.remove(groupIdB64);
}
