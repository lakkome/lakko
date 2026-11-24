// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the chat says when a message could not be sent.
 *
 * `docs/ux/client-redesign.md` section 8: "Errors are one sentence plus an
 * action." The send path was showing neither - it surfaced whatever the
 * transport threw, so a dropped connection read
 * `ds append failed: network error: TypeError: Failed to fetch`, which names
 * a JavaScript type to a person who was writing to a friend.
 *
 * The ACTION is not in this string, on purpose. It lives on the bubble, which
 * stays in the timeline reading "Not sent - Retry"
 * (`docs/ux/media-messages.md` section 3) - so the sentence says what
 * happened and the thing the user touches is next to the message it belongs
 * to, rather than in a bar at the top that could refer to any of them.
 *
 * Kept out of `routes/Chat.tsx` so it can be run by a test: vitest has no
 * jsdom and no Solid transform here, so nothing in that file is reachable.
 */

import { t } from "../l10n";
import type { TimelineRow } from "./timeline-row";

/** The curated copy for a refusal the core named, when it named one. */
export interface CuratedRefusal {
  readonly message: string;
}

/** A send failure that is worth naming rather than generalising. */
export const SEND_FAILED_NOTICE = (): string => t("chat.send.failed");

/**
 * The one sentence for a failed send.
 *
 * A membership refusal keeps its own words: "you are no longer a member of
 * this group" is the difference between retrying usefully and retrying
 * forever, and that copy is already curated per reason
 * (`membership-refusal.ts`). Everything else - a dropped connection, a
 * server that answered 500, an envelope that would not compose - collapses
 * to one sentence, because the distinctions among them are not ones the
 * person can act on differently: the action is the same, and it is on the
 * bubble.
 *
 * Deliberately does NOT read `err`. Passing it and ignoring it would invite
 * a later edit to start formatting it again, which is the defect.
 */
export function sendFailureNotice(refusal: CuratedRefusal | null): string {
  if (refusal === null) return SEND_FAILED_NOTICE();
  const curated = refusal.message.trim();
  return curated.length === 0 ? SEND_FAILED_NOTICE() : curated;
}

/**
 * Settle one outgoing row's ack, keeping the row.
 *
 * The keeping is the point, and it is why this is a named function rather
 * than a `map` inline at the call site: the defect was that a failed send
 * REMOVED the row, and the words went with it, because the composer had
 * already been cleared. A future edit that reaches for a filter here is the
 * defect coming back, and there is now a test that says so.
 *
 * Returns the input array unchanged (by identity) when no row matched, so a
 * settle for a row the timeline no longer holds - the chat was reset while
 * the send was in flight - re-renders nothing.
 */
export function withSendAck(
  rows: readonly TimelineRow[],
  tempId: number,
  ack: "pending" | "sent" | "failed" | "waiting",
): readonly TimelineRow[] {
  let found = false;
  const next = rows.map((r) => {
    if (r.kind !== "outgoing" || r.tempId !== tempId) return r;
    found = true;
    return r.ack === ack ? r : { ...r, ack };
  });
  return found ? next : rows;
}
