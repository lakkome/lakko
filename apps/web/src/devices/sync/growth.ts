// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The growth re-send of ADR-0058 section 5.1, the part that is pure.
 *
 * A device that joins the DeviceGroup cannot read anything sealed before it
 * joined, so the pins, mutes, drafts, read cursors and folders the account
 * already had reach it only if a device that was there re-sends them. Exactly
 * one device should: the one with the lowest leaf among those that were
 * members BEFORE the growth. `shouldSendSnapshot` (Rust, shared with iOS and
 * Android) makes that call; this module computes the two inputs it takes
 * from what the web can observe, and the delay.
 */

/** One device of the DeviceGroup, as `DeviceGroup.devices()` reports it:
 *  its leaf and its signature key (hex or base64 - any stable string). */
export interface LeafMember {
  readonly leaf: number;
  readonly key: string;
}

/** What a membership change looked like. */
export interface Growth {
  /** A device is in the group that was not before. */
  readonly grew: boolean;
  /** Leaves of the devices that were members before AND still are. A device
   *  removed in the same change cannot send anything, so it is not a
   *  candidate. */
  readonly priorLeaves: number[];
}

/**
 * Compare two membership snapshots. Keyed on the signature key, never the
 * leaf: a removal followed by an add can hand a vacated leaf to the newcomer,
 * and a leaf-keyed diff would then see no change at all.
 */
export function detectGrowth(
  before: readonly LeafMember[],
  after: readonly LeafMember[],
): Growth {
  const beforeKeys = new Set(before.map((m) => m.key));
  const afterKeys = new Set(after.map((m) => m.key));
  const grew = after.some((m) => !beforeKeys.has(m.key));
  const priorLeaves = before
    .filter((m) => afterKeys.has(m.key))
    .map((m) => m.leaf)
    .sort((a, b) => a - b);
  return { grew, priorLeaves };
}

/** The longest wait before the re-send (ADR-0058 section 5.1). */
export const SNAPSHOT_MAX_DELAY_MS = 30_000;

/** A random delay in `[0, 30 s)`, so the new device's own first writes are
 *  not raced and two senders rarely collide. `random` is `Math.random` in
 *  the app and fixed in a test. */
export function snapshotDelayMs(random: () => number = Math.random): number {
  const r = random();
  const clamped = Number.isFinite(r) ? Math.min(Math.max(r, 0), 0.999_999) : 0;
  return Math.floor(clamped * SNAPSHOT_MAX_DELAY_MS);
}

// ---------- a change made by another handle on this device ----------------

/** What a one-shot DeviceGroup mutation on this device announces. */
export interface DeviceGroupChangedHere {
  /** For an add: the leaves that were members before it, and this device's
   *  own leaf. Absent for a change that added nobody. */
  readonly growth?: { readonly priorLeaves: number[]; readonly ownLeaf: number };
}

type Listener = (change: DeviceGroupChangedHere) => void;
const listeners = new Set<Listener>();

/**
 * Tell the sync runtime that the DeviceGroup was changed through ANOTHER
 * handle on this device (the pairing flow commits through a handle of its
 * own). Called after the new state is on disk and BEFORE the commit is
 * published, so the runtime's long-lived handle - now an epoch behind - stops
 * before it can see the commit and write its stale state over the new one,
 * then reloads.
 */
export function announceDeviceGroupChangedHere(change: DeviceGroupChangedHere): void {
  for (const l of listeners) l(change);
}

/** Subscribe; returns the unsubscribe. */
export function onDeviceGroupChangedHere(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
