// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What follows a PublisherGroup commit that a replay after a reload finished
 * (G-50, `replayPublisherCommits`), as the live paths do it: the composer
 * takes the seed of the epoch the reseal landed at (the outgoing one stays
 * under its epoch), and the channel record is issued again under that epoch
 * so a publisher or reader let in later finds the channel's name it can
 * open (ADR-0051 section 4, ADR-0087 section 15). The live approve, removal
 * and rotation do both; a reload between the reseal and these steps used to
 * lose them.
 *
 * One pass per channel, at the newest epoch any of its entries landed at.
 * Never throws: a failed step is logged by kind only and left to the next
 * epoch change.
 */
import type { LandedPublisherCommit } from "./copublisher";

export interface ReplayFollowupPorts {
  /** The content seed of the channel's PublisherGroup now, or `null` when
   *  this device holds no active group for it. */
  seedOf(channelIdHex: string): Promise<Uint8Array | null>;
  /** The composer takes `seed` for channel epoch `epoch`. */
  landed(channelIdHex: string, epoch: number, seed: Uint8Array): void;
  /** Issue the channel record again under the current seed. */
  reissueRecord(channelIdHex: string): Promise<void>;
}

export async function afterReplayedPublisherCommits(
  landed: readonly LandedPublisherCommit[],
  ports: ReplayFollowupPorts,
): Promise<void> {
  const newest = new Map<string, number>();
  for (const l of landed) newest.set(l.channelIdHex, Math.max(newest.get(l.channelIdHex) ?? -1, l.serverTargetEpoch));
  for (const [channelIdHex, epoch] of newest) {
    try {
      const seed = await ports.seedOf(channelIdHex);
      if (seed === null) continue;
      ports.landed(channelIdHex, epoch, seed);
      await ports.reissueRecord(channelIdHex);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[channels] follow-up of a replayed commit failed", err instanceof Error ? err.name : "");
    }
  }
}
