// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 3.9 — the one place a commit THIS device merged reaches
 * the attachment kinds: the rings of open anonymous polls grow (our own
 * commit never comes back through the receive path), and when the commit
 * added a leaf (a newcomer, or a new device of a member, ADR-0050 included)
 * the open-items hand-over is posted, sealed at the new epoch.
 *
 * Since ADR-0081's second amendment every add in an ordinary group is a
 * proposal swept by a committer, so every add this device commits leaves
 * `ContactsClient` through a sweep outcome — the live sweep, or an owed
 * verdict merged at restart. Both call {@link afterOwnCommit}. The publish is
 * installed by the app-wide receive context (it owns a `ChatClient`).
 */
import { receiveCommit, type ReceiveGroup } from "./receive";
import { sendOpenItemsHandover } from "./handover";

export type HandoverPublisher = (group: ReceiveGroup, row: Uint8Array) => Promise<unknown>;

let publisher: HandoverPublisher | null = null;

/** Install the quiet group send; returns the uninstall. */
export function installHandoverPublisher(next: HandoverPublisher): () => void {
  publisher = next;
  return () => {
    if (publisher === next) publisher = null;
  };
}

/** Called after this device merged a commit that added `addedLeaves` leaves.
 *  Never throws; the work runs after the caller's lock is released. */
export function afterOwnCommit(group: ReceiveGroup, groupIdB64: string, addedLeaves: number): Promise<void> {
  return (async () => {
    try {
      await receiveCommit(group, groupIdB64, Date.now());
      const publish = publisher;
      if (addedLeaves > 0 && publish !== null) {
        await sendOpenItemsHandover(groupIdB64, addedLeaves, (row) => publish(group, row));
      }
    } catch (err) {
      console.warn("[attach] after-commit work failed", err instanceof Error ? err.name : "unknown");
    }
  })();
}
