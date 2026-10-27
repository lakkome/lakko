// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The rules behind `LakkoPicker` (design system section 3, "Picker"): which
 * row is active when the list opens, and what a key does.
 *
 * Kept apart from the component because `vitest` runs plain node with no
 * Solid JSX transform, so a `.tsx` cannot be tested (see `ConfirmDialog.tsx`).
 * The component only reads these answers and moves focus.
 *
 * Keyboard, per the design system: Enter / Space (and the arrows) open the
 * list, ArrowUp / ArrowDown move, Home / End jump, Enter / Space pick, Escape
 * closes and returns focus to the field, Tab closes and lets focus move on.
 */

/** What a key press asks the picker to do. */
export type PickerAction = "none" | "open" | "move" | "pick" | "close";

export interface PickerKeyResult {
  readonly action: PickerAction;
  /** The active row after the key; `-1` when the list has no rows. */
  readonly index: number;
  /** Whether the event's default must be prevented (the page must not scroll
   *  on an arrow, a form must not submit on Enter). Tab is never prevented. */
  readonly prevent: boolean;
}

/** The active row when the list opens: the current value, else the first. */
export function initialPickerIndex(selectedIndex: number, count: number): number {
  if (count <= 0) return -1;
  if (selectedIndex >= 0 && selectedIndex < count) return selectedIndex;
  return 0;
}

function clamp(index: number, count: number): number {
  if (count <= 0) return -1;
  return Math.max(0, Math.min(count - 1, index));
}

/**
 * Decide one key press. `active` is the active row (the current value's
 * index while closed); `count` the number of rows.
 */
export function pickerKey(
  key: string,
  open: boolean,
  active: number,
  count: number,
): PickerKeyResult {
  if (!open) {
    switch (key) {
      case "Enter":
      case " ":
      case "ArrowDown":
      case "ArrowUp":
        return { action: "open", index: initialPickerIndex(active, count), prevent: true };
      default:
        return { action: "none", index: active, prevent: false };
    }
  }
  switch (key) {
    case "ArrowDown":
      return { action: "move", index: clamp(active < 0 ? 0 : active + 1, count), prevent: true };
    case "ArrowUp":
      return { action: "move", index: clamp(active < 0 ? 0 : active - 1, count), prevent: true };
    case "Home":
      return { action: "move", index: clamp(0, count), prevent: true };
    case "End":
      return { action: "move", index: clamp(count - 1, count), prevent: true };
    case "Enter":
    case " ":
      if (active >= 0 && active < count) return { action: "pick", index: active, prevent: true };
      return { action: "none", index: active, prevent: true };
    case "Escape":
      return { action: "close", index: active, prevent: true };
    case "Tab":
      return { action: "close", index: active, prevent: false };
    default:
      return { action: "none", index: active, prevent: false };
  }
}

/**
 * The words offered under a recovery-phrase field as the person types: those
 * starting with what is typed, in list order, at most `limit`. Nothing for an
 * empty field, and nothing once the field holds the one word that matches, so
 * the list goes away when the word is settled.
 */
export function suggestWords(
  typed: string,
  list: readonly string[],
  limit = 5,
): string[] {
  const prefix = typed.trim().toLowerCase();
  if (prefix.length === 0) return [];
  const out: string[] = [];
  for (const w of list) {
    if (w.startsWith(prefix)) {
      out.push(w);
      if (out.length >= limit) break;
    }
  }
  if (out.length === 1 && out[0] === prefix) return [];
  return out;
}
