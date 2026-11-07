// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One PublisherGroup handle per channel for the whole session (ADR-0087
 * section 12: the rotation and the co-publisher knock poll are app-wide).
 *
 * Load-bearing, as the retired Manage screen said: a rotation and an approve
 * both advance the group's epoch durably, and two handles in one session each
 * cache their own epoch, so the next commit on a stale one would fork the
 * group. Every caller here shares the one handle, and every commit runs under
 * the per-group MLS lock.
 *
 * The DS proves this journal's requests under the group's access key for as
 * long as the handle is held (`registerPublisherGroupAccess`).
 */

import type { Wasm } from "../wasm";
import { base64Encode } from "../auth/account-id";
import { withGroupMlsLock } from "../mls/group-lock";
import { awaitChannelsHydration } from "../state/channels-state";
import { openPublisherGroup, registerPublisherGroupAccess } from "./copublisher";
import type { WasmIdentity } from "./composer";

type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;

interface Held {
  readonly identity: WasmIdentity;
  readonly group: PublisherGroup;
  readonly unregister: () => void;
}

const held = new Map<string, Held>();
const opening = new Map<string, Promise<Held>>();

async function open(identity: WasmIdentity, idHex: string): Promise<Held> {
  const have = held.get(idHex);
  if (have !== undefined && have.identity === identity) return have;
  if (have !== undefined) release(idHex);
  const pending = opening.get(idHex);
  if (pending !== undefined) return pending;
  const p = (async (): Promise<Held> => {
    // The provider is restored by the hydration; opening before it would
    // found a second, forked group.
    await awaitChannelsHydration();
    const group = await openPublisherGroup(identity, idHex);
    const unregister = registerPublisherGroupAccess(identity, group);
    const h: Held = { identity, group, unregister };
    held.set(idHex, h);
    return h;
  })();
  opening.set(idHex, p);
  try {
    return await p;
  } finally {
    opening.delete(idHex);
  }
}

/** Run `fn` with the channel's shared PublisherGroup handle, under the
 *  per-group MLS lock. Never free the handle in `fn`. */
export async function withPublisherGroup<T>(
  identity: WasmIdentity,
  idHex: string,
  fn: (group: PublisherGroup) => Promise<T>,
): Promise<T> {
  const h = await open(identity, idHex);
  return withGroupMlsLock(base64Encode(h.group.groupId()), () => fn(h.group));
}

/** The shared handle without the lock (for a caller that takes it itself,
 *  such as the approve, which locks inside). */
export async function sharedPublisherGroup(identity: WasmIdentity, idHex: string): Promise<PublisherGroup> {
  return (await open(identity, idHex)).group;
}

function release(idHex: string): void {
  const h = held.get(idHex);
  if (h === undefined) return;
  held.delete(idHex);
  // Withdraw the DS access source BEFORE the handle it reads is freed.
  h.unregister();
  try {
    h.group.free();
  } catch {
    /* already freed */
  }
}

/** Drop one channel's handle (the channel ended here). */
export function releasePublisherGroup(idHex: string): void {
  release(idHex);
}

/** Drop every handle (the session ended). */
export function releaseAllPublisherGroups(): void {
  for (const id of [...held.keys()]) release(id);
}
