// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The timeline's row type and the four reads every renderer of a row needs:
 * its author identity, its DS message id, its DS `created_at` and its sender
 * key. Lifted out of `routes/Chat.tsx` so the bubble component
 * (`TimelineBubble.tsx`) and the layout computation (`timeline-layout.ts`)
 * can share it without importing the route.
 */
import { insertByMessageId } from "./gap-line";
import { historyRowId } from "./lineage";
import type { MemberIdentity } from "./author-identity";
import type { AttachmentRefBytes, ForwardRef, ReplyRef } from "./attachments";
import type { IncomingChatEvent } from "./client";
import type { ContentState, MessageContent } from "../../wasm/lakko_ffi_wasm";

/** A timeline entry — verified incoming events PLUS optimistic
 *  outbound bubbles that haven't been confirmed yet.
 *
 *  Phase 1.19c2: outgoing bubbles now carry an optional
 *  `attachments` field so the optimistic UI can show the chips
 *  immediately after Send (the bytes are already uploaded to
 *  storage by the time the bubble renders). */
export type TimelineRow =
  | { kind: "incoming"; event: IncomingChatEvent }
  | {
      kind: "outgoing";
      /** Stable client-side id for `<For>` keying — distinct
       *  from any DS-assigned id. */
      tempId: number;
      plaintext: string;
      attachments: AttachmentRefBytes[];
      /** `"failed"` is an outgoing message that did not reach the server:
       *  it stays in the timeline with `Not sent · Retry` and nothing
       *  reached the other side. Never persisted.
       *
       *  It began as a media-only state (ADR-0047) and now covers every
       *  outgoing row. A composer message used to be DELETED on failure -
       *  and since the composer had already been cleared, the words went
       *  with it. */
      ack: "pending" | "sent" | "failed";
      /** ADR-0047 — set on an outgoing media message: the key its local
       *  bytes are filed under in the media hub until the upload lands. */
      mediaLocalKey?: string;
      /** Filled when ack flips to "sent". */
      messageId?: number;
      createdAt?: string;
      /** ADR-0023 — the identity that authored this row: this
       *  device's own signature key at send time, or the key the
       *  message log persisted for a row restored on reload.
       *
       *  Stamped at send rather than read from `ownLeaf()` at gate
       *  time because a leaf is a position, not a person: any leaf,
       *  ours included, can be reassigned to a member added later.
       *  `undefined` (unresolvable) fails the gate closed. */
      authorKey?: MemberIdentity | undefined;
      /** ADR-0052 — the reply this row was sent as, so the sender's own bubble
       *  draws the quote it sent and Retry re-sends the same reference. */
      reply?: ReplyRef;
      /** ADR-0052 decision 2 — the forward attribution this row was sent
       *  with, so the sender's own bubble draws the line it sent. */
      forward?: ForwardRef;
      /** ADR-0087 section 14 — the channel invitation card this row was
       *  sent with (standard base64), so the sender's own bubble and Chats
       *  row read the invitation, never the link. */
      channelInviteB64?: string;
      /** ADR-0092 — a poll, checklist, location, contact card or article. */
      content?: MessageContent;
      /** ADR-0092 section 3.11 — a restored poll's or checklist's state. */
      contentState?: ContentState;
    };

/** Read the author IDENTITY a displayed row should be gated on:
 *  incoming application messages carry the author's signature key,
 *  resolved from the MLS sender leaf at receipt; outgoing rows carry
 *  the key stamped when we sent (or restored from the log).
 *  `undefined` ⇒ no author is known for this row (a control frame, a
 *  row stored before the identity was persisted) — its edit/delete
 *  gate fails closed.
 *
 *  Never a leaf index: OpenMLS hands a vacated leaf to the next
 *  member added, so a leaf read at gate time can name somebody who
 *  was not there when the message was written. */
export function rowAuthorIdentity(row: TimelineRow): MemberIdentity | undefined {
  if (row.kind === "outgoing") return row.authorKey;
  if (row.event.kind === "application") return row.event.senderKey;
  return undefined;
}

/** The DS `messageId` of a displayed row, or `undefined` when
 *  it doesn't have one yet (an un-acked outgoing bubble). Every
 *  edit/delete/react affordance is gated on this being present
 *  (the op references its target by id). */
export function rowMessageId(row: TimelineRow): number | undefined {
  if (row.kind === "outgoing") {
    return row.ack === "sent" ? row.messageId : undefined;
  }
  if (row.event.kind === "application") return row.event.messageId;
  return undefined;
}

