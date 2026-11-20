// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 4 / docs/ux/saved-messages-and-file-lifetime.md sections
 * 2 and 3 — saved messages: the register edits a Save becomes, the client
 * limits, and the rows of the Saved screen. Pure; the fold is the Rust
 * `SyncedState` (`lakko-sync`), carried over the DeviceGroup like folders.
 */

import type { AttachmentRefBytes } from "./attachments";
import { blobIdsOfRefs } from "./blob-ids";
import { historyRowId, historyRowOrigin } from "./lineage";
import type { ConversationRef, RegisterEdit } from "./folders/folder-model";
import type { SavedEntry } from "../devices/sync/synced-projection";

/** The core's client limits (`savedMaxClient` / `savedMaxPerChangeSet`),
 *  pinned by a test against the real core. */
export const SAVED_MAX_CLIENT = 2000;
export const SAVED_MAX_PER_CHANGE_SET = 32;

/** The kind token a saved row names its media by (`saved.kind.*`). */
export type SavedKind = "" | "photo" | "video" | "voice" | "video_note" | "gif" | "file";

/** The kind of a message's first attachment, or `""` for none. */
export function savedKindOf(attachments: readonly AttachmentRefBytes[]): SavedKind {
  const a = attachments[0];
  if (a === undefined) return "";
  if (a.kind === "voice") return "voice";
  if (a.kind === "video_note") return "video_note";
  if (a.kind === "gif") return "gif";
  const mime = (a.mime ?? "").toLowerCase();
  if (mime.startsWith("image/")) return "photo";
  if (mime.startsWith("video/")) return "video";
  return "file";
}

/** What a Save of one message needs from the message. */
export interface SaveSource {
  readonly ref: ConversationRef;
  readonly messageId: number;
  readonly author: string;
  readonly text: string;
  readonly attachments: readonly AttachmentRefBytes[];
  /** The message's own time, unix ms; `0` when not known. */
  readonly sentAtMs: number;
  /** ADR-0087 section 14 — what stands for the text of a channel invitation
   *  (`channelInviteRowPreview`); the text itself is the `/g/` link with its
   *  secret, which a saved snapshot never carries. */
  readonly invitationLine?: string | null;
}

/** The register edit a Save becomes. A file's name stands in for the text
 *  when there is none, so the row can say `File: name`. */
export function saveEdit(src: SaveSource, nowMs: number): RegisterEdit {
  const kind = savedKindOf(src.attachments);
  const first = src.attachments[0];
  const text = src.invitationLine != null && src.invitationLine.length > 0 ? src.invitationLine : src.text;
  const excerpt = text.length > 0 ? text : kind === "file" ? (first?.name ?? "") : "";
  return {
    t: "saved",
    ref: src.ref,
    messageId: BigInt(src.messageId),
    savedAtMs: BigInt(Math.floor(nowMs)),
    sentAtMs: BigInt(Number.isFinite(src.sentAtMs) && src.sentAtMs > 0 ? Math.floor(src.sentAtMs) : 0),
    author: src.author,
    excerpt,
    attachmentKind: kind,
    blobIdsHex: blobIdsOfRefs(src.attachments),
  };
}

export function unsaveEdit(ref: ConversationRef, messageId: number): RegisterEdit {
  return { t: "saved-removed", ref, messageId: BigInt(messageId) };
}

/** Why a save is refused before it is made. */
export type SaveRefusal = "limit" | null;

/** A save of `adding` more messages: refused when the account would hold
 *  more than {@link SAVED_MAX_CLIENT}. */
export function saveRefusal(heldCount: number, adding: number): SaveRefusal {
  return heldCount + adding > SAVED_MAX_CLIENT ? "limit" : null;
}

/** Split edits into change sets of at most {@link SAVED_MAX_PER_CHANGE_SET}
 *  (a live change set is one journal row). */
export function chunkSaveEdits(edits: readonly RegisterEdit[]): RegisterEdit[][] {
  const out: RegisterEdit[][] = [];
  for (let i = 0; i < edits.length; i += SAVED_MAX_PER_CHANGE_SET) {
    out.push(edits.slice(i, i + SAVED_MAX_PER_CHANGE_SET));
  }
  return out;
}

/** The key a saved message is known by in a set. */
export function savedEntryKey(refIdHex: string, messageId: number): string {
  return `${refIdHex}:${messageId}`;
}

function hex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** Every saved message as a set of `savedEntryKey`s (the mark, the menu). */
export function savedKeySet(entries: readonly SavedEntry[]): ReadonlySet<string> {
  return new Set(entries.map((e) => savedEntryKey(hex(e.ref.id), e.messageId)));
}

