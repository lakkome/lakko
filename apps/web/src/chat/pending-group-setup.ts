// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The hand-off from the New-group screen to the chat view.
 *
 * Creating a group is one Welcome to the first member; every further member is
 * an add on the live group, and the chat view already owns that path with all
 * its obligations (ADR-0040 outbox, ADR-0045 proposal / sweep, echo
 * suppression, push-routing redistribution). So the New-group screen creates
 * the group with the first pick, parks the rest here, and opens the chat,
 * which takes the list once on mount and runs the adds through its own
 * machinery. In memory only: a reload before the chat mounted simply leaves
 * the group with its first member, which the members panel can extend.
 */
export interface PendingGroupMember {
  readonly accountIdHex: string;
  readonly signaturePubkeyB64: string;
  /** For the failure notice. */
  readonly label: string;
}

export interface PendingGroupSetup {
  readonly groupIdB64: string;
  readonly members: readonly PendingGroupMember[];
}

let pending: PendingGroupSetup | null = null;

export function setPendingGroupSetup(setup: PendingGroupSetup): void {
  pending = setup;
}

/** Take (and clear) the parked setup for `groupIdB64`, if any. */
export function takePendingGroupSetup(groupIdB64: string): PendingGroupSetup | null {
  if (pending === null || pending.groupIdB64 !== groupIdB64) return null;
  const out = pending;
  pending = null;
  return out;
}

/** 16-byte account id from its lower-case hex form. */
export function accountIdBytesFromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
