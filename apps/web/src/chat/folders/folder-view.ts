// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the list shows inside a folder (`docs/ux/chat-folders.md` 2.3).
 *
 * Which rows a folder holds is `folder_matches`, and their order is
 * `order_conversations_in_folder`, both in `crates/lakko-ui-rules` - one copy
 * for three clients. This adapter maps the web's rich row into the rule's six
 * fields plus `muted` and `hasUnread`, and applies the indices that come
 * back, the way `conversation-list.ts` does for All chats.
 */
import type { UiRules } from "../ui-rules";
import { kindFor, rowHasUnread, type ConversationRow } from "../conversation-list";
import {
  pinIdsOf,
  ruleInputsOf,
  ruleRowId,
  unreadConversationCount,
  type FolderModel,
} from "./folder-model";

export type FolderRules = Pick<
  UiRules,
  "ConversationKind" | "ConversationRow" | "FolderRule" | "folderMatches" | "orderConversationsInFolder"
>;

type WasmRow = InstanceType<UiRules["ConversationRow"]>;

function toWasmRow(rules: FolderRules, r: ConversationRow): WasmRow {
  return new rules.ConversationRow(
    ruleRowId(r.kind, r.id),
    kindFor(rules as UiRules, r.kind),
    r.title,
    r.lastActivityMs === null ? undefined : BigInt(r.lastActivityMs),
    r.pinned,
    r.archived,
    r.muted,
    rowHasUnread(r),
  );
}

/** The rows of `rows` the folder matches, unordered (input order). */
export function rowsInFolder(
  rows: readonly ConversationRow[],
  folder: FolderModel,
  rules: FolderRules,
): ConversationRow[] {
  const inputs = ruleInputsOf(folder);
  const rule = new rules.FolderRule(
    inputs.includeKinds,
    inputs.excludeFlags,
    inputs.included,
    inputs.excluded,
  );
  try {
    return rows.filter((r) => {
      const w = toWasmRow(rules, r);
      try {
        return rules.folderMatches(rule, w);
      } finally {
        w.free();
      }
    });
  } finally {
    rule.free();
  }
}

/** The folder's rows in the folder's order: its own pins first, then the
 *  All-chats rule with the global pin ignored. */
export function folderRowsOrdered(
  rows: readonly ConversationRow[],
  folder: FolderModel,
  rules: FolderRules,
): ConversationRow[] {
  const matched = rowsInFolder(rows, folder, rules);
  const order = rules.orderConversationsInFolder(
    matched.map((r) => toWasmRow(rules, r)),
    pinIdsOf(folder),
  );
  return Array.from(order, (i) => matched[i]).filter(
    (r): r is ConversationRow => r !== undefined,
  );
}

/** Whether one row is in the folder (the Add-to-folder checkmark). */
export function rowIsInFolder(
  row: ConversationRow,
  folder: FolderModel,
  rules: FolderRules,
): boolean {
  return rowsInFolder([row], folder, rules).length === 1;
}

/** The tab count: conversations in the folder with anything unread, muted
 *  ones not counted. */
export function folderUnreadCount(
  rows: readonly ConversationRow[],
  folder: FolderModel,
  rules: FolderRules,
): number {
  return unreadConversationCount(
    rowsInFolder(rows, folder, rules).map((r) => ({ muted: r.muted, hasUnread: rowHasUnread(r) })),
  );
}

/** The same count over every conversation (All chats, not a folder): the
 *  desktop app-icon badge (`docs/ux/desktop.md` section 2). One rule with
 *  the folder tabs: conversations with anything unread, muted ones not
 *  counted. */
export function allChatsUnreadCount(rows: readonly ConversationRow[]): number {
  return unreadConversationCount(
    rows.map((r) => ({ muted: r.muted, hasUnread: rowHasUnread(r) })),
  );
}

/** Whether the row is pinned inside this folder. */
export function isPinnedInFolder(row: ConversationRow, folder: FolderModel): boolean {
  const id = ruleRowId(row.kind, row.id);
  return pinIdsOf(folder).includes(id);
}
