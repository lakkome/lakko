// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Base64 and hex helpers. The API answers standard padded base64. */

export function b64ToBytes(b64: string): Uint8Array {
  const normal = b64.replace(/-/g, "+").replace(/_/g, "/");
  const padded = normal + "===".slice((normal.length + 3) % 4);
  const bin = atob(padded);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

export function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** `null` for an odd length or a non-hex character. */
export function hexToBytes(hex: string): Uint8Array | null {
  const h = hex.trim().toLowerCase();
  if (h.length % 2 !== 0 || !/^[0-9a-f]*$/.test(h)) return null;
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}

export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/** A short form of a target for lists: the first 8 bytes in hex. */
export function shortHex(hex: string): string {
  return hex.length <= 16 ? hex : `${hex.slice(0, 16)}...`;
}

/** The bytes as a standalone ArrayBuffer (WebAuthn wants a BufferSource). */
export function toBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(out).set(bytes);
  return out;
}
