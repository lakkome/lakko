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

/** The picture the capture view asks for (UX 5). */
export const CAMERA_CAPTURE_IDEAL = { width: { ideal: 1920 }, height: { ideal: 1080 } } as const;

/** How long the capture view waits for the first frame before it treats the
 *  camera as giving no picture (chosen, not measured). */
export const CAMERA_FIRST_FRAME_MS = 2_000;

/**
 * The video constraint the capture view opens a camera with. A chosen device
 * (the camera picked for calls, or the one the switch button moved to) is
 * asked for by id. Without one, a phone-class browser asks for the front
 * camera by facing mode; any other browser asks for nothing and gets the
 * default device. A computer with several cameras (a phone used as a
 * webcam, virtual cameras) answers `facingMode: "user"` with whichever one
 * claims to face the user, which may give no picture.
 */
export function cameraVideoConstraint(args: {
  readonly deviceId: string | null | undefined;
  readonly phoneClass: boolean;
}): MediaTrackConstraints {
  if (args.deviceId != null && args.deviceId.length > 0) {
    return { ...CAMERA_CAPTURE_IDEAL, deviceId: { exact: args.deviceId } };
  }
  if (args.phoneClass) return { ...CAMERA_CAPTURE_IDEAL, facingMode: "user" };
  return { ...CAMERA_CAPTURE_IDEAL };
}

/** The camera after `current` in the device list, wrapping; the first one
 *  when `current` is unknown; null when there is no other camera. Devices
 *  without an id (listed before permission) are skipped. */
export function nextCameraId(deviceIds: readonly string[], current: string | null | undefined): string | null {
  const ids = deviceIds.filter((id) => id.length > 0);
  if (ids.length === 0) return null;
  const at = current == null ? -1 : ids.indexOf(current);
  if (at < 0) return ids[0] ?? null;
  if (ids.length === 1) return null;
  return ids[(at + 1) % ids.length] ?? null;
}

/** Whether this is a phone-class browser, the only kind that asks for a
 *  camera by facing mode. */
export function isPhoneClassBrowser(nav: {
  readonly userAgent: string;
  readonly userAgentData?: { readonly mobile?: boolean } | undefined;
}): boolean {
  if (nav.userAgentData?.mobile === true) return true;
  return /\b(Android|iPhone|iPod|iPad)\b/.test(nav.userAgent);
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
