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

import { isDesktop } from "../platform";
import { saveFileOnHost } from "../platform/host";
import { loadWasm } from "../wasm";
import { t } from "../l10n";
import { StorageClient } from "../storage/client";
import type {
  Article,
  AttachmentRef as WasmAttachmentRef,
  ChecklistItemRef,
  ContentEnvelopeParts,
  MessageContent,
  SignedChecklist,
} from "../../wasm/lakko_ffi_wasm";
import { displayLocale } from "../l10n/display-locale";

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
  /** ADR-0090 section 2 — an `audio` attachment's title and artist tags,
   *  read on the sender's device. */
  readonly title?: string | undefined;
  readonly artist?: string | undefined;
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

/**
 * ADR-0073 R.7 case 1 — compose a **re-send**: the same chat envelope as
 * [`composeEnvelope`] (text + refs), also carrying the ORIGINAL group id and
 * message id of the message it repeats. The marker is never rendered; a
 * device that already holds the original shows the message once
 * (`resend-marker.ts`). The core carries the marker only when both halves
 * are given and within bounds.
 *
 * `parts` keeps the message FAITHFUL: a re-sent reply carries its quote
 * and a re-sent forward its attribution and origin, exactly as the original
 * did. The caller decides the reply's target (`reset-repair.ts`
 * `resendParts`: 0, "no jump", since the original lives in another journal).
 */
