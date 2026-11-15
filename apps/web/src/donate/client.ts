// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ways to donate (`docs/ux/donations.md` section 3): the three
 * anonymous discovery routes the donations page reads.
 */
import { DEFAULT_BASE_URL } from "../discovery/client";

/** What kind of card a method is drawn as. */
export type DonateKind = "sbp" | "revolut" | "bmc" | "crypto";

/** One way to donate, as `GET /discovery/donate` names it. */
export interface DonateMethod {
  readonly id: string;
  readonly kind: DonateKind;
  readonly link: string | null;
  readonly hasQr: boolean;
  readonly hasPhone: boolean;
  /** For a crypto method: the coin, the network and the address. */
  readonly crypto: { readonly coin: string; readonly network: string; readonly address: string } | null;
}

const KINDS: ReadonlySet<string> = new Set(["sbp", "revolut", "bmc", "crypto"]);
const ID_RE = /^[a-z0-9_-]{1,32}$/;

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() !== "" ? v : null;
}

/** Read the list route's answer, dropping any entry that is not a method
 *  this client can draw (an unknown kind, a link that is not https, a crypto
 *  method missing a field). */
export function parseDonateMethods(json: unknown): DonateMethod[] {
  if (typeof json !== "object" || json === null) return [];
  const list = (json as { methods?: unknown }).methods;
  if (!Array.isArray(list)) return [];
  const out: DonateMethod[] = [];
  for (const raw of list) {
    if (typeof raw !== "object" || raw === null) continue;
    const m = raw as Record<string, unknown>;
    const id = str(m.id);
    const kind = str(m.kind);
    if (id === null || !ID_RE.test(id) || kind === null || !KINDS.has(kind)) continue;
    const link = str(m.link);
    const safeLink = link !== null && link.startsWith("https://") ? link : null;
    let crypto: DonateMethod["crypto"] = null;
    if (kind === "crypto") {
      const coin = str(m.coin);
      const network = str(m.network);
      const address = str(m.address);
      if (coin === null || network === null || address === null) continue;
      crypto = { coin, network, address };
    }
    const method: DonateMethod = {
      id,
      kind: kind as DonateKind,
      link: safeLink,
      hasQr: m.has_qr === true,
      hasPhone: m.has_phone === true,
      crypto,
    };
    if (crypto === null && method.link === null && !method.hasQr && !method.hasPhone) continue;
    out.push(method);
  }
  return out;
}

/** `GET /discovery/donate`. Throws on a network or server failure. */
export async function fetchDonateMethods(baseUrl: string = DEFAULT_BASE_URL): Promise<DonateMethod[]> {
  const res = await fetch(`${baseUrl}/discovery/donate`, { credentials: "omit", cache: "no-store" });
  if (!res.ok) throw new Error(`donate list ${res.status}`);
  return parseDonateMethods(await res.json());
}

/** The URL of a method's QR image. */
export function donateQrUrl(id: string, baseUrl: string = DEFAULT_BASE_URL): string {
  return `${baseUrl}/discovery/donate/${encodeURIComponent(id)}/qr`;
}

/** `POST /discovery/donate/{id}/phone`. Throws when the number is not
 *  given. */
export async function revealDonatePhone(id: string, baseUrl: string = DEFAULT_BASE_URL): Promise<string> {
  const res = await fetch(`${baseUrl}/discovery/donate/${encodeURIComponent(id)}/phone`, {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ action: "reveal" }),
  });
  if (!res.ok) throw new Error(`donate phone ${res.status}`);
  const phone = str(((await res.json()) as { phone?: unknown }).phone);
  if (phone === null) throw new Error("donate phone empty");
  return phone;
}

/** The masked form shown before the number is revealed: the first two
 *  characters of a Russian mobile number, then dots in the usual grouping. */
export const MASKED_PHONE = "+7 9•• ••• •• ••";
