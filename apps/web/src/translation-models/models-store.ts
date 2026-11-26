// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 sections 3 to 5 — downloading and keeping the model files of
 * Lakko's own translation engine.
 *
 * This module lives outside `translation/`, whose code may reach no network
 * (`translation/fail-closed.test.ts`). It makes exactly two kinds of request,
 * both to this page's own origin and both checked by
 * `models-store.test.ts`:
 *
 * - `GET /translation-models/<sha256>`: one language group's file, after the
 *   person agreed to download it (UX 3.5). It carries no cookie and no
 *   token; the server learns which group was asked for, which the dialog
 *   says.
 * - the engine binary, a static asset of the web build.
 *
 * A group file is the concatenation of its model files. It is kept only when
 * every model file in it hashes to what the manifest says and the files
 * cover it exactly; otherwise nothing is stored and the download fails.
 *
 * Group files are kept in IndexedDB as Blobs, per device and not per
 * account: they are public files every account on the device reads. Forget
 * device deletes them (`auth/wipe-account.ts`), because which groups a device
 * holds tells which languages its person reads.
 */

import engineWasmUrl from "../../wasm-translate/lakko_translate.wasm?url";

/** The server path group files are served under. */
export const MODELS_PATH = "/translation-models/";
const DB_NAME = "lakko-translation-models";
const STORE = "groups";
const HEX64 = /^[0-9a-f]{64}$/;

/** A model file's place in its group file, and its hash. */
export interface GroupSlice {
  readonly offset: number;
  readonly length: number;
  /** SHA-256 of the bytes, lower-case hex. */
  readonly sha256: string;
}

export interface DownloadOptions {
  readonly onProgress?: (fraction: number) => void;
  readonly signal?: AbortSignal;
}

function hex(buf: ArrayBuffer): string {
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => {
      req.result.createObjectStore(STORE);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("models db"));
  });
}

async function withStore<T>(mode: IDBTransactionMode, run: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await open();
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = db.transaction(STORE, mode);
      const req = run(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(req.result);
      tx.onabort = () => reject(tx.error ?? new Error("models db"));
      tx.onerror = () => reject(tx.error ?? new Error("models db"));
    });
  } finally {
    db.close();
  }
}

/** The SHA-256 names of the group files on this device. */
export async function storedGroups(): Promise<string[]> {
  const keys = await withStore("readonly", (s) => s.getAllKeys());
  return keys.filter((k): k is string => typeof k === "string" && HEX64.test(k));
}

/** The bytes of one model file of a stored group, or `null` when the group
 *  is not on this device. */
export async function readSlice(group: string, offset: number, length: number): Promise<ArrayBuffer | null> {
  const blob = await withStore<unknown>("readonly", (s) => s.get(group));
  if (!(blob instanceof Blob) || offset + length > blob.size) return null;
  return blob.slice(offset, offset + length).arrayBuffer();
}

/** Delete one group file. */
export async function removeGroup(group: string): Promise<void> {
  await withStore("readwrite", (s) => s.delete(group));
}

/** Delete every group file (Forget device). */
export async function removeAllGroups(): Promise<void> {
  if (typeof indexedDB === "undefined") return;
  await new Promise<void>((resolve) => {
    const req = indexedDB.deleteDatabase(DB_NAME);
    req.onsuccess = () => resolve();
    req.onerror = () => resolve();
    req.onblocked = () => resolve();
  });
}

/**
 * Whether `slices` cover a file of `bytes` exactly, end to end with no gap
 * and no overlap. Only then does checking every slice check every byte.
 */
export function slicesCover(slices: readonly GroupSlice[], bytes: number): boolean {
  const sorted = [...slices].sort((a, b) => a.offset - b.offset);
  let at = 0;
  for (const s of sorted) {
    if (s.offset !== at || s.length <= 0) return false;
    at += s.length;
  }
  return at === bytes && bytes > 0;
}

/** Check a downloaded group file against its slices. */
export async function verifyGroup(blob: Blob, bytes: number, slices: readonly GroupSlice[]): Promise<boolean> {
  if (blob.size !== bytes || !slicesCover(slices, bytes)) return false;
  for (const s of slices) {
    const digest = hex(await crypto.subtle.digest("SHA-256", await blob.slice(s.offset, s.offset + s.length).arrayBuffer()));
    if (digest !== s.sha256) return false;
  }
  return true;
}

/**
 * How much of a group file one request asks for. A request through the
 * desktop app's anti-censorship tunnel may return at most 8 MiB
 * (`lakko-transport` `MAX_RESPONSE_BYTES`), and a request through the desktop
 * host arrives whole, so smaller ranges are also what makes progress move
 * there. Chosen, not measured.
 */
export const RANGE_BYTES = 4 * 1024 * 1024;

/** The `Content-Range` a 206 answer to `bytes=first-last` must carry. */
function rangeMatches(header: string | null, first: number, last: number, total: number): boolean {
  return header === `bytes ${first}-${last}/${total}`;
}

/**
 * Download one group file in ranges, check it, and keep it. Resolves once it
 * is stored; rejects on a network failure, a cancel, an answer of another
 * size, or a file that does not check. Nothing is stored before the whole
 * file checks.
 */
export async function downloadGroup(
  group: string,
  bytes: number,
  slices: readonly GroupSlice[],
  options: DownloadOptions = {},
  fetchImpl: typeof fetch = fetch,
): Promise<void> {
  const blob = await fetchGroup(group, bytes, slices, options, fetchImpl);
  await withStore("readwrite", (s) => s.put(blob, group));
}

/** {@link downloadGroup} without the storing: the checked file. */
export async function fetchGroup(
  group: string,
  bytes: number,
  slices: readonly GroupSlice[],
  options: DownloadOptions = {},
  fetchImpl: typeof fetch = fetch,
): Promise<Blob> {
  if (!HEX64.test(group)) throw new Error("bad group name");
  if (!Number.isSafeInteger(bytes) || bytes <= 0) throw new Error("bad group size");
  const parts: ArrayBuffer[] = [];
  let received = 0;
  while (received < bytes) {
    const first = received;
    const last = Math.min(bytes, first + RANGE_BYTES) - 1;
    const response = await fetchImpl(`${MODELS_PATH}${group}`, {
      credentials: "omit",
      cache: "no-store",
      headers: { Range: `bytes=${first}-${last}` },
      ...(options.signal !== undefined ? { signal: options.signal } : {}),
    });
    let chunk: ArrayBuffer;
    if (response.status === 206 && rangeMatches(response.headers.get("Content-Range"), first, last, bytes)) {
      chunk = await response.arrayBuffer();
      if (chunk.byteLength !== last - first + 1) throw new Error("group range of another size");
    } else if (response.status === 200 && first === 0) {
      // A server that ignores ranges sends the whole file at once.
      chunk = await response.arrayBuffer();
      if (chunk.byteLength !== bytes) throw new Error("group of another size");
    } else {
      throw new Error(`group download ${response.status}`);
    }
    parts.push(chunk);
    received += chunk.byteLength;
    options.onProgress?.(received / bytes);
  }
  const blob = new Blob(parts, { type: "application/octet-stream" });
  parts.length = 0;
  if (!(await verifyGroup(blob, bytes, slices))) throw new Error("group file does not check");
  return blob;
}

/** The engine binary, a static asset of this build. */
export async function fetchEngineBinary(fetchImpl: typeof fetch = fetch): Promise<ArrayBuffer> {
  const response = await fetchImpl(engineWasmUrl, { credentials: "omit" });
  if (!response.ok) throw new Error(`engine binary ${response.status}`);
  return response.arrayBuffer();
}
