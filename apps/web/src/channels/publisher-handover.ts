// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The rules of two PublisherGroup application messages (ADR-0087 sections 7
 * and 15, amended 2026-11-08; UX `docs/ux/group-and-channel-privacy.md`
 * 15.2.1), pure so each is asserted without a group:
 *
 * - the earlier channel seeds the owner's device hands a new publisher
 *   (`lakko/channel-epoch-seeds/v1`): which to send, whether to keep what
 *   arrived, which seeds to add;
 * - the rotate request a co-publisher sends after removing a subscriber
 *   (`lakko/channel-rotate-request/v1`): whether the owner's device acts.
 */

/** How many seeds one hand-over carries at most (the core's
 *  `channelEpochSeedsMax()`). */
export const EPOCH_SEEDS_MAX = 64;

/**
 * The epochs to hand a publisher: the newest `max` of the epochs this device
 * holds a seed for that are not yet recorded as sent to them, ascending.
 * Empty when there is nothing new.
 */
export function epochSeedsToSend(
  held: readonly number[],
  sent: readonly number[],
  max: number = EPOCH_SEEDS_MAX,
): number[] {
  const done = new Set(sent);
  const fresh = [...new Set(held)].filter((e) => Number.isInteger(e) && e >= 0 && !done.has(e));
  fresh.sort((a, b) => b - a);
  return fresh.slice(0, Math.max(0, max)).sort((a, b) => a - b);
}

/**
 * Whether a received hand-over is kept: on this channel's PublisherGroup, on
 * a publisher's device that is not the owner's, addressed to this account,
 * and sent by the channel's owner (the MLS sender resolved at receipt,
 * C-68). Anything else is dropped.
 */
export function keepsEpochSeeds(input: {
  readonly deviceIsOwner: boolean;
  readonly channelIdHex: string;
  readonly envelopeChannelHex: string;
  readonly memberAccountKeyB64: string;
  readonly ownAccountKeysB64: readonly string[];
  readonly senderB64: string | undefined;
  readonly ownerKeyB64: string | null;
}): boolean {
  if (input.deviceIsOwner) return false;
  if (input.envelopeChannelHex !== input.channelIdHex) return false;
  if (!input.ownAccountKeysB64.includes(input.memberAccountKeyB64)) return false;
  if (input.senderB64 === undefined || input.ownerKeyB64 === null) return false;
  return input.senderB64 === input.ownerKeyB64;
}

/** The opened seeds to add: only epochs this device holds no seed for; a
 *  held seed is never overwritten. */
export function epochSeedsToAdd(
  opened: readonly { readonly epoch: number; readonly seed: Uint8Array }[],
  holds: (epoch: number) => boolean,
): { readonly epoch: number; readonly seed: Uint8Array }[] {
  const out: { epoch: number; seed: Uint8Array }[] = [];
  for (const o of opened) {
    if (holds(o.epoch) || out.some((x) => x.epoch === o.epoch)) continue;
    out.push({ epoch: o.epoch, seed: o.seed });
  }
  return out;
}

/**
 * Whether the owner's device takes a rotate request: only the owner's
 * device, only for the channel whose PublisherGroup carried it, and only
 * from a current publisher (a member of the group other than the owner,
 * resolved at receipt). Several before the rotation are one request.
 */
export function takesRotateRequest(input: {
  readonly deviceIsOwner: boolean;
  readonly channelIdHex: string;
  readonly requestChannelHex: string;
  readonly senderB64: string | undefined;
  readonly ownerKeyB64: string | null;
  readonly memberKeysB64: readonly string[];
}): boolean {
  if (!input.deviceIsOwner) return false;
  if (input.requestChannelHex !== input.channelIdHex) return false;
  if (input.senderB64 === undefined) return false;
  if (input.senderB64 === input.ownerKeyB64) return false;
  return input.memberKeysB64.includes(input.senderB64);
}