export async function composeResendEnvelope(
  text: string,
  refs: readonly AttachmentRefBytes[],
  origin: ForwardOrigin,
  parts: { readonly reply?: ReplyRef; readonly forward?: ForwardRef } = {},
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const builder = new wasm.EnvelopeBuilder(text);
  try {
    addRefsToBuilder(wasm, builder, refs);
    if (parts.reply !== undefined) {
      builder.setReply(BigInt(parts.reply.target), parts.reply.author, parts.reply.excerpt);
    }
    if (parts.forward !== undefined) {
      builder.setForward(
        parts.forward.author,
        parts.forward.origin?.groupIdB64,
        parts.forward.origin === undefined ? undefined : BigInt(parts.forward.origin.messageId),
      );
    }
    builder.setResendOrigin(origin.groupIdB64, BigInt(origin.messageId));
    return builder.build();
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
      if (r.title !== undefined) wasmRef.title = r.title;
      if (r.artist !== undefined) wasmRef.artist = r.artist;
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
    ...(wasmRef.title === undefined ? {} : { title: wasmRef.title }),
    ...(wasmRef.artist === undefined ? {} : { artist: wasmRef.artist }),
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
 *   - `"reaction"` → add or take off `(reactor, emoji)` on the target.
 *
 * `target` is the DS `messageId` (already converted to a plain
 * JS `number` here; the wasm side hands it as a `bigint`). Ops
 * never carry attachments.
 */
/** An explicit reaction op: add the emoji, or take it off. */
export type ReactionAction = "add" | "remove";

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
      /** ADR-0073 R.7 case 1 — set on a message RE-SENT into a successor
       *  one-to-one: where it was originally sent. Never drawn
       *  (`resend-marker.ts`). */
      resendOrigin?: ForwardOrigin;
    }
  | { kind: "edit"; target: number; text: string }
  | { kind: "delete"; target: number }
  | {
      kind: "reaction";
      target: number;
      emoji: string;
      /** UX reactions-and-emoji section 17: an explicit add or remove, never a
       *  toggle, so two devices of one person pressing at once cannot cancel
       *  each other. */
      action: ReactionAction;
      /** The 32-byte account key of the reacted-to message's author as the
       *  reactor saw it, or `undefined`. A claim, never an identity: only the
       *  reaction notification reads it. */
      targetAuthor?: Uint8Array;
    }
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
  | { kind: "unsupported"; version: number }
  // ADR-0090 — a poll, checklist, location, contact card or article. A bubble
  // of its own; an article's images are its attachments.
  | {
      kind: "content";
      content: MessageContent;
      attachments: AttachmentRefBytes[];
      reply?: ReplyRef;
      forward?: ForwardRef;
    }
  // ADR-0090 — an op on a poll or checklist; never a bubble.
  | { kind: "content-op"; op: ContentOp };

/** ADR-0090 — the ops a poll or checklist message receives, by DS `messageId`
 *  of the target. */
export type ContentOp =
  | { readonly op: "poll-vote"; readonly target: number; readonly choices: readonly number[] }
  | {
      readonly op: "poll-close";
      readonly target: number;
      /** A channel poll's frozen tally, when the closing post carries one. */
      readonly tally?: { readonly voters: number; readonly counts: readonly number[] };
    }
  | {
      readonly op: "checklist-tick";
      readonly target: number;
      readonly item: ChecklistItemRef;
      readonly done: boolean;
    }
  | { readonly op: "checklist-add"; readonly target: number; readonly text: string }
  | { readonly op: "edit-checklist"; readonly target: number; readonly checklist: SignedChecklist }
  | {
      readonly op: "edit-article";
      readonly target: number;
      readonly article: Article;
      readonly attachments: readonly AttachmentRefBytes[];
    };

/** ADR-0090 — the content kinds `parseBodyEnvelope` names. */
const CONTENT_KINDS = new Set(["poll", "checklist", "location", "contact-card", "article"]);

/**
 * ADR-0090 — compose a content message (poll, checklist, location, contact
 * card, article). `refs` are the article's images; empty for the others.
 */
export async function composeContentEnvelope(
  content: MessageContent,
  refs: readonly AttachmentRefBytes[] = [],
  parts?: ContentEnvelopeParts,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const handles = refs.map((r) => toWasmRef(wasm, r));
  try {
    return wasm.composeContentEnvelope(content, handles, parts);
  } finally {
    freeUnmoved(handles);
  }
}

/** ADR-0052 decision 2 / UX `attachments.md` section 11a — the forward
 *  attribution as the content envelope carries it. */
export function contentPartsOfForward(forward: ForwardRef): ContentEnvelopeParts {
  return {
    ...(forward.author === undefined ? {} : { forwardedFrom: forward.author }),
    ...(forward.origin === undefined
      ? {}
      : { forwardOriginGroupB64: forward.origin.groupIdB64, forwardOriginMessageId: forward.origin.messageId }),
  };
}

/** ADR-0090 — compose an article edit: the whole article replaced. */
export async function composeArticleEditEnvelope(
  target: number,
  article: Article,
  refs: readonly AttachmentRefBytes[],
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const handles = refs.map((r) => toWasmRef(wasm, r));
  try {
    return wasm.composeArticleEditEnvelope(BigInt(target), article, handles);
  } finally {
    freeUnmoved(handles);
  }
}

/** Free the handles a compose did not take: the content composers take
 *  their `AttachmentRef[]` by value (moved into the envelope), and freeing a
 *  moved handle throws ("null pointer passed to rust"), which failed every
 *  article sent with images. */
function freeUnmoved(handles: readonly WasmAttachmentRef[]): void {
  for (const h of handles) {
    try {
      h.free();
    } catch {
      /* moved */
    }
  }
}

/** One wasm `AttachmentRef` handle for `r`, metadata included; the caller
 *  frees it. */
function toWasmRef(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  r: AttachmentRefBytes,
): WasmAttachmentRef {
  const wasmRef = new wasm.AttachmentRef(r.blobId, r.key, r.nonce, r.mime, BigInt(r.size), r.name);
  if (r.kind !== undefined) wasmRef.kind = r.kind;
  if (r.durationMs !== undefined) wasmRef.durationMs = r.durationMs;
  if (r.waveform !== undefined) wasmRef.waveform = r.waveform;
  if (r.width !== undefined) wasmRef.width = r.width;
  if (r.height !== undefined) wasmRef.height = r.height;
  if (r.title !== undefined) wasmRef.title = r.title;
  if (r.artist !== undefined) wasmRef.artist = r.artist;
  if (r.thumb !== undefined) {
    const thumb = new wasm.ThumbRef(
      r.thumb.blobId,
      r.thumb.key,
      r.thumb.nonce,
      r.thumb.mime,
      BigInt(r.thumb.size),
      r.thumb.width,
      r.thumb.height,
    );
    try {
      wasmRef.setThumb(thumb);
    } finally {
      thumb.free();
    }
  }
  return wasmRef;
}

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
  return parseBodyEnvelopeWith(await loadWasm(), bytes);
}

/** {@link parseBodyEnvelope} for a caller that already holds the loaded wasm
 *  (a channel post's decode runs synchronously after the open). */
