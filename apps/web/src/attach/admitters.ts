// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3.9 — who admitted this device into each group: the
 * account key the sender of its Welcome resolves to (`peekWelcomeFounder`),
 * recorded when the Welcome is joined. A hand-over is taken from an owner or
 * admin, or from this account; `openItemsSenderTrusted` reads this as the
 * admitting account so a plain member's self-service admission (ADR-0050) is
 * trusted too. One sealed `account-kv` row per account.
 */
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import { registerAccountMemory } from "../auth/account-memory";

const ROW = "attach-admitters";
const memory = new Map<string, Map<string, string>>();

async function load(accountId: string): Promise<Map<string, string>> {
  const have = memory.get(accountId);
  if (have !== undefined) return have;
  const m = new Map<string, string>();
  try {
    const raw = (await loadAccountJson(accountId, ROW)) as { a?: Record<string, string> } | null;
    for (const [g, k] of Object.entries(raw?.a ?? {})) if (typeof k === "string") m.set(g, k);
  } catch {
    /* starts empty */
  }
  memory.set(accountId, m);
  return m;
}

/** Record the admitting account of a group this device just joined. */
export async function recordAdmitter(accountId: string, groupIdB64: string, accountKeyB64: string): Promise<void> {
  const m = await load(accountId);
  if (m.get(groupIdB64) === accountKeyB64) return;
  m.set(groupIdB64, accountKeyB64);
  await saveAccountJson(accountId, ROW, { a: Object.fromEntries(m) });
}

/** The admitting account of a group, or `undefined`. */
export async function admitterOf(accountId: string, groupIdB64: string): Promise<string | undefined> {
  return (await load(accountId)).get(groupIdB64);
}

registerAccountMemory("attach-admitters", (ids) => {
  for (const id of ids) memory.delete(id);
});
