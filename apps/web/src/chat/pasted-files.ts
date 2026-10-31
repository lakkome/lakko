// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a paste into the composer attaches (`docs/ux/desktop.md` section 6).
 *
 * The clipboard of a screenshot or a copied picture holds an image. A copy
 * from an office document or a spreadsheet holds the text too, as plain and
 * as HTML, with a picture of the selection beside them: that paste is text.
 * A browser's "copy image" holds HTML alone and a file manager's copy plain
 * text alone (the file name), so only both together mean text.
 */
export function pastedImages(clipboard: DataTransfer | null): File[] {
  if (clipboard === null) return [];
  const types = Array.from(clipboard.types);
  if (types.includes("text/plain") && types.includes("text/html")) return [];
  return Array.from(clipboard.files).filter((f) => f.type.startsWith("image/"));
}
