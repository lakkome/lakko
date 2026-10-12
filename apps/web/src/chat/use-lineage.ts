// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 R.5 - the conversation lineage as the views read it.
 *
 * `lineage.ts` is the pure model; this binds it to the synced state's
 * registers (`DeviceGroupSyncContext`) and the chat registry
 * (`ChatContext`), so every view answers the same three questions the same
 * way: which conversation a group belongs to, which group a conversation
 * currently is, and which older groups of it this device still holds.
 *
 * Person-facing stores (drafts, pin / mute / archive, read position, folder
 * refs, call history, the route) take the CONVERSATION id; protocol stores
 * take the group id. A view that needs both asks here.
 */
import { createMemo, type Accessor } from "solid-js";

import { base64Decode, base64Encode } from "../auth/account-id";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex } from "../devices/sync/state";
import { useChats, type ActiveGroup } from "./ChatContext";
import { buildLineage, planConversations, type ConversationPlan, type Lineage } from "./lineage";
import { readIsActive } from "./membership-ended";

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** One conversation as a view opens it. */
export interface ConversationView {
  /** The conversation id (the lineage root), hex and standard base64. */
  readonly conversationHex: string;
  readonly conversationIdB64: string;
  /** The group it opens and sends into. */
  readonly head: ActiveGroup;
  /** The older segments this device holds, oldest first (the head excluded,
   *  and any successor beyond the head excluded: it is not history). */
  readonly history: readonly ActiveGroup[];
  /** EVERY segment this device holds, in timeline order: the history, the
   *  head, and any successor beyond the head. What a delete of the
   *  conversation removes. */
  readonly held: readonly ActiveGroup[];
}

export interface LineageView {
  readonly lineage: Accessor<Lineage>;
  /** Every conversation this device holds, one per conversation. */
  readonly conversations: Accessor<readonly ConversationView[]>;
  /** The conversation id of a group, as bytes: the key of every
   *  person-facing store. */
  conversationIdOf(groupId: Uint8Array): Uint8Array;
  /** The same, hex. */
  conversationHexOf(groupId: Uint8Array): string;
  /** The conversation a route id names: the id of the conversation itself
   *  or of ANY of its groups (a link minted before the move still opens the
   *  conversation). `undefined` when this device holds none of it. */
  resolve(idB64: string): ConversationView | undefined;
  /** The index of a group in its conversation's segments. */
  segmentIndexOf(groupId: Uint8Array): number;
}

export function useLineage(): LineageView {
  const sync = useDeviceGroupSync();
  const chats = useChats();
  const lineage = createMemo(() => buildLineage(sync.lineage()));

  const conversations = createMemo((): readonly ConversationView[] => {
    const l = lineage();
    const groups = chats.groups();
    const byHex = new Map(groups.map((g) => [groupIdHex(g.group.groupId), g]));
    const plans: ConversationPlan[] = planConversations(
      groups.map((g) => ({
        groupHex: groupIdHex(g.group.groupId),
        active: readIsActive(g.group) !== false,
      })),
      l,
    );
    const out: ConversationView[] = [];
    for (const plan of plans) {
      const head = byHex.get(plan.headHex);
      if (head === undefined) continue;
      const headAt = plan.heldSegments.indexOf(plan.headHex);
      const history = plan.heldSegments
        .slice(0, Math.max(0, headAt))
        .map((hex) => byHex.get(hex))
        .filter((g): g is ActiveGroup => g !== undefined);
      out.push({
        conversationHex: plan.conversationHex,
        conversationIdB64: base64Encode(hexToBytes(plan.conversationHex)),
        head,
        history,
        held: plan.heldSegments
          .map((hex) => byHex.get(hex))
          .filter((g): g is ActiveGroup => g !== undefined),
      });
    }
    return out;
  });

  return {
    lineage,
    conversations,
    conversationIdOf(groupId) {
      return hexToBytes(lineage().conversationOf(groupIdHex(groupId)));
    },
    conversationHexOf(groupId) {
      return lineage().conversationOf(groupIdHex(groupId));
    },
    resolve(idB64) {
      let hex: string;
      try {
        hex = lineage().conversationOf(groupIdHex(base64Decode(idB64)));
      } catch {
        return undefined;
      }
      return conversations().find((c) => c.conversationHex === hex);
    },
    segmentIndexOf(groupId) {
      return lineage().segmentIndexOf(groupIdHex(groupId));
    },
  };
}
