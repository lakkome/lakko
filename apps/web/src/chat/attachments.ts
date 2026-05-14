// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Chat attachment bridge — file picker bytes ↔ wasm encryption ↔
 * storage HTTP.
 *
 * This module is the join between three Phase-1.19 layers on the
 * Web client:
 *
 *   1. **`<input type="file">`** in the chat composer — yields a
 *      browser `File` instance.
 *   2. **`lakko-ffi-wasm`** (`encryptAttachment` / `decryptAttachment`
 *      / `EnvelopeBuilder` / `parseEnvelope`) — the wasm side of
 *      `lakko-attachments` exposed in 1.19b.
 *   3. **`apps/web/src/storage/client.ts`** — the `StorageClient`
 *      added in 1.19c1 that PUTs / GETs ciphertext blobs by
 *      content hash.
 *
 * The two flows it owns:
 *
 *   * **Upload** (`encryptAndUploadFile`) — read a `File` into a
 *     `Uint8Array`, hand to `encryptAttachment`, PUT the
 *     ciphertext to storage under the returned `blob_id`, return
 *     a TS-side [`AttachmentRefBytes`] with the raw byte fields
 *     for embedding into the next MLS Application message.
 *
 *   * **Download** (`downloadAndDecryptRef`) — GET the ciphertext
 *     from storage by `blob_id`, hand to `decryptAttachment` with
 *     the per-attachment key + nonce, return the plaintext bytes.
 *
 * Plus a small `composeEnvelope(text, refs)` helper that hides
 * the wasm `EnvelopeBuilder` builder-pattern dance from the route
 * layer.
 *
 * ## Type strategy
 *
 * The wasm-bindgen `AttachmentRef` is an opaque handle backed by
 * a wasm-side `Vec<u8>` per field. Across a normal JS heap that's
 * fine, but JS code passing refs around (component state,
 * timeline rendering, optimistic-bubble bookkeeping) wants plain
 * `Uint8Array` + primitive fields rather than handle objects
 * with getters that allocate fresh `Uint8Array`s on every read.
 *
 * So this module canonicalises on a TS-side plain-data shape
 * [`AttachmentRefBytes`]; the wasm-side `AttachmentRef` handles
 * are only ever live across the wasm boundary call. `toWasmRef`
 * + `fromWasmRef` are the tiny converter pair.
 *
 * ## Memory ceiling
 *
 * `MAX_ATTACHMENT_BYTES = 50 MB` mirrors the
 * `lakko-attachments` + `services/storage` constants. The
 * pre-upload size gate fails fast with a friendly message
 * before either the wasm AEAD seal or the storage PUT spends
 * cycles.
 */

import { loadWasm } from "../wasm";
import { StorageClient } from "../storage/client";
import type { AttachmentRef as WasmAttachmentRef } from "../../wasm/lakko_ffi_wasm";

/** Per-attachment ceiling — must match the
 *  `MAX_ATTACHMENT_BYTES` constant in `crates/lakko-attachments`
 *  and `services/storage`. */
export const MAX_ATTACHMENT_BYTES = 50 * 1024 * 1024;

/**
 * TS-side plain-data attachment ref. Mirror of the
 * wasm-bindgen `AttachmentRef` class but with primitive fields
 * suitable for plain JS data flow (component state, JSON
 * snapshots, structured-clone across postMessage if ever
 * needed).
 */
export interface AttachmentRefBytes {
  /** Raw 32-byte SHA-256 of the ciphertext blob. */
  readonly blobId: Uint8Array;
  /** Raw 32-byte ChaCha20-Poly1305 CEK. */
  readonly key: Uint8Array;
  /** Raw 12-byte AEAD nonce. */
  readonly nonce: Uint8Array;
  /** Optional MIME type sniffed from the source `File`. */
  readonly mime: string | undefined;
  /** Plaintext size in bytes — surfaced in the UI so the
   *  recipient can decide whether to fetch on a metered
   *  connection. */
  readonly size: number;
  /** Optional human-visible filename. Set to the source
   *  `File.name` on upload. */
  readonly name: string | undefined;
}

