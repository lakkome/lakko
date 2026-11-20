// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - the ONE queue of held messages for the tab, and its
 * mirror in the message log.
 *
 * `held-send.ts` holds the rules and the in-memory store type. This module
 * is the instance every sender shares (the chat view, the forward picker,
 * the feed, the background flush) and the write-through that keeps a held
 * message across a reload: the list is stored in the message log's `held`
 * field, the one store that already keeps message plaintext under the
 * device key. Nothing here is ever sent in its stored form.
 *
 * What a reload still loses: nothing of a held message itself (its text,
 * its uploaded attachment references, its reply and forward attributions
 * are all restored). Its give-up clock restarts with the page, and a
 * message that had already shown the failed state (which takes it out of
 * the queue; its Retry lives in the row) is not kept. A media message whose
 * bytes were still UPLOADING when the page went is lost, exactly as it is
 * in a conversation that is not held: there is no attachment reference to
 * keep yet.
 */
import { registerAccountMemory } from "../auth/account-memory";
import { getDeviceKey } from "../auth/key-store";
import {
  composeContentEnvelope,
  composeEnvelope,
  composeForwardEnvelope,
  composeReplyEnvelope,
  contentPartsOfForward,
  type AttachmentRefBytes,
  type ForwardRef,
  type ReplyRef,
} from "./attachments";
import { forwardRefFromPersisted, persistedFromForwardRef } from "./forward";
import { HeldSendStore, type HeldSend } from "./held-send";
import {
  loadMessageLog,
  setHeldInMessageLog,
  type PersistedHeldSend,
  type PersistedMessage,
} from "./message-log-store";
import { attachmentRefFromPersisted, persistedFromAttachmentRef } from "./persisted-attachment";
import { persistedFromReplyRef, replyRefFromPersisted } from "./reply-snapshot";
import { contentFromPersisted } from "../attach/preview";
import type { MessageContent } from "../../wasm/lakko_ffi_wasm";

/** One held message as a sender hands it over. */
export interface HeldMessage {
  readonly text: string;
  readonly attached: AttachmentRefBytes[];
  readonly reply: ReplyRef | undefined;
  /** The group (standard base64) `reply.target` is a message id of: the
   *  group the reply was composed in. A flush into any other group keeps the
   *  quote and drops the jump target (`held-send.ts` `replyForSend`). */
  readonly replyGroupIdB64?: string | undefined;
  /** ADR-0052: set for a forward INTO the held conversation. */
  readonly forward?: ForwardRef | undefined;
  /** ADR-0092: a location, contact card, article, poll or checklist; its
   *  `text` is empty and `attached` are an article's images. */
  readonly content?: MessageContent | undefined;
}

/** The tab's queue, by conversation id (hex of the lineage root). */
export const heldSends = new HeldSendStore<HeldMessage>();

/**
 * The envelope a released held message is sent as: the content message for
 * content (with its forward line or reply), else the forward, the reply or
 * the plain message. `reply` is the one the flush resolved
 * (`held-send.ts` `replyForSend`).
 */
export async function composeHeldEnvelope(m: HeldMessage, reply: ReplyRef | undefined): Promise<Uint8Array> {
  if (m.content !== undefined) {
    return composeContentEnvelope(
      m.content,
      m.attached,
      m.forward !== undefined
        ? contentPartsOfForward(m.forward)
        : reply === undefined
          ? undefined
          : { replyTo: reply.target, replyAuthor: reply.author ?? null, replyExcerpt: reply.excerpt ?? null },
    );
  }
  if (m.forward !== undefined) return composeForwardEnvelope(m.text, m.attached, m.forward);
  return reply === undefined ? composeEnvelope(m.text, m.attached) : composeReplyEnvelope(m.text, m.attached, reply);
}

/** The stored form of one held message. Pure. */
export function heldToPersisted(conversationHex: string, held: HeldSend<HeldMessage>): PersistedHeldSend {
  const m = held.payload;
  return {
    conversationHex,
    heldAtMs: held.heldAtMs,
    text: m.text,
    attachments: m.attached.map((a) => persistedFromAttachmentRef(a)),
    ...(m.reply === undefined ? {} : { reply: persistedFromReplyRef(m.reply) }),
    ...(m.reply === undefined || m.replyGroupIdB64 === undefined
      ? {}
      : { replyGroupIdB64: m.replyGroupIdB64 }),
    ...(m.forward === undefined ? {} : { forward: persistedFromForwardRef(m.forward) }),
    ...(m.content === undefined ? {} : { content: m.content }),
  };
}

/**
 * The held message a stored row reads back as, or `null` for a row that is
 * not one (a store is not a wire: junk is dropped, never sent). Pure.
 */
