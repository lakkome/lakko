// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The person's own background photos (ADR-0089 section 5, UX
 * `docs/ux/call-backgrounds.md` section 2.3): at most twelve, on this device
 * only, never uploaded and never sent.
 *
 * ```text
 *   db   "lakko-call-backgrounds"  v1
 *   └── store "photos"  keyPath: "id"
 *         row { id: "<accountId>|<photoId>", addedAt: <unix ms>,
 *               ciphertext: Uint8Array  // [12-byte IV | AES-GCM ct+tag] }
 * ```
 *
 * A photo of someone's home is personal data, so the picture (already scaled
 * to at most 1920 pixels on its longer side and re-encoded) is sealed under
 * the account's device key, the posture of `profile/avatar-cache-idb.ts`.
 * Every read-write transaction asks the ADR-0077 gate synchronously right
 * before it is created (`account-store-gate.ts`), and Forget device removes
 * the account's rows ({@link deleteBackgroundPhotos}, `auth/wipe-account.ts`).
 */

import { accountStoresWritable } from "../auth/account-store-gate";
import type { DeviceCipher } from "../auth/device-cipher";

export const BACKGROUND_PHOTOS_DB_NAME = "lakko-call-backgrounds";
const DB_VERSION = 1;
const STORE = "photos";
const IV_LEN = 12;

/** How many own photos are kept (UX section 2.3). */
export const MAX_BACKGROUND_PHOTOS = 12;

/** The longer side of a stored photo, at most (UX section 2.3). */
export const MAX_PHOTO_SIDE = 1920;

/** One stored photo, without its picture. */
export interface PhotoEntry {
  readonly id: string;
  /** When it was added, unix ms. Orders the strip (newest first) and the
   *  eviction (oldest first). */
  readonly addedAt: number;
}

interface PersistedRow {
  id: string;
  addedAt: number;
  ciphertext: Uint8Array;
}

/**
 * Which photos go when one more is added to `existing`: the oldest ones that
 * are not `selectedId`, until the new one fits under `max`. A selected photo
 * is never removed to make room (UX section 2.3).
 */
export function photosToEvict(
  existing: readonly PhotoEntry[],
  selectedId: string | null,
  max: number = MAX_BACKGROUND_PHOTOS,
): string[] {
  const over = existing.length + 1 - max;
  if (over <= 0) return [];
  return [...existing]
    .filter((p) => p.id !== selectedId)
    .sort((a, b) => a.addedAt - b.addedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
    .slice(0, over)
    .map((p) => p.id);
}

/** Newest first, the strip's order. */
export function stripOrder(entries: readonly PhotoEntry[]): PhotoEntry[] {
  return [...entries].sort((a, b) => b.addedAt - a.addedAt || (a.id < b.id ? 1 : a.id > b.id ? -1 : 0));
}

/** A new photo id: 16 random bytes as hex, which the shared rule
 *  (`backgroundIsPhotoId`: 1 to 64 of `[A-Za-z0-9_-]`) accepts. */
export function newPhotoId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let hex = "";
  for (const b of bytes) hex += b.toString(16).padStart(2, "0");
  return hex;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(BACKGROUND_PHOTOS_DB_NAME, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "id" });
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function awaitTx(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

function awaitReq<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void => reject(req.error ?? new Error("idb request failed"));
  });
}

/** What adding a photo did. */
export interface AddedPhoto {
  readonly id: string;
  /** Photos removed to keep at most twelve. */
  readonly evicted: readonly string[];
}

/** One account's own background photos. */
export class BackgroundPhotoStore {
  constructor(
    readonly accountId: string,
    private readonly deviceKey: () => Promise<DeviceCipher | null>,
    private readonly now: () => number = Date.now,
  ) {}

  private prefix(): string {
    return `${this.accountId}|`;
  }