/**
 * Errors surfaced from the upload / download paths. Distinct
 * names so the route layer can branch (e.g. surface a
 * "file too large" message on `tooLarge`, but a generic
 * "couldn't upload, try again" on `transport`).
 */
export class AttachmentSizeError extends Error {
  override name = "AttachmentSizeError";
  constructor(
    public readonly got: number,
    public readonly max: number,
  ) {
    super(`attachment ${got} bytes exceeds the ${max}-byte ceiling`);
  }
}

export class AttachmentMissingError extends Error {
  override name = "AttachmentMissingError";
  constructor(public readonly blobId: Uint8Array) {
    super(`attachment blob not found on storage server`);
  }
}

/**
 * Encrypt a picked `File` under a fresh random CEK + nonce, PUT
 * the ciphertext to `lakko-svc-storage`, return the TS-side ref
 * the caller will embed into the next outgoing MLS Application
 * message envelope.
 *
 * The CEK + nonce are generated **inside wasm** (via
 * `getrandom` → browser CSPRNG); JS never holds them as
 * persistent state, only as the in-flight ref returned here.
 * Once that ref makes it into the MLS application message and
 * the message ships, the JS object goes out of scope and is GC'd.
 *
 * @param file     The browser `File` from `<input type="file">`
 *                 (or drag-drop).
 * @param storage  An optional `StorageClient` instance — defaults
 *                 to a fresh one with the standard base URL so
 *                 most callers don't need to thread one through.
 */
export async function encryptAndUploadFile(
  file: File,
  storage: StorageClient = new StorageClient(),
): Promise<AttachmentRefBytes> {
  if (file.size > MAX_ATTACHMENT_BYTES) {
    throw new AttachmentSizeError(file.size, MAX_ATTACHMENT_BYTES);
  }

  const wasm = await loadWasm();
  const plaintext = new Uint8Array(await file.arrayBuffer());

  // wasm encrypt — returns an EncryptedAttachment handle with
  // ciphertext / key / nonce / blobId getters that each allocate
  // a fresh Uint8Array.
  const enc = wasm.encryptAttachment(plaintext);

  try {
    await storage.putBlob(enc.blobId, enc.ciphertext);
  } finally {
    // Even on PUT failure we still want to drop the wasm handle
    // — the encrypted bytes only live in JS heap as the
    // ciphertext getter copies above; the handle itself holds a
    // wasm-side Vec<u8> we should free to keep peak memory
    // bounded on a long composer session.
    enc.free();
  }

  // Sniff a MIME type from the File. Browser `File.type` is the
  // empty string when the OS picker can't guess (e.g. a binary
  // with no extension); surface `undefined` so the wire ref's
  // optional field stays empty rather than carrying `""`.
  const mime = file.type.length > 0 ? file.type : undefined;
  const name = file.name.length > 0 ? file.name : undefined;

  return {
    blobId: enc.blobId,
    key: enc.key,
    nonce: enc.nonce,
    mime,
    size: file.size,
    name,
  };
}

/**
 * GET the ciphertext for an [`AttachmentRefBytes`] from
 * storage, hand to `decryptAttachment`, return plaintext bytes.
 *
 * Throws [`AttachmentMissingError`] on 404 from storage — the
 * blob has been GC'd or never made it onto the server (a sender
 * whose PUT failed but whose MLS message still shipped). The
 * caller decides whether to surface this as "attachment
 * unavailable" or to retry.
 */
export async function downloadAndDecryptRef(
  ref: AttachmentRefBytes,
  storage: StorageClient = new StorageClient(),
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const ciphertext = await storage.getBlob(ref.blobId);
  if (ciphertext === null) {
    throw new AttachmentMissingError(ref.blobId);
  }
  return wasm.decryptAttachment(ciphertext, ref.key, ref.nonce);
}

/**
 * Build the MLS Application-message JSON envelope from
 * `(text, refs)`. Hand the resulting bytes to
 * `Group.createApplicationMessage(identity, bytes)` exactly like
 * the pre-1.19 plain-UTF-8 path did.
 *
 * Empty `text` is valid (attachment-only message); empty `refs`
 * is valid (text-only message — same wire shape as the
 * 1.19a `composeEnvelopeText` convenience).
 */
