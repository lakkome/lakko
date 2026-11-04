// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - the `/g/` joiner's `reply` grant, kept by an admin
 * until the sweep that admits the joiner writes their Welcome with it.
 *
 * A join request's self-signed attribution carries the joiner's reply grant;
 * the committer - which may be any admin that read the attribution - writes
 * the sweep's Welcome to the joiner's account mailbox under it (C-69). Keyed
 * by the GROUP and the joiner's LEAF signature key (hex), the key the
 * attribution is self-signed with and the key the sweep's swept Add names:
 * a grant belongs to one request, so a refusal in one group never drops the
 * grant another group's request from the same joiner still needs.
 *
 * Kept per account (`storage/account-kv.ts`, sealed under the device key),
 * not in memory only: a sealed knock (ADR-0087 section 5) is read once, the
 * knock cursor moves past it and is persisted, so an approval after a reload
 * or a restart would otherwise find no grant and the Welcome would stay owed
 * for good. A grant is kept until it expires ({@link JOIN_REPLY_LIFETIME_MS},
 * the reply key's own life) or its joiner is refused: NOT when the Welcome is
 * written, since a Welcome re-sent from the commit outbox after a crash, or
 * after a refused write and an address re-check, needs it again. The row is
 * bounded, and goes with the account on Forget device.
 */
import { registerAccountMemory } from "../auth/account-memory";
import { journalHex } from "../ds/access";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** The part of a wasm `ReachGrant` the binding check needs. */
export interface BindingCheckable {
  mailboxPkVerified(ownerAccountPk: Uint8Array): Uint8Array;
}

/**
 * ADR-0078 - whether a join attribution's `reply` grant names the joiner's
 * own mailbox: its binding verifies against the joiner's ACCOUNT key (for a
 * chat group, the account the proposal's leaf is bound to; for a channel
 * co-publisher, the leaf key itself). A grant that fails is not kept: a
 * member could otherwise hand out its own mailbox under the joiner's name and
 * swallow the joiner's Welcome. `null` account key (unresolvable): `false`.
 * Never throws; logs without identifiers.
 */
export function joinReplyBindingVerifies(
  grant: BindingCheckable,
  joinerAccountKey: Uint8Array | null | undefined,
): boolean {
  if (joinerAccountKey === null || joinerAccountKey === undefined) return false;
  try {
    grant.mailboxPkVerified(joinerAccountKey);
    return true;
  } catch {
    // eslint-disable-next-line no-console
    console.warn("ADR-0078: a join reply grant was not kept; its mailbox binding did not verify");
    return false;
  }
}

/** More than the 64 a group holds waiting, for a few groups at once. */
const MAX_REPLIES = 512;
const ROW = "join-replies";

/** How long a kept grant lives: the reply key's own life. */
export const JOIN_REPLY_LIFETIME_MS = 7 * 24 * 60 * 60 * 1000;

interface Kept {
  readonly grant: string;
  readonly atMs: number;
}

let replies = new Map<string, Kept>();
let now: () => number = Date.now;

/** Test hook: the clock. */
export function setJoinReplyClock(clock: () => number): void {
  now = clock;
}

function live(k: Kept): boolean {
  return now() - k.atMs < JOIN_REPLY_LIFETIME_MS;
}
let account: string | null = null;
let opened: Promise<void> = Promise.resolve();
let chain: Promise<void> = Promise.resolve();

/** The key of one request: group and joiner. */
function keyOf(groupId: Uint8Array, joinerSignatureKey: Uint8Array): string {
  return `${journalHex(groupId)}|${journalHex(joinerSignatureKey)}`;
}

function decode(raw: unknown): Map<string, Kept> {
  const out = new Map<string, Kept>();
  const list = (raw as { r?: unknown } | null)?.r;
  if (!Array.isArray(list)) return out;
  for (const e of list) {
    if (e === null || typeof e !== "object") continue;
    const k = (e as { k?: unknown }).k;
    const g = (e as { g?: unknown }).g;
    const t = (e as { t?: unknown }).t;
    // A key without its group is an older row: not this request's.
    if (typeof k !== "string" || typeof g !== "string" || !k.includes("|")) continue;
    // A row without a time reads as kept now (it gets a full lifetime).
    const kept: Kept = { grant: g, atMs: typeof t === "number" && Number.isFinite(t) ? t : now() };
    if (live(kept)) out.set(k, kept);
  }
  return out;
}

/** Load this account's kept grants (idempotent per account). A grant kept
 *  in memory before the load (no account open yet) is kept and saved. */
export function openJoinReplies(accountId: string): Promise<void> {
  if (account === accountId) return opened;
  const early = account === null ? replies : new Map<string, Kept>();
  account = accountId;
  replies = new Map(early);
  opened = loadAccountJson(accountId, ROW).then(
    async (raw) => {
      if (account !== accountId) return;
      const loaded = decode(raw);
      for (const [k, g] of replies) loaded.set(k, g);
      replies = loaded;
      if (early.size > 0) await persist();
    },
    () => undefined,
  );
  return opened;
}

/** The load of the open account's grants; await before {@link joinReplyFor}. */
export function joinRepliesOpened(): Promise<void> {
  return opened;
}

function persist(): Promise<void> {
  const acct = account;
  if (acct === null) return Promise.resolve();
  const body = { v: 1, r: [...replies].map(([k, e]) => ({ k, g: e.grant, t: e.atMs })) };
  const next = chain.then(() => saveAccountJson(acct, ROW, body));
  chain = next.catch(() => undefined);
  return next;
}

/** Keep a joiner's reply grant (the 134-byte wire, base64; ADR-0078),
 *  durably: awaited before the knock cursor moves past the row. */
export async function rememberJoinReply(
  groupId: Uint8Array,
  joinerSignatureKey: Uint8Array,
  grantWireB64: string,
): Promise<void> {
  await opened;
  const key = keyOf(groupId, joinerSignatureKey);
  const held = replies.get(key);
  if (held !== undefined && held.grant === grantWireB64 && live(held)) return;
  for (const [k, e] of replies) if (!live(e)) replies.delete(k);
  replies.delete(key);
  replies.set(key, { grant: grantWireB64, atMs: now() });
  // Oldest first in insertion order: drop from the front.
  while (replies.size > MAX_REPLIES) {
    const first = replies.keys().next().value;
    if (first === undefined) break;
    replies.delete(first);
  }
  await persist();
}

/** The kept reply grant of a joiner, or `null`. */
export function joinReplyFor(groupId: Uint8Array, joinerSignatureKey: Uint8Array): string | null {
  const kept = replies.get(keyOf(groupId, joinerSignatureKey));
  return kept !== undefined && live(kept) ? kept.grant : null;
}

/** The joiner was refused: forget their grant. Never called on admission
 *  (a re-sent Welcome needs the grant; it expires on its own). */
export async function forgetJoinReply(groupId: Uint8Array, joinerSignatureKey: Uint8Array): Promise<void> {
  if (!replies.delete(keyOf(groupId, joinerSignatureKey))) return;
  await persist().catch(() => undefined);
}

/** Drop the grants in memory (the row stays with the account). */
export function clearJoinReplies(): void {
  replies = new Map();
  account = null;
  opened = Promise.resolve();
}

// Sign-out and Forget device: the memory goes; the row is re-read when the
// account lands again, and goes with the account's store on a wipe.
registerAccountMemory(
  "join-replies",
  (ids) => {
    if (account === null || ids.includes(account)) clearJoinReplies();
  },
  { signOut: true },
);
