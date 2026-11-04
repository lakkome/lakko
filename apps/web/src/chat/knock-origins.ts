// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 8 — where each request to
 * join came from, for the line under it: `Asked 2 hours ago`, or from a link
 * `Invited by Anna · 2 hours ago`.
 *
 * A sealed knock row (ADR-0087 section 5) carries the `/g/` capability it was
 * made with, when it was made from a link; the capability names its issuer.
 * The admin's device records, per joiner (the leaf signature key the request
 * is keyed on), when the row was posted and who issued the link, as it opens
 * the row. The rows are read once (the knock cursor moves past them), so the
 * record is kept per account (`storage/account-kv.ts`), bounded: it only ever
 * needs the requests still waiting.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** Where one request came from. */
export interface KnockOrigin {
  /** When the knock row was posted (the DS stamp), ms; `null` when unknown. */
  readonly askedAtMs: number | null;
  /** The link's issuer (account key, base64); `null` when not from a link. */
  readonly issuerKeyB64: string | null;
}

/** More than the 64 a group can hold waiting, for a few groups at once. */
const MAX_ORIGINS = 512;
const ROW = "knock-origins";

const [origins, setOrigins] = createSignal<ReadonlyMap<string, KnockOrigin>>(new Map());
let account: string | null = null;
let opened: Promise<void> = Promise.resolve();

function decode(raw: unknown): Map<string, KnockOrigin> {
  const out = new Map<string, KnockOrigin>();
  const list = (raw as { o?: unknown } | null)?.o;
  if (!Array.isArray(list)) return out;
  for (const e of list) {
    if (e === null || typeof e !== "object") continue;
    const k = (e as { k?: unknown }).k;
    const a = (e as { a?: unknown }).a;
    const i = (e as { i?: unknown }).i;
    if (typeof k !== "string") continue;
    out.set(k, {
      askedAtMs: typeof a === "number" && Number.isFinite(a) ? a : null,
      issuerKeyB64: typeof i === "string" ? i : null,
    });
  }
  return out;
}

export function openKnockOrigins(accountId: string): Promise<void> {
  if (account === accountId) return opened;
  account = accountId;
  setOrigins(new Map());
  opened = loadAccountJson(accountId, ROW).then(
    (raw) => {
      if (account === accountId) setOrigins(decode(raw));
    },
    () => undefined,
  );
  return opened;
}

/** Record where a request came from (the first record for a joiner stays). */
export async function rememberKnockOrigin(joinerKeyB64: string, origin: KnockOrigin): Promise<void> {
  await opened;
  const acct = account;
  if (acct === null || origins().has(joinerKeyB64)) return;
  const next = new Map(origins());
  next.set(joinerKeyB64, origin);
  // Oldest first in insertion order: drop from the front.
  while (next.size > MAX_ORIGINS) {
    const first = next.keys().next().value;
    if (first === undefined) break;
    next.delete(first);
  }
  setOrigins(next);
  const body = { v: 1, o: [...next].map(([k, o]) => ({ k, a: o.askedAtMs, i: o.issuerKeyB64 })) };
  await saveAccountJson(acct, ROW, body).catch(() => undefined);
}

/** Where a waiting request came from, or `undefined`. Reactive. */
export function knockOriginFor(joinerKeyB64: string): KnockOrigin | undefined {
  return origins().get(joinerKeyB64);
}

/** Parse a DS row stamp (an ISO or SQL timestamp) to ms; `null` when not one. */
export function rowStampMs(createdAt: string): number | null {
  const direct = Date.parse(createdAt);
  if (Number.isFinite(direct)) return direct;
  const iso = createdAt.includes("T") ? createdAt : createdAt.replace(" ", "T");
  const withZone = /[zZ]|[+-]\d\d:?\d\d$/.test(iso) ? iso : `${iso}Z`;
  const ms = Date.parse(withZone);
  return Number.isFinite(ms) ? ms : null;
}

registerAccountMemory("knock-origins", (ids) => {
  if (account !== null && ids.includes(account)) {
    account = null;
    setOrigins(new Map());
  }
});
