// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Up` in an empty composer edits my last message (`docs/ux/desktop.md`
 * section 5): which message that is.
 *
 * The same gate the message menu offers Edit on (`TimelineBubble.tsx`): an
 * own row whose send is complete (it has a DS id), of this conversation's
 * current group (an older segment's row is read-only, ADR-0073 R.5), and
 * not deleted. The text is the one on screen, so an edited message is
 * edited from its edited text.
 */
import { isHistoryRowId } from "./lineage";
import type { MessageOpState } from "./message-ops";
import type { TimelineRow } from "./timeline-row";

/** The message the shortcut would edit. */
export interface LastEditable {
  readonly messageId: number;
  readonly text: string;
}

/** What the composer knows when `Up` is pressed. */
export interface ComposerUp {
  readonly desktop: boolean;
  readonly key: string;
  readonly modified: boolean;
  readonly composing: boolean;
  readonly empty: boolean;
  readonly editing: boolean;
  readonly actionsAvailable: boolean;
}

/** Whether this key press in the composer asks to edit the last message:
 *  desktop only, a bare `Up`, an empty composer (no text, no attachment),
 *  no edit already open, and the conversation still taking edits. */
export function upEditsLastMessage(s: ComposerUp): boolean {
  return (
    s.desktop &&
    s.key === "ArrowUp" &&
    !s.modified &&
    !s.composing &&
    s.empty &&
    !s.editing &&
    s.actionsAvailable
  );
}

/** The newest editable own message in `rows` (oldest first), or `null`. */
export function lastEditableOwnMessage(
  rows: readonly TimelineRow[],
  opOf: (messageId: number) => MessageOpState | undefined,
): LastEditable | null {
  for (let i = rows.length - 1; i >= 0; i -= 1) {
    const row = rows[i];
    if (row === undefined || row.kind !== "outgoing") continue;
    if (row.ack !== "sent" || row.messageId === undefined) continue;
    if (isHistoryRowId(row.messageId)) continue;
    const op = opOf(row.messageId);
    if (op?.deleted === true) continue;
    return { messageId: row.messageId, text: op?.editedText ?? row.plaintext };
  }
  return null;
}
