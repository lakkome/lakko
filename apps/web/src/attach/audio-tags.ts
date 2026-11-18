// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 6 — an audio file's own tags, read on the
 * sender's device before upload: title, artist, duration, embedded cover.
 * Best effort: a file without readable tags is titled by its file name, and
 * nothing here throws. No server sees any of it (ADR-0090 section 2).
 *
 * Read: ID3v2.3 / v2.4 (`TIT2`, `TPE1`, `APIC`) and the MP4 `ilst` items
 * (`©nam`, `©ART`, `covr`). ID3v2.2 and Vorbis comments are not read.
 */
import { reencodePhoto } from "./photo-encode";

/** Cover thumbnail size (UX 6, chosen). */
export const AUDIO_COVER_PX = 320;
/** Tag bound in bytes (ADR-0090 section 2); longer text is cut on a char. */
export const AUDIO_TAG_MAX_BYTES = 255;
const DURATION_TIMEOUT_MS = 5000;

export interface RawAudioTags {
  title?: string;
  artist?: string;
  cover?: { bytes: Uint8Array; mime: string };
}

export interface AudioTags {
  readonly title: string;
  readonly artist?: string;
  readonly durationMs?: number;
  readonly cover?: Blob;
}

/** File name without its extension. */
export function titleFromFileName(name: string): string {
  const dot = name.lastIndexOf(".");
  const base = dot > 0 ? name.slice(0, dot) : name;
  return base.trim().length > 0 ? base.trim() : name;
}

/** Cut to the byte bound on a character boundary; drop control characters. */
export function boundTag(s: string): string | undefined {
  // eslint-disable-next-line no-control-regex
  const clean = s.replace(/[\u0000-\u001f\u007f-\u009f​-‏‪-‮⁠-⁤﻿]/g, "").trim();
  if (clean.length === 0) return undefined;
  const enc = new TextEncoder();
  let out = "";
  for (const ch of clean) {
    if (enc.encode(out + ch).length > AUDIO_TAG_MAX_BYTES) break;
    out += ch;
  }
  return out.length > 0 ? out : undefined;
}

function syncsafe(b: Uint8Array, o: number): number {
  return (((b[o] ?? 0) & 0x7f) << 21) | (((b[o + 1] ?? 0) & 0x7f) << 14) | (((b[o + 2] ?? 0) & 0x7f) << 7) | ((b[o + 3] ?? 0) & 0x7f);
}

function u32(b: Uint8Array, o: number): number {
  return (((b[o] ?? 0) << 24) >>> 0) + ((b[o + 1] ?? 0) << 16) + ((b[o + 2] ?? 0) << 8) + (b[o + 3] ?? 0);
}

function ascii(b: Uint8Array, o: number, n: number): string {
  let s = "";
  for (let i = 0; i < n; i++) s += String.fromCharCode(b[o + i] ?? 0);
  return s;
}

function decodeText(enc: number, data: Uint8Array): string {
  try {
    if (enc === 0) return new TextDecoder("latin1").decode(data);
    if (enc === 1) return new TextDecoder("utf-16").decode(data);
    if (enc === 2) return new TextDecoder("utf-16be").decode(data);
    return new TextDecoder("utf-8").decode(data);
  } catch {
    return "";
  }
}

/** First text value of a text frame (values are NUL separated). */
function textFrame(data: Uint8Array): string {
  if (data.length === 0) return "";
  const s = decodeText(data[0] ?? 3, data.subarray(1));
  return s.split("\u0000")[0] ?? "";
}

/** End of a NUL-terminated string in `enc` from `o`; returns the index past it. */
function skipTerminated(data: Uint8Array, o: number, enc: number): number {
  const wide = enc === 1 || enc === 2;
  let i = o;
  while (i < data.length) {
    if (wide) {
      if (data[i] === 0 && data[i + 1] === 0) return i + 2;
      i += 2;
    } else {
      if (data[i] === 0) return i + 1;
      i += 1;
    }
  }
  return data.length;
}

function apicFrame(data: Uint8Array): { bytes: Uint8Array; mime: string } | undefined {
  if (data.length < 4) return undefined;
  const enc = data[0] ?? 0;
  const mimeEnd = skipTerminated(data, 1, 0);
  const mime = ascii(data, 1, Math.max(0, mimeEnd - 2)).toLowerCase();
  const descStart = mimeEnd + 1; // picture type byte
  const imageStart = skipTerminated(data, descStart, enc);
  if (imageStart >= data.length) return undefined;
  return { bytes: data.slice(imageStart), mime: mime.includes("/") ? mime : `image/${mime || "jpeg"}` };
}

