// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 phase B — rules for the application messages on a channel's
 * PublisherGroup journal (admission seeds, the read key, step-downs, the
 * ADR-0059 bindings), kept out of the Solid component so they are tested.
 */
import type { PublisherEntry } from "./channel-access-record";

/** What an owner device does with a decoded co-publisher binding. */
export type BindingVerdict = "file" | "drop";

/** The facts the binding rule looks at, as standard base64 / lower hex. */
export interface BindingFacts {
  /** This device's account owns the channel. */
  readonly isOwner: boolean;
  /** The account key the MLS sender resolved to at receipt (C-68), or
   *  `undefined` for a message with no member sender. */
  readonly senderB64: string | undefined;
  /** The channel owner's account key, or `null` when unknown. */
  readonly ownerKeyB64: string | null;
  /** The account the binding is signed by. */
  readonly bindingAccountB64: string;
  /** The channel the binding names. */
  readonly bindingChannelHex: string;
  /** The channel whose journal carried it. */
  readonly channelIdHex: string;
}

/**
 * ADR-0059 — an owner device files a binding as the sender's appointed key
 * only when the binding's account IS the MLS sender resolved at receipt, the
 * binding names this channel, and the sender is not the owner. Anything else
 * is dropped: a member cannot file a key for someone else.
 */
export function bindingVerdict(f: BindingFacts): BindingVerdict {
  if (!f.isOwner || f.senderB64 === undefined) return "drop";
  if (f.ownerKeyB64 === null || f.senderB64 === f.ownerKeyB64) return "drop";
  if (f.bindingAccountB64 !== f.senderB64) return "drop";
  if (f.bindingChannelHex.toLowerCase() !== f.channelIdHex.toLowerCase()) return "drop";
  return "file";
}

/** The publisher entries with `accountKeyB64`'s appointed key set to
 *  `appointedB64` (an entry is added when there is none). The same array
 *  when nothing changes. */
export function withAppointedKey(
  entries: readonly PublisherEntry[],
  accountKeyB64: string,
  appointedB64: string,
): readonly PublisherEntry[] {
  const have = entries.find((p) => p.accountKeyB64 === accountKeyB64);
  if (have?.copublisherKeyB64 === appointedB64) return entries;
  if (have === undefined) {
    return [...entries, { accountKeyB64, copublisherKeyB64: appointedB64, accountIdHex: "", sentVersions: [] }];
  }
  return entries.map((p) => (p.accountKeyB64 === accountKeyB64 ? { ...p, copublisherKeyB64: appointedB64 } : p));
}

/**
 * Publish a PublisherGroup application message only after the provider
 * snapshot that holds its moved sending ratchet is durable: a crash between
 * the two would reuse a ratchet generation (G-34 class). A persist that
 * fails throws and nothing is published.
 */
export async function persistThenPublish(persist: () => Promise<void>, publish: () => Promise<void>): Promise<void> {
  await persist();
  await publish();
}

/**
 * UX 15.2 — what a device does once its publisher leaf is gone (removed by
 * the owner, or after stepping down): a person who was subscribed before
 * becoming a publisher keeps the subscriber view (their subscriber key and
 * seeds were kept); one who never subscribed ends the channel.
 */
export function removedPublisherFallback(holdsSubscriberKey: boolean): "subscriber" | "end" {
  return holdsSubscriberKey ? "subscriber" : "end";
}

/** A publisher link as this device recorded it (the fields the match reads). */
export interface LocalPublisherLink {
  readonly kind: string;
  readonly ledgerIdB64Url: string;
  readonly capabilityB64?: string | undefined;
  readonly maxUses: number;
  readonly used?: boolean | undefined;
  readonly revoked: boolean;
}

/** A publisher link from the account's link register, its sealed capability
 *  already opened (`capabilityB64` `null` when it did not open). */
export interface RegisteredPublisherLink {
  readonly ledgerIdB64Url: string;
  readonly maxUses: number;
  readonly revoked: boolean;
  readonly live: boolean;
  readonly capabilityB64: string | null;
}

/** A publisher link admits one person: the capability's use limit 0. */
export function isOneUse(maxUses: number): boolean {
  return maxUses === 0;
}

/**
 * UX 15.2 — the link a co-publisher request came by, for the owner's
 * automatic approve: this device's record of it, or a register record made on
 * another device. Only a ONE-USE publisher link that is still live counts; a
 * link revoked on any device, a used one, or one that is not one use (a
 * shareable link) matches nothing, so it can never auto-admit a stranger; such
 * a request waits for a person's Approve. `recordIndex` points into `records`
 * (or `-1`).
 */
export function matchPublisherLink(
  cap: string,
  locals: readonly LocalPublisherLink[],
  records: readonly RegisteredPublisherLink[],
): { readonly localLedger: string | null; readonly recordIndex: number } | null {
  const local = locals.find(
    (l) => l.kind === "publisher" && l.capabilityB64 !== undefined && l.capabilityB64 === cap && l.used !== true && !l.revoked,
  );
  if (local !== undefined) {
    if (!isOneUse(local.maxUses)) return null;
    const recordIndex = records.findIndex((r) => r.ledgerIdB64Url === local.ledgerIdB64Url);
    const rec = recordIndex >= 0 ? records[recordIndex] : undefined;
    if (rec !== undefined && (rec.revoked || !isOneUse(rec.maxUses))) return null;
    return { localLedger: local.ledgerIdB64Url, recordIndex };
  }
  const recordIndex = records.findIndex((r) => r.live && !r.revoked && isOneUse(r.maxUses) && r.capabilityB64 === cap);
  return recordIndex < 0 ? null : { localLedger: null, recordIndex };
}
