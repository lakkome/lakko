// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 / `docs/ux/message-actions.md` section 3.1 — the two strings a
 * reply carries about the message it quotes, and the two lines a bubble
 * draws from them.
 *
 * **The snapshot is taken when Reply is chosen**, not when the message is
 * sent and never when it is rendered: both strings go on the wire as they
 * were, so a later edit or deletion of the original changes nothing, and a
 * receiver who does not hold the original draws the same quote as one who
 * does. `replySnapshotFor` is that capture; `replyQuoteLines` is the render
 * rule, including the fallback for a reply that arrived with no snapshot at
 * all (only the target id): the name line empty, the excerpt "Message".
 *
 * Pure: no wasm, no Solid. What a message with no text is called ("Voice
 * message", "Photo", ...) follows from how the TIMELINE draws that
 * attachment, so the caller hands in the same `bubbleOf` decision the bubble
 * renders from (`hub.decision(ref).bubble`, `media/inline-media.ts`) rather
 * than this module re-deriving it from the MIME type and disagreeing with the
 * bubble one day. A test hands in a stub.
 *
 * Lengths are not capped here: the core truncates an over-long author (255
 * bytes) or excerpt (512 bytes) on a character boundary and drops an empty
 * one, so counting bytes on this side would be a second rule for the same
 * thing.
 */
import type { ReplyRef } from "./attachments";
import type { PersistedReply } from "./message-log-store";
import type { MediaBubble } from "../media/inline-media";
import { t } from "../l10n";

/** What the strip shows and the envelope carries. */
export interface ReplySnapshot {
  /** The quoted author, as the timeline names them ("You" for an own
   *  message). */
  readonly author: string;
  /** The quoted message's text on one line, or the kind of its first
   *  attachment when it has no text. */
  readonly excerpt: string;
}

/** The excerpt drawn for a reply that carries no snapshot. */
export const REPLY_EXCERPT_FALLBACK = (): string => t("chat.reply.fallback_excerpt");

/** Section 3.1 — the word for a message that is nothing but this kind of
 *  attachment. The bubble shape decides, the same one the timeline renders. */
export function labelForBubble(bubble: MediaBubble): string {
  switch (bubble) {
    case "voice":
      return t("chat.media.voice_message");
    case "video_note":
      return t("chat.media.video_message");
    case "photo":
      return t("chat.media.photo");
    case "video":
      return t("chat.media.video");
    case "gif":
      return t("chat.media.gif");
    case "file":
      return t("chat.media.file");
  }
}

/** The message's text on one line: every run of whitespace, line breaks
 *  included, becomes one space; nothing leads or trails. */
export function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/**
 * Capture the snapshot for the message being replied to.
 *
 * `message.text` is the text the person is looking at — the caller passes
 * the edited text of an edited message. A message with no text is named by
 * its first attachment; a message with neither (which the composer refuses
 * to send, but a wire can carry) gets the same fallback the bubble draws for
 * a missing snapshot.
 */
export function replySnapshotFor<A>(
  message: { readonly text: string; readonly attachments: readonly A[] },
  authorLabel: string,
  bubbleOf: (attachment: A) => MediaBubble,
): ReplySnapshot {
  const text = oneLine(message.text);
  if (text.length > 0) return { author: authorLabel, excerpt: text };
  const first = message.attachments[0];
  if (first === undefined) return { author: authorLabel, excerpt: REPLY_EXCERPT_FALLBACK() };
  return { author: authorLabel, excerpt: labelForBubble(bubbleOf(first)) };
}

/** The two lines the quote block draws: the name, then the excerpt. */
export interface ReplyQuoteLines {
  readonly name: string;
  readonly excerpt: string;
}

/**
 * Section 3.1 — the block is the snapshot the envelope carries and never
 * re-reads the original. A reply with no snapshot at all draws the name
 * line empty and the excerpt "Message"; a snapshot missing only one half
 * gets the same fallback for that half, since the core never carries an
 * empty string.
 */
export function replyQuoteLines(reply: Pick<ReplyRef, "author" | "excerpt">): ReplyQuoteLines {
  return {
    name: reply.author ?? "",
    excerpt: reply.excerpt ?? REPLY_EXCERPT_FALLBACK(),
  };
}

/** The accessible name of the quote block: "Reply to <name>", or just
 *  "Reply" when the snapshot names nobody. */
export function replyQuoteLabel(lines: ReplyQuoteLines): string {
  return lines.name.length > 0 ? t("chat.reply.quote_to", { name: lines.name }) : t("chat.reply.quote");
}

// ----- persistence -------------------------------------------------------

/** The on-disk shape of a reply reference (`message-log-store.ts`). The
 *  target is stringified like every DS id the log stores. */
export function persistedFromReplyRef(reply: ReplyRef): PersistedReply {
  return {
    target: String(reply.target),
    ...(reply.author === undefined ? {} : { author: reply.author }),
    ...(reply.excerpt === undefined ? {} : { excerpt: reply.excerpt }),
  };
}

/** Inverse of [`persistedFromReplyRef`]. A row whose target does not parse
 *  (never written by this client, but a store is not a wire) reads as no
 *  reply rather than as a reply to message 0 / NaN. */
export function replyRefFromPersisted(
  reply: PersistedReply | undefined,
): ReplyRef | undefined {
  if (reply === undefined) return undefined;
  const target = Number(reply.target);
  if (!Number.isFinite(target)) return undefined;
  return {
    target,
    ...(typeof reply.author === "string" ? { author: reply.author } : {}),
    ...(typeof reply.excerpt === "string" ? { excerpt: reply.excerpt } : {}),
  };
}
