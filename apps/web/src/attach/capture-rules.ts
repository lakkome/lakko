// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` sections 3-5 — the pure rules of the camera and the
 * send preview: which recording format a browser may use, and which picked
 * items can be sent as what.
 */
import { MAX_ATTACHMENT_BYTES } from "../chat/attachments";

/** A captured video is at most this long (UX 5, chosen). */
export const CAMERA_VIDEO_MAX_MS = 60_000;

/** The MP4 / H.264 recording types, in preference order. Nothing else is
 *  recorded: the receiving clients play only MP4 (UX 5). */
export const CAMERA_VIDEO_MIMES: readonly string[] = [
  "video/mp4;codecs=avc1.42E01E,mp4a.40.2",
  "video/mp4;codecs=avc1",
  "video/mp4",
];

/** The first MP4 type `isSupported` accepts, or `null` (the Video switch is
 *  then hidden). */
export function pickVideoMime(isSupported: (mime: string) => boolean): string | null {
  for (const m of CAMERA_VIDEO_MIMES) {
    try {
      if (isSupported(m)) return m;
    } catch {
      // A browser that throws on a type does not support it.
    }
  }
  return null;
}

export type PreviewVerdict = "photo" | "video" | "video-refused" | "file" | "too-big";

/** What one picked item becomes on the send preview. */
export function previewVerdict(
  file: { readonly type: string; readonly size: number },
  mode: "media" | "file" | "audio",
  asFile: boolean,
): PreviewVerdict {
  if (file.size > MAX_ATTACHMENT_BYTES) return mode === "media" && !asFile && file.type.startsWith("video/") ? "video-refused" : "too-big";
  if (mode !== "media" || asFile) return "file";
  const type = file.type.toLowerCase();
  if (type.startsWith("image/")) return "photo";
  if (type.startsWith("video/")) return type === "video/mp4" ? "video" : "video-refused";
  return "file";
}