export function heldFromPersisted(
  row: unknown,
): { readonly conversationHex: string; readonly held: HeldSend<HeldMessage> } | null {
  if (typeof row !== "object" || row === null) return null;
  const r = row as Partial<PersistedHeldSend>;
  if (typeof r.conversationHex !== "string" || !/^[0-9a-f]+$/.test(r.conversationHex)) return null;
  if (typeof r.heldAtMs !== "number" || !Number.isFinite(r.heldAtMs)) return null;
  if (typeof r.text !== "string" || !Array.isArray(r.attachments)) return null;
  let attached: AttachmentRefBytes[];
  try {
    attached = r.attachments.map((a) => attachmentRefFromPersisted(a).ref);
  } catch {
    return null;
  }
  const content = contentFromPersisted(r.content);
  if (r.text.length === 0 && attached.length === 0 && content === undefined) return null;
  const reply = replyRefFromPersisted(r.reply);
  const forward = forwardRefFromPersisted(r.forward);
  return {
    conversationHex: r.conversationHex,
    held: {
      heldAtMs: r.heldAtMs,
      payload: {
        text: r.text,
        attached,
        reply,
        ...(reply !== undefined && typeof r.replyGroupIdB64 === "string" && r.replyGroupIdB64.length > 0
          ? { replyGroupIdB64: r.replyGroupIdB64 }
          : {}),
        ...(forward === undefined ? {} : { forward }),
        ...(content === undefined ? {} : { content }),
      },
    },
  };
}

/** Everything the queue holds, as stored rows, in send order. */
function snapshot(): PersistedHeldSend[] {
  const out: PersistedHeldSend[] = [];
  for (const [conversationHex, items] of heldSends.all()) {
    for (const item of items) out.push(heldToPersisted(conversationHex, item));
  }
  return out;
}

/**
 * Mirror the queue into the message log. Called after every change of the
 * queue; best-effort (a failed write costs the reload-survival of what
 * changed, never the message while the page lives). Never throws.
 */
export async function persistHeldSends(accountIdB64: string | null): Promise<void> {
  if (accountIdB64 === null) return;
  try {
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) return;
    await setHeldInMessageLog(accountIdB64, deviceKey, snapshot());
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[held-send] the held messages were not stored", err instanceof Error ? err.name : "");
  }
}

let restoredFor: string | null = null;

/**
 * Read the held messages back after a reload: once per signed-in account,
 * and only into an EMPTY queue (a queue that already holds something is
 * this page's own, newer than the disk). Never throws.
 */
export async function restoreHeldSends(accountIdB64: string, nowMs: number = Date.now()): Promise<void> {
  if (restoredFor === accountIdB64) return;
  restoredFor = accountIdB64;
  try {
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) return;
    const log = await loadMessageLog(accountIdB64, deviceKey);
    const stored = log?.held ?? [];
    if (stored.length === 0 || heldSends.all().length > 0) return;
    const entries: (readonly [string, HeldSend<HeldMessage>])[] = [];
    for (const row of stored) {
      const read = heldFromPersisted(row);
      // The give-up clock restarts with the page: a message read back is in
      // the ordinary sending state again, not failed on arrival.
      if (read !== null) {
        entries.push([read.conversationHex, { ...read.held, heldAtMs: nowMs }]);
      }
    }
    heldSends.replaceAll(entries);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[held-send] the held messages were not read back", err instanceof Error ? err.name : "");
  }
}

/** Sign-out / account switch: nothing held survives into the next account
 *  (the log row is the account's own and stays with it). */
export function resetHeldSends(): void {
  heldSends.clear();
  restoredFor = null;
}

// Forget device: the queue is mirrored into the message log after every
// change, so a message still held for the wiped account (or added by a send
// that was in flight across the wipe) would be written back into that
// account's new log once it signs in again on this page. The queue holds the
// session's messages only; `restoredFor` names the account when it is known.
registerAccountMemory("held-sends", (ids) => {
  if (restoredFor === null || ids.includes(restoredFor)) resetHeldSends();
});

// ---------------------------------------------------------------------------
// What the background flush sent
// ---------------------------------------------------------------------------

/** One background flush of one conversation: the group the messages went
 *  into and the stored rows they became, in send order. */
export interface HeldFlushReport {
  readonly groupIdB64: string;
  readonly messages: readonly PersistedMessage[];
}

const flushListeners = new Set<(report: HeldFlushReport) => void>();

/**
 * Hear every background flush. A chat view subscribes BEFORE it reads the
 * log, so a flush that raced its mount is either in what it read (the flush
 * reports after its log write) or reported to it here; `held-send.ts`
 * `flushedRowsToShow` removes the overlap.
 */
export function subscribeHeldFlushed(listener: (report: HeldFlushReport) => void): () => void {
  flushListeners.add(listener);
  return () => {
    flushListeners.delete(listener);
  };
}

/** Report one flush. Called by the background flush after its log write. */
export function notifyHeldFlushed(report: HeldFlushReport): void {
  if (report.messages.length === 0) return;
  for (const listener of [...flushListeners]) {
    try {
      listener(report);
    } catch {
      /* a view's failure is its own */
    }
  }
}