export function parseBodyEnvelopeWith(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  bytes: Uint8Array,
): ParsedBodyResult {
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
      const author = parsed.reactionTargetAuthor;
      return {
        kind: "reaction",
        target: Number(parsed.target ?? 0n),
        emoji: parsed.emoji ?? "",
        action: parsed.reactionAction === "remove" ? "remove" : "add",
        ...(author !== undefined && author.length === 32 ? { targetAuthor: author } : {}),
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
    // ADR-0090 — the ops on a poll or checklist.
    if (kind === "poll-vote") {
      return {
        kind: "content-op",
        op: { op: "poll-vote", target: Number(parsed.target ?? 0n), choices: Array.from(parsed.pollChoices ?? []) },
      };
    }
    if (kind === "poll-close") {
      // The frozen tally of a channel poll's close (a wasm handle, copied
      // out and freed here).
      const handle = parsed.pollTally;
      let tally: { voters: number; counts: number[] } | undefined;
      if (handle !== undefined && handle !== null) {
        try {
          tally = { voters: Number(handle.voters), counts: Array.from(handle.counts, (c) => Number(c)) };
        } finally {
          handle.free();
        }
      }
      return {
        kind: "content-op",
        op: {
          op: "poll-close",
          target: Number(parsed.target ?? 0n),
          ...(tally === undefined ? {} : { tally }),
        },
      };
    }
    if (kind === "checklist-tick") {
      const item = parsed.checklistItem;
      if (item === undefined) return { kind: "service-envelope" };
      return {
        kind: "content-op",
        op: {
          op: "checklist-tick",
          target: Number(parsed.target ?? 0n),
          item,
          done: parsed.checklistDone === true,
        },
      };
    }
    if (kind === "checklist-add") {
      return { kind: "content-op", op: { op: "checklist-add", target: Number(parsed.target ?? 0n), text: parsed.text } };
    }
    if (kind === "edit-checklist" || kind === "edit-article") {
      const content = parsed.content;
      const target = Number(parsed.target ?? 0n);
      if (content?.kind === "checklist" && kind === "edit-checklist") {
        return { kind: "content-op", op: { op: "edit-checklist", target, checklist: content.checklist } };
      }
      if (content?.kind === "article" && kind === "edit-article") {
        return {
          kind: "content-op",
          op: { op: "edit-article", target, article: content.article, attachments: attachmentsOf(parsed) },
        };
      }
      return { kind: "service-envelope" };
    }
    if (CONTENT_KINDS.has(kind)) {
      const content = parsed.content;
      // A content kind whose payload did not decode reads as a message this
      // build cannot show, never as JSON or text.
      if (content === undefined) return { kind: "unsupported", version: 1 };
      const { reply, forward } = replyForwardOf(parsed);
      return {
        kind: "content",
        content,
        attachments: attachmentsOf(parsed),
        ...(reply === undefined ? {} : { reply }),
        ...(forward === undefined ? {} : { forward }),
      };
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
    // ADR-0073 R.7 case 1 — the re-send marker: both halves or neither.
    const resendGroup = parsed.resendOriginGroupB64;
    const resendMessage = parsed.resendOriginMessageId;
    const resendOrigin: ForwardOrigin | undefined =
      resendGroup === undefined || resendMessage === undefined
        ? undefined
        : { groupIdB64: resendGroup, messageId: Number(resendMessage) };
    return {
      kind: "chat",
      text: parsed.text,
      attachments,
      ...(reply === undefined ? {} : { reply }),
      ...(forward === undefined ? {} : { forward }),
      ...(resendOrigin === undefined ? {} : { resendOrigin }),
    };
  } finally {
    parsed.free();
  }
}

type WasmParsedBody = ReturnType<Awaited<ReturnType<typeof loadWasm>>["parseBodyEnvelope"]>;

/** The attachments of a parsed body, as plain data; each handle freed. */
function attachmentsOf(parsed: WasmParsedBody): AttachmentRefBytes[] {
  const out: AttachmentRefBytes[] = [];
  for (let i = 0; i < parsed.attachmentCount; i++) {
    const wasmRef: WasmAttachmentRef = parsed.attachment(i);
    try {
      out.push(fromWasmRef(wasmRef));
    } finally {
      wasmRef.free();
    }
  }
  return out;
}

/** ADR-0052 — the reply and forward parts of a parsed body (chat or content). */
function replyForwardOf(parsed: WasmParsedBody): { reply?: ReplyRef; forward?: ForwardRef } {
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
  return { ...(reply === undefined ? {} : { reply }), ...(forward === undefined ? {} : { forward }) };
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
 * member may react. `action` says add or take off (an add of an emoji
 * the person holds, or a remove of one they do not, is a no-op on
 * every receiver); `targetAuthor` is the reacted-to message's author
 * account key, which lets that author's devices notify. Throws if
 * `emoji` is empty or exceeds the wasm-side `MAX_EMOJI_BYTES` cap.
 */
export async function composeReactionEnvelope(
  targetMessageId: number,
  emoji: string,
  action: ReactionAction,
  targetAuthor?: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composeReactionEnvelope(BigInt(targetMessageId), emoji, action, targetAuthor ?? null);
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
    new Intl.NumberFormat(displayLocale(), {
      minimumFractionDigits: 1,
      maximumFractionDigits: 1,
    }).format(n);
  if (bytes < 1000) {
    return t("common.size.bytes", { size: new Intl.NumberFormat(displayLocale()).format(bytes) });
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
 *
 * The desktop webview has no download from its app scheme: there the
 * host opens the system save dialog.
 */
export async function saveDecryptedBytes(
  bytes: Uint8Array,
  filename: string,
  mime: string | undefined,
): Promise<void> {
  if (isDesktop()) {
    await saveFileOnHost(bytes, filename);
    return;
  }
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