/** ID3v2.3 / v2.4 at the start of `b`, or `null` when there is none. */
export function parseId3(b: Uint8Array): RawAudioTags | null {
  if (b.length < 10 || ascii(b, 0, 3) !== "ID3") return null;
  const major = b[3] ?? 0;
  if (major !== 3 && major !== 4) return null;
  const flags = b[5] ?? 0;
  const size = syncsafe(b, 6);
  const end = Math.min(b.length, 10 + size);
  let o = 10;
  if ((flags & 0x40) !== 0) {
    // Extended header: its size is syncsafe in v2.4, plain (excluding itself) in v2.3.
    const ext = major === 4 ? syncsafe(b, o) : u32(b, o) + 4;
    o += ext;
  }
  const out: RawAudioTags = {};
  while (o + 10 <= end) {
    const id = ascii(b, o, 4);
    if (!/^[A-Z0-9]{4}$/.test(id)) break;
    const frameSize = major === 4 ? syncsafe(b, o + 4) : u32(b, o + 4);
    const start = o + 10;
    const stop = start + frameSize;
    if (frameSize <= 0 || stop > end) break;
    const data = b.subarray(start, stop);
    if (id === "TIT2" && out.title === undefined) out.title = textFrame(data);
    else if (id === "TPE1" && out.artist === undefined) out.artist = textFrame(data);
    else if (id === "APIC" && out.cover === undefined) {
      const cover = apicFrame(data);
      if (cover !== undefined) out.cover = cover;
    }
    o = stop;
  }
  return out;
}

function findAscii(b: Uint8Array, needle: string, from = 0): number {
  const n = needle.length;
  const c0 = needle.charCodeAt(0);
  for (let i = from; i + n <= b.length; i++) {
    if (b[i] !== c0) continue;
    let ok = true;
    for (let j = 1; j < n; j++) {
      if (b[i + j] !== needle.charCodeAt(j)) {
        ok = false;
        break;
      }
    }
    if (ok) return i;
  }
  return -1;
}

/** MP4 `ilst` items, or `null` when there is no `ilst` box. */
export function parseMp4Tags(b: Uint8Array): RawAudioTags | null {
  const at = findAscii(b, "ilst");
  if (at < 4) return null;
  const ilstSize = u32(b, at - 4);
  const end = Math.min(b.length, at - 4 + ilstSize);
  let o = at + 4;
  const out: RawAudioTags = {};
  while (o + 8 <= end) {
    const size = u32(b, o);
    if (size < 8 || o + size > end) break;
    const name = ascii(b, o + 4, 4);
    // The first `data` child: size(4) "data"(4) type(4) locale(4) payload.
    const d = o + 8;
    if (d + 16 <= o + size && ascii(b, d + 4, 4) === "data") {
      const dSize = u32(b, d);
      const type = u32(b, d + 8);
      const payload = b.subarray(d + 16, Math.min(o + size, d + dSize));
      if (name === "©nam") out.title = new TextDecoder().decode(payload);
      else if (name === "©ART") out.artist = new TextDecoder().decode(payload);
      else if (name === "covr") out.cover = { bytes: payload.slice(), mime: type === 14 ? "image/png" : "image/jpeg" };
    }
    o += size;
  }
  return out;
}

async function readDurationMs(file: Blob): Promise<number | undefined> {
  if (typeof document === "undefined" || typeof URL.createObjectURL !== "function") return undefined;
  const url = URL.createObjectURL(file);
  try {
    return await new Promise<number | undefined>((resolve) => {
      const audio = document.createElement("audio");
      const timer = setTimeout(() => resolve(undefined), DURATION_TIMEOUT_MS);
      audio.preload = "metadata";
      audio.onloadedmetadata = (): void => {
        clearTimeout(timer);
        const d = audio.duration;
        resolve(Number.isFinite(d) && d > 0 ? Math.round(d * 1000) : undefined);
      };
      audio.onerror = (): void => {
        clearTimeout(timer);
        resolve(undefined);
      };
      audio.src = url;
    });
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** Title, artist, duration and cover of `file`; never throws. */
export async function readAudioTags(file: File): Promise<AudioTags> {
  let raw: RawAudioTags = {};
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    raw = parseId3(bytes) ?? parseMp4Tags(bytes) ?? {};
  } catch {
    raw = {};
  }
  const title = boundTag(raw.title ?? "") ?? boundTag(titleFromFileName(file.name)) ?? "audio";
  const artist = raw.artist === undefined ? undefined : boundTag(raw.artist);
  let durationMs: number | undefined;
  try {
    durationMs = await readDurationMs(file);
  } catch {
    durationMs = undefined;
  }
  let cover: Blob | undefined;
  if (raw.cover !== undefined && raw.cover.bytes.length > 0) {
    try {
      const blob = new Blob([raw.cover.bytes as BlobPart], { type: raw.cover.mime });
      cover = await reencodePhoto(Object.assign(blob, { name: "cover" }), AUDIO_COVER_PX);
    } catch {
      cover = undefined;
    }
  }
  return {
    title,
    ...(artist === undefined ? {} : { artist }),
    ...(durationMs === undefined ? {} : { durationMs }),
    ...(cover === undefined ? {} : { cover }),
  };
}