/**
 * `/chat/:id` (from the Chats list) and `/groups/:id` (from the Groups list)
 * both render this; `backTo` is the list the conversation was opened from, so

/**
 * Which row this IS, as a string that outlives the row object.
 *
 * A row object is immutable and REPLACED on every state change: the ack flip,
 * a failed send, and an upload landing (which swaps the placeholder attachment
 * ref for the uploaded one and gives the row its DS id). So the object is not
 * the row's identity — `tempId` is, for an outgoing bubble, and the DS
 * `messageId` for an incoming one. Both are stable for as long as the row
 * exists, and every incoming event kind carries a `messageId`, so this is
 * total.
 *
 * Load-bearing for the timeline's reconciliation (`call-timeline.ts`): keyed on
 * the object, a replaced row looks like a NEW row to the list that draws it,
 * and the bubble — with whatever is playing inside it — is torn down and
 * rebuilt.
 *
 * The two prefixes are what keeps a `tempId` from ever colliding with a DS id.
 */
export function rowIdentity(row: TimelineRow): string {
  return row.kind === "outgoing" ? `out:${row.tempId}` : `in:${row.event.messageId}`;
}

/** When a timeline row was created: the DS row's `created_at`, or `undefined`
 *  for an outgoing bubble the DS has not acknowledged yet. Read-only. */
export function rowCreatedAt(row: TimelineRow): string | undefined {
  if (row.kind === "incoming") {
    return (row.event as { createdAt?: string }).createdAt;
  }
  return row.createdAt;
}

/** The sender key of an incoming row, for sender runs; `undefined` for an
 *  outgoing row or a frame with no resolved sender. */
export function rowSenderKey(row: TimelineRow): string | undefined {
  if (row.kind !== "incoming") return undefined;
  return (row.event as { senderKey?: string }).senderKey;
}

/** The DS id a row stands for, for every incoming kind (a commit row
 *  included), and for an outgoing row once the DS acknowledged it. */
function dsIdOf(row: TimelineRow): number | undefined {
  if (row.kind === "outgoing") return row.ack === "sent" ? row.messageId : undefined;
  return row.event.messageId;
}

/**
 * The timeline once the stored log of the conversation has been read
 * (`routes/Chat.tsx` `hydrateFromLog`). Every stored row the view does not
 * already hold goes in by DS id; a row the view holds (a newer copy of the
 * same message) is kept as it is. A row with no DS id yet stays where it is.
 *
 * The view used to seed from the log only when it held nothing yet, so a row
 * that reached it first (a poll answer, a flushed send, a gap row) hid the
 * whole stored history, and the view's next write then removed that history
 * from the log (found while tracing a one-to-one that lost the other
 * person's message after they deleted their account, 2026-11-20).
 */
export function seedTimelineFromLog(
  current: readonly TimelineRow[],
  stored: readonly TimelineRow[],
): TimelineRow[] {
  if (current.length === 0) return [...stored];
  return insertByMessageId(current, stored, dsIdOf);
}

/**
 * One DS id, one row. Rows that reached the list by two routes (a row this
 * device filed into another conversation's log and then met again on that
 * conversation's mount, a persisted row and its live twin) collapse to the
 * FIRST occurrence; a row with no DS id yet (an unsent outgoing bubble) is
 * always kept. Order is preserved. Found live on 2026-09-26: a message
 * forwarded into another chat was drawn there three and four times over.
 */
export function dedupByMessageId(rows: readonly TimelineRow[]): TimelineRow[] {
  const seen = new Set<number>();
  const out: TimelineRow[] = [];
  for (const row of rows) {
    const id = rowMessageId(row);
    if (id !== undefined) {
      if (seen.has(id)) continue;
      seen.add(id);
    }
    out.push(row);
  }
  return out;
}

/**
 * ADR-0073 R.5 - a stored row of an OLDER segment of the conversation, as
 * the open timeline shows it: the same row under its on-screen history id
 * (`lineage.ts` `historyRowId`), with a reply's target moved the same way so
 * a quote still jumps to the message it answered. A reply target is a
 * message of the SAME segment (ADR-0052: one level, one journal).
 *
 * The result is display-only: it is never persisted and never the target of
 * an edit, a deletion, a reaction, a reply or a pin.
 */
export function asHistoryRow(row: TimelineRow, distanceFromHead: number): TimelineRow {
  const shift = (id: number): number => historyRowId(id, distanceFromHead);
  if (row.kind === "outgoing") {
    return {
      ...row,
      ...(row.messageId === undefined ? {} : { messageId: shift(row.messageId) }),
      ...(row.reply === undefined ? {} : { reply: { ...row.reply, target: shift(row.reply.target) } }),
    };
  }
  const event = row.event;
  if (event.kind === "application") {
    return {
      kind: "incoming",
      event: {
        ...event,
        messageId: shift(event.messageId),
        ...(event.reply === undefined
          ? {}
          : { reply: { ...event.reply, target: shift(event.reply.target) } }),
      },
    };
  }
  return { kind: "incoming", event: { ...event, messageId: shift(event.messageId) } };
}
