// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel record owed after a PublisherGroup commit (ADR-0051 section 4,
 * ADR-0087 section 15): each commit of this device moves the channel to a new
 * epoch, and a reader let in after it opens only that epoch, so the record
 * (name, description, photo, @name) is posted again under it. The owed mark
 * is written with the commit's own record (`recordPublisherCommit`), so a
 * failed re-post, or a reload before it, is retried by the publisher pass
 * until it lands; it never waits for the next epoch change.
 */
import { channelsActions } from "../state/channels-state";

/** What one try came to. */
export type OwedRecordOutcome = "none" | "posted" | "failed";

/**
 * Post the owed record of `idHex` once, if one is owed. `post` re-issues the
 * record and resolves `false` when there is nothing to post (no name known
 * on this device), which also settles the mark. A failure keeps it owed.
 */
export async function settleOwedRecord(idHex: string, post: () => Promise<boolean>): Promise<OwedRecordOutcome> {
  if (channelsActions.accessOf(idHex).recordOwed !== true) return "none";
  try {
    await post();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] owed channel record not posted; retried next pass", err instanceof Error ? err.name : "");
    return "failed";
  }
  channelsActions.updateAccess(idHex, (r) => {
    if (r.recordOwed !== true) return r;
    const { recordOwed: _settled, ...rest } = r;
    return rest;
  });
  return "posted";
}
