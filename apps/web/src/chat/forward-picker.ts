// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 2 / `docs/ux/message-actions.md` section 5.1 — the rows
 * of the "Forward to" picker.
 *
 * The picker is "the same merged list the Chats tab shows" with two
 * differences that are rules rather than chrome, and so live here where a
 * test can run them:
 *
 *  - **Live memberships only.** A conversation this device has been removed
 *    from is still in the Chats list (its history is kept, ADR-0045 UX
 *    section 6.2.6), but a forward is a SEND, and a send into an ended
 *    membership is refused by the core. Offering the row would offer a
 *    failure.
 *  - **No channels.** A channel is a broadcast the reader subscribes to, not
 *    an MLS group the reader can send into; the Chats list carries channels
 *    because it is a list of conversations, and this is a list of places a
 *    message can go.
 *
 * The ORDER is the shared rule (`sortConversations` over
 * `crates/lakko-ui-rules`): pinned first, archived last, newest between — the
 * same order the Chats tab draws, so the row a person expects near the top is
 * near the top here too. The search field filters by title and second line
 * with the list's own predicate.
 *
 * Pure: no Solid, no store. The component (`ForwardPicker.tsx`) computes one
 * candidate per active group from the contexts the Chats list reads, and
 * this module decides which appear and in what order.
 */
import { sortConversations, type ConversationRow } from "./conversation-list";
import { rowMatchesQuery } from "./list-preview";
import type { UiRules } from "./ui-rules";

/** One active group, as the component sees it before the rules run. */
export interface ForwardCandidate {
  /** The group id, standard base64 — the `ChatContext` registry key. */
  readonly groupIdB64: string;
  readonly kind: "chat" | "group";
  readonly title: string;
  /** The second line, as the Chats list draws it. */
  readonly sub: string;
  readonly lastActivityMs: number | null;
  readonly pinned: boolean;
  readonly muted: boolean;
  readonly archived: boolean;
  /** `Group.isActive`, or `undefined` when the read threw. Only `true` is
   *  offered: a membership whose state cannot be read is not a place a
   *  message can be promised to reach. */
  readonly live: boolean | undefined;
  /** ADR-0084 - the person of a one-to-one, whose photo the row shows. */
  readonly personKey?: string | undefined;
}

/** One row of the picker. `id` is the group id, standard base64. */
export type ForwardPickerRow = ConversationRow & { readonly kind: "chat" | "group" };

/** Section 5.1 — which conversations are offered, in the Chats tab's order. */
export function forwardPickerRows(
  candidates: readonly ForwardCandidate[],
  rules: UiRules,
): readonly ForwardPickerRow[] {
  const rows: ForwardPickerRow[] = candidates
    .filter((c) => c.live === true)
    .map((c) => ({
      id: c.groupIdB64,
      kind: c.kind,
      title: c.title,
      preview: c.sub,
      lastActivityMs: c.lastActivityMs,
      unread: 0,
      pinned: c.pinned,
      muted: c.muted,
      archived: c.archived,
      ...(c.personKey === undefined ? {} : { personKey: c.personKey }),
    }));
  return sortConversations(rows, rules).filter(
    (r): r is ForwardPickerRow => r.kind !== "channel",
  );
}

/** The search field: the list's own loose match over title and second line;
 *  an empty or blank query keeps every row. */
export function filterForwardRows<R extends ConversationRow>(
  rows: readonly R[],
  query: string,
): readonly R[] {
  if (query.trim().length === 0) return rows;
  return rows.filter((r) => rowMatchesQuery(query, r.title, r.preview));
}
