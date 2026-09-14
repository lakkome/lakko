// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One batch of the DeviceGroup journal receive loop, kept free of DOM, wasm
 * and IndexedDB so its ordering rules can be tested.
 *
 * ## What it fixes
 *
 * The loop in `DeviceGroupSyncContext.tsx` started every page lifetime at
 * cursor 0. Every row this account's DeviceGroup had ever carried was fetched
 * again and handed to OpenMLS on each reload — and every one of them failed:
 * this device's own rows as `CannotDecryptOwnMessage`, every other device's
 * rows because the generation they were sealed at was consumed the first time
 * round. The warnings grew with the journal and told the user nothing.
 *
 * Two changes, each covering what the other cannot:
 *
 *   - **The cursor is persisted** with the DeviceGroup snapshot, in the same
 *     IndexedDB write (`device-group-snapshot-store.ts`), and read back as the
 *     starting position. Written together means the cursor can never be
 *     recorded ahead of the state it describes — the C-67 direction the chat
 *     receive path pins in `chat/durable-cursor.ts`: the cursor may lag the
 *     durable state, never lead it. A lagging cursor re-reads a few rows; a
 *     leading one would skip another device's rows for good.
 *   - **This device's own publishes are skipped by id.** The DS does not
 *     de-duplicate, so every draft / flags / read-cursor message this device
 *     sends comes straight back on the next poll. Matching the DS id the
 *     publish returned is exact: a row from ANY other device has an id this
 *     device never received from a publish, so it is never skipped.
 *
 * ## The rules
 *
 *   1. Every row moves the batch cursor, applied or not — a row that can never
 *      apply must not wedge the loop.
 *   2. An own echo is never handed to `apply`.
 *   3. Any other row is always handed to `apply`; a throw is reported and the
 *      batch continues.
 *   4. A non-empty batch is persisted together with its final cursor. Only if
 *      that persist succeeds does the loop's position move and do the echo ids
 *      it matched get consumed; on failure both are held, so the rows come
 *      back and are skipped again rather than being handed to OpenMLS.
 */

/** One DeviceGroup journal row, as the DS returns it. */
export interface DeviceJournalRow {
  readonly id: number;
  readonly payload: Uint8Array;
}

export interface DeviceJournalBatchDeps {
  /** `true` when this device published the row with this DS id. */
  readonly isOwnEcho: (id: number) => boolean;
  /** Apply one row. May throw; the batch continues. */
  readonly apply: (row: DeviceJournalRow) => Promise<void>;
  /** Persist the DeviceGroup snapshot together with `cursor`. MUST reject on a
   *  failed write. */
  readonly persist: (cursor: number) => Promise<void>;
  /** Observes a row `apply` threw on. */
  readonly onApplyError?: (row: DeviceJournalRow, err: unknown) => void;
  /** Observes a failed persist. */
  readonly onPersistError?: (err: unknown) => void;
}

export interface DeviceJournalBatchResult {
  /** Where the next fetch starts. Equal to the starting cursor when the batch
   *  was empty or its persist failed. */
  readonly nextCursor: number;
  /** Whether the snapshot + cursor reached disk. `false` for an empty batch. */
  readonly persisted: boolean;
  /** Own-echo ids the batch skipped. The caller forgets them only when
   *  `persisted` is true. */
  readonly skippedOwnEchoes: readonly number[];
  /** How many rows were handed to `apply`. */
  readonly applied: number;
}

/** Run one batch under the rules in the module header. Never rejects. */
export async function applyDeviceJournalBatch(
  rows: readonly DeviceJournalRow[],
  startCursor: number,
  deps: DeviceJournalBatchDeps,
): Promise<DeviceJournalBatchResult> {
  if (rows.length === 0) {
    return { nextCursor: startCursor, persisted: false, skippedOwnEchoes: [], applied: 0 };
  }
  let cursor = startCursor;
  let applied = 0;
  const skippedOwnEchoes: number[] = [];
  for (const row of rows) {
    // A cursor never moves backward, even if a DS batch were out of order.
    if (row.id > cursor) cursor = row.id;
    if (deps.isOwnEcho(row.id)) {
      skippedOwnEchoes.push(row.id);
      continue;
    }
    applied += 1;
    try {
      await deps.apply(row);
    } catch (err) {
      deps.onApplyError?.(row, err);
    }
  }
  try {
    await deps.persist(cursor);
  } catch (err) {
    deps.onPersistError?.(err);
    return { nextCursor: startCursor, persisted: false, skippedOwnEchoes, applied };
  }
  return { nextCursor: cursor, persisted: true, skippedOwnEchoes, applied };
}

/**
 * The position a fresh loop starts from: the cursor stored with the snapshot,
 * or 0 when there is none (a first run, a device that never persisted one) or
 * the stored value is not a usable journal position.
 */
export function startingCursor(stored: number | null | undefined): number {
  if (stored === null || stored === undefined) return 0;
  if (!Number.isSafeInteger(stored) || stored < 0) return 0;
  return stored;
}
