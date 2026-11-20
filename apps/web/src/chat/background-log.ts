// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The persisted message log, written by more than one path.
 *
 * The log (`message-log-store.ts`) is one encrypted blob per account holding
 * every group's timeline and journal cursor. Until the background receive
 * existed only the open chat view wrote it, and it wrote its group's slot by
 * REPLACING it with the rows it held. Two writers make that unsafe:
 *
 *   - the background receive appends rows for a chat that is not open, and a
 *     chat view that was open a moment earlier can still land its final
 *     debounced write afterwards — replacing the slot would erase the rows the
 *     background just wrote, and MLS forward secrecy means they can never be
 *     decrypted again;
 *   - both writers read-modify-write the same blob, so they are serialized per
 *     account (`updateMessageLog`) and each merges into what it read instead of
 *     assuming it is the only writer.
 *
 * The two merges differ on purpose. The chat view's rows are authoritative for
 * everything up to the newest message it holds, so its merge keeps only the
 * stored messages NEWER than that (it may have dropped older ones on purpose);
 * the background only ever adds, so its merge is a de-duplicated union. Both
 * keep the cursor at the maximum, never moving it backward.
 *
 * Also here: the chat-list summary (unread count + last-message preview) read
 * from a slot, and the conversion from a received event to a persisted row.
 */
import { base64Encode } from "../auth/account-id";
import { contentPreviewOf } from "../attach/preview";
import { authorIdentityToPersisted } from "./author-identity";
import type { IncomingChatEvent } from "./client";
import { messagePreviewWords, previewMediaOf, type PreviewMedia } from "./list-preview";
import type { PersistedGroup, PersistedMessage } from "./message-log-store";
import { persistedFromAttachmentRef } from "./persisted-attachment";
import { persistedFromMembership } from "./membership-rows";
import { persistedFromForwardRef } from "./forward";
import { persistedFromReplyRef } from "./reply-snapshot";
import { t } from "../l10n";

/** Numeric DS id of a persisted row, or `null` when it does not parse. */
function idOf(m: PersistedMessage): bigint | null {
  try {
    return BigInt(m.messageId);
  } catch {
    return null;
  }
}

function compareById(a: PersistedMessage, b: PersistedMessage): number {
  const x = idOf(a);
  const y = idOf(b);
  if (x === null || y === null) return 0;
  return x < y ? -1 : x > y ? 1 : 0;
}

function maxCursor(a: string | undefined, b: number): string {
  const prev = a === undefined ? Number.NaN : Number(a);
  if (!Number.isFinite(prev)) return String(b);
  return String(Math.max(prev, b));
}

/**
 * The background receive's write: add `messages` the slot does not already
 * hold (by DS id), keep the slot sorted by id, and move the cursor to
 * `max(stored, cursor)`. Everything else in the slot is preserved.
 */
export function appendToGroupSlot(
  slot: PersistedGroup | undefined,
  messages: readonly PersistedMessage[],
  cursor: number,
): PersistedGroup {
  const existing = slot?.messages ?? [];
  const have = new Set(existing.map((m) => m.messageId));
  const added = messages.filter((m) => {
    if (have.has(m.messageId)) return false;
    have.add(m.messageId);
    return true;
  });
  const merged =
    added.length === 0 ? [...existing] : [...existing, ...added].sort(compareById);
  return {
    ...(slot ?? {}),
    cursor: maxCursor(slot?.cursor, cursor),
    messages: merged,
  };
}

/**
 * The chat view's write: its own rows, plus every stored row it does not hold
 * (the background receive wrote it after the view last looked, a history
 * bundle or the gap filed it below, or the view never read it). A row the
 * view holds replaces the stored row of the same DS id: the view's copy
 * carries the latest edit, deletion and reactions. The view never removes a
 * row (a deletion is an op on the row), so a stored row it lacks is one it
 * did not load, never one it dropped; until 2026-11-20 every such row at or
 * below the view's newest was erased here, which a view that missed the
 * stored history turned into lost messages. The cursor is the maximum of
 * both; `lastViewedMessageId` — everything the open view holds counts as
 * seen — only ever moves forward. The history floor is not the view's to
 * change.
 */
