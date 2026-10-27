// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0084 — the session's avatar runtime: which photo a person has (the
 * lookup `ProfileProvider` installs), the decrypted-avatar cache bound to the
 * signed-in account, and the object URLs the one avatar component draws.
 *
 * Browser-bound (IndexedDB, `createImageBitmap`, `<video>`); the decisions it
 * relies on are in `avatar-cache.ts` and `avatar-model.ts`, which are tested.
 * Never imported by the service worker: background code shows initials.
 */

import { createSignal, type Accessor } from "solid-js";

import { getDeviceKey } from "../auth/key-store";
import { downloadAndDecryptRef } from "../chat/attachments";
import { AvatarCache, MemoryAvatarBackend, type AvatarMediaKind } from "./avatar-cache";
import { IdbAvatarBackend } from "./avatar-cache-idb";
import { b64ToBytes, looksLikeJpeg, looksLikeMp4, type AvatarData, type AvatarRefData } from "./avatar-model";

/** The key the own account is named by in the lookup. */
export const OWN_AVATAR_KEY = "self";

let lookup: (keyB64: string) => AvatarData | null = () => null;
const [tick, setTick] = createSignal(0);
const bump = (): void => {
  setTick((t) => t + 1);
};

/** Installed by `ProfileProvider`: the photo a person (or `self`) has. */
export function setAvatarLookup(fn: ((keyB64: string) => AvatarData | null) | null): void {
  lookup = fn ?? (() => null);
  bump();
}

/** ADR-0084 - photos from channel member cards (not filed in the profile
 *  store, which keeps profiles from conversations), by account key. */
const cardAvatars = new Map<string, AvatarData>();

/** A member card was read: its photo (or none) for `keyB64`. */
export function rememberCardAvatar(keyB64: string, avatar: AvatarData | null): void {
  if (avatar === null) cardAvatars.delete(keyB64);
  else cardAvatars.set(keyB64, avatar);
  bump();
}

/** Something a lookup reads changed (a profile arrived, the own photo was
 *  saved). */
export function avatarLookupChanged(): void {
  bump();
}

async function decodesAsImage(bytes: Uint8Array): Promise<boolean> {
  if (!looksLikeJpeg(bytes)) return false;
  if (typeof createImageBitmap !== "function") return true;
  try {
    const bmp = await createImageBitmap(new Blob([bytes.slice()], { type: "image/jpeg" }));
    const ok = bmp.width > 0 && bmp.height > 0;
    bmp.close();
    return ok;
  } catch {
    return false;
  }
}

async function decodesAsVideo(bytes: Uint8Array): Promise<boolean> {
  if (!looksLikeMp4(bytes)) return false;
  if (typeof document === "undefined") return false;
  const url = URL.createObjectURL(new Blob([bytes.slice()], { type: "video/mp4" }));
  try {
    return await new Promise<boolean>((resolve) => {
      const v = document.createElement("video");
      v.muted = true;
      v.preload = "metadata";
      const done = (ok: boolean): void => {
        clearTimeout(timer);
        v.removeAttribute("src");
        v.load();
        resolve(ok);
      };
      const timer = setTimeout(() => done(false), 8000);
      v.onloadedmetadata = (): void => done(v.videoWidth > 0 && v.videoHeight > 0);
      v.onerror = (): void => done(false);
      v.src = url;
    });
  } finally {
    URL.revokeObjectURL(url);
  }
}

function verify(bytes: Uint8Array, kind: AvatarMediaKind): Promise<boolean> {
  return kind === "still" ? decodesAsImage(bytes) : decodesAsVideo(bytes);
}

function fetchAndDecrypt(ref: AvatarRefData): Promise<Uint8Array> {
  return downloadAndDecryptRef({
    blobId: b64ToBytes(ref.blobIdB64),
    key: b64ToBytes(ref.keyB64),
    nonce: b64ToBytes(ref.nonceB64),
    mime: undefined,
    size: ref.size,
    name: undefined,
  });
}

let boundAccount: string | null = null;
let cache = new AvatarCache({
  backend: new MemoryAvatarBackend(),
  fetchAndDecrypt,
  verify,
  now: () => Date.now(),
});
let unsubscribe = cache.subscribe(bump);
const urls = new Map<string, string>();

function revokeAll(): void {
  for (const url of urls.values()) URL.revokeObjectURL(url);
  urls.clear();
}

/** Bind the cache to the signed-in account (`null`: signed out). */
export function bindAvatarRuntime(accountId: string | null): void {
  if (accountId === boundAccount) return;
  boundAccount = accountId;
  cardAvatars.clear();
  unsubscribe();
  cache.clear();
  revokeAll();
  cache = new AvatarCache({
    backend:
      accountId === null ? new MemoryAvatarBackend() : new IdbAvatarBackend(accountId, () => getDeviceKey(accountId)),
    fetchAndDecrypt,
    verify,
    now: () => Date.now(),
  });
  unsubscribe = cache.subscribe(bump);
  bump();
}

/** This device uploaded `bytes` under `blobIdB64`: hold them, no download. */
export function seedAvatarBytes(blobIdB64: string, bytes: Uint8Array): void {
  cache.seed(blobIdB64, bytes);
}

function urlFor(ref: AvatarRefData, kind: AvatarMediaKind): string | null {
  const held = urls.get(ref.blobIdB64);
  if (held !== undefined) return held;
  const bytes = cache.bytes(ref.blobIdB64);
  if (bytes === null) {
    void cache.want(ref, kind);
    return null;
  }
  const url = URL.createObjectURL(
    new Blob([bytes.slice()], { type: kind === "still" ? "image/jpeg" : "video/mp4" }),
  );
  urls.set(ref.blobIdB64, url);
  return url;
}

/** What a circle draws for a person. */
export interface AvatarMedia {
  readonly stillUrl: string | null;
  readonly videoUrl: string | null;
}

/**
 * The media to draw for `keyB64` (a person's key, or {@link OWN_AVATAR_KEY}).
 * Reactive: the photo replaces the initials when it arrives. `withVideo` is
 * for the large views only; elsewhere the still is shown.
 */
export function avatarMediaFor(keyB64: string | undefined, withVideo: boolean): AvatarMedia | null {
  tick();
  if (keyB64 === undefined || keyB64.length === 0) return null;
  const a = lookup(keyB64) ?? cardAvatars.get(keyB64) ?? null;
  if (a === null) return null;
  const stillUrl = urlFor(a.still, "still");
  const videoUrl = withVideo && a.video !== null ? urlFor(a.video.ref, "video") : null;
  if (stillUrl === null && videoUrl === null) return null;
  return { stillUrl, videoUrl };
}

/** Reactive tick, for a caller that derives from the lookup itself. */
export const avatarTick: Accessor<number> = tick;