export async function composeEnvelope(
  text: string,
  refs: readonly AttachmentRefBytes[],
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  if (refs.length === 0) {
    return wasm.composeEnvelopeText(text);
  }
  // Multi-ref path: EnvelopeBuilder one ref at a time. The
  // builder rejects wrong-length blobId / key / nonce up front
  // so a misbehaving caller fails before JSON serialisation.
  const builder = new wasm.EnvelopeBuilder(text);
  try {
    for (const r of refs) {
      const wasmRef = new wasm.AttachmentRef(
        r.blobId,
        r.key,
        r.nonce,
        r.mime,
        BigInt(r.size),
        r.name,
      );
      try {
        builder.add(wasmRef);
      } finally {
        wasmRef.free();
      }
    }
    return builder.build();
  } catch (err) {
    builder.free();
    throw err;
  }
}

/**
 * Parse an incoming MLS Application-message body. Returns the
 * canonical `(text, attachments)` pair from the wasm envelope
 * decoder, surfaced as plain TS data structures.
 *
 * **Never throws.** Legacy pre-1.19 plain-UTF-8 bytes collapse
 * to `{ text: <utf-8 lossy>, attachments: [] }`, unknown-version
 * envelopes collapse to `{ text: "[unsupported v$N message]",
 * attachments: [] }`, malformed individual refs are silently
 * skipped. Same posture as the wasm `parseEnvelope` documented
 * fallback paths (1.19a + 1.19b).
 */
export async function parseEnvelope(
  bytes: Uint8Array,
): Promise<{ text: string; attachments: AttachmentRefBytes[] }> {
  const wasm = await loadWasm();
  const parsed = wasm.parseEnvelope(bytes);
  try {
    const count = parsed.attachmentCount;
    const attachments: AttachmentRefBytes[] = [];
    for (let i = 0; i < count; i++) {
      const wasmRef: WasmAttachmentRef = parsed.attachment(i);
      try {
        attachments.push({
          blobId: wasmRef.blobId,
          key: wasmRef.key,
          nonce: wasmRef.nonce,
          mime: wasmRef.mime,
          size: Number(wasmRef.size),
          name: wasmRef.name,
        });
      } finally {
        wasmRef.free();
      }
    }
    return { text: parsed.text, attachments };
  } finally {
    parsed.free();
  }
}

/**
 * Format an attachment size for UI display — `42 B`, `1.2 kB`,
 * `4.7 MB`. Stays SI-decimal (matches macOS / GNOME / Windows
 * 11 file sizes); base-2 conversion is too pedantic for chat
 * UI.
 */
export function formatAttachmentSize(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1_000_000) return `${(bytes / 1000).toFixed(1)} kB`;
  if (bytes < 1_000_000_000) return `${(bytes / 1_000_000).toFixed(1)} MB`;
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
}

/**
 * Trigger a browser save dialog for the given plaintext bytes
 * with the suggested `filename`. The standard "create a blob,
 * `URL.createObjectURL`, click a synthetic `<a download>`, then
 * revoke" dance — encapsulated here so call sites don't repeat
 * it.
 *
 * `mime` defaults to `application/octet-stream` so a missing
 * type doesn't accidentally inline a binary into a browser
 * preview pane.
 */
export function saveDecryptedBytes(
  bytes: Uint8Array,
  filename: string,
  mime: string | undefined,
): void {
  const blob = new Blob([bytes.slice()], {
    type: mime ?? "application/octet-stream",
  });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  // `display: none` keeps the synthetic anchor out of the
  // layout — Safari historically required the element to be in
  // the DOM for the click to count.
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  // Revoke after a tick so Safari has time to start the
  // download — immediate revoke races with the click.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/**
 * `true` when the ref's MIME type is one the chat UI knows how
 * to inline-preview. Currently: the four core web image
 * formats. SVG is deliberately excluded — an attacker-supplied
 * SVG can carry JS, and inlining it as an `<img>` source is
 * the safe path but disabling preview keeps the UX consistent
 * across formats that can / cannot tile.
 */
export function isInlinePreviewable(ref: AttachmentRefBytes): boolean {
  const m = ref.mime;
  if (m === undefined) return false;
  return m === "image/jpeg" || m === "image/png" || m === "image/webp" || m === "image/gif";
}
