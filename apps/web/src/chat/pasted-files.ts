// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a paste into a conversation attaches (`docs/ux/desktop.md` section 6).
 *
 * Any file on the clipboard attaches, as a drop would: a screenshot or a
 * copied picture, a video, a document copied in a file manager. A copy from
 * an office document or a spreadsheet holds the text too, as plain and as
 * HTML, with a picture of the selection beside it: that paste is text. A
 * browser's "copy image" holds HTML alone and a file manager's copy plain
 * text alone (the file name), so only both together mean text.
 *
 * Some engines list a copied file under `items` but not under `files`, so
 * both are read; the same file is never taken twice.
 */
export function pastedFiles(clipboard: DataTransfer | null): File[] {
  if (clipboard === null) return [];
  const types = Array.from(clipboard.types ?? []);
  if (types.includes("text/plain") && types.includes("text/html")) return [];
  const out = Array.from(clipboard.files ?? []);
  if (out.length > 0) return out;
  const items = Array.from(clipboard.items ?? []);
  for (const item of items) {
    if (item.kind !== "file") continue;
    const file = item.getAsFile();
    if (file !== null) out.push(file);
  }
  return out;
}

/** Whether a paste aimed at `target` is a field's own text paste, which a
 *  conversation-wide paste handler must leave alone. */
export function pasteBelongsToField(target: EventTarget | null): boolean {
  if (target === null || typeof (target as { closest?: unknown }).closest !== "function") return false;
  const el = target as Element;
  return el.closest("input, textarea, [contenteditable=''], [contenteditable='true']") !== null;
}
