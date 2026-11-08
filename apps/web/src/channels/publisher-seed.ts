// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a publisher keeps when a PublisherGroup commit replaces its content
 * seed (ADR-0087 section 15; UX `docs/ux/group-and-channel-privacy.md`
 * section 13): the outgoing seed stays filed under the channel epoch it
 * served, so what was posted under it (the record at creation first) still
 * opens on this device. Without it the publisher's own channel reads
 * "Earlier posts aren't available." after every approve, removal or
 * rotation. Pure; the twin of Android's `outgoingPublisherSeed`.
 */

/** The (epoch, seed) to keep, or `null` when there is nothing to keep: no
 *  previous seed, an unknown or negative epoch, the same seed again, or a
 *  seed already held for that epoch (the first one filed wins). */
export function outgoingPublisherSeed(
  previousSeed: Uint8Array | undefined,
  previousEpoch: number | undefined,
  nextSeed: Uint8Array,
  held: (epoch: number) => boolean,
): { readonly epoch: number; readonly seed: Uint8Array } | null {
  if (previousSeed === undefined || previousEpoch === undefined) return null;
  if (!Number.isInteger(previousEpoch) || previousEpoch < 0) return null;
  if (sameBytes(previousSeed, nextSeed)) return null;
  if (held(previousEpoch)) return null;
  return { epoch: previousEpoch, seed: previousSeed.slice() };
}

/**
 * Whether a publisher's catch-up reads the channel's public seeds: only
 * with the read key held, and not for a channel known to be private (no
 * public seed is filed for one, so the read would only cost a request).
 * A subscriber reads them whenever it holds the read key, as before.
 */
export function readsPublicSeeds(
  isPublisher: boolean,
  hasReadKey: boolean,
  visibility: "public" | "private" | null,
): boolean {
  if (!hasReadKey) return false;
  return !isPublisher || visibility !== "private";
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * Which seed a publisher seals a post with, and under which channel epoch.
 *
 * The composer's seed serves `seedEpoch` (the PublisherGroup epoch it was
 * read at). A co-publisher applies the owner's commit from the group's
 * journal before the owner's reseal moves the channel service to that
 * epoch, and the readers get the new seed only with that reseal. So:
 *
 * - the channel epoch has reached `seedEpoch` (or passed it, when this
 *   device has not applied later commits yet): seal with the composer's
 *   seed under `seedEpoch`, an epoch every reader holds the seed of;
 * - the channel epoch trails it: seal with the seed this device holds for
 *   the channel's current epoch (the one the composer used before the
 *   commit), under that epoch; never under an epoch not yet committed;
 * - it trails and no seed is held for it (a reload in that window): no
 *   post now (`null`); the caller reports a failure the person can retry.
 *
 * Keys with no `seedEpoch` keep the old rule: the channel's current epoch.
 */
export function composerSeedFor(
  keys: { readonly seed: Uint8Array; readonly seedEpoch?: number },
  channelEpoch: number | undefined,
  held: (epoch: number) => Uint8Array | undefined,
): { readonly epoch: number; readonly seed: Uint8Array } | null {
  const m = keys.seedEpoch;
  if (m === undefined) return { epoch: channelEpoch ?? 0, seed: keys.seed };
  if (channelEpoch === undefined || channelEpoch >= m) return { epoch: m, seed: keys.seed };
  const old = held(channelEpoch);
  return old === undefined ? null : { epoch: channelEpoch, seed: old };
}