export function mergeForegroundSlot(
  slot: PersistedGroup | undefined,
  foreground: { readonly messages: readonly PersistedMessage[]; readonly cursor: number },
): PersistedGroup {
  let newest: bigint | null = null;
  for (const m of foreground.messages) {
    const id = idOf(m);
    if (id !== null && (newest === null || id > newest)) newest = id;
  }
  const held = new Set(foreground.messages.map((m) => m.messageId));
  const storedOnly = (slot?.messages ?? []).filter((m) => !held.has(m.messageId));
  const allNewer = storedOnly.every((m) => {
    const id = idOf(m);
    return id !== null && (newest === null || id > newest);
  });
  const lastViewed = maxBigintString(slot?.lastViewedMessageId, newest);
  // The feed's seen ids above the new watermark survive the view's write;
  // the ones it now covers are dropped (ADR-0063 section 5).
  const through = lastViewed === undefined ? null : BigInt(lastViewed);
  const feedSeen = (slot?.feedSeen ?? []).filter((id) => {
    try {
      return through === null || BigInt(id) > through;
    } catch {
      return false;
    }
  });
  return {
    cursor: maxCursor(slot?.cursor, foreground.cursor),
    messages: allNewer
      ? [...foreground.messages, ...storedOnly.sort(compareById)]
      : [...foreground.messages, ...storedOnly].sort(compareById),
    ...(lastViewed === undefined ? {} : { lastViewedMessageId: lastViewed }),
    ...(feedSeen.length === 0 ? {} : { feedSeen }),
    ...(slot?.syncedThroughMessageId === undefined
      ? {}
      : { syncedThroughMessageId: slot.syncedThroughMessageId }),
  };
}

/**
 * A pairing history bundle's write for one group: add the rows the slot does
 * not hold (the stored row wins a shared id), keep the slot sorted by id, and
 * raise the history floor to `bundleNewest`, the highest id in the bundle for
 * this group, kept rows or not. Everything else in the slot is preserved: the
 * bundle knows nothing of what this device has shown.
 *
 * A bundle can arrive after the same messages came over the journal, so the
 * floor covers rows already stored as well as the ones added here. A message
 * above it counts normally (docs/ux/second-device.md section 2).
 */
export function fileHistoryRows(
  slot: PersistedGroup | undefined,
  rows: readonly PersistedMessage[],
  bundleNewest: bigint | null,
): { readonly slot: PersistedGroup; readonly added: number } {
  const existing = slot?.messages ?? [];
  const have = new Set(existing.map((m) => m.messageId));
  const merged = [...existing];
  let added = 0;
  for (const m of rows) {
    if (have.has(m.messageId)) continue;
    have.add(m.messageId);
    merged.push(m);
    added += 1;
  }
  merged.sort(compareById);
  const floor = maxBigintString(slot?.syncedThroughMessageId, bundleNewest);
  return {
    slot: {
      ...(slot ?? {}),
      cursor: slot?.cursor ?? "0",
      messages: merged,
      ...(floor === undefined ? {} : { syncedThroughMessageId: floor }),
    },
    added,
  };
}

/**
 * A CLOSED chat view's write: the rows an abandoned request changed after the
 * view went away, and nothing else.
 *
 * A send, an edit or a poll batch that was in flight when the person left the
 * chat still completes — nothing cancels it — and what it produced exists
 * nowhere but in the closed view's memory: the delivery service never hands a
 * device its own message back, and MLS forward secrecy means a consumed row
 * cannot be decrypted a second time. So it has to reach the log.
 *
 * Neither of the other two merges fits. The view's own merge treats its rows
 * as authoritative for everything up to the newest one it holds, which is no
 * longer true: the background receive took the group over the moment the view
 * closed and may have filed rows the view never saw, below the id of a send
 * that lands later. And the background's merge only ever adds, so an edit that
 * completed after the close would not replace the stored row.
 *
 * So: each row in `changed` replaces the stored row of the same DS id or is
 * added; every other stored row is kept; the cursor only moves forward. And
 * `lastViewedMessageId` is left alone — nobody is looking at this chat, so a
 * message that arrived after the person left stays unread.
 */
