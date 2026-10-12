// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the feed has shown the person (ADR-0063 section 5), kept per group in
 * the message log.
 *
 * A group's seen set is every message at or below `lastViewedMessageId` (the
 * open chat marks all it holds, which is what "highest viewed" always meant)
 * plus the ids in `feedSeen`, the messages the feed kept fully on screen for
 * `FEED_SEEN_DWELL_MS`. The person's own and system rows count as seen.
 * When the seen set becomes contiguous past `lastViewedMessageId` —
 * `contiguousSeen` in the core, the one definition every client uses — the
 * watermark moves up to it, the ids below it are dropped from `feedSeen`,
 * and the caller sends the group a read receipt and the other devices a read
 * cursor at the new watermark. It never moves past a message the person did
 * not see.
 *
 * Pure: the log write and the sends live in the Feed route.
 */

import type { PersistedGroup, PersistedMessage } from "../chat/message-log-store";

/** How long an item must be fully on screen to count as seen (chosen). */
export const FEED_SEEN_DWELL_MS = 2000;

/** The core's rule, `contiguousSeen`: the highest id with every listed id
 *  at or below it seen. */
export type ContiguousSeen = (ids: BigInt64Array, seen: BigInt64Array) => bigint | undefined;

function parseId(raw: string | undefined): bigint | null {
  if (raw === undefined || !/^\d{1,19}$/.test(raw)) return null;
  const id = BigInt(raw);
  return id <= 0x7fff_ffff_ffff_ffffn ? id : null;
}

function idOf(m: PersistedMessage): bigint | null {
  return parseId(m.messageId);
}

/** `true` when the person has seen `message` in this slot. */
export function isSeenInSlot(slot: PersistedGroup | undefined, message: PersistedMessage): boolean {
  if (message.direction !== "incoming") return true;
  const id = idOf(message);
  if (id === null) return false;
  const through = parseId(slot?.lastViewedMessageId);
  if (through !== null && id <= through) return true;
  return (slot?.feedSeen ?? []).includes(message.messageId);
}

export interface MarkSeenResult {
  readonly slot: PersistedGroup;
  /** The new watermark when it moved, else `null`. */
  readonly advancedTo: bigint | null;
}

/**
 * Add `ids` to the slot's seen set and move the watermark as far as the set
 * is contiguous. Ids that name no message in the slot are ignored.
 */
export function markSeenInSlot(
  slot: PersistedGroup,
  ids: readonly string[],
  contiguous: ContiguousSeen,
): MarkSeenResult {
  const present = new Set(slot.messages.map((m) => m.messageId));
  const feedSeen = new Set((slot.feedSeen ?? []).filter((id) => present.has(id)));
  for (const id of ids) if (present.has(id) && parseId(id) !== null) feedSeen.add(id);

  const through = parseId(slot.lastViewedMessageId);
  const all: bigint[] = [];
  const seen: bigint[] = [];
  for (const m of slot.messages) {
    const id = idOf(m);
    if (id === null) continue;
    all.push(id);
    const isSeen =
      m.direction !== "incoming" ||
      (through !== null && id <= through) ||
      feedSeen.has(m.messageId);
    if (isSeen) seen.push(id);
  }
  const watermark = contiguous(BigInt64Array.from(all), BigInt64Array.from(seen));
  const advancedTo =
    watermark !== undefined && (through === null || watermark > through) ? watermark : null;
  const newThrough = advancedTo ?? through;

  const kept = [...feedSeen].filter((id) => {
    const n = parseId(id);
    return n !== null && (newThrough === null || n > newThrough);
  });
  const { feedSeen: _drop, ...rest } = slot;
  return {
    slot: {
      ...rest,
      ...(newThrough === null ? {} : { lastViewedMessageId: String(newThrough) }),
      ...(kept.length === 0 ? {} : { feedSeen: kept }),
    },
    advancedTo,
  };
}
