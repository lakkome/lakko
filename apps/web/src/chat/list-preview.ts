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
import { inlineThumbFromPersisted } from "./persisted-attachment";
import { jpegDataUrl } from "../media/visual-media";
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

/** Whether an attachment is a photo or a video of the mosaic (section 11): no
 *  sender-chosen kind, and an image or video type. */
export function visualKindOf(a: Pick<PersistedAttachment, "mime" | "kind">): "photo" | "video" | null {
  if (typeof a.kind === "string" && a.kind.length > 0) return null;
  const mime = (a.mime ?? "").toLowerCase();
  if (mime.startsWith("image/")) return "photo";
  if (mime.startsWith("video/")) return "video";
  return null;
}

/** How many photos, videos and other attachments a message carries. */
export function mediaCounts(attachments: readonly Pick<PersistedAttachment, "mime" | "kind">[]): {
  readonly photos: number;
  readonly videos: number;
  readonly others: number;
} {
  let photos = 0;
  let videos = 0;
  let others = 0;
  for (const a of attachments) {
    const v = visualKindOf(a);
    if (v === "photo") photos += 1;
    else if (v === "video") videos += 1;
    else others += 1;
  }
  return { photos, videos, others };
}

/**
 * The words for a message's attachments when it has no text
 * (`docs/ux/media-messages.md` section 11, chat list row and notifications):
 * `Photo`, `3 photos`, `Video`, `2 videos`, `4 photos and videos`, and for
 * files only `File`, `3 files`. A voice message, video note, GIF or audio file
 * keeps its own word (one per message is how they are sent); when one rides
 * beside others, "and N more". Empty when there are none.
 */
export function attachmentsPreviewLabel(
  attachments: readonly Pick<PersistedAttachment, "mime" | "kind">[],
): string {
  const first = attachments[0];
  if (first === undefined) return "";
  const { photos, videos, others } = mediaCounts(attachments);
  const recorded = attachments.some((a) => typeof a.kind === "string" && a.kind.length > 0);
  if (!recorded) {
    if (photos > 0 && videos > 0) return t("chat.media.photos_and_videos", { count: photos + videos });
    if (photos > 0) return t("chat.media.photos", { count: photos });
    if (videos > 0) return t("chat.media.videos", { count: videos });
    if (others > 0 && attachments.every((a) => !(a.mime ?? "").toLowerCase().startsWith("audio/"))) {
      return t("chat.media.files", { count: others });
    }
  }
  const label = attachmentPreviewLabel(first);
  const rest = attachments.length - 1;
  return rest > 0 ? t("chat.media.and_more", { label, count: rest }) : label;
}

/** Between the count words and the text of a message that carries files
 *  only (section 11: `2 files · Weekend plans`). */
export const PREVIEW_WORDS_SEPARATOR = " \u00b7 ";

/**
 * The count words of a message whose attachments are all plain files (no
 * photo, video, recording, GIF or audio file): `File`, `3 files`. `null`
 * otherwise, and for no attachments.
 */
export function filesOnlyLabel(
  attachments: readonly Pick<PersistedAttachment, "mime" | "kind">[],
): string | null {
  if (attachments.length === 0) return null;
  for (const a of attachments) {
    if (typeof a.kind === "string" && a.kind.length > 0) return null;
    if (visualKindOf(a) !== null) return null;
    if ((a.mime ?? "").toLowerCase().startsWith("audio/")) return null;
  }
  return t("chat.media.files", { count: attachments.length });
}

/**
 * The words of a row's line, or a notification's body, for one message
 * (`docs/ux/media-messages.md` section 11), from its one-line `text`:
 *
 *   * files only, with text: the count words, ` · `, the text
 *     (`2 files · Weekend plans`);
 *   * anything else with text: the text alone (a photo or video shows its
 *     thumbnail before it, drawn by the row);
 *   * no text: the count words alone (`3 photos`, `File`).
 */
export function messagePreviewWords(
  text: string,
  attachments: readonly Pick<PersistedAttachment, "mime" | "kind">[],
): string {
  if (text.length === 0) return attachmentsPreviewLabel(attachments);
  const files = filesOnlyLabel(attachments);
  return files === null ? text : `${files}${PREVIEW_WORDS_SEPARATOR}${text}`;
}

/** The picture at the start of a row's preview line (section 11): the first
 *  photo's or video's micro thumbnail carried inside the message (ADR-0047
 *  section 6), else its own sent thumbnail, and how many photos and videos
 *  the message holds (a stack behind it for more than one). `null` for a
 *  message with none. */
export interface PreviewMedia {
  readonly thumb: PersistedAttachment["thumb"] | null;
  /** Base64url of the first photo's or video's micro thumbnail, when the
   *  message carries one: drawn at once, no fetch. */
  readonly inlineThumbB64?: string;
  readonly video: boolean;
  readonly count: number;
}

export function previewMediaOf(attachments: readonly PersistedAttachment[]): PreviewMedia | null {
  let first: PersistedAttachment | null = null;
  let count = 0;
  for (const a of attachments) {
    if (visualKindOf(a) === null) continue;
    if (first === null) first = a;
    count += 1;
  }
  if (first === null) return null;
  const inline = first.inlineThumbB64;
  return {
    thumb: first.thumb ?? null,
    ...(typeof inline === "string" && inline.length > 0 ? { inlineThumbB64: inline } : {}),
    video: visualKindOf(first) === "video",
    count,
  };
}

/** Where the row's 20 px picture comes from: the micro thumbnail carried
 *  inside the message, drawn at once as a `data:` URL (ADR-0047 section 6);
 *  else the sent thumb blob, fetched and decrypted; else nothing (the
 *  neutral placeholder). The micro thumbnail is used only when it is a JPEG
 *  within the core's bound. */
export type RowThumbSource =
  | { readonly kind: "inline"; readonly url: string }
  | { readonly kind: "thumb" }
  | { readonly kind: "none" };

export function rowThumbSource(media: Pick<PreviewMedia, "thumb" | "inlineThumbB64">): RowThumbSource {
  const inline = inlineThumbFromPersisted(media.inlineThumbB64);
  if (inline !== undefined) return { kind: "inline", url: jpegDataUrl(inline) };
  if (media.thumb !== null && media.thumb !== undefined) return { kind: "thumb" };
  return { kind: "none" };
}

/** How many offset edges the row thumbnail's stack draws behind it: none for
 *  one item, one for two, two for three or more. */
export function stackEdges(count: number): number {
  return count >= 3 ? 2 : count === 2 ? 1 : 0;
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

/**
 * The row's line when it has no last message to show: `No messages yet`
 * only once this device's log has been read and holds none, and nothing
 * while it is still being read (on a cold start every row said `No messages
 * yet` for a second or two before its preview arrived; seen 2026-11-18).
 */
export function emptyPreviewLine(logKnown: boolean): string {
  return logKnown ? t("chats.row.no_messages") : "";
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
