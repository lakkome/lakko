// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 6 - who this account blocked, as every receive path on
 * this page reads it.
 *
 * The register itself is the synced state's (`lakko-sync` blocked register,
 * carried between the account's own devices like folders); the
 * `DeviceGroupSyncProvider` owns it, publishes what it holds here on every
 * fold ({@link publishBlocked}) and installs the writer ({@link
 * installBlockWriter}). This module is the read side the chat client, the
 * mailbox, the call paths and the list counts consult, so none of them needs
 * the provider in its component tree.
 *
 * Keys are the blocked person's 32-byte ACCOUNT signature key, standard
 * base64: the person key the receive paths already resolve a sender to
 * (C-68), so a lookup is a set membership.
 */

import { createSignal, type Accessor } from "solid-js";

/** One blocked account, most recent first. */
export interface BlockedEntry {
  /** The account signature key, standard base64. */
  readonly keyB64: string;
  /** When it was blocked, wall-clock milliseconds. */
  readonly blockedAtMs: number;
}

const [entries, setEntries] = createSignal<readonly BlockedEntry[]>([]);
const [keys, setKeys] = createSignal<ReadonlySet<string>>(new Set());

/** Reactive: every blocked account, most recently blocked first. */
export const blockedEntries: Accessor<readonly BlockedEntry[]> = entries;

/** Reactive: the blocked account keys (standard base64). */
export const blockedKeys: Accessor<ReadonlySet<string>> = keys;

/** Reactive: is the account `keyB64` blocked. `undefined` is never blocked. */
export function isBlockedKeyB64(keyB64: string | null | undefined): boolean {
  if (keyB64 === null || keyB64 === undefined || keyB64.length === 0) return false;
  return keys().has(keyB64);
}

/** The provider's publish: the register as it now stands. Pure replace. */
export function publishBlocked(next: readonly BlockedEntry[]): void {
  const held = entries();
  if (
    next.length === held.length &&
    next.every((e, i) => e.keyB64 === held[i]?.keyB64 && e.blockedAtMs === held[i]?.blockedAtMs)
  ) {
    return;
  }
  setEntries(next);
  setKeys(new Set(next.map((e) => e.keyB64)));
}

/** Sign-out and account switch: nobody is blocked until the next fold. */
export function clearBlocked(): void {
  publishBlocked([]);
}

/** The write side the provider installs: fold the register here, keep it on
 *  disk and send it to the account's other devices. `false` when nothing
 *  changed or the synced state is not loaded. */
export type BlockWriter = (accountKey: Uint8Array, blocked: boolean) => Promise<boolean>;

let writer: BlockWriter | null = null;

/** Install (or with `null` remove) the provider's writer. */
export function installBlockWriter(next: BlockWriter | null): void {
  writer = next;
}

/** The synced state is not loaded on this page yet: nothing was written. */
export class BlockRegisterUnavailableError extends Error {
  override name = "BlockRegisterUnavailableError";
  constructor() {
    super("the block register is not loaded");
  }
}

/** Write a block or an unblock through the installed writer. Throws
 *  {@link BlockRegisterUnavailableError} when none is installed. */
export async function writeBlock(accountKey: Uint8Array, blocked: boolean): Promise<boolean> {
  if (writer === null) throw new BlockRegisterUnavailableError();
  return writer(accountKey, blocked);
}

/** The flat `blockedAccounts()` bytes and `blockedAccountTimes()` read into
 *  entries; a length that is not whole keys reads as nothing. Pure. */
export function entriesFromFlat(
  flat: Uint8Array,
  times: ArrayLike<bigint | number>,
  encode: (bytes: Uint8Array) => string,
): BlockedEntry[] {
  if (flat.length % 32 !== 0) return [];
  const out: BlockedEntry[] = [];
  for (let i = 0; i * 32 < flat.length; i += 1) {
    const t = times[i];
    out.push({
      keyB64: encode(flat.subarray(i * 32, i * 32 + 32)),
      blockedAtMs: t === undefined ? 0 : Number(t),
    });
  }
  return out;
}
