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
import { currentLanguage, t } from "../l10n";
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
  /** ADR-0047 — `"voice"`, `"video_note"`, `"gif"`, an unknown future
   *  value, or absent for a plain file. Sender-chosen: it picks the bubble
   *  shape only. Whether bytes render inline is `inlineMediaType(mime)`. */
  readonly kind?: string | undefined;
  /** ADR-0047 — playback length, bounded by the core per kind. */
  readonly durationMs?: number | undefined;
  /** ADR-0047 — at most 128 amplitude bytes for the voice bubble. */
  readonly waveform?: Uint8Array | undefined;
  /** ADR-0047 — pixel size (1..=4096) for layout without a jump. */
  readonly width?: number | undefined;
  readonly height?: number | undefined;
  /** ADR-0047 — a video note's poster frame, its own encrypted blob. */
  readonly thumb?: ThumbRefBytes | undefined;
}

/** ADR-0047 — the poster-frame reference inside an [`AttachmentRefBytes`]. */
export interface ThumbRefBytes {
  readonly blobId: Uint8Array;
  readonly key: Uint8Array;
  readonly nonce: Uint8Array;
  readonly mime: string | undefined;
  /** Plaintext size, at most 256 KiB on the wire. */
  readonly size: number;
  readonly width: number | undefined;
  readonly height: number | undefined;
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
  /** Optional progress callback — invoked once per coarse-
   *  grained stage (`"encrypting"` immediately after the
   *  `file.arrayBuffer()` read returns, `"uploading"` right
   *  before the PUT). The two stages cover the wall-clock
   *  budget reasonably: encrypt is sub-second for sub-
   *  MAX_ATTACHMENT_BYTES files, the PUT dominates on real
   *  networks. */
  onStage?: (stage: "encrypting" | "uploading") => void,
): Promise<AttachmentRefBytes> {
  if (file.size > MAX_ATTACHMENT_BYTES) {
    throw new AttachmentSizeError(file.size, MAX_ATTACHMENT_BYTES);
  }

  const wasm = await loadWasm();
  onStage?.("encrypting");
  const plaintext = new Uint8Array(await file.arrayBuffer());

  // wasm encrypt — returns an EncryptedAttachment handle with
  // ciphertext / key / nonce / blobId getters that each allocate
  // a fresh Uint8Array.
  const enc = wasm.encryptAttachment(plaintext);

  // Every getter copies out of the wasm handle, so all of them are read
  // BEFORE `free()`. A getter on a freed handle throws "null pointer passed
  // to rust" — this function used to read `blobId` / `key` / `nonce` after
  // the free, which failed every composer file send.
  let blobId: Uint8Array;
  let key: Uint8Array;
  let nonce: Uint8Array;
  try {
    blobId = enc.blobId;
    key = enc.key;
    nonce = enc.nonce;
    onStage?.("uploading");
    await storage.putBlob(blobId, enc.ciphertext);
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
    blobId,
    key,
    nonce,
    mime,
    size: file.size,
    name,
  };
}

/** ADR-0047 — media metadata carried on a ref built from raw bytes. */
export interface AttachmentMediaMeta {
  readonly kind?: string | undefined;
  readonly durationMs?: number | undefined;
  readonly waveform?: Uint8Array | undefined;
  readonly width?: number | undefined;
  readonly height?: number | undefined;
  readonly thumb?: ThumbRefBytes | undefined;
}

/**
 * ADR-0047 — encrypt plaintext `bytes` (a recording, a poster frame, a GIF
 * downloaded through the proxy) under a fresh CEK + nonce, PUT the ciphertext
 * to storage, and return the ref. The bytes-based sibling of
 * [`encryptAndUploadFile`]: same size gate, same wasm seal, same handle
 * hygiene.
 *
 * `onProgress` receives the upload's completed fraction (0..1) when the
 * transport can report it, so the timeline can draw a determinate ring.
 */
