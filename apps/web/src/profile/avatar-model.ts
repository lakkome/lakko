// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0084 — the profile photo as this client holds it: plain data, detached
 * from the wasm handles, that survives `JSON.stringify` (the own profile in
 * `localStorage`, the received profiles inside the encrypted IndexedDB row).
 *
 * The bounds are the core's (`lakko-contacts::profile`); they are repeated
 * here only so a stored value that no longer fits is dropped on read instead
 * of making every later envelope compose fail. The core stays the authority
 * at compose and decode time.
 *
 * Every reference carries its blob KEY: SECRET. It is only ever written into
 * the device-key-encrypted profile row or the account's own `localStorage`
 * record, never logged.
 */

/** One encrypted avatar blob. */
export interface AvatarRefData {
  /** Standard base64 of the 32-byte blob id (SHA-256 of the ciphertext). */
  readonly blobIdB64: string;
  /** Standard base64 of the 32-byte key. SECRET. */
  readonly keyB64: string;
  /** Standard base64 of the 12-byte nonce. */
  readonly nonceB64: string;
  /** Plaintext size in bytes. */
  readonly size: number;
  /** Side in pixels (the media is square). */
  readonly side: number;
}

/** A photo, or a profile video with its first-frame still. */
export interface AvatarData {
  readonly still: AvatarRefData;
  readonly video: { readonly ref: AvatarRefData; readonly durationMs: number } | null;
}

export const STILL_MAX_SIDE = 640;
export const STILL_MAX_BYTES = 262_144;
export const VIDEO_MAX_SIDE = 480;
export const VIDEO_MAX_BYTES = 4_194_304;
export const VIDEO_MAX_DURATION_MS = 10_000;

/** The wasm `AvatarRef` getters, structurally. */
export interface WasmAvatarRefLike {
  readonly blobId: Uint8Array;
  readonly key: Uint8Array;
  readonly nonce: Uint8Array;
  readonly size: number;
  readonly side: number;
  free?(): void;
}

/** The wasm `ProfileAvatar` getters, structurally. */
export interface WasmProfileAvatarLike {
  readonly still: WasmAvatarRefLike;
  readonly video: WasmAvatarRefLike | undefined;
  readonly videoDurationMs: number | undefined;
  free?(): void;
}

/** The two wasm constructors, structurally. */
export interface AvatarWasm {
  readonly AvatarRef: new (
    blobId: Uint8Array,
    key: Uint8Array,
    nonce: Uint8Array,
    size: number,
    sidePx: number,
  ) => unknown;
  readonly ProfileAvatar: new (still: never, video?: never, videoDurationMs?: number | null) => unknown;
}

export function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i] ?? 0);
  return btoa(bin);
}

export function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function freeQuietly(h: { free?(): void } | undefined): void {
  try {
    h?.free?.();
  } catch {
    /* already moved or freed */
  }
}

function refFromWasm(r: WasmAvatarRefLike): AvatarRefData {
  return {
    blobIdB64: bytesToB64(r.blobId),
    keyB64: bytesToB64(r.key),
    nonceB64: bytesToB64(r.nonce),
    size: r.size,
    side: r.side,
  };
}

/** Copy a decoded avatar out of its wasm handles (and free them). `null` for
 *  `undefined` (no photo). */
export function avatarFromWasm(a: WasmProfileAvatarLike | undefined | null): AvatarData | null {
  if (a === undefined || a === null) return null;
  const still = a.still;
  const video = a.video;
  try {
    const duration = a.videoDurationMs;
    return {
      still: refFromWasm(still),
      video: video === undefined || duration === undefined ? null : { ref: refFromWasm(video), durationMs: duration },
    };
  } finally {
    freeQuietly(still);
    freeQuietly(video);
    freeQuietly(a);
  }
}

/** Build the wasm `ProfileAvatar` for compose. The caller hands it to
 *  `composeProfileEnvelope`, which takes it by value. */
