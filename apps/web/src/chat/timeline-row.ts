// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The timeline's row type and the four reads every renderer of a row needs:
 * its author identity, its DS message id, its DS `created_at` and its sender
 * key. Lifted out of `routes/Chat.tsx` so the bubble component
 * (`TimelineBubble.tsx`) and the layout computation (`timeline-layout.ts`)
 * can share it without importing the route.
 */
import type { MemberIdentity } from "./author-identity";
import type { AttachmentRefBytes } from "./attachments";
import type { IncomingChatEvent } from "./client";

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
      /** ADR-0047 — `"failed"` is an outgoing media message whose upload or
       *  publish failed: it stays in the timeline with `Not sent · Retry`
       *  and nothing reached the other side. Never persisted. */
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