/** One row of the Saved screen. */
export interface SavedRow {
  readonly key: string;
  readonly ref: ConversationRef;
  readonly messageId: number;
  readonly savedAtMs: number;
  /** The snapshot's message time, `0` when unknown. */
  readonly sentAtMs: number;
  /** The chat's title as this device names it, or `null` when the chat is
   *  not held here (the row then reads from the snapshot alone). */
  readonly chatTitle: string | null;
  readonly author: string;
  readonly excerpt: string;
  readonly kind: SavedKind;
  /** The chat is held and the message is in its stored history: the row
   *  opens, and its thumbnail can be drawn. */
  readonly held: boolean;
}

/**
 * The Saved screen's rows, most recently saved first (ties: the newer
 * message first). `titleOf` names a held chat; `holds` says whether this
 * device holds the message.
 */
export function savedRows(
  entries: readonly SavedEntry[],
  titleOf: (ref: ConversationRef) => string | null,
  holds: (ref: ConversationRef, messageId: number) => boolean,
): SavedRow[] {
  const rows = entries.map((e): SavedRow => {
    const known: readonly SavedKind[] = ["photo", "video", "voice", "video_note", "gif", "file"];
    const kind = (known as readonly string[]).includes(e.attachmentKind) ? (e.attachmentKind as SavedKind) : "";
    const chatTitle = titleOf(e.ref);
    return {
      key: savedEntryKey(hex(e.ref.id), e.messageId),
      ref: e.ref,
      messageId: e.messageId,
      savedAtMs: e.savedAtMs,
      sentAtMs: e.sentAtMs,
      chatTitle,
      author: e.author,
      excerpt: e.excerpt,
      kind,
      held: chatTitle !== null && holds(e.ref, e.messageId),
    };
  });
  rows.sort((a, b) => b.savedAtMs - a.savedAtMs || b.messageId - a.messageId);
  return rows;
}

/**
 * The date a row shows (UX section 3: the message date): the snapshot's
 * message time, else the held message's, else the save time.
 */
export function savedRowDateMs(row: SavedRow, heldCreatedAtMs: number | null): number {
  if (row.sentAtMs > 0) return row.sentAtMs;
  if (heldCreatedAtMs !== null && Number.isFinite(heldCreatedAtMs) && heldCreatedAtMs > 0) return heldCreatedAtMs;
  return row.savedAtMs;
}

/** The saved entries naming `messageIds` of the conversation `refIdHex`
 *  (a delete for everyone takes them out of Saved). */
export function savedEntriesOf(
  entries: readonly SavedEntry[],
  refIdHex: string,
  messageIds: readonly number[],
): SavedEntry[] {
  const wanted = new Set(messageIds.map((id) => savedEntryKey(refIdHex, id)));
  return entries.filter((e) => wanted.has(savedEntryKey(hex(e.ref.id), e.messageId)));
}

/**
 * The segment a timeline row lives in (the maintainer's rule of
 * 2026-10-27): a saved register names a message by the MLS group of the
 * SEGMENT that holds it and the DS id the DS assigned there, never by the
 * conversation root and never by the on-screen id an older segment's row is
 * drawn under (`lineage.ts` `historyRowId`). `historyGroupIds` is the older
 * segments, oldest first, as `historySegments` holds them. `null` for an id
 * no row can have.
 */
export function savedTargetOf(
  rowId: number,
  headGroupId: Uint8Array,
  historyGroupIds: readonly Uint8Array[],
): { readonly groupId: Uint8Array; readonly messageId: number } | null {
  if (!Number.isSafeInteger(rowId) || rowId === 0) return null;
  if (rowId > 0) return { groupId: headGroupId, messageId: rowId };
  const origin = historyRowOrigin(rowId);
  if (origin === null) return null;
  const segment = historyGroupIds[historyGroupIds.length - origin.distanceFromHead];
  return segment === undefined ? null : { groupId: segment, messageId: origin.messageId };
}

/**
 * Where a Saved row opens: the on-screen row id in the conversation whose
 * head is `headIdB64` (the segment's own DS id when it is the head, its
 * history id when it is an older segment), or `null` when the conversation
 * does not hold the segment.
 */
export function savedOpenRow(
  segmentIdB64: string,
  messageId: number,
  headIdB64: string,
  historyIdsB64: readonly string[],
): number | null {
  if (segmentIdB64 === headIdB64) return messageId;
  const index = historyIdsB64.indexOf(segmentIdB64);
  if (index === -1) return null;
  return historyRowId(messageId, historyIdsB64.length - index);
}

/** The catalog key (and arguments) of a row's media line, or `null` for a
 *  text message. */
export function savedKindLabel(kind: SavedKind, excerpt: string): readonly [string, Record<string, string>?] | null {
  switch (kind) {
    case "":
      return null;
    case "file":
      return ["saved.kind.file", { name: excerpt }];
    default:
      return [`saved.kind.${kind}`];
  }
}