export function mergeClosedViewSlot(
  slot: PersistedGroup | undefined,
  changed: readonly PersistedMessage[],
  cursor: number,
): PersistedGroup {
  const byId = new Map<string, PersistedMessage>();
  for (const m of changed) byId.set(m.messageId, m);
  const stored = new Set<string>();
  const kept = (slot?.messages ?? []).map((m) => {
    stored.add(m.messageId);
    return byId.get(m.messageId) ?? m;
  });
  const added = [...byId.values()].filter((m) => !stored.has(m.messageId));
  return {
    ...(slot ?? {}),
    cursor: maxCursor(slot?.cursor, cursor),
    messages: added.length === 0 ? kept : [...kept, ...added].sort(compareById),
  };
}

/**
 * What a closed chat view has already handed to the log, so that its later
 * writes carry only what changed since ({@link mergeClosedViewSlot}).
 *
 * Built from the rows the view held when it closed (its last ordinary write
 * covers those). `changed` compares by content, not identity: the view
 * re-derives its persisted rows on every write.
 */
export class ClosedViewBaseline {
  private readonly handed = new Map<string, string>();
  private cursor: number;

  constructor(messages: readonly PersistedMessage[], cursor: number) {
    for (const m of messages) this.handed.set(m.messageId, JSON.stringify(m));
    this.cursor = cursor;
  }

  /** The rows of `messages` that are new or differ from what was handed. */
  changed(messages: readonly PersistedMessage[]): PersistedMessage[] {
    return messages.filter((m) => this.handed.get(m.messageId) !== JSON.stringify(m));
  }

  /** Whether `cursor` is past the position already handed. */
  cursorAdvanced(cursor: number): boolean {
    return cursor > this.cursor;
  }

  /** Record a write that reached the log. */
  accept(written: readonly PersistedMessage[], cursor: number): void {
    for (const m of written) this.handed.set(m.messageId, JSON.stringify(m));
    this.cursor = Math.max(this.cursor, cursor);
  }
}

function maxBigintString(stored: string | undefined, next: bigint | null): string | undefined {
  let prev: bigint | null = null;
  if (stored !== undefined) {
    try {
      prev = BigInt(stored);
    } catch {
      prev = null;
    }
  }
  if (next === null) return prev === null ? undefined : String(prev);
  if (prev === null || next > prev) return String(next);
  return String(prev);
}

/** Longest preview line the chat list shows. */
export const PREVIEW_MAX_CHARS = 80;

export interface ChatListSummary {
  /** Incoming rows newer than what this account has read. Capped at 100 (the
   *  list renders "99+"). */
  readonly unread: number;
  /** The newest message, flattened to one line; `null` for an empty slot. */
  readonly preview: string | null;
  /** Whether the newest message is this device's own. */
  readonly previewIsOwn: boolean;
  /** The newest message's DS `created_at`, for the row's time; `null` for an
   *  empty slot. */
  readonly lastAt: string | null;
  /** The newest message's author key (base64) when it is someone else's and
   *  the log recorded one, for the sender prefix in a group row; else `null`. */
  readonly lastAuthorKeyB64: string | null;
  /** ADR-0087 section 14 — the newest message's channel invitation card
   *  (base64), when it carried one; the row reads the invitation from it. */
  readonly previewChannelInviteB64?: string;
  /** `docs/ux/media-messages.md` section 11 — the newest message's first
   *  photo or video, for the row's thumbnail; absent when it has none. */
  readonly previewMedia?: PreviewMedia;
}

/**
 * Unread count + preview for one chat-list row.
 *
 * Read position = the later of the cross-device read cursor (DeviceGroup sync,
 * when it has arrived) and the newest row this device's own chat view has held
 * (`lastViewedMessageId`). Before `lastViewedMessageId` existed a chat with no
 * synced read cursor counted as fully read, so a chat that had never been
 * opened showed no badge however many messages the background receive filed
 * into it. With neither position known, every incoming row is unread — which
 * is exactly true for a chat this device has never displayed.
 *
 * A row at or below the history floor (`syncedThroughMessageId`) is never
 * unread, whatever those positions say: it came with the pairing history, and
 * history is not news (docs/ux/second-device.md section 2).
 */
