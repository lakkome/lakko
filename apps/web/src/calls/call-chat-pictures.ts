// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pictures in a group call's chat (`docs/ux/calls.md` section 4.1.4.1): how a
 * pasted or dropped picture is prepared, and the file-store side of
 * {@link CallChatPictureIo}.
 *
 * A picture is decoded on the device, its longest side brought down to the
 * conversation's limit and re-encoded as JPEG, which carries no metadata
 * (`attach/photo-encode.ts`). Its file is encrypted with a fresh key and
 * uploaded asking the store for an idle life of one day, so the store's daily
 * sweep removes it even when this device never deletes it; this device
 * deletes its own uploads when its call ends.
 */
import { reencodePhoto } from "../attach/photo-encode";
import { bytesToHex } from "../chat/blob-ids";
import { blobAccess } from "../storage/blob-access";
import { deleteBlobs } from "../storage/blob-deletes";
import { StorageClient } from "../storage/client";
import { loadWasm } from "../wasm";
import type { CallChatPictureIo, CallChatPictureRef } from "./call-chat";

/** The idle life a call-chat picture asks the store for, in days. */
export const CALL_CHAT_PICTURE_IDLE_DAYS = 1;

/** Largest prepared picture, in bytes (the core's bound). */
export const CALL_CHAT_PICTURE_MAX_BYTES = 10 * 1024 * 1024;

/** A prepared picture, or why there is none. */
export type PreparedPicture =
  | { readonly kind: "ok"; readonly jpeg: Uint8Array; readonly width: number; readonly height: number }
  | { readonly kind: "not-picture" }
  | { readonly kind: "too-large" };

/** Whether `type` names something the device may try to decode as a picture. */
export function looksLikePicture(type: string): boolean {
  return /^image\//i.test(type) && !/svg/i.test(type);
}

/**
 * The first picture among pasted or dropped items, or `null` when there is
 * none. Anything that is not a picture is ignored here; the caller says
 * `Only pictures can be sent in a call.` when files came and none was one.
 */
export function firstPicture(files: readonly File[]): File | null {
  return files.find((f) => looksLikePicture(f.type)) ?? null;
}

/** Decode, scale, re-encode. Never throws. */
export async function preparePicture(file: Blob): Promise<PreparedPicture> {
  if (!looksLikePicture(file.type)) return { kind: "not-picture" };
  try {
    const wasm = await loadWasm();
    const jpegFile = await reencodePhoto(file, wasm.photoMaxLongestSidePx());
    if (jpegFile.size > CALL_CHAT_PICTURE_MAX_BYTES) return { kind: "too-large" };
    const bitmap = await createImageBitmap(jpegFile);
    const width = bitmap.width;
    const height = bitmap.height;
    bitmap.close();
    const jpeg = new Uint8Array(await jpegFile.arrayBuffer());
    return { kind: "ok", jpeg, width, height };
  } catch {
    return { kind: "not-picture" };
  }
}

/**
 * The bubble's box for a `width` x `height` picture (section 4.1.4.1): fitted
 * into `max` x `max` keeping its shape, then each side at least `min`; a
 * very long or very tall picture is cropped by the box (`object-fit: cover`).
 */
export function pictureBox(
  width: number,
  height: number,
  max = 240,
  min = 80,
): { readonly width: number; readonly height: number } {
  if (!(width > 0) || !(height > 0)) return { width: max, height: max };
  const scale = Math.min(max / width, max / height, 1);
  return {
    width: Math.max(min, Math.round(width * scale)),
    height: Math.max(min, Math.round(height * scale)),
  };
}

/** The file store side of a call's pictures. `accountId` names the account
 *  whose owed deletes keep this call's uploads (default: the signed-in one). */
export function storeCallChatPictureIo(
  accountId: () => string | null = () => blobAccess()?.accountId ?? null,
): CallChatPictureIo {
  return {
    async upload(jpeg, width, height) {
      const wasm = await loadWasm();
      const sealed = wasm.encryptAttachment(jpeg);
      try {
        const blobId = sealed.blobId;
        const ref: CallChatPictureRef = {
          blobId,
          key: sealed.key,
          nonce: sealed.nonce,
          size: jpeg.length,
          width,
          height,
        };
        await new StorageClient(undefined, CALL_CHAT_PICTURE_IDLE_DAYS).putBlob(blobId, sealed.ciphertext);
        return { ref, blobIdHex: bytesToHex(blobId) };
      } catch {
        return null;
      } finally {
        sealed.free();
      }
    },
    async download(ref) {
      const ciphertext = await new StorageClient().getBlob(ref.blobId);
      if (ciphertext === null) return null;
      const wasm = await loadWasm();
      try {
        return wasm.decryptAttachment(ciphertext, ref.key, ref.nonce);
      } catch {
        return null;
      }
    },
    makeUrl(jpeg) {
      return URL.createObjectURL(new Blob([jpeg.slice()], { type: "image/jpeg" }));
    },
    revokeUrl(url) {
      try {
        URL.revokeObjectURL(url);
      } catch {
        // Nothing to free.
      }
    },
    deleteUploaded(blobIdsHex) {
      const account = accountId();
      if (account === null) return;
      void deleteBlobs(account, blobIdsHex);
    },
  };
}
