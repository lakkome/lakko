// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the rules of the composer tray, the
 * strip above the message field that holds what one message will carry: which
 * shape a dropped, pasted or picked file takes there, how many of each fit, and
 * when Send may run.
 *
 * Pure: no Solid, no DOM, no wasm. The store that holds the items and runs
 * their uploads (`tray-store.ts`) and the strip that draws them
 * (`ComposerTray.tsx`) ask this module.
 */
import { MAX_ATTACHMENT_BYTES, type AttachmentRefBytes } from "./attachments";

/** At most this many photos and videos in one message (chosen, not measured). */
export const TRAY_MAX_MEDIA = 10;
/** At most this many files in one message (chosen, not measured). */
export const TRAY_MAX_FILES = 10;
/** How long the limit line stays under the strip. */
export const TRAY_LIMIT_NOTICE_MS = 4_000;

/** The shape of one item in the tray: a thumbnail, or a file chip. */
export type TrayKind = "photo" | "video" | "file";

/** Where an item came from: the paperclip's File and Audio rows send the
 *  bytes as a file whatever they are; everything else (Photo or Video, the
 *  camera, a paste, a drop) is sent as media when it can be. */
export type TraySource = "media" | "file" | "audio";

/** What one item of an upload is doing. */
export type TrayStatus = "preparing" | "uploading" | "ready" | "failed";

/**
 * The shape a file takes in the tray, or `"too-big"` when it cannot be sent at
 * all (UX attachments section 4: `Files up to 50 MB can be sent.`, not added).
 *
 * `inline` is the core's allowlist answer for the file's type (`inlineMediaType`
 * through the media hub). A photo or video the receivers cannot show inline (a
 * HEIC photo, a QuickTime video) goes into the tray as a file chip and is sent
 * as a file; it is never refused (attachments section 3).
 */
export function trayKindOf(
  file: { readonly type: string; readonly size: number },
  source: TraySource,
  inline: "image" | "audio" | "video" | null,
): TrayKind | "too-big" {
  if (file.size > MAX_ATTACHMENT_BYTES) return "too-big";
  if (source !== "media") return "file";
  if (inline === "image") return "photo";
  if (inline === "video") return "video";
  return "file";
}

/** Whether an item counts against the photos-and-videos limit. */
export function isVisual(kind: TrayKind): boolean {
  return kind === "photo" || kind === "video";
}

/** Which limit line to show, if any. */
export type TrayLimitNotice = "media" | "files" | null;

/**
 * Which of `incoming` fit beside `current`: the ones that fit, in order, are
 * taken and the rest dropped (section 11, Limits: "adds what fits"). The notice
 * names the limit that dropped something; when both did, the photos-and-videos
 * one, which is the one a person is likelier to have reached.
 */
export function fitIntoTray(
  current: readonly TrayKind[],
  incoming: readonly TrayKind[],
): { readonly accepted: readonly number[]; readonly notice: TrayLimitNotice } {
  let media = current.filter(isVisual).length;
  let files = current.length - media;
  const accepted: number[] = [];
  let droppedMedia = false;
  let droppedFiles = false;
  incoming.forEach((kind, i) => {
    if (isVisual(kind)) {
      if (media >= TRAY_MAX_MEDIA) {
        droppedMedia = true;
        return;
      }
      media += 1;
    } else {
      if (files >= TRAY_MAX_FILES) {
        droppedFiles = true;
        return;
      }
      files += 1;
    }
    accepted.push(i);
  });
  return { accepted, notice: droppedMedia ? "media" : droppedFiles ? "files" : null };
}

/**
 * Whether Send runs: the field has text or the tray holds an item, and no item
 * is still on its way up or failed (section 11, Send).
 */
export function traySendable(text: string, statuses: readonly TrayStatus[]): boolean {
  if (statuses.some((s) => s !== "ready")) return false;
  return text.length > 0 || statuses.length > 0;
}

/**
 * Whether `Backspace` in the message field removes the tray's last item
 * instead of deleting text: only in an empty field with nothing selected, and
 * never with a modifier (section 11, Remove).
 */
export function backspaceRemovesLast(args: {
  readonly key: string;
  readonly fieldEmpty: boolean;
  readonly modified: boolean;
  readonly composing: boolean;
  readonly items: number;
}): boolean {
  return args.key === "Backspace" && args.fieldEmpty && !args.modified && !args.composing && args.items > 0;
}

/** `m:ss` for the duration pill of a video in the tray. */
export function trayDurationLabel(ms: number | undefined): string | null {
  if (ms === undefined || !Number.isFinite(ms) || ms <= 0) return null;
  const total = Math.round(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/**
 * The refs one message carries, in tray order, with the micro thumbnail
 * (ADR-0047 section 6) kept on the carrier only: the first ref whose type
 * shows inline as an image or a video, however the item was added (a JPEG
 * picked through File is a file chip, yet it is the carrier when it comes
 * first). The core applies the same rule (`inline_thumb_carrier`) and drops
 * the field everywhere else, so nothing more is sent. Items not yet uploaded
 * are skipped.
 */
export function messageRefs(
  items: readonly { readonly ref: AttachmentRefBytes | null }[],
  inlineType: (mime: string | undefined) => "image" | "audio" | "video" | null,
): AttachmentRefBytes[] {
  const out: AttachmentRefBytes[] = [];
  let carrierSeen = false;
  for (const item of items) {
    const ref = item.ref;
    if (ref === null) continue;
    const inline = inlineType(ref.mime);
    const isCarrier = !carrierSeen && (inline === "image" || inline === "video");
    if (isCarrier) carrierSeen = true;
    if (isCarrier || ref.inlineThumb === undefined) {
      out.push(ref);
    } else {
      const rest: { -readonly [K in keyof AttachmentRefBytes]: AttachmentRefBytes[K] } = { ...ref };
      delete rest.inlineThumb;
      out.push(rest);
    }
  }
  return out;
}