  /** The account's photos, newest first. */
  async list(): Promise<PhotoEntry[]> {
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readonly");
      const rows = (await awaitReq(tx.objectStore(STORE).getAll())) as PersistedRow[];
      await awaitTx(tx);
      return stripOrder(
        rows
          .filter((r) => r.id.startsWith(this.prefix()))
          .map((r) => ({ id: r.id.slice(this.prefix().length), addedAt: r.addedAt })),
      );
    } finally {
      db.close();
    }
  }

  /** The picture of photo `id` (an encoded image), or `null` when it is not
   *  there or cannot be opened. */
  async get(id: string): Promise<Uint8Array | null> {
    const key = await this.deviceKey();
    if (key === null) return null;
    const db = await openDb();
    let row: PersistedRow | undefined;
    try {
      const tx = db.transaction(STORE, "readonly");
      row = (await awaitReq(tx.objectStore(STORE).get(this.prefix() + id))) as PersistedRow | undefined;
      await awaitTx(tx);
    } finally {
      db.close();
    }
    if (row === undefined || row.ciphertext.length < IV_LEN + 16) return null;
    try {
      return new Uint8Array(
        await key.decrypt(row.ciphertext.slice(0, IV_LEN), row.ciphertext.slice(IV_LEN)),
      );
    } catch {
      return null;
    }
  }

  /**
   * Stores `picture` as a new photo and removes the oldest unselected ones
   * past twelve, in one transaction. Rejects when there is no device key or
   * the account's stores are sealed: nothing is written then.
   */
  async add(picture: Uint8Array, selectedId: string | null): Promise<AddedPhoto> {
    const key = await this.deviceKey();
    if (key === null) throw new Error("no device key");
    const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
    const ct = new Uint8Array(await key.encrypt(iv.slice(), picture.slice()));
    const ciphertext = new Uint8Array(IV_LEN + ct.length);
    ciphertext.set(iv, 0);
    ciphertext.set(ct, IV_LEN);
    const existing = await this.list();
    const id = newPhotoId();
    const evicted = photosToEvict(existing, selectedId);
    const db = await openDb();
    try {
      if (!accountStoresWritable(this.accountId)) throw new Error("account stores sealed");
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      for (const gone of evicted) store.delete(this.prefix() + gone);
      store.put({ id: this.prefix() + id, addedAt: this.now(), ciphertext } satisfies PersistedRow);
      await awaitTx(tx);
    } finally {
      db.close();
    }
    return { id, evicted };
  }

  /** Removes photo `id`. */
  async remove(id: string): Promise<void> {
    const db = await openDb();
    try {
      if (!accountStoresWritable(this.accountId)) return;
      const tx = db.transaction(STORE, "readwrite");
      tx.objectStore(STORE).delete(this.prefix() + id);
      await awaitTx(tx);
    } finally {
      db.close();
    }
  }
}

/** Erase every background photo of an account (Forget device). */
export async function deleteBackgroundPhotos(accountId: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    const req = store.getAllKeys();
    req.onsuccess = (): void => {
      for (const key of req.result) {
        if (typeof key === "string" && key.startsWith(`${accountId}|`)) store.delete(key);
      }
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

// ---------------------------------------------------------------------------
// The signed-in account's store, for the call screen and the call sessions.
// ---------------------------------------------------------------------------

let bound: BackgroundPhotoStore | null = null;
const changeListeners = new Set<() => void>();

/** Binds the store to the signed-in account (`null`: signed out). */
export function bindBackgroundPhotos(store: BackgroundPhotoStore | null): void {
  if (bound?.accountId === store?.accountId) return;
  bound = store;
  notifyPhotosChanged();
}

/** The signed-in account's store, or `null`. */
export function backgroundPhotos(): BackgroundPhotoStore | null {
  return bound;
}

/** Tells the strip the photo list changed. */
export function notifyPhotosChanged(): void {
  for (const listener of [...changeListeners]) {
    try {
      listener();
    } catch {
      // One subscriber's failure must not stop the others.
    }
  }
}

/** Subscribes to photo-list changes; returns the unsubscribe function. */
export function onPhotosChanged(listener: () => void): () => void {
  changeListeners.add(listener);
  return () => {
    changeListeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------
// Reading a picked file.
// ---------------------------------------------------------------------------

/** The scaled size of a `width` x `height` picture: its longer side at most
 *  {@link MAX_PHOTO_SIDE}, never enlarged. */
export function scaledPhotoSize(width: number, height: number): { width: number; height: number } {
  const longer = Math.max(width, height);
  if (longer <= MAX_PHOTO_SIDE) return { width, height };
  const scale = MAX_PHOTO_SIDE / longer;
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

/**
 * Decodes a picked file and re-encodes it as a JPEG whose longer side is at
 * most {@link MAX_PHOTO_SIDE}. Rejects when the file cannot be read as a
 * picture (the strip then says `call.background.photo_unusable`).
 * Browser-bound (`createImageBitmap`, canvas); not reachable from vitest.
 */
export async function preparePickedPhoto(file: Blob): Promise<Uint8Array> {
  const bitmap = await createImageBitmap(file);
  try {
    if (bitmap.width < 1 || bitmap.height < 1) throw new Error("empty picture");
    const size = scaledPhotoSize(bitmap.width, bitmap.height);
    let blob: Blob;
    if (typeof OffscreenCanvas === "function") {
      const canvas = new OffscreenCanvas(size.width, size.height);
      const ctx = canvas.getContext("2d");
      if (ctx === null) throw new Error("no 2d context");
      ctx.drawImage(bitmap, 0, 0, size.width, size.height);
      blob = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.9 });
    } else {
      const canvas = document.createElement("canvas");
      canvas.width = size.width;
      canvas.height = size.height;
      const ctx = canvas.getContext("2d");
      if (ctx === null) throw new Error("no 2d context");
      ctx.drawImage(bitmap, 0, 0, size.width, size.height);
      blob = await new Promise<Blob>((resolve, reject) => {
        canvas.toBlob((b) => (b === null ? reject(new Error("toBlob failed")) : resolve(b)), "image/jpeg", 0.9);
      });
    }
    return new Uint8Array(await blob.arrayBuffer());
  } finally {
    bitmap.close();
  }
}
