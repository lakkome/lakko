// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 6.2 — the requests to join
 * this device sent, kept until the group appears (joined) or the person
 * dismisses it. A request cannot be withdrawn and is never said to be refused:
 * under 7 days it is "Waiting for an admin.", after that "No answer yet." with
 * `Ask again`. A device asks once per group per 7 days.
 *
 * One sealed row per account (`storage/account-kv.ts`).
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import type { AvatarData } from "../profile/avatar-model";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** A request may be asked again after this long (UX 6.2). */
export const ASK_AGAIN_AFTER_MS = 7 * 24 * 60 * 60 * 1000;

export interface SentRequest {
  /** The group id, base64. */
  readonly idB64: string;
  readonly name: string;
  readonly handle: string;
  readonly sentAtMs: number;
  readonly avatar: AvatarData | null;
  /** The group's knock public key and its version, for `Ask again`. */
  readonly knockKeyB64: string;
  readonly knockVersion: number;
}

const ROW = "requests-sent";

const [rows, setRows] = createSignal<readonly SentRequest[]>([]);
let account: string | null = null;

/** The line a request shows (UX 6.2). */
export function sentRequestState(r: SentRequest, nowMs: number): "waiting" | "no-answer" {
  return nowMs - r.sentAtMs >= ASK_AGAIN_AFTER_MS ? "no-answer" : "waiting";
}

/** Whether this device may ask again now. */
export function mayAskAgain(r: SentRequest | undefined, nowMs: number): boolean {
  return r === undefined || nowMs - r.sentAtMs >= ASK_AGAIN_AFTER_MS;
}

function decode(raw: unknown): SentRequest[] {
  if (raw === null || typeof raw !== "object") return [];
  const list = (raw as { r?: unknown }).r;
  if (!Array.isArray(list)) return [];
  return list.filter(
    (x): x is SentRequest =>
      x !== null &&
      typeof x === "object" &&
      typeof (x as SentRequest).idB64 === "string" &&
      typeof (x as SentRequest).sentAtMs === "number",
  );
}

export async function openRequestsSent(accountId: string): Promise<void> {
  if (account === accountId) return;
  account = accountId;
  setRows([]);
  try {
    const raw = await loadAccountJson(accountId, ROW);
    if (account === accountId) setRows(decode(raw));
  } catch {
    /* starts empty */
  }
}

function persist(): void {
  if (account === null) return;
  void saveAccountJson(account, ROW, { v: 1, r: rows() }).catch(() => undefined);
}

/** The requests this device sent, newest first. Reactive. */
export function requestsSent(): readonly SentRequest[] {
  return [...rows()].sort((a, b) => b.sentAtMs - a.sentAtMs);
}

export function sentRequestFor(idB64: string): SentRequest | undefined {
  return rows().find((r) => r.idB64 === idB64);
}

export function recordRequestSent(r: SentRequest): void {
  setRows([...rows().filter((x) => x.idB64 !== r.idB64), r]);
  persist();
}

/** The group appeared in Chats (joined), or the person dismissed it. */
export function dropRequestSent(idB64: string): void {
  if (!rows().some((r) => r.idB64 === idB64)) return;
  setRows(rows().filter((r) => r.idB64 !== idB64));
  persist();
}

registerAccountMemory("requests-sent", (ids) => {
  if (account !== null && ids.includes(account)) {
    account = null;
    setRows([]);
  }
});
