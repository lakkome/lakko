// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every knock read an owner or admin device makes, handed to whoever
 * announces join requests (`JoinRequestNotifier.tsx`,
 * `docs/ux/group-and-channel-privacy.md` section 8.1).
 *
 * Published by `ChatClient.pollMessages` after every applied poll, from the
 * open conversation and from the background receive of every other one, so
 * this is the one place both paths meet. A read carries the role gate, the
 * whole pending set (which prunes what the device keeps, or clears it when
 * the gate fails) and, when the knock queue was read in that poll, the
 * requests that arrived in it (retained, or asked again while already
 * pending).
 */

/** One knock read of one group. */
export interface JoinRequestRead {
  /** The signed-in account that read it (the knock cursor's owner). */
  readonly accountIdB64: string;
  /** The group, standard base64 of its id. */
  readonly groupIdB64: string;
  /** Joiner signature keys (standard base64) of the requests in this read. */
  readonly arrivedKeysB64: readonly string[];
  /** Every request still waiting after the read. */
  readonly pendingKeysB64: readonly string[];
  /** This device is the group's owner or an admin (the knock-read gate). */
  readonly manages: boolean;
  /** The membership is still active (an ended one announces nothing). */
  readonly active: boolean;
  /** The knock queue was read in this poll. Without it the read only prunes
   *  (`arrivedKeysB64` is empty) and never announces. */
  readonly knocksRead: boolean;
}

type Listener = (read: JoinRequestRead) => void;

const listeners = new Set<Listener>();

/** Hear every knock read. Returns the unsubscribe. */
export function subscribeJoinRequestReads(fn: Listener): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** Hand one read to every listener. A listener that throws is logged and the
 *  others still run; the read itself never fails. */
export function publishJoinRequestRead(read: JoinRequestRead): void {
  for (const fn of listeners) {
    try {
      fn(read);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[chat] join request listener failed", err instanceof Error ? err.name : "");
    }
  }
}

/** The 32-byte keys of a flat `pendingExternalJoins()` read, in base64. */
export function splitPendingJoinKeys(flat: Uint8Array, encode: (b: Uint8Array) => string): string[] {
  const out: string[] = [];
  for (let i = 0; i + 32 <= flat.length; i += 32) out.push(encode(flat.slice(i, i + 32)));
  return out;
}
