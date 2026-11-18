// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ways to donate (`docs/ux/donations.md` section 3): the three
 * anonymous discovery routes the donations page reads.
 *
 * Every method is a statement signed offline by the donations key; the
 * client verifies each one against the public key pinned into this build
 * (`VITE_LAKKO_DONATIONS_KEY`) and shows only what verifies. A build with no
 * pinned key shows no method at all, and nothing the server sends outside a
 * statement (the list entry's id aside, which must equal the signed id) is
 * ever shown. Nothing here logs a method, a link, an address or a number.
 */
import { DEFAULT_BASE_URL } from "../discovery/client";
import { base64Decode, base64UrlDecodeNoPad } from "../auth/account-id";
import { loadWasm } from "../wasm";

/** What kind of card a method is drawn as. */
export type DonateKind = "sbp" | "revolut" | "bmc" | "crypto";

/** One verified way to donate. Every field comes from the signed statement. */
export interface DonateMethod {
  readonly id: string;
  readonly kind: DonateKind;
  readonly link: string | null;
  /** SHA-256 of the QR image the server serves for this method (32 bytes),
   *  or null when there is none. The image is shown only when its hash
   *  equals this. */
  readonly qrSha256: Uint8Array | null;
  readonly hasPhone: boolean;
  /** For a crypto method: the coin, the network and the address. */
  readonly crypto: { readonly coin: string; readonly network: string; readonly address: string } | null;
}

/** The getters of the wasm `DonationMethod` this module reads. */
export interface VerifiedStatement {
  readonly id: string;
  readonly kind: string;
  readonly link: string | null | undefined;
  readonly coin: string | null | undefined;
  readonly network: string | null | undefined;
  readonly address: string | null | undefined;
  readonly qrSha256: Uint8Array | null | undefined;
  readonly hasPhone: boolean;
  free(): void;
}

/** `verifyDonationMethod` of the wasm core: throws on any failure. */
export type MethodVerifier = (statement: Uint8Array, key: Uint8Array) => VerifiedStatement;

const KINDS: ReadonlySet<string> = new Set(["sbp", "revolut", "bmc", "crypto"]);
const SHA256_LEN = 32;
const KEY_LEN = 32;
/** Largest QR image accepted (the signing tool and discovery refuse larger). */
const MAX_QR_BYTES = 128 * 1024;

function env(): Record<string, string | undefined> {
  return import.meta.env as Record<string, string | undefined>;
}

/** The build-time pinned 32-byte Ed25519 donations key
 *  (`VITE_LAKKO_DONATIONS_KEY`, base64url without padding), or null when
 *  unset, blank, malformed or not 32 bytes. There is no fallback. */
export function pinnedDonationsKey(): Uint8Array | null {
  const raw = env()["VITE_LAKKO_DONATIONS_KEY"];
  if (typeof raw !== "string" || raw.trim().length === 0) return null;
  try {
    const key = base64UrlDecodeNoPad(raw.trim());
    return key.length === KEY_LEN ? key : null;
  } catch {
    return null;
  }
}

function text(v: string | null | undefined): string | null {
  return typeof v === "string" && v.trim() !== "" ? v : null;
}

function decodeStatement(v: unknown): Uint8Array | null {
  if (typeof v !== "string" || v.length === 0) return null;
  try {
    return base64Decode(v);
  } catch {
    return null;
  }
}

/** Read one verified statement into a method, or null when it is not one
 *  this client draws. */
function toMethod(v: VerifiedStatement): DonateMethod | null {
  const kind = v.kind;
  if (!KINDS.has(kind)) return null;
  const rawLink = text(v.link);
  const link = rawLink !== null && rawLink.startsWith("https://") ? rawLink : null;
  const qr = v.qrSha256 instanceof Uint8Array && v.qrSha256.length === SHA256_LEN ? new Uint8Array(v.qrSha256) : null;
  if (kind === "crypto") {
    const coin = text(v.coin);
    const network = text(v.network);
    const address = text(v.address);
    if (coin === null || network === null || address === null) return null;
    return { id: v.id, kind, link: null, qrSha256: null, hasPhone: false, crypto: { coin, network, address } };
  }
  const hasPhone = v.hasPhone === true;
  if (link === null && qr === null && !hasPhone) return null;
  return { id: v.id, kind: kind as DonateKind, link, qrSha256: qr, hasPhone, crypto: null };
}

/**
 * Verify the list route's answer against `key`. An entry is dropped when its
 * statement does not decode or verify, when the signed id differs from the
 * entry's id, when it repeats an id already shown, or when it is not a method
 * this client can draw. Order is the server's.
 */
export function verifyDonateMethods(json: unknown, key: Uint8Array, verify: MethodVerifier): DonateMethod[] {
  if (typeof json !== "object" || json === null) return [];
  const list = (json as { methods?: unknown }).methods;
  if (!Array.isArray(list)) return [];
  const out: DonateMethod[] = [];
  const seen = new Set<string>();
  for (const raw of list) {
    if (typeof raw !== "object" || raw === null) continue;
    const entry = raw as { id?: unknown; statement_b64?: unknown };
    if (typeof entry.id !== "string") continue;
    const statement = decodeStatement(entry.statement_b64);
    if (statement === null) continue;
    let verified: VerifiedStatement;
    try {
      verified = verify(statement, key);
    } catch {
      continue;
    }
    let method: DonateMethod | null = null;
    try {
      if (verified.id === entry.id && !seen.has(verified.id)) method = toMethod(verified);
    } finally {
      verified.free();
    }
    if (method === null) continue;
    seen.add(method.id);
    out.push(method);
  }
  return out;
}

/** `GET /discovery/donate`, verified. A build with no pinned key returns no
 *  method and asks the server nothing. Throws on a network or server
 *  failure. */
export async function fetchDonateMethods(
  baseUrl: string = DEFAULT_BASE_URL,
  key: Uint8Array | null = pinnedDonationsKey(),
): Promise<DonateMethod[]> {
  if (key === null) return [];
  const res = await fetch(`${baseUrl}/discovery/donate`, { credentials: "omit", cache: "no-store" });
  if (!res.ok) throw new Error(`donate list ${res.status}`);
  const json: unknown = await res.json();
  const wasm = await loadWasm();
  return verifyDonateMethods(json, key, wasm.verifyDonationMethod);
}

/** The URL of a method's QR image. */
export function donateQrUrl(id: string, baseUrl: string = DEFAULT_BASE_URL): string {
  return `${baseUrl}/discovery/donate/${encodeURIComponent(id)}/qr`;
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

/** Fetch a method's QR image and return it only when its SHA-256 equals the
 *  signed `expectedSha256`; null on a mismatch or any failure (the card then
 *  shows no QR). */
export async function fetchDonateQr(
  id: string,
  expectedSha256: Uint8Array,
  baseUrl: string = DEFAULT_BASE_URL,
): Promise<Blob | null> {
  if (expectedSha256.length !== SHA256_LEN) return null;
  try {
    const res = await fetch(donateQrUrl(id, baseUrl), { credentials: "omit", cache: "no-store" });
    if (!res.ok) return null;
    const buf = await res.arrayBuffer();
    if (buf.byteLength === 0 || buf.byteLength > MAX_QR_BYTES) return null;
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", buf));
    if (!sameBytes(digest, expectedSha256)) return null;
    return new Blob([buf], { type: "image/png" });
  } catch {
    return null;
  }
}

/** `POST /discovery/donate/{id}/phone`, verified against the pinned key and
 *  the method's id. Throws when the number is not given or does not verify. */
export async function revealDonatePhone(
  id: string,
  baseUrl: string = DEFAULT_BASE_URL,
  key: Uint8Array | null = pinnedDonationsKey(),
): Promise<string> {
  if (key === null) throw new Error("donate phone: no pinned key");
  const res = await fetch(`${baseUrl}/discovery/donate/${encodeURIComponent(id)}/phone`, {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ action: "reveal" }),
  });
  if (!res.ok) throw new Error(`donate phone ${res.status}`);
  const statement = decodeStatement(((await res.json()) as { statement_b64?: unknown }).statement_b64);
  if (statement === null) throw new Error("donate phone: no statement");
  const wasm = await loadWasm();
  const phone = text(wasm.verifyDonationPhone(statement, key, id));
  if (phone === null) throw new Error("donate phone empty");
  return phone;
}

/** The masked form shown before the number is revealed: the first two
 *  characters of a Russian mobile number, then dots in the usual grouping. */
export const MASKED_PHONE = "+7 9•• ••• •• ••";
