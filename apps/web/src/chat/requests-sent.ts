// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 6.2 — the requests to join
 * this device sent, kept until the group appears (joined) or the person
 * dismisses it. A request cannot be withdrawn and is never said to be refused:
 * under a day it is "Waiting for an admin.", after that "No answer yet." with
 * `Ask again`. A device asks once per group per day (the rule is
 * `./ask-again.ts`, shared with the `/g/` page and the publisher card).
 * Recorded by a request from search (`search/ConversationCard.tsx`) and by one
 * from a `/g/` link (`routes/JoinGroup.tsx`).
 *
 * One sealed row per account (`storage/account-kv.ts`).
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import type { AvatarData } from "../profile/avatar-model";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import { ASK_AGAIN_AFTER_MS, askAgainState, type AskAgainState } from "./ask-again";

/** A request may be asked again after this long (UX 6.2): a day. */
export { ASK_AGAIN_AFTER_MS };

export interface SentRequest {
  /** The group id, base64. */
  readonly idB64: string;
  /** The group's name; empty when a link carried none. */
  readonly name: string;
  /** The group's `@name` without the `@`; empty when unknown (a link). */
  readonly handle: string;
  readonly sentAtMs: number;
  readonly avatar: AvatarData | null;
  /** The group's knock public key and its version, for `Ask again`; empty
   *  when a link carried none. */
  readonly knockKeyB64: string;
  readonly knockVersion: number;
  /** A request from a `/g/` link: the capability the link opened to and its
   *  ledger id, base64, so `Ask again` goes through the link again. */
  readonly capabilityWireB64?: string | undefined;
  readonly blobIdB64?: string | undefined;
}

const ROW = "requests-sent";

const [rows, setRows] = createSignal<readonly SentRequest[]>([]);
let account: string | null = null;

/** The line a request shows (UX 6.2). */
export function sentRequestState(r: SentRequest, nowMs: number): AskAgainState {
  return askAgainState(r.sentAtMs, nowMs);
}

/** Whether this device may ask again now. */
export function mayAskAgain(r: SentRequest | undefined, nowMs: number): boolean {
  return r === undefined || askAgainState(r.sentAtMs, nowMs) === "no-answer";
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

/**
 * The rows left once `idB64`'s requests sent at or before `upToMs` are
 * forgotten: the group landed on this device at `upToMs` (the request was
 * answered), or this account's membership in it ended then. A request sent
 * after `upToMs` is a new one and stays. Pure.
 *
 * Before 2026-11-23 a row went only when the chat list rendered with the group
 * in it, so a joined group's row could outlive the membership: after `Leave
 * group` the invite page opened on the old row, showed "Request sent." and
 * posted nothing.
 */
export function sentRequestsAfter(
  list: readonly SentRequest[],
  idB64: string,
  upToMs: number,
): SentRequest[] {
  return list.filter((r) => !(r.idB64 === idB64 && r.sentAtMs <= upToMs));
}

/** `sentRequestsAfter` on the bound account's rows. */
export function forgetRequestsSent(idB64: string, upToMs: number): void {
  const kept = sentRequestsAfter(rows(), idB64, upToMs);
  if (kept.length === rows().length) return;
  setRows(kept);
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
