// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Touching the encrypted `publication` row from outside the runtime that
 * owns it (`ScopedDiscoveryRuntime.tsx`).
 */
import { decodePublication, encodePublication, staleEveryoneNote } from "./publication-model";
import { loadReachRow, saveReachRow } from "./reach-store";

/** A restore: nothing says what the row remembers still stands at discovery,
 *  so the next pass publishes (or withdraws) again. */
export async function resetEveryoneNote(accountIdB64: string): Promise<void> {
  const state = decodePublication(await loadReachRow(accountIdB64, "publication"));
  const next = staleEveryoneNote(state);
  if (next !== state) await saveReachRow(accountIdB64, "publication", encodePublication(next));
}
