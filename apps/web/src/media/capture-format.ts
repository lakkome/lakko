// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §2 — which container a take is recorded in, and the square crop a
 * video note is drawn through.
 *
 * One format per kind on every platform, so every client plays every other
 * client's recordings without a transcoder: voice is AAC in MP4, a video note
 * is H.264 + AAC in MP4. A browser whose `MediaRecorder` cannot produce that
 * disables recording (UX spec §2 states) rather than falling back to
 * WebM/Opus — there is deliberately no fallback candidate below.
 */

import type { CaptureKind } from "./recording-machine";

/** Most specific first: naming the codecs is what pins AAC / H.264 rather
 *  than whatever the browser's default for the container is. */
export const VOICE_MIME_CANDIDATES: readonly string[] = [
  "audio/mp4;codecs=mp4a.40.2",
  "audio/mp4",
];

export const VIDEO_NOTE_MIME_CANDIDATES: readonly string[] = [
  "video/mp4;codecs=avc1.42E01E,mp4a.40.2",
  "video/mp4;codecs=avc1,mp4a.40.2",
  "video/mp4",
];

/** The MIME type written on the attachment ref for each kind. The codecs
 *  parameter is dropped: `inlineMediaType` ignores parameters anyway, and a
 *  plain type is what the other two clients send. */
export const ATTACHMENT_MIME: Readonly<Record<CaptureKind, string>> = {
  voice: "audio/mp4",
  video_note: "video/mp4",
};

/** §2 states / §3 states — the disabled record button's tooltip. */
export const RECORDING_UNSUPPORTED: Readonly<Record<CaptureKind, string>> = {
  voice: "Voice messages can't be recorded in this browser",
  video_note: "Video messages can't be recorded in this browser",
};

/**
 * The `MediaRecorder` MIME type to record `kind` with, or `null` when this
 * browser cannot record the one allowed format. `isTypeSupported` is injected
 * so the choice is testable; a throwing check counts as unsupported.
 */
export function pickRecordingMime(
  kind: CaptureKind,
  isTypeSupported: ((mime: string) => boolean) | undefined,
): string | null {
  if (isTypeSupported === undefined) return null;
  const candidates = kind === "voice" ? VOICE_MIME_CANDIDATES : VIDEO_NOTE_MIME_CANDIDATES;
  for (const mime of candidates) {
    let ok = false;
    try {
      ok = isTypeSupported(mime);
    } catch {
      ok = false;
    }
    if (ok) return mime;
  }
  return null;
}

/** Lowercased type/subtype with any parameters removed. */
export function baseMime(mime: string): string {
  const semi = mime.indexOf(";");
  return (semi === -1 ? mime : mime.slice(0, semi)).trim().toLowerCase();
}

/** ADR-0047 §2 — a video note is square and at most this many pixels a side. */
export const VIDEO_NOTE_MAX_SIDE = 480;

export interface SquareCrop {
  /** Source rectangle, in source pixels. */
  readonly sx: number;
  readonly sy: number;
  readonly sourceSide: number;
  /** Output canvas side, in pixels (even, for the H.264 encoder). */
  readonly outputSide: number;
}

/**
 * The centred square of a `sourceWidth` x `sourceHeight` camera frame, and
 * the output side it is scaled to: the shorter source side, capped at
 * `maxSide`, rounded down to an even number. `null` for a frame with no
 * usable size (the camera has not produced one yet).
 */
export function squareCrop(
  sourceWidth: number,
  sourceHeight: number,
  maxSide: number = VIDEO_NOTE_MAX_SIDE,
): SquareCrop | null {
  if (
    !Number.isFinite(sourceWidth) ||
    !Number.isFinite(sourceHeight) ||
    sourceWidth < 2 ||
    sourceHeight < 2
  ) {
    return null;
  }
  const sourceSide = Math.floor(Math.min(sourceWidth, sourceHeight));
  const outputSide = Math.max(2, Math.floor(Math.min(sourceSide, maxSide) / 2) * 2);
  return {
    sx: Math.floor((sourceWidth - sourceSide) / 2),
    sy: Math.floor((sourceHeight - sourceSide) / 2),
    sourceSide,
    outputSide,
  };
}

/** ADR-0047 §1 — the poster thumb's size bound. */
export const MAX_THUMB_BYTES = 256 * 1024;
/** Poster frame side, in pixels. The bubble is about 240 px (UX spec §3). */
export const POSTER_SIDE = 240;