export function avatarToWasm<T>(w: AvatarWasm, a: AvatarData): T {
  const ref = (r: AvatarRefData): unknown =>
    new w.AvatarRef(b64ToBytes(r.blobIdB64), b64ToBytes(r.keyB64), b64ToBytes(r.nonceB64), r.size, r.side);
  const still = ref(a.still);
  if (a.video === null) return new w.ProfileAvatar(still as never, undefined, undefined) as T;
  return new w.ProfileAvatar(still as never, ref(a.video.ref) as never, a.video.durationMs) as T;
}

function sameRef(a: AvatarRefData, b: AvatarRefData): boolean {
  return (
    a.blobIdB64 === b.blobIdB64 &&
    a.keyB64 === b.keyB64 &&
    a.nonceB64 === b.nonceB64 &&
    a.size === b.size &&
    a.side === b.side
  );
}

/** Whether two avatars are the same (both absent counts). */
export function sameAvatar(a: AvatarData | null, b: AvatarData | null): boolean {
  if (a === null || b === null) return a === b;
  if (!sameRef(a.still, b.still)) return false;
  if (a.video === null || b.video === null) return a.video === b.video;
  return a.video.durationMs === b.video.durationMs && sameRef(a.video.ref, b.video.ref);
}

const B64_RE = /^[A-Za-z0-9+/]+={0,2}$/;

function decodedLength(b64: string): number {
  const pad = b64.endsWith("==") ? 2 : b64.endsWith("=") ? 1 : 0;
  return (b64.length / 4) * 3 - pad;
}

function refFromJson(raw: unknown, maxSide: number, maxBytes: number): AvatarRefData | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const fields = [r.blobIdB64, r.keyB64, r.nonceB64];
  if (!fields.every((f) => typeof f === "string" && B64_RE.test(f) && f.length % 4 === 0)) return null;
  const [blobIdB64, keyB64, nonceB64] = fields as [string, string, string];
  if (decodedLength(blobIdB64) !== 32 || decodedLength(keyB64) !== 32 || decodedLength(nonceB64) !== 12) return null;
  const size = r.size;
  const side = r.side;
  if (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > maxBytes) return null;
  if (typeof side !== "number" || !Number.isInteger(side) || side < 1 || side > maxSide) return null;
  return { blobIdB64, keyB64, nonceB64, size, side };
}

/** Read a stored avatar back, dropping anything out of bounds. `null` for
 *  "no photo" and for anything that is not a valid avatar. */
export function avatarFromJson(raw: unknown): AvatarData | null {
  if (typeof raw !== "object" || raw === null) return null;
  const a = raw as Record<string, unknown>;
  const still = refFromJson(a.still, STILL_MAX_SIDE, STILL_MAX_BYTES);
  if (still === null) return null;
  if (a.video === null || a.video === undefined) return { still, video: null };
  if (typeof a.video !== "object") return null;
  const v = a.video as Record<string, unknown>;
  const ref = refFromJson(v.ref, VIDEO_MAX_SIDE, VIDEO_MAX_BYTES);
  const d = v.durationMs;
  if (ref === null || typeof d !== "number" || !Number.isInteger(d) || d < 1 || d > VIDEO_MAX_DURATION_MS) {
    return null;
  }
  return { still, video: { ref, durationMs: d } };
}

/** A cheap check that `bytes` start like a JPEG (SOI marker). */
export function looksLikeJpeg(bytes: Uint8Array): boolean {
  return bytes.length > 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff;
}

/** A cheap check that `bytes` start like an ISO-BMFF (MP4) file: a box whose
 *  type is `ftyp` at offset 4. */
export function looksLikeMp4(bytes: Uint8Array): boolean {
  return (
    bytes.length > 12 &&
    bytes[4] === 0x66 && // f
    bytes[5] === 0x74 && // t
    bytes[6] === 0x79 && // y
    bytes[7] === 0x70 // p
  );
}
