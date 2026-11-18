// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3.9 — a device that adopts an open anonymous poll from a
 * hand-over fetches the poll's earlier ballot rows from the group journal
 * itself: a ballot is a journal row sealed under the poll's secret (which the
 * hand-over carried), not an MLS message, so the device can open and verify
 * each one although it reads nothing else sealed before its admission. Rows
 * the journal still holds (its 30-day retention) come from here; older ones
 * came with the hand-over. `withBallotRow` dedups the two by row.
 */
import { receiveBallotRow } from "./receive";

/** The journal read this needs, structurally (`DsClient.fetchGroup`). */
export type JournalPage = (after: number) => Promise<{
  readonly messages: readonly { readonly id: number; readonly payload: Uint8Array; readonly createdAt: string }[];
  readonly hasMore: boolean;
}>;

/** Pages read at most per backfill: a bound on one hand-over's cost. Chosen. */
export const BACKFILL_MAX_PAGES = 200;

/**
 * Read the journal from the poll's own row onward and file every ballot row
 * in it. Returns how many ballot rows were seen. Stops at the end of the
 * journal, on a failed read, or after {@link BACKFILL_MAX_PAGES} pages.
 */
export async function backfillBallots(args: {
  readonly groupId: Uint8Array;
  readonly groupIdB64: string;
  readonly pollMessageId: number;
  readonly page: JournalPage;
  readonly isBallotRow: (payload: Uint8Array) => boolean;
  readonly file?: typeof receiveBallotRow;
}): Promise<number> {
  const file = args.file ?? receiveBallotRow;
  let after = args.pollMessageId;
  let seen = 0;
  for (let i = 0; i < BACKFILL_MAX_PAGES; i++) {
    let res;
    try {
      res = await args.page(after);
    } catch (err) {
      console.warn("[attach] ballot backfill read failed", err instanceof Error ? err.name : "unknown");
      return seen;
    }
    for (const m of res.messages) {
      if (m.id > after) after = m.id;
      if (!args.isBallotRow(m.payload)) continue;
      seen += 1;
      await file(args.groupId, args.groupIdB64, m.id, m.createdAt, m.payload);
    }
    if (!res.hasMore || res.messages.length === 0) return seen;
  }
  return seen;
}
