// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 item 8 - a channel this account took back from its
 * owned-channels list after a key reset (`owned/owned-runtime.ts`), until
 * its PublisherGroup is founded again here.
 *
 * The reset lost what the owner's devices kept about the channel; the list
 * entry brings back its read key (a public channel), its admission seeds
 * (a private one) and its newest epoch seeds. Who reads the channel is
 * still on the channels service. At the retake the device records which
 * subscriber rows stay let in (`throughSeq`), and the re-founding (ADR-0090
 * decision A, `publisher-devices.ts`) reads it as the channel's admission
 * mark, set by this device:
 *
 * - a private channel with listed admission seeds: `0`. Every row is judged
 *   again by its ticket under those seeds; a row with no valid ticket is
 *   not resealed to and the admission pass removes it. A reader let in
 *   before the channel went private carries no ticket and goes the same
 *   way: the list does not carry the old admission mark.
 * - any other channel (public, or of unknown type with nothing listed to
 *   judge a row with): the service's highest subscriber row at the retake.
 *   Every row up to it stays a reader and the reseal reaches each of them.
 *   Cost, stated: a row the service itself added before the retake is let
 *   in too.
 *
 * The mark goes once the group is founded again and the channel's type is
 * known (`settleRecoveredChannels`).
 */

interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function storage(): KeyValueStorage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

function keyOf(accountIdB64: string): string {
  return `lakko:channel-recovered:${accountIdB64}`;
}

/** One recovered channel. */
export interface RecoveredChannel {
  /** The admission mark the re-founding starts from: the highest subscriber
   *  row the service listed at the retake, or 0 when every row is judged
   *  again by its ticket. */
  readonly throughSeq: number;
}

/** Every recovered channel of the account. */
export function readRecoveredChannels(
  accountIdB64: string,
  s: KeyValueStorage | undefined = storage(),
): Record<string, RecoveredChannel> {
  try {
    const raw = s?.getItem(keyOf(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as Record<string, Partial<RecoveredChannel>>;
    const out: Record<string, RecoveredChannel> = {};
    for (const [id, v] of Object.entries(parsed)) {
      if (typeof v.throughSeq === "number" && Number.isSafeInteger(v.throughSeq) && v.throughSeq >= 0) {
        out[id] = { throughSeq: v.throughSeq };
      }
    }
    return out;
  } catch {
    return {};
  }
}

/** Record one (a later recovery of the same channel raises the mark). */
export function markRecoveredChannel(
  accountIdB64: string,
  channelIdHex: string,
  throughSeq: number,
  s: KeyValueStorage | undefined = storage(),
): void {
  const all = readRecoveredChannels(accountIdB64, s);
  all[channelIdHex] = { throughSeq: Math.max(throughSeq, all[channelIdHex]?.throughSeq ?? 0) };
  try {
    s?.setItem(keyOf(accountIdB64), JSON.stringify(all));
  } catch {
    /* the access record carries the mark too */
  }
}

/** The type is settled: forget it. */
export function clearRecoveredChannel(
  accountIdB64: string,
  channelIdHex: string,
  s: KeyValueStorage | undefined = storage(),
): void {
  const all = readRecoveredChannels(accountIdB64, s);
  if (all[channelIdHex] === undefined) return;
  delete all[channelIdHex];
  try {
    if (Object.keys(all).length === 0) s?.removeItem(keyOf(accountIdB64));
    else s?.setItem(keyOf(accountIdB64), JSON.stringify(all));
  } catch {
    /* settled again next pass */
  }
}

/** Wipe. */
export function clearRecoveredChannels(accountIdB64: string, s: KeyValueStorage | undefined = storage()): void {
  try {
    s?.removeItem(keyOf(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}

/**
 * The admission mark a re-founding keeps: the recovery's mark for a
 * recovered channel (every reader at recovery stays let in, so the reseal
 * reaches them), else 0 (the re-founding of a channel this device simply
 * lost judges every row again). Pure.
 */
export function refoundAdmittedThrough(recovered: RecoveredChannel | undefined): number {
  return recovered?.throughSeq ?? 0;
}

/** The highest subscriber row of a listing (0 for none). Pure. */
export function highestSeq(entries: readonly { readonly seq: number }[], held = 0): number {
  return entries.reduce((m, e) => (Number.isSafeInteger(e.seq) && e.seq > m ? e.seq : m), held);
}

/**
 * The type a recovered channel takes from its newest metadata record:
 * `public` / `private` when the record says, `null` while none is held.
 * Pure.
 */
export function recoveredVisibility(metaListed: boolean | undefined): "public" | "private" | null {
  if (metaListed === undefined) return null;
  return metaListed ? "public" : "private";
}
