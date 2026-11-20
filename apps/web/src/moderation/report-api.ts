// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 2 - `services/moderation` from the web client, same-origin
 * under `/api/moderation` (the service's paths start with `/moderation/`, so
 * the report route is `/api/moderation/moderation/report`). Every request is
 * anonymous: no session, no cookie, no referrer; the report itself spends
 * one `report` token.
 */

import { anonTokens, anonymousFetchInit, anonTokenQuotaExhausted } from "../tokens/anon-token-pool";
import { acceptListVersion, postOutcome, statusOf, type ReportStatus, type SendOutcome } from "./report-model";

/** The service's base, beside the other services' `/api/<svc>` bases. */
export const MODERATION_API_BASE: string =
  (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_LAKKO_MODERATION_URL ?? "/api/moderation";

/** The pinned moderator root key (build time, base64url, 32 bytes), or `null`
 *  when this build pins none: reporting is then unavailable. */
export function pinnedModerationRootKey(): Uint8Array | null {
  const raw = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_LAKKO_MODERATION_ROOT_KEY;
  return decodeRootKey(raw);
}

/** A base64url (padding optional) 32-byte key, or `null`. Pure. */
export function decodeRootKey(raw: string | undefined): Uint8Array | null {
  if (raw === undefined) return null;
  const s = raw.trim();
  if (s.length === 0 || !/^[A-Za-z0-9_-]+={0,2}$/.test(s)) return null;
  try {
    const b64 = s.replace(/-/g, "+").replace(/_/g, "/").replace(/=+$/, "");
    const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
    const bin = atob(padded);
    if (bin.length !== 32) return null;
    const out = new Uint8Array(32);
    for (let i = 0; i < 32; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** The moderator list fetch could not reach the server. */
export class ModerationNetworkError extends Error {
  override name = "ModerationNetworkError";
}

/** `GET /moderation/moderators`: the signed list's raw bytes, or `null` when
 *  the server has no reporting (503). Throws {@link ModerationNetworkError}
 *  for no answer and for any other refusal. */
export async function fetchModeratorList(base: string = MODERATION_API_BASE): Promise<Uint8Array | null> {
  let response: Response;
  try {
    response = await fetch(`${base}/moderation/moderators`, anonymousFetchInit({ method: "GET", headers: {} }));
  } catch (err) {
    throw new ModerationNetworkError(err instanceof Error ? err.message : "fetch failed");
  }
  if (response.status === 503) return null;
  if (!response.ok) throw new ModerationNetworkError(`moderators answered ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

/** `POST /moderation/report` with one `report` token. Never throws: every
 *  failure is an outcome. */
export async function postReport(sealed: Uint8Array, base: string = MODERATION_API_BASE): Promise<SendOutcome> {
  let response: Response;
  try {
    response = await anonTokens.spend("report", (headerName, headerValue) =>
      fetch(
        `${base}/moderation/report`,
        anonymousFetchInit({
          method: "POST",
          headers: { "content-type": "application/octet-stream", [headerName]: headerValue },
          body: sealed.slice().buffer,
        }),
      ),
    );
  } catch (err) {
    return tokenFailureOutcome(err);
  }
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    body = null;
  }
  return postOutcome(response.status, body);
}

/** What a failure to spend a token means to the sheet: the daily allowance
 *  used up is the daily limit; tokens switched off is a server without
 *  reporting; anything else is the network. */
export function tokenFailureOutcome(err: unknown): SendOutcome {
  if (anonTokenQuotaExhausted(err) !== null) return { kind: "limited" };
  if (err !== null && typeof err === "object" && (err as { name?: unknown }).name === "AnonTokensDisabledError") {
    return { kind: "unavailable" };
  }
  return { kind: "failed" };
}

/** `GET /moderation/report/{id}`: the status, or `null` when it cannot be
 *  read (unknown id, no network). */
export async function fetchReportStatus(
  receiptId: string,
  base: string = MODERATION_API_BASE,
): Promise<ReportStatus | null> {
  if (!/^[A-Za-z0-9_-]{22}$/.test(receiptId)) return null;
  try {
    const response = await fetch(
      `${base}/moderation/report/${receiptId}`,
      anonymousFetchInit({ method: "GET", headers: {} }),
    );
    let body: unknown = null;
    try {
      body = await response.json();
    } catch {
      body = null;
    }
    return statusOf(response.status, body);
  } catch {
    return null;
  }
}

const LIST_FLOOR_KEY = "lakko.moderation.list-version.v1";

/** The highest moderator-list version this device has seen, or `null`. */
export function readListFloor(): bigint | null {
  try {
    const raw = localStorage.getItem(LIST_FLOOR_KEY);
    if (raw === null || !/^[0-9]+$/.test(raw)) return null;
    return BigInt(raw);
  } catch {
    return null;
  }
}

/** Keep the floor. A storage that cannot be written keeps nothing. */
export function writeListFloor(version: bigint): void {
  try {
    localStorage.setItem(LIST_FLOOR_KEY, version.toString());
  } catch {
    /* storage unavailable */
  }
}

// ----------------------------------------------------------------------------
// The moderator list, held in memory (F2: proving a suspension)
// ----------------------------------------------------------------------------

/** A held moderator list is fetched again after this long. Chosen. */
export const MODERATOR_LIST_CACHE_MS = 10 * 60_000;

let heldList: { readonly bytes: Uint8Array; readonly atMs: number } | null = null;
let listRequest: Promise<Uint8Array | null> | null = null;

/** The report sheet fetched (and verified) the list: keep it for the
 *  standing proofs. Memory only. */
export function rememberModeratorList(bytes: Uint8Array, nowMs: number = Date.now()): void {
  heldList = { bytes, atMs: nowMs };
}

/**
 * The moderator list's bytes from memory, else from the server (one request
 * at a time). `null` when the server has no reporting; throws for no answer.
 * The caller verifies the bytes under the pinned root key.
 */
export async function cachedModeratorList(
  fetchList: () => Promise<Uint8Array | null> = () => fetchModeratorList(),
  nowMs: number = Date.now(),
): Promise<Uint8Array | null> {
  if (heldList !== null && nowMs - heldList.atMs < MODERATOR_LIST_CACHE_MS) return heldList.bytes;
  if (listRequest !== null) return listRequest;
  listRequest = fetchList()
    .then((bytes) => {
      if (bytes !== null) rememberModeratorList(bytes, nowMs);
      return bytes;
    })
    .finally(() => {
      listRequest = null;
    });
  return listRequest;
}

/** Sign-out and tests: nothing held. */
export function forgetModeratorList(): void {
  heldList = null;
  listRequest = null;
}

/**
 * The moderator list `list` if it may be sealed to or proven against: it
 * verifies under `rootKey` (`listVersion` throws otherwise) and its version
 * is not below the highest this device has seen. A newer one raises that
 * floor. `null` otherwise, so nothing is ever sealed to (or proven against)
 * a list that still names a moderator a newer list removed. Used by the
 * report sheet's twin rule, the appeal sheet and the standing proofs.
 */
export function acceptModeratorList(
  list: Uint8Array,
  rootKey: Uint8Array,
  listVersion: (list: Uint8Array, rootKey: Uint8Array) => bigint,
  readFloor: () => bigint | null = readListFloor,
  writeFloor: (version: bigint) => void = writeListFloor,
): Uint8Array | null {
  let version: bigint;
  try {
    version = listVersion(list, rootKey);
  } catch {
    return null;
  }
  const floor = acceptListVersion(readFloor(), version);
  if (!floor.accept) return null;
  writeFloor(floor.floor);
  return list;
}

/** The `listVersion` port over the core's `verifyModeratorList`. */
export function wasmListVersion(wasm: {
  verifyModeratorList(list: Uint8Array, rootKey: Uint8Array): { readonly listVersion: bigint; free(): void };
}): (list: Uint8Array, rootKey: Uint8Array) => bigint {
  return (list, rootKey) => {
    const verified = wasm.verifyModeratorList(list, rootKey);
    try {
      return verified.listVersion;
    } finally {
      verified.free();
    }
  };
}
