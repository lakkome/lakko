// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The panel API, relative to the page's own origin (`/panel/api/*`). The
 * bearer token comes from the in-memory session; a `401 session-required`
 * signs the moderator out on the spot.
 */

import { session, signOut } from "../session";
import { ApiError, isSignedOut } from "./errors";
import type {
  Appeals,
  CaseDetail,
  Cases,
  Checks,
  DecideAnswer,
  Lock,
  LogPage,
  Me,
  Moderators,
  Numbers,
  SessionFinish,
  SessionStart,
} from "./types";
import { queueQuery, type QueueFilters } from "../model/queue";

const BASE = "/panel/api";

async function call<T>(method: string, path: string, body?: unknown, auth = true): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (auth) {
    const s = session();
    if (s === null) throw new ApiError(401, "session-required", null);
    headers.Authorization = `Bearer ${s.token}`;
  }
  let res: Response;
  try {
    res = await fetch(`${BASE}${path}`, {
      method,
      headers,
      body: body === undefined ? null : JSON.stringify(body),
      cache: "no-store",
      credentials: "same-origin",
    });
  } catch {
    throw new ApiError(0, null, null);
  }
  const text = await res.text();
  let json: unknown = null;
  if (text !== "") {
    try {
      json = JSON.parse(text);
    } catch {
      json = null;
    }
  }
  if (!res.ok) {
    const code =
      json !== null && typeof json === "object" && typeof (json as { error?: unknown }).error === "string"
        ? (json as { error: string }).error
        : null;
    const err = new ApiError(res.status, code, json);
    if (isSignedOut(err)) signOut();
    throw err;
  }
  return json as T;
}

/** A binary GET (an evidence file's ciphertext). */
async function callBytes(path: string): Promise<Uint8Array> {
  const s = session();
  if (s === null) throw new ApiError(401, "session-required", null);
  let res: Response;
  try {
    res = await fetch(`${BASE}${path}`, {
      headers: { Authorization: `Bearer ${s.token}` },
      cache: "no-store",
      credentials: "same-origin",
    });
  } catch {
    throw new ApiError(0, null, null);
  }
  if (!res.ok) {
    let code: string | null = null;
    try {
      const json: unknown = await res.json();
      if (json !== null && typeof json === "object" && typeof (json as { error?: unknown }).error === "string") {
        code = (json as { error: string }).error;
      }
    } catch {
      code = null;
    }
    const err = new ApiError(res.status, code, null);
    if (isSignedOut(err)) signOut();
    throw err;
  }
  return new Uint8Array(await res.arrayBuffer());
}

/**
 * The decide answer is meaningful on 422 / 502 too (logged, not applied).
 * Every decision goes through here, keeping a suspension on appeal included
 * (a signed `keep-suspension`); there is no unsigned decline.
 */
async function decideCall(body: unknown): Promise<{ status: number; answer: DecideAnswer }> {
  try {
    const answer = await call<DecideAnswer>("POST", "/decide", body);
    return { status: 200, answer };
  } catch (e) {
    if (
      e instanceof ApiError &&
      (e.status === 422 || e.status === 502) &&
      e.body !== null &&
      typeof e.body === "object" &&
      typeof (e.body as { log_index?: unknown }).log_index === "number"
    ) {
      return { status: e.status, answer: e.body as DecideAnswer };
    }
    throw e;
  }
}

export const api = {
  sessionStart: () => call<SessionStart>("POST", "/session/start", undefined, false),
  sessionFinish: (body: {
    moderator_id_b64: string;
    authenticator_data_b64: string;
    client_data_json_b64: string;
    signature_b64: string;
  }) => call<SessionFinish>("POST", "/session/finish", body, false),
  me: () => call<Me>("GET", "/me"),
  moderators: () => call<Moderators>("GET", "/moderators"),
  cases: (f: QueueFilters) => call<Cases>("GET", `/cases${queueQuery(f)}`),
  caseDetail: (kind: string, hex: string) =>
    call<CaseDetail>("GET", `/cases/${encodeURIComponent(kind)}/${encodeURIComponent(hex)}`),
  lock: (kind: string, hex: string) =>
    call<{ lock: Lock }>("POST", `/cases/${encodeURIComponent(kind)}/${encodeURIComponent(hex)}/lock`, {}),
  unlock: (kind: string, hex: string) =>
    call<{ unlocked: boolean }>("POST", `/cases/${encodeURIComponent(kind)}/${encodeURIComponent(hex)}/unlock`, {}),
  postChecks: (receiptHex: string, checks: Checks) =>
    call<Checks>("POST", `/reports/${encodeURIComponent(receiptHex)}/checks`, checks),
  decide: decideCall,
  noAction: (kind: string, hex: string, reason: number, note: string) => {
    const trimmed = note.trim();
    return call<{ reports_closed: number }>(
      "POST",
      `/cases/${encodeURIComponent(kind)}/${encodeURIComponent(hex)}/no-action`,
      trimmed === "" ? { reason } : { reason, note: trimmed },
    );
  },
  blob: (blobHex: string) => callBytes(`/blob/${encodeURIComponent(blobHex)}`),
  appeals: () => call<Appeals>("GET", "/appeals"),
  log: (targetHex: string, before: number | null) => {
    const q = new URLSearchParams();
    if (targetHex !== "") q.set("target_hex", targetHex);
    if (before !== null) q.set("before", String(before));
    const s = q.toString();
    return call<LogPage>("GET", `/log${s === "" ? "" : `?${s}`}`);
  },
  numbers: (weeks: number) => call<Numbers>("GET", `/numbers?weeks=${weeks}`),
};
