// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Files what the feed showed into the message log (ADR-0063 section 5),
 * through the same serialized `updateMessageLog` every other writer uses, so
 * a background receive or an open chat writing the same group cannot lose
 * the mark or be lost to it.
 */

import { MESSAGE_LOG_VERSION, updateMessageLog } from "../chat/message-log-store";
import { markSeenInSlot, type ContiguousSeen } from "./feed-seen";

/**
 * Mark `seen` (group log key -> message ids) and return, per group whose
 * watermark moved, the new watermark. A group absent from the log is
 * skipped.
 */
export async function recordFeedSeen(
  accountIdB64: string,
  deviceKey: CryptoKey,
  seen: ReadonlyMap<string, readonly string[]>,
  contiguous: ContiguousSeen,
): Promise<Map<string, bigint>> {
  const advanced = new Map<string, bigint>();
  await updateMessageLog(accountIdB64, deviceKey, (current) => {
    advanced.clear();
    if (current === null) return null;
    const groups = { ...current.groups };
    let changed = false;
    for (const [hex, ids] of seen) {
      const slot = groups[hex];
      if (slot === undefined) continue;
      const res = markSeenInSlot(slot, ids, contiguous);
      groups[hex] = res.slot;
      changed = true;
      if (res.advancedTo !== null) advanced.set(hex, res.advancedTo);
    }
    return changed ? { version: MESSAGE_LOG_VERSION, groups } : null;
  });
  return advanced;
}