export async function encryptAndUploadBytes(
  bytes: Uint8Array,
  file: { readonly mime: string | undefined; readonly name: string | undefined },
  meta: AttachmentMediaMeta = {},
  storage: StorageClient = new StorageClient(),
  onProgress?: (fraction: number) => void,
): Promise<AttachmentRefBytes> {
  if (bytes.length === 0) {
    throw new AttachmentSizeError(0, MAX_ATTACHMENT_BYTES);
  }
  if (bytes.length > MAX_ATTACHMENT_BYTES) {
    throw new AttachmentSizeError(bytes.length, MAX_ATTACHMENT_BYTES);
  }
  const wasm = await loadWasm();
  const enc = wasm.encryptAttachment(bytes);
  let blobId: Uint8Array;
  let key: Uint8Array;
  let nonce: Uint8Array;
  try {
    blobId = enc.blobId;
    key = enc.key;
    nonce = enc.nonce;
    await storage.putBlobWithProgress(blobId, enc.ciphertext, onProgress);
  } finally {
    enc.free();
  }
  return {
    blobId,
    key,
    nonce,
    mime: file.mime,
    size: bytes.length,
    name: file.name,
    ...meta,
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
 * ADR-0052 decision 2, OQ1 — re-encrypt one attachment for a forward: fetch
 * and open the source blob, seal the plaintext under a FRESH key + nonce,
 * upload it as its own blob, and hand back a ref that carries the source's
 * MIME, name and ADR-0047 media metadata (kind / duration / waveform /
 * dimensions) verbatim. A video note's poster frame is re-encrypted the same
 * way, so the forwarded message references no blob the source does.
 *
 * The two conversations therefore never share a ciphertext object: the
 * storage tier sees two unrelated blobs fetched by two unrelated groups. The
 * cost is one download and one upload per attachment, and it is paid.
 */
export async function reencryptAttachmentRef(
  ref: AttachmentRefBytes,
  storage: StorageClient = new StorageClient(),
): Promise<AttachmentRefBytes> {
  let thumb: ThumbRefBytes | undefined;
  if (ref.thumb !== undefined) {
    const t = ref.thumb;
    const posterBytes = await downloadAndDecryptRef(
      { blobId: t.blobId, key: t.key, nonce: t.nonce, mime: t.mime, size: t.size, name: undefined },
      storage,
    );
    const fresh = await encryptAndUploadBytes(
      posterBytes,
      { mime: t.mime, name: undefined },
      {},
      storage,
    );
    thumb = {
      blobId: fresh.blobId,
      key: fresh.key,
      nonce: fresh.nonce,
      mime: fresh.mime,
      size: fresh.size,
      width: t.width,
      height: t.height,
    };
  }
  const bytes = await downloadAndDecryptRef(ref, storage);
  return encryptAndUploadBytes(
    bytes,
    { mime: ref.mime, name: ref.name },
    {
      ...(ref.kind === undefined ? {} : { kind: ref.kind }),
      ...(ref.durationMs === undefined ? {} : { durationMs: ref.durationMs }),
      ...(ref.waveform === undefined ? {} : { waveform: ref.waveform }),
      ...(ref.width === undefined ? {} : { width: ref.width }),
      ...(ref.height === undefined ? {} : { height: ref.height }),
      ...(thumb === undefined ? {} : { thumb }),
    },
    storage,
  );
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
    addRefsToBuilder(wasm, builder, refs);
    return builder.build();
  } catch (err) {
    builder.free();
    throw err;
  }
}

/**
 * ADR-0052 — the reply reference a chat envelope carries beside its text:
 * the DS `messageId` it answers plus the snapshot of the quoted message as
 * the replier saw it. `author` / `excerpt` are absent when the replier
 * carried none (the bubble then draws the no-snapshot fallback,
 * `reply-snapshot.ts`). A claim by the replier, never verified against the
 * original.
 */
export interface ReplyRef {
  readonly target: number;
  readonly author?: string;
  readonly excerpt?: string;
}

/**
 * ADR-0052 — compose a **reply**: the same chat envelope as
 * [`composeEnvelope`] (text + refs), also naming the message it answers and
 * carrying the quoted author + excerpt snapshot. The core truncates an
 * over-long snapshot on a character boundary and drops an empty one, so the
 * caller passes what the UX spec says without counting bytes.
 */
export async function composeReplyEnvelope(
  text: string,
  refs: readonly AttachmentRefBytes[],
  reply: ReplyRef,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const target = BigInt(reply.target);
  if (refs.length === 0) {
    return wasm.composeReplyEnvelopeText(text, target, reply.author, reply.excerpt);
  }
  const builder = new wasm.EnvelopeBuilder(text);
  try {
    addRefsToBuilder(wasm, builder, refs);
    return builder.buildReply(target, reply.author, reply.excerpt);
  } catch (err) {
    builder.free();
    throw err;
  }
}

/**
 * ADR-0052 decision 2 — where a forwarded message came from: the origin
 * conversation's group id (standard base64, the `ChatContext` registry key)
 * and the original's DS `messageId`. Travels only inside the sealed envelope
 * and is used for one thing: a recipient who also holds that conversation
 * can jump to the original. Both halves are set or neither is.
 */
export interface ForwardOrigin {
  readonly groupIdB64: string;
  readonly messageId: number;
}

/**
 * ADR-0052 decision 2 — what a forwarded chat message carries about its
 * origin: the ORIGINAL author's display name as the forwarder's timeline
 * showed it (a label, never a reachable identity, never verified) and the
 * origin reference. Either may be absent; the bubble then reads "Forwarded"
 * and a tap on it does nothing.
 */
export interface ForwardRef {
  readonly author?: string;
  readonly origin?: ForwardOrigin;
}

/**
 * ADR-0052 decision 2 — compose a **forward**: the same chat envelope as
 * [`composeEnvelope`] (text + refs — the refs are the RE-ENCRYPTED ones, never
 * the source's), also carrying the original author's name and the origin
 * reference. The core truncates an over-long name to 255 bytes on a
 * character boundary and drops an origin it cannot carry, so the caller
 * passes what it has without counting bytes.
 */
export async function composeForwardEnvelope(
  text: string,
  refs: readonly AttachmentRefBytes[],
  forward: ForwardRef,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const builder = new wasm.EnvelopeBuilder(text);
  try {
    addRefsToBuilder(wasm, builder, refs);
    return builder.buildForward(
      forward.author,
      forward.origin?.groupIdB64,
      forward.origin === undefined ? undefined : BigInt(forward.origin.messageId),
    );
  } catch (err) {
    builder.free();
    throw err;
  }
}

/** Append every ref to `builder`, ADR-0047 media metadata included. Shared by
 *  the plain and the reply composers so the two cannot drift on what a ref
 *  is. Each wasm handle is freed once the builder has copied it. */
function addRefsToBuilder(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  builder: InstanceType<Awaited<ReturnType<typeof loadWasm>>["EnvelopeBuilder"]>,
  refs: readonly AttachmentRefBytes[],
): void {
  for (const r of refs) {
    const wasmRef = new wasm.AttachmentRef(
      r.blobId,
      r.key,
      r.nonce,
      r.mime,
      BigInt(r.size),
      r.name,
    );
    let wasmThumb: InstanceType<typeof wasm.ThumbRef> | null = null;
    try {
      // ADR-0047 — optional media metadata. Out-of-bound values are not
      // errors here: `build` drops them exactly as every receiver would.
      if (r.kind !== undefined) wasmRef.kind = r.kind;
      if (r.durationMs !== undefined) wasmRef.durationMs = r.durationMs;
      if (r.waveform !== undefined) wasmRef.waveform = r.waveform;
      if (r.width !== undefined) wasmRef.width = r.width;
      if (r.height !== undefined) wasmRef.height = r.height;
      if (r.thumb !== undefined) {
        wasmThumb = new wasm.ThumbRef(
          r.thumb.blobId,
          r.thumb.key,
          r.thumb.nonce,
          r.thumb.mime,
          BigInt(r.thumb.size),
          r.thumb.width,
          r.thumb.height,
        );
        wasmRef.setThumb(wasmThumb);
      }
      builder.add(wasmRef);
    } finally {
      wasmThumb?.free();
      wasmRef.free();
    }
  }
}

/**
 * Copy a wasm `AttachmentRef` handle into plain data, ADR-0047 media fields
 * included. The caller still owns (and frees) `wasmRef`; the `thumb` getter
 * returns a fresh handle, which is freed here.
 */
function fromWasmRef(wasmRef: WasmAttachmentRef): AttachmentRefBytes {
  const thumbHandle = wasmRef.thumb;
  let thumb: ThumbRefBytes | undefined;
  if (thumbHandle !== undefined) {
    try {
      thumb = {
        blobId: thumbHandle.blobId,
        key: thumbHandle.key,
        nonce: thumbHandle.nonce,
        mime: thumbHandle.mime,
        size: Number(thumbHandle.size),
        width: thumbHandle.width,
        height: thumbHandle.height,
      };
    } finally {
      thumbHandle.free();
    }
  }
  return {
    blobId: wasmRef.blobId,
    key: wasmRef.key,
    nonce: wasmRef.nonce,
    mime: wasmRef.mime,
    size: Number(wasmRef.size),
    name: wasmRef.name,
    kind: wasmRef.kind,
    durationMs: wasmRef.durationMs,
    waveform: wasmRef.waveform,
    width: wasmRef.width,
    height: wasmRef.height,
    thumb,
  };
}

/**
 * Parse an incoming MLS Application-message body. Returns the
 * canonical `(text, attachments)` pair from the wasm envelope
 * decoder, surfaced as plain TS data structures.
 *
 * **Never throws.** Legacy pre-1.19 plain-UTF-8 bytes collapse
 * to `{ text: <utf-8 lossy>, attachments: [] }`, unknown-version
 * envelopes collapse to `{ text: "", attachments: [] }`, malformed
 * individual refs are silently skipped.
 *
 * ADR-0061 section 6: the core's text for an unknown-version envelope is the
 * English "[unsupported vN message]". It is never returned from here; a caller
 * that renders a message uses `parseBodyEnvelope`, whose `"unsupported"` kind
 * renders `chat.message.unsupported`.
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
        attachments.push(fromWasmRef(wasmRef));
      } finally {
        wasmRef.free();
      }
    }
    return { text: isUnsupportedPlaceholder(parsed.text) ? "" : parsed.text, attachments };
  } finally {
    parsed.free();
  }
}

/**
 * The core's English placeholder for an envelope of an unknown wire version
 * (`lakko_attachments::parse`, "[unsupported vN message]"). Matched exactly so
 * a real message that happens to read like it is only blanked when it IS that
 * shape and nothing else.
 */
export function isUnsupportedPlaceholder(text: string): boolean {
  return /^\[unsupported v\d+ message\]$/.test(text);
}

/**
 * ADR-0023 — the structured result of `parseBodyEnvelope`. A
 * discriminated union mirroring the Rust `MessageBody` enum the
 * wasm `ParsedBody` flat class surfaces. The chat receive loop
 * branches on `kind`:
 *
 *   - `"chat"`     → a normal text + attachments bubble.
 *   - `"edit"`     → fold into the target message's text.
 *   - `"delete"`   → tombstone the target message.
 *   - `"reaction"` → toggle `(reactorLeaf, emoji)` on the target.
 *
 * `target` is the DS `messageId` (already converted to a plain
 * JS `number` here; the wasm side hands it as a `bigint`). Ops
 * never carry attachments.
 */
export type ParsedBodyResult =
  | {
      kind: "chat";
      text: string;
      attachments: AttachmentRefBytes[];
      /** ADR-0052 — set when this chat message answers another. A reply is an
       *  ordinary chat message; `target` stays the op field and is never set
       *  here. */
      reply?: ReplyRef;
      /** ADR-0052 decision 2 — set when this chat message was forwarded: the
       *  original author's name and / or the origin reference, as the
       *  forwarder wrote them. */
      forward?: ForwardRef;
    }
  | { kind: "edit"; target: number; text: string }
  | { kind: "delete"; target: number }
  | { kind: "reaction"; target: number; emoji: string }
  // ADR-0024 — ephemeral status signals (typing / read receipt).
  // Neither renders a bubble; the receive loop folds them into the
  // ephemeral typing registry / the monotone read watermark.
  | { kind: "typing"; isTyping: boolean }
  | { kind: "read-receipt"; target: number }
  // A `lakko/…` service envelope this build did not recognise (a newer
  // version of a known kind, or a kind it does not know at all). The core
  // classifies it instead of degrading it to lossy-UTF-8 chat text, and it is
  // NEVER rendered: no bubble, no preview line, no notification text.
  | { kind: "service-envelope" }
  // ADR-0061 section 6 — a chat envelope from a newer Lakko whose wire
  // version this build does not read. It IS a message, so it keeps its place
  // in the timeline, but its words come from the catalog
  // (`chat.message.unsupported`), never from the core: the core's legacy
  // "[unsupported vN message]" text is English and must not reach a screen.
  | { kind: "unsupported"; version: number };

/**
 * Parse an incoming MLS Application-message body into the
 * structured ADR-0023 `MessageBody` shape (chat / edit / delete
 * / reaction). Wraps the wasm `parseBodyEnvelope` flat class +
 * its `kind` discriminator, converting the `bigint` target to a
 * plain `number` for the timeline bookkeeping.
 *
 * **Never throws.** The wasm parser is bounded + fail-closed: a
 * malformed or ambiguous (more than one op field set) envelope
 * degrades to `{ kind: "chat" }` with the raw text. An
 * unrecognised `kind` string likewise collapses to a chat body
 * so the receive loop always has something safe to render.
 */
export async function parseBodyEnvelope(
  bytes: Uint8Array,
): Promise<ParsedBodyResult> {
  const wasm = await loadWasm();
  const parsed = wasm.parseBodyEnvelope(bytes);
  try {
    const kind = parsed.kind;
    if (kind === "edit") {
      return {
        kind: "edit",
        target: Number(parsed.target ?? 0n),
        text: parsed.text,
      };
    }
    if (kind === "delete") {
      return { kind: "delete", target: Number(parsed.target ?? 0n) };
    }
    if (kind === "reaction") {
      return {
        kind: "reaction",
        target: Number(parsed.target ?? 0n),
        emoji: parsed.emoji ?? "",
      };
    }
    // ADR-0024 — typing signal. `isTyping` is the new wasm getter
    // (undefined unless `kind === "typing"`); default to `false`
    // (treated as a clear) if the FFI couldn't surface it.
    if (kind === "typing") {
      return { kind: "typing", isTyping: parsed.isTyping ?? false };
    }
    // ADR-0024 — read receipt. `target` is the read-up-to messageId.
    if (kind === "read-receipt") {
      return { kind: "read-receipt", target: Number(parsed.target ?? 0n) };
    }
    // A service envelope the receive loop's own sniffers did not claim (the
    // live walk of 2026-09-29: a profile envelope v3 from another build).
    // Before the core classified these they reached the chat fallback below
    // and rendered as `lakko/profile/v1` plus binary. Dropped by every caller.
    if (kind === "service-envelope") {
      return { kind: "service-envelope" };
    }
    // ADR-0061 section 6 — a newer wire version. `text` is empty; the
    // version rides `unsupportedVersion` (and `target`).
    if (kind === "unsupported") {
      return { kind: "unsupported", version: unsupportedVersionOf(parsed) };
    }
    // "chat" (and any unexpected discriminator) — surface text +
    // attachments. The same memory dance parseEnvelope runs.
    const count = parsed.attachmentCount;
    const attachments: AttachmentRefBytes[] = [];
    for (let i = 0; i < count; i++) {
      const wasmRef: WasmAttachmentRef = parsed.attachment(i);
      try {
        attachments.push(fromWasmRef(wasmRef));
      } finally {
        wasmRef.free();
      }
    }
    // ADR-0052 — the reply reference, only ever present on a chat body. The
    // author / excerpt are kept out of the object when absent so a reply with
    // no snapshot and a plain message read the same way everywhere.
    const replyTo = parsed.replyTo;
    let reply: ReplyRef | undefined;
    if (replyTo !== undefined) {
      const author = parsed.replyAuthor;
      const excerpt = parsed.replyExcerpt;
      reply = {
        target: Number(replyTo),
        ...(author === undefined ? {} : { author }),
        ...(excerpt === undefined ? {} : { excerpt }),
      };
    }
    // ADR-0052 decision 2 — the forward attribution, only ever present on a
    // chat body. The core hands the origin as a pair that is either both set
    // or both absent; a name alone is a forward that reads "Forwarded from X"
    // with nothing to jump to. Absent fields stay out of the object so a
    // plain message and a forward read the same way everywhere.
    const forwardedFrom = parsed.forwardedFrom;
    const originGroup = parsed.forwardOriginGroupB64;
    const originMessage = parsed.forwardOriginMessageId;
    let forward: ForwardRef | undefined;
    if (forwardedFrom !== undefined || (originGroup !== undefined && originMessage !== undefined)) {
      forward = {
        ...(forwardedFrom === undefined ? {} : { author: forwardedFrom }),
        ...(originGroup === undefined || originMessage === undefined
          ? {}
          : { origin: { groupIdB64: originGroup, messageId: Number(originMessage) } }),
      };
    }
    return {
      kind: "chat",
      text: parsed.text,
      attachments,
      ...(reply === undefined ? {} : { reply }),
      ...(forward === undefined ? {} : { forward }),
    };
  } finally {
    parsed.free();
  }
}

/** The wire version of an `"unsupported"` body, `0` when the core gave none. */
function unsupportedVersionOf(parsed: { unsupportedVersion?: number | undefined; target?: bigint | undefined }): number {
  if (typeof parsed.unsupportedVersion === "number") return parsed.unsupportedVersion;
  return parsed.target === undefined ? 0 : Number(parsed.target);
}

/**
 * ADR-0023 — compose an **edit** op envelope: rewrite the text of
 * the message identified by DS `targetMessageId` to `newText`.
 * The result is a normal MLS Application message body; the caller
 * publishes it via the existing application send path. Only the
 * original author's edit is accepted by peers (the receive-side
 * author-only gate enforces it).
 */
export async function composeEditEnvelope(
  targetMessageId: number,
  newText: string,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composeEditEnvelope(BigInt(targetMessageId), newText);
}

/**
 * ADR-0023 — compose a **delete** op envelope: tombstone the
 * message identified by DS `targetMessageId`. Author-only on the
 * receive side.
 */
export async function composeDeleteEnvelope(
  targetMessageId: number,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composeDeleteEnvelope(BigInt(targetMessageId));
}

/**
 * ADR-0023 — compose a **reaction** op envelope: react to the
 * message identified by DS `targetMessageId` with `emoji`. Any
 * member may react; re-sending the same emoji toggles it off on
 * the receive side. Throws if `emoji` is empty or exceeds the
 * wasm-side `MAX_EMOJI_BYTES` cap (64 bytes).
 */
export async function composeReactionEnvelope(
  targetMessageId: number,
  emoji: string,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composeReactionEnvelope(BigInt(targetMessageId), emoji);
}

/**
 * ADR-0024 — compose a **typing** signal envelope. `isTyping`:
 * `true` while the user is actively editing the composer (throttled
 * by the caller to ~1/4 s), `false` once on blur / on sending a
 * message. The receiver folds it into an ephemeral per-leaf typing
 * state; it never renders a bubble. The result is a normal MLS
 * Application message body; the caller publishes it through the
 * existing application send path.
 */
export async function composeTypingEnvelope(
  isTyping: boolean,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composeTypingEnvelope(isTyping);
}

/**
 * ADR-0024 — compose a **read-receipt** envelope: broadcast that the
 * sender has read up to (and including) DS `upToMessageId` in this
 * group. The receiver advances a monotone per-leaf watermark (a
 * lower value never lowers a higher one, so re-sync replay is
 * idempotent). Never renders a bubble.
 */
export async function composeReadReceiptEnvelope(
  upToMessageId: number,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composeReadReceiptEnvelope(BigInt(upToMessageId));
}

/**
 * Format an attachment size for UI display — `42 B`, `1.2 kB`,
 * `4.7 MB`. Stays SI-decimal (matches macOS / GNOME / Windows
 * 11 file sizes); base-2 conversion is too pedantic for chat
 * UI.
 */
export function formatAttachmentSize(bytes: number): string {
  // ADR-0061: the number in the interface language's format, the unit
  // abbreviation from the catalog.
  const oneDecimal = (n: number): string =>
    new Intl.NumberFormat(currentLanguage(), {
      minimumFractionDigits: 1,
      maximumFractionDigits: 1,
    }).format(n);
  if (bytes < 1000) {
    return t("common.size.bytes", { size: new Intl.NumberFormat(currentLanguage()).format(bytes) });
  }
  if (bytes < 1_000_000) return t("common.size.kb", { size: oneDecimal(bytes / 1000) });
  if (bytes < 1_000_000_000) return t("common.size.mb", { size: oneDecimal(bytes / 1_000_000) });
  return t("common.size.gb", { size: oneDecimal(bytes / 1_000_000_000) });
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
