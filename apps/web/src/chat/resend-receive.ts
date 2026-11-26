// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - the receive side of the re-send marker: gathers the
 * facts `resend-marker.ts` `resendVerdict` reads, for one batch of one
 * group, and answers "is this message a re-send whose original this device
 * already holds".
 *
 * One function for both receivers (the background receive and the open
 * chat's own poll), so the two cannot disagree on what a marker matches. The
 * message log is read at most once per batch, and only for a batch that
 * carries a marker at all.
 */
import { base64Decode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import type { IncomingChatEvent } from "./client";
import type { MessageLogPayload } from "./message-log-store";
import { heldRowAuthor, resendVerdict } from "./resend-marker";

type ApplicationEvent = Extract<IncomingChatEvent, { kind: "application" }>;

export interface ResendReceivePort {
  /** The group the batch was received in. */
  readonly receivedGroupId: Uint8Array;
  /** The lineage: a group's conversation (hex) and its segment index. */
  conversationHexOf(groupId: Uint8Array): string;
  segmentIndexOf(groupId: Uint8Array): number;
  /** This account's person key, standard base64, or `null`. */
  readonly ownPersonKeyB64: string | null;
  /** A key through its aliases (a proven rotation names the same person). */
  canonical(keyB64: string): string;
  /** The stored message log, or `null`. */
  loadLog(): Promise<MessageLogPayload | null>;
}

const NEVER = (): boolean => false;

/**
 * The predicate for one batch. Never throws: a batch whose facts cannot be
 * read gets "nothing is held", so every message renders (the marker is
 * ignored, which is its fail-safe direction).
 */
export async function heldResendPredicate(
  events: readonly IncomingChatEvent[],
  port: ResendReceivePort,
): Promise<(e: ApplicationEvent) => boolean> {
  if (!events.some((e) => e.kind === "application" && e.resendOrigin !== undefined)) return NEVER;
  let log: MessageLogPayload | null;
  let receivedConversation: string;
  let receivedIndex: number;
  try {
    log = await port.loadLog();
    receivedConversation = port.conversationHexOf(port.receivedGroupId);
    receivedIndex = port.segmentIndexOf(port.receivedGroupId);
  } catch {
    return NEVER;
  }
  if (log === null) return NEVER;
  const held = log;
  return (e) => {
    const marker = e.resendOrigin;
    if (marker === undefined) return false;
    try {
      const originId = base64Decode(marker.groupIdB64);
      const originHex = groupIdHex(originId);
      const slot = held.groups[originHex];
      const row = slot?.messages.find((m) => Number(m.messageId) === marker.messageId);
      // ADR-0101 walk 2026-11-26 (D3): the returned history's copy, filed
      // into the receiving group under the original id.
      const receivedSlot = held.groups[groupIdHex(port.receivedGroupId)];
      const copy =
        originHex === groupIdHex(port.receivedGroupId)
          ? undefined
          : receivedSlot?.messages.find((m) => Number(m.messageId) === marker.messageId);
      return (
        resendVerdict({
          marker,
          receivedConversation,
          // A group this device holds no stored slot for is not a segment it
          // can hold the original in.
          originConversation: slot === undefined ? null : port.conversationHexOf(originId),
          originIsOlderSegment: port.segmentIndexOf(originId) < receivedIndex,
          heldAuthor: heldRowAuthor(row, port.ownPersonKeyB64, (k) => port.canonical(k)),
          senderAuthor: e.senderKey === undefined ? null : port.canonical(e.senderKey),
          heldInReceivedAuthor: heldRowAuthor(copy, port.ownPersonKeyB64, (k) => port.canonical(k)),
        }) === "skip"
      );
    } catch {
      return false;
    }
  };
}
