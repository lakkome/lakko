// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Replayed PublisherGroup Welcomes (lakko-security F2 / F3, 2026-11-15).
 *
 * A device's KeyPackage is last-resort, so the Delivery Service can hand a
 * device an OLD owner-signed Welcome for a channel's PublisherGroup again.
 * Two durable facts per channel, kept in the channel's access record (so
 * they go with the channel and with the account wipe), refuse it:
 *
 * - the hashes (SHA-256 of the Welcome bytes) of the last
 *   {@link JOINED_WELCOMES_KEPT} Welcomes this device JOINED: a Welcome this
 *   device already joined is old by definition, while a channel founded again
 *   elsewhere sends a new one;
 * - the epoch of the last inactive group this device deleted (before an
 *   entry from the saved copy): a join with no held group must land above
 *   it, unless the channel is under reset suspicion (a channel founded again
 *   starts at a low epoch).
 */
import type { ChannelAccessRecord } from "./channel-access-record";

/** How many joined-Welcome hashes a channel keeps (chosen). */
export const JOINED_WELCOMES_KEPT = 16;

/** Lower-case hex SHA-256 of a Welcome's bytes. */
export async function welcomeHashHex(welcome: Uint8Array): Promise<string> {
  const copy = new Uint8Array(welcome.length);
  copy.set(welcome);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", copy.buffer));
  let out = "";
  for (const b of digest) out += b.toString(16).padStart(2, "0");
  return out;
}

/** Whether this device already joined the Welcome whose hash is `hashHex`. */
export function welcomeAlreadyJoined(record: ChannelAccessRecord, hashHex: string): boolean {
  return (record.joinedWelcomeHashes ?? []).includes(hashHex);
}

/** The record with `hashHex` noted as joined, keeping the newest
 *  {@link JOINED_WELCOMES_KEPT}. */
export function withJoinedWelcome(record: ChannelAccessRecord, hashHex: string): ChannelAccessRecord {
  const held = (record.joinedWelcomeHashes ?? []).filter((h) => h !== hashHex);
  return { ...record, joinedWelcomeHashes: [...held, hashHex].slice(-JOINED_WELCOMES_KEPT) };
}

/** The record with the epoch of an inactive group this device just deleted
 *  (the newest deletion replaces an older one). */
export function withDeletedGroupEpoch(record: ChannelAccessRecord, epoch: bigint): ChannelAccessRecord {
  return { ...record, deletedGroupEpoch: Number(epoch) };
}

/**
 * Whether a join that replaced NO held group is refused as a rollback: the
 * joined group's epoch is not above the epoch of the last group this device
 * deleted, and the channel is not under reset suspicion. Pure.
 */
export function joinBelowDeletedGroup(input: {
  readonly joinedEpoch: bigint;
  readonly deletedGroupEpoch: number | undefined;
  readonly resetSuspected: boolean;
}): boolean {
  if (input.resetSuspected || input.deletedGroupEpoch === undefined) return false;
  return !(input.joinedEpoch > BigInt(input.deletedGroupEpoch));
}

/** What a landing path knows about a Welcome before it joins it. */
export interface WelcomeReplayGuard {
  /** This device already joined these exact bytes. */
  readonly alreadyJoined: boolean;
  /** The epoch of the last inactive group this device deleted, if any. */
  readonly deletedGroupEpoch: number | undefined;
}

/** No guard (tests and paths with no record). */
export const NO_REPLAY_GUARD: WelcomeReplayGuard = { alreadyJoined: false, deletedGroupEpoch: undefined };

/** The guard for a Welcome against a channel's record. */
export function replayGuardOf(record: ChannelAccessRecord, hashHex: string): WelcomeReplayGuard {
  return { alreadyJoined: welcomeAlreadyJoined(record, hashHex), deletedGroupEpoch: record.deletedGroupEpoch };
}