export function summarizeGroupSlot(
  slot: PersistedGroup | undefined,
  syncedReadUpTo: bigint | null,
  ownPersonKeyB64: string | null = null,
  /** ADR-0099 section 6 - a blocked author's row (kept, collapsed, in a
   *  group) is never unread, so it raises no badge and no notification, and
   *  the preview reads as the collapsed row. Absent: nobody is blocked. */
  isBlockedAuthor?: (keyB64: string) => boolean,
): ChatListSummary {
  // A message this account sent from another of its devices arrives as an
  // incoming row; the timeline draws it as the person's own by its author
  // key, and so does the list: "You: ..." and never unread (base scenario
  // 33, 2026-11-11: the list read "Ada Two: ..." for the person's own line).
  const fromSelf = (m: PersistedMessage): boolean =>
    m.direction === "outgoing" ||
    (ownPersonKeyB64 !== null && m.direction === "incoming" && m.authorKeyB64 === ownPersonKeyB64);
  const blockedRow = (m: PersistedMessage): boolean =>
    isBlockedAuthor !== undefined &&
    m.direction === "incoming" &&
    typeof m.authorKeyB64 === "string" &&
    isBlockedAuthor(m.authorKeyB64);
  const messages = slot?.messages ?? [];
  let readUpTo: bigint | null = syncedReadUpTo;
  if (slot?.lastViewedMessageId !== undefined) {
    try {
      const v = BigInt(slot.lastViewedMessageId);
      if (readUpTo === null || v > readUpTo) readUpTo = v;
    } catch {
      // Unparseable: ignore, keep the synced position.
    }
  }
  // ADR-0063 section 5 - a message the feed showed is not unread.
  const feedSeen = new Set(slot?.feedSeen ?? []);
  const floor = maxBigintString(slot?.syncedThroughMessageId, null);
  const historyThrough = floor === undefined ? null : BigInt(floor);
  let unread = 0;
  for (const m of messages) {
    if (m.direction !== "incoming" || fromSelf(m)) continue;
    if (blockedRow(m)) continue;
    const id = idOf(m);
    if (id === null) continue;
    if (historyThrough !== null && id <= historyThrough) continue;
    if ((readUpTo === null || id > readUpTo) && !feedSeen.has(m.messageId)) {
      unread += 1;
      if (unread >= 100) break;
    }
  }
  let last: PersistedMessage | undefined;
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i] as PersistedMessage;
    if (m.direction !== "system") {
      last = m;
      break;
    }
  }
  return {
    unread,
    preview: last === undefined ? null : blockedRow(last) ? t("block.group_row") : previewText(last),
    previewIsOwn: last !== undefined && fromSelf(last),
    lastAt: last?.createdAt ?? null,
    lastAuthorKeyB64:
      last !== undefined && !fromSelf(last) && last.direction === "incoming" && typeof last.authorKeyB64 === "string"
        ? last.authorKeyB64
        : null,
    ...(last !== undefined && last.deleted !== true && !blockedRow(last) && typeof last.channelInviteB64 === "string"
      ? { previewChannelInviteB64: last.channelInviteB64 }
      : {}),
    ...(() => {
      if (last === undefined || last.deleted === true || blockedRow(last) || contentPreviewOf(last.content) !== null) return {};
      const media = previewMediaOf(last.attachments);
      return media === null ? {} : { previewMedia: media };
    })(),
  };
}

