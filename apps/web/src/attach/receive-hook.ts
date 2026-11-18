// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 — the one seam the chat receive paths (`chat/client.ts` poll and
 * the ADR-0045 sweep verdict in `contacts/client.ts`) call for polls and
 * checklists. A replaceable object so a test can stub it; the default files
 * everything in `content-store.ts` through `receive.ts` and `handover.ts`.
 * Every method swallows its own failures: a poll that cannot be filed must
 * never stall the journal cursor.
 */
import { wasmSync } from "../wasm";
import type { IncomingChatEvent } from "../chat/client";

import {
  receiveBallotRow,
  receiveCommit,
  receiveContent,
  receiveContentOp,
  type ReceiveGroup,
} from "./receive";
import { receiveOpenItems } from "./handover";

type ApplicationEvent = Extract<IncomingChatEvent, { kind: "application" }>;
type ContentOpEvent = Extract<IncomingChatEvent, { kind: "contentOp" }>;
type OpenItemsEvent = Extract<IncomingChatEvent, { kind: "openItems" }>;

export interface AttachReceiveHook {
  /** Synchronous: is this journal row an anonymous ballot (not MLS)? */
  isBallotRow(payload: Uint8Array): boolean;
  ballotRow(group: ReceiveGroup, groupIdB64: string, messageId: number, createdAt: string, row: Uint8Array): Promise<void>;
  content(group: ReceiveGroup, groupIdB64: string, event: ApplicationEvent): Promise<void>;
  contentOp(group: ReceiveGroup, groupIdB64: string, event: ContentOpEvent): Promise<void>;
  /** Adopted items come back as timeline events (the newcomer never saw the
   *  poll's own row). */
  openItems(group: ReceiveGroup, groupIdB64: string, event: OpenItemsEvent): Promise<ApplicationEvent[]>;
  commit(group: ReceiveGroup, groupIdB64: string): Promise<void>;
}

/** The ballot row domain; a cheap prefix check before the core's. */
const BALLOT_PREFIX = new TextEncoder().encode("lakko/poll-ballot/v1\0");

function startsWith(bytes: Uint8Array, prefix: Uint8Array): boolean {
  if (bytes.length < prefix.length) return false;
  for (let i = 0; i < prefix.length; i++) if (bytes[i] !== prefix[i]) return false;
  return true;
}

const defaultHook: AttachReceiveHook = {
  isBallotRow(payload) {
    if (!startsWith(payload, BALLOT_PREFIX)) return false;
    // The prefix decides when the core is not loaded yet (a ballot row is
    // never MLS framing); otherwise the core's exact check.
    try {
      return wasmSync().isPollBallotRow(payload);
    } catch {
      return true;
    }
  },
  async ballotRow(group, groupIdB64, messageId, createdAt, row) {
    try {
      await receiveBallotRow(group.groupId, groupIdB64, messageId, createdAt, row);
    } catch (err) {
      console.warn("[attach] ballot row not filed", err instanceof Error ? err.name : "unknown");
    }
  },
  async content(group, groupIdB64, event) {
    if (event.content === undefined) return;
    try {
      await receiveContent(group, groupIdB64, event.messageId, event.createdAt, event.senderKey, event.content);
    } catch (err) {
      console.warn("[attach] content not filed", err instanceof Error ? err.name : "unknown");
    }
  },
  async contentOp(group, groupIdB64, event) {
    try {
      await receiveContentOp(group, groupIdB64, event.messageId, event.createdAt, event.senderKey, event.op);
    } catch (err) {
      console.warn("[attach] op not filed", err instanceof Error ? err.name : "unknown");
    }
  },
  async openItems(group, groupIdB64, event) {
    try {
      return await receiveOpenItems(group, groupIdB64, event);
    } catch (err) {
      console.warn("[attach] hand-over not adopted", err instanceof Error ? err.name : "unknown");
      return [];
    }
  },
  async commit(group, groupIdB64) {
    try {
      await receiveCommit(group, groupIdB64, Date.now());
    } catch (err) {
      console.warn("[attach] ring not extended", err instanceof Error ? err.name : "unknown");
    }
  },
};

let hook: AttachReceiveHook = defaultHook;

export function attachReceiveHook(): AttachReceiveHook {
  return hook;
}

/** Tests only: replace the hook; returns the restore. */
export function setAttachReceiveHookForTest(next: AttachReceiveHook): () => void {
  const prev = hook;
  hook = next;
  return () => {
    hook = prev;
  };
}

