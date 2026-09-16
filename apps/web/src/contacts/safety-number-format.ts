// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Presentation of the safety number (docs/ux/client-redesign.md section 5.3):
 * the 60 digits `computeSafetyNumber` returns, as groups of five laid out in
 * rows, so the contact detail can render them large and monospace without
 * touching the number itself. The digits are never reordered.
 */

/** Digits per group, as the core formats them. */
export const SAFETY_NUMBER_GROUP_DIGITS = 5;

/** Groups per row in the detail view. */
export const SAFETY_NUMBER_GROUPS_PER_ROW = 4;

/** The number as five-digit groups. Whitespace in the input is ignored, so
 *  the core's space-separated string and a bare digit string give the same
 *  result; anything that is not a digit is dropped. */
export function safetyNumberGroups(raw: string): string[] {
  const digits = raw.replace(/\D/g, "");
  const groups: string[] = [];
  for (let i = 0; i < digits.length; i += SAFETY_NUMBER_GROUP_DIGITS) {
    groups.push(digits.slice(i, i + SAFETY_NUMBER_GROUP_DIGITS));
  }
  return groups;
}

/** The groups cut into rows of `perRow`. */
export function safetyNumberRows(
  raw: string,
  perRow: number = SAFETY_NUMBER_GROUPS_PER_ROW,
): string[][] {
  const groups = safetyNumberGroups(raw);
  const rows: string[][] = [];
  for (let i = 0; i < groups.length; i += perRow) {
    rows.push(groups.slice(i, i + perRow));
  }
  return rows;
}
