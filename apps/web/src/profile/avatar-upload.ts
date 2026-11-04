// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Upload a cropped photo (a still and an optional video) as encrypted blobs
 * and seed the avatar cache with the bytes this device already holds, so the
 * new photo shows at once. Shared by the group photo (ADR-0087 section 18,
 * UX 4.5) and the new-group form; the profile screen has its own copy of the
 * same steps.
 */
import { encryptAndUploadBytes } from "../chat/attachments";
import { seedAvatarBytes } from "./avatar-runtime";
import { bytesToB64, type AvatarData, type AvatarRefData } from "./avatar-model";
import type { LocalAvatar } from "./avatar-media";

async function uploadRef(blob: Blob, side: number, mime: string): Promise<{ ref: AvatarRefData; bytes: Uint8Array }> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const r = await encryptAndUploadBytes(bytes, { mime, name: undefined });
  return {
    ref: {
      blobIdB64: bytesToB64(r.blobId),
      keyB64: bytesToB64(r.key),
      nonceB64: bytesToB64(r.nonce),
      size: bytes.length,
      side,
    },
    bytes,
  };
}

export async function uploadLocalAvatar(media: LocalAvatar): Promise<AvatarData> {
  const still = await uploadRef(media.still, media.stillSide, "image/jpeg");
  let video: AvatarData["video"] = null;
  let videoBytes: Uint8Array | null = null;
  if (media.video !== null) {
    const v = await uploadRef(media.video.blob, media.video.side, "video/mp4");
    video = { ref: v.ref, durationMs: media.video.durationMs };
    videoBytes = v.bytes;
  }
  seedAvatarBytes(still.ref.blobIdB64, still.bytes);
  if (video !== null && videoBytes !== null) seedAvatarBytes(video.ref.blobIdB64, videoBytes);
  return { still: still.ref, video };
}

/** The blob ids of a photo, lower-case hex (for the keep tick and deletes). */
export function avatarBlobIdsHex(a: AvatarData | null): string[] {
  if (a === null) return [];
  const out: string[] = [];
  for (const b64 of [a.still.blobIdB64, a.video?.ref.blobIdB64]) {
    if (b64 === undefined) continue;
    const bin = atob(b64);
    let hex = "";
    for (let i = 0; i < bin.length; i++) hex += bin.charCodeAt(i).toString(16).padStart(2, "0");
    out.push(hex);
  }
  return out;
}
