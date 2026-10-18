// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 (order of 2026-10-17) - the journal readability rule, applied
 * while the DeviceGroup receive loop reads a batch.
 *
 * A claim or link-founded note COUNTS only if the epoch it was sealed at (its
 * public framing, `epochOfMessage`) equals the DeviceGroup journal's epoch at
 * its row: a commit that landed between its seal and its row leaves every
 * other device past that epoch, unable to read it. Another device's row this
 * device could open counted by definition; this device's OWN rows (which it
 * cannot open) are judged here, from the stored sealed bytes and the row's
 * position, and the commits are fed in so the journal's epoch is known at
 * every row. The rule itself is the core's (`JournalEpochs`); this module
 * only feeds it and keeps its record next to the claim record.
 */

import type { LinkClaimRecord, StoredJournalEpochs } from "./link-claims";

/** The slice of the core's `JournalEpochs` this module drives. */
export interface JournalEpochsLike {
  readonly base: bigint;
  commitRowIds(): ArrayLike<bigint>;
  commitEpochsAfter(): ArrayLike<bigint>;
  rowCounts(rowId: bigint, sealedEpoch: bigint): boolean;
  observe(rowId: bigint, sealedEpoch: bigint, epochAfter: bigint, groupEpochAfter: bigint): boolean;
  free?(): void;
}

export interface JournalReadabilityDeps {
  readonly record: LinkClaimRecord;
  readonly groupIdHex: string;
  epochOf(mls: Uint8Array): bigint | undefined;
  epochAfter(mls: Uint8Array): bigint | undefined;
  /** The DeviceGroup's epoch now. */
  groupEpoch(): bigint;
  newEpochs(base: bigint): JournalEpochsLike;
  fromParts(base: bigint, ids: BigUint64Array, afters: BigUint64Array): JournalEpochsLike;
  hex(bytes: Uint8Array): string;
}

export interface JournalReadability {
  /** Before processing a row: when its bytes are one of this device's own
   *  sealed claims or notes, judge it (counts or straddled) and record the
   *  answer. Returns which it was, or `null`. */
  judgeOwn(rowId: number, mls: Uint8Array): "claim" | "note" | null;
  /** After a row was processed (or skipped as this device's own): a commit
   *  that took effect moves the journal's epoch. */
  observe(rowId: number, mls: Uint8Array): void;
  /** Persist anything not yet persisted and release the core object. */
  dispose(): void;
}

function toStored(groupIdHex: string, e: JournalEpochsLike): StoredJournalEpochs {
  return {
    groupIdHex,
    base: e.base.toString(),
    commitRowIds: Array.from(e.commitRowIds(), (v) => v.toString()),
    commitEpochsAfter: Array.from(e.commitEpochsAfter(), (v) => v.toString()),
  };
}

/** Start reading a batch. The record is taken from the claim record, or
 *  started at the group's epoch when none is held for this DeviceGroup. */
export function journalReadability(d: JournalReadabilityDeps): JournalReadability {
  const stored = d.record.journal(d.groupIdHex);
  const epochs =
    stored === null
      ? d.newEpochs(d.groupEpoch())
      : d.fromParts(
          BigInt(stored.base),
          BigUint64Array.from(stored.commitRowIds.map((v) => BigInt(v))),
          BigUint64Array.from(stored.commitEpochsAfter.map((v) => BigInt(v))),
        );
  let unsaved = stored === null;
  const save = (): void => {
    d.record.setJournal(toStored(d.groupIdHex, epochs));
    unsaved = false;
  };
  // A fresh record is kept at once: its start must not move with the group's
  // epoch on later batches (an own commit merged early would then make an own
  // claim sealed before it look straddled). `dispose` retries a failed write.
  if (unsaved) {
    try {
      save();
    } catch {
      // kept unsaved; `dispose` tries again
    }
  }
  return {
    judgeOwn(rowId, mls) {
      if (!d.record.hasUnmatchedWires()) return null;
      const sealed = d.epochOf(mls);
      const counts = sealed !== undefined && epochs.rowCounts(BigInt(rowId), sealed);
      return d.record.judgeOwnRow(d.hex(mls), rowId, counts);
    },
    observe(rowId, mls) {
      const sealed = d.epochOf(mls);
      const after = d.epochAfter(mls);
      if (sealed === undefined || after === undefined || after <= sealed) return;
      if (epochs.observe(BigInt(rowId), sealed, after, d.groupEpoch())) save();
    },
    dispose() {
      try {
        if (unsaved) save();
      } finally {
        epochs.free?.();
      }
    },
  };
}
