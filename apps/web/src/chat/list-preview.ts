// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The second line of a chat-list row (docs/ux/client-redesign.md section
 * 5.1): the last message, prefixed "You: " when it is the reader's own and
 * with the sender's name in a group; a media message shows its kind
 * ("Photo", "Voice message", "Video message", "GIF", "File") instead of text.
 *
 * Pure: no wasm, no Solid. The name resolution is the caller's (the one
 * profile resolver); this module only puts the pieces in order.
 */
import type { PersistedAttachment } from "./message-log-store";
import { t } from "../l10n";

/** The one-word label for a media attachment, from its kind and MIME type.
 *  The sender-chosen `kind` names the recording flow (voice, video note,
 *  GIF); a plain image or video is told from its MIME type; anything else is
 *  a file. */
export function attachmentPreviewLabel(
  a: Pick<PersistedAttachment, "mime" | "kind">,
): string {
  const kind = typeof a.kind === "string" ? a.kind : "";
  if (kind === "voice") return t("chat.media.voice_message");
  if (kind === "video_note") return t("chat.media.video_message");
  if (kind === "gif") return t("chat.media.gif");
  const mime = (a.mime ?? "").toLowerCase();
  if (mime.startsWith("image/")) return t("chat.media.photo");
  if (mime.startsWith("video/")) return t("chat.media.video");
  if (mime.startsWith("audio/")) return t("chat.media.voice_message");
  return t("chat.media.file");
}

/** The label for a message's attachments: the first one's kind, "and N
 *  more" beyond it. Empty when there are none. */
export function attachmentsPreviewLabel(
  attachments: readonly Pick<PersistedAttachment, "mime" | "kind">[],
): string {
  const first = attachments[0];
  if (first === undefined) return "";
  const label = attachmentPreviewLabel(first);
  const rest = attachments.length - 1;
  return rest > 0 ? t("chat.media.and_more", { label, count: rest }) : label;
}

export interface PreviewLineInput {
  /** The flattened last-message text or media label; `null` for none. */
  readonly preview: string | null;
  /** The last message is the reader's own. */
  readonly isOwn: boolean;
  /** The sender's resolved name for a message someone else sent, or `null`
   *  when unknown. */
  readonly senderName: string | null;
  /** A group row prefixes the sender; a 1x1 has one other person and does
   *  not. */
  readonly isGroup: boolean;
}

/** "You: hi", "Anna: hi", "hi", or "" for an empty conversation. */
export function previewLine(input: PreviewLineInput): string {
  if (input.preview === null || input.preview.length === 0) return "";
  if (input.isOwn) return t("chats.preview.own", { text: input.preview });
  if (input.isGroup && input.senderName !== null && input.senderName.length > 0) {
    return t("chats.preview.sender", { name: input.senderName, text: input.preview });
  }
  return input.preview;
}

/** Case-insensitive match of a search query against a row's name and
 *  preview. An empty query matches everything. */
export function rowMatchesQuery(query: string, name: string, preview: string, handle?: string | null): boolean {
  const q = query.trim().toLocaleLowerCase();
  if (q.length === 0) return true;
  if (name.toLocaleLowerCase().includes(q) || preview.toLocaleLowerCase().includes(q)) return true;
  // A group's or channel's `@name` (ADR-0087), typed with or without the `@`.
  const bare = q.replace(/^@+/, "");
  return handle !== undefined && handle !== null && bare.length > 0 && handle.toLocaleLowerCase().includes(bare);
}