function previewText(m: PersistedMessage): string {
  // ADR-0023 — a deleted message reads as the tombstone, the same words the
  // bubble shows and the same rule Android's `previewLine` applies. Checked
  // FIRST: the row is stored with no text and no attachments, so without this
  // the conversation would show a blank second line rather than saying what
  // happened - and before the delete was persisted at all it showed the
  // deleted text, which is worse than blank.
  if (m.deleted === true) return t("chat.message.deleted");
  // ADR-0061 section 6 — a message from a newer Lakko: the catalog's words,
  // never the core's English placeholder.
  if (typeof m.unsupportedVersion === "number") return t("chat.message.unsupported");
  // ADR-0092 — a poll, checklist, location, contact card or article.
  const contentLine = contentPreviewOf(m.content);
  if (contentLine !== null) return contentLine;
  // docs/ux/media-messages.md section 11 — the text, after the count words
  // when the message carries files only; with no text, the words alone.
  const line = messagePreviewWords(m.text.replace(/\s+/g, " ").trim(), m.attachments);
  return line.length > PREVIEW_MAX_CHARS ? `${line.slice(0, PREVIEW_MAX_CHARS - 1)}…` : line;
}

/**
 * The event kinds the background receive files into the log as timeline rows.
 * Every other kind is either settled where it is received (profile, push
 * routing, beacons, proposals, call signaling) or handed to the chat view.
 */
export function isTimelineEvent(
  e: IncomingChatEvent,
): e is Extract<IncomingChatEvent, { kind: "application" | "system" }> {
  return e.kind === "application" || e.kind === "system";
}

/**
 * A received timeline event as the log stores it — the same shape the chat
 * view's own persist writes for an incoming row (`timelineRowToPersisted` in
 * `routes/Chat.tsx`, which delegates here), so a row filed by the background
 * receive reads back identically when the chat is opened.
 */
export function eventToPersistedMessage(
  e: Extract<IncomingChatEvent, { kind: "application" | "system" }>,
  played: boolean = false,
): PersistedMessage {
  if (e.kind === "application") {
    return {
      messageId: String(e.messageId),
      direction: "incoming",
      text: e.text,
      attachments: e.attachments.map((a) => persistedFromAttachmentRef(a, played)),
      createdAt: e.createdAt,
      // ADR-0023 / C-68 — the author's signature key resolved at receipt, never
      // the leaf, which names somebody else after the next commit.
      authorKeyB64: authorIdentityToPersisted(e.senderKey),
      // ADR-0052 — the reply reference, verbatim.
      ...(e.reply === undefined ? {} : { reply: persistedFromReplyRef(e.reply) }),
      // ADR-0052 decision 2 — the forward attribution, verbatim.
      ...(e.forward === undefined ? {} : { forward: persistedFromForwardRef(e.forward) }),
      // ADR-0061 section 6 — kept so the bubble and the preview still say the
      // message needs a newer Lakko after a reload.
      ...(e.unsupportedVersion === undefined ? {} : { unsupportedVersion: e.unsupportedVersion }),
      // ADR-0099 section 1 - the report evidence, captured at receipt.
      ...(e.evidence === undefined ? {} : { evidence: e.evidence }),
      // ADR-0092 — the content object, verbatim.
      ...(e.content === undefined ? {} : { content: e.content }),
      ...(e.contentState === undefined ? {} : { contentState: e.contentState }),
      // ADR-0087 section 14 — the invitation card, opened offline when drawn.
      ...(e.channelInvite === undefined ? {} : { channelInviteB64: base64Encode(e.channelInvite) }),
    };
  }
  const membership = persistedFromMembership(e.membership);
  return {
    messageId: String(e.messageId),
    direction: "system",
    text: systemRowText(e.systemKind),
    attachments: [],
    createdAt: e.createdAt,
    // docs/ux/second-device.md section 4 — the people the batch changed.
    ...(membership === undefined ? {} : { membership }),
  };
}

/** The persisted text of a system row: `[mls <kind>]`. */
export function systemRowText(systemKind: string): string {
  return `[mls ${systemKind}]`;
}

/**
 * The highest DS message id in a slot, or `null` for an empty one. What "Mark
 * all as read" (`docs/ux/chat-folders.md` 2.2) moves the read cursor to - the
 * same cursor opening the chat moves.
 */
export function newestMessageId(slot: PersistedGroup | undefined): bigint | null {
  let best: bigint | null = null;
  for (const m of slot?.messages ?? []) {
    const id = idOf(m);
    if (id !== null && (best === null || id > best)) best = id;
  }
  return best;
}
