// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/reactions-and-emoji.md` section 12 — unseen reactions: per
 * conversation, the person's own messages others reacted to that have not
 * been on screen since. Drives the round heart button in the conversation and
 * the small heart in the chat list. Local to this device; nothing is sent.
 *
 * Kept in `localStorage` per account (a list of message ids per group, the
 * newest {@link MAX_PER_GROUP}); a convenience, so a blocked storage only
 * costs the button after a reload.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";

const PREFIX = "lakko.unseen-reactions.v1:";
/** Chosen, not measured: more than anyone scrolls back through. */
const MAX_PER_GROUP = 200;

type Rows = Record<string, number[]>;

const [version, setVersion] = createSignal(0);
let account: string | null = null;
let rows: Rows = {};

function load(accountId: string): Rows {
  try {
    const raw = globalThis.localStorage?.getItem(PREFIX + accountId);
    if (raw === null || raw === undefined) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") return {};
    const out: Rows = {};
    for (const [g, ids] of Object.entries(parsed as Record<string, unknown>)) {
      if (!Array.isArray(ids)) continue;
      const list = ids.filter((n): n is number => typeof n === "number" && Number.isFinite(n));
      if (list.length > 0) out[g] = list;
    }
    return out;
  } catch {
    return {};
  }
}

function save(): void {
  if (account === null) return;
  try {
    globalThis.localStorage?.setItem(PREFIX + account, JSON.stringify(rows));
  } catch {
    /* a convenience */
  }
}

function bind(accountId: string): void {
  if (account === accountId) return;
  account = accountId;
  rows = load(accountId);
  setVersion((v) => v + 1);
}

/** Others reacted to the person's own message `messageId` in `groupIdB64`. */
export function markUnseenReaction(accountId: string, groupIdB64: string, messageId: number): void {
  bind(accountId);
  const list = rows[groupIdB64] ?? [];
  if (list.includes(messageId)) return;
  rows = { ...rows, [groupIdB64]: [...list, messageId].sort((a, b) => a - b).slice(-MAX_PER_GROUP) };
  save();
  setVersion((v) => v + 1);
}

/** The message was on screen: its reactions are seen. */
export function markReactionsSeen(accountId: string, groupIdB64: string, messageId: number): void {
  bind(accountId);
  const list = rows[groupIdB64];
  if (list === undefined || !list.includes(messageId)) return;
  const next = list.filter((id) => id !== messageId);
  const copy = { ...rows };
  if (next.length === 0) delete copy[groupIdB64];
  else copy[groupIdB64] = next;
  rows = copy;
  save();
  setVersion((v) => v + 1);
}

/** The unseen message ids of a conversation, oldest first. Reactive. */
export function unseenReactionIds(accountId: string | null, groupIdB64: string): readonly number[] {
  version();
  if (accountId === null) return [];
  bind(accountId);
  return rows[groupIdB64] ?? [];
}

/** Forget a conversation (its membership ended, it was deleted). */
export function forgetUnseenReactions(accountId: string, groupIdB64: string): void {
  bind(accountId);
  if (rows[groupIdB64] === undefined) return;
  const copy = { ...rows };
  delete copy[groupIdB64];
  rows = copy;
  save();
  setVersion((v) => v + 1);
}

registerAccountMemory("unseen-reactions", (ids) => {
  for (const id of ids) {
    try {
      globalThis.localStorage?.removeItem(PREFIX + id);
    } catch {
      /* nothing to drop */
    }
  }
  if (account !== null && ids.includes(account)) {
    account = null;
    rows = {};
    setVersion((v) => v + 1);
  }
});
