// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 10 — the groups this device
 * created, for the timeline row "You created this group". The row is part of
 * the conversation: it stays when a second person joins (only the "Only you
 * are here." block goes). Nothing in the group says who created it, so the
 * device that did keeps the mark. One sealed row per account
 * (`storage/account-kv.ts`).
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

const ROW = "groups-created";

const [created, setCreated] = createSignal<ReadonlySet<string>>(new Set());
let account: string | null = null;

export async function openCreatedGroups(accountId: string): Promise<void> {
  if (account === accountId) return;
  account = accountId;
  setCreated(new Set<string>());
  try {
    const raw = await loadAccountJson(accountId, ROW);
    const list = (raw as { g?: unknown } | null)?.g;
    if (account === accountId && Array.isArray(list)) {
      setCreated(new Set(list.filter((x): x is string => typeof x === "string")));
    }
  } catch {
    /* starts empty */
  }
}

/** This device created the group. Durable before it resolves. */
export async function markGroupCreated(accountId: string, groupIdB64: string): Promise<void> {
  await openCreatedGroups(accountId);
  if (account !== accountId) return;
  const next = new Set(created()).add(groupIdB64);
  setCreated(next);
  await saveAccountJson(accountId, ROW, { v: 1, g: [...next] });
}

/** Whether this device created the group. Reactive. */
export function createdHere(groupIdB64: string): boolean {
  return created().has(groupIdB64);
}

registerAccountMemory("groups-created", (ids) => {
  if (account !== null && ids.includes(account)) {
    account = null;
    setCreated(new Set<string>());
  }
});
