// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `Background` strip of the call's Settings sheet (UX
 * `docs/ux/call-backgrounds.md` section 2.1), as plain data so a test can
 * reach it: which thumbnails it shows in which order, which one is
 * selected, and where the arrow keys move.
 */

import type { BackgroundChoice } from "./background-choice";
import { encodeChoice } from "./background-choice";
import type { PhotoEntry } from "./background-photos";

export type StripItem =
  | { readonly kind: "choice"; readonly choice: BackgroundChoice; readonly key: string }
  | { readonly kind: "add"; readonly key: "add" };

/**
 * None, Blur, Strong blur, the six built-ins in the shared order, the own
 * photos newest first (as given), then Add photo.
 */
export function stripItems(builtinIds: readonly string[], photos: readonly PhotoEntry[]): StripItem[] {
  const choices: BackgroundChoice[] = [
    { kind: "none" },
    { kind: "blur", strong: false },
    { kind: "blur", strong: true },
    ...builtinIds.map((id): BackgroundChoice => ({ kind: "builtin", id })),
    ...photos.map((p): BackgroundChoice => ({ kind: "photo", id: p.id })),
  ];
  return [
    ...choices.map((choice): StripItem => ({ kind: "choice", choice, key: encodeChoice(choice) })),
    { kind: "add", key: "add" },
  ];
}

/** Whether `item` is the selected thumbnail. */
export function isSelectedItem(item: StripItem, selected: BackgroundChoice): boolean {
  return item.kind === "choice" && item.key === encodeChoice(selected);
}

/** The id of the selected own photo, or `null`. */
export function selectedPhotoId(selected: BackgroundChoice): string | null {
  return selected.kind === "photo" ? selected.id : null;
}

/**
 * Where focus moves in a strip of `count` items on `key` (arrows along the
 * strip, Home / End to its ends), or `null` when the key does not move it.
 * `rtl` mirrors the arrows for a right-to-left interface.
 */
export function nextStripIndex(current: number, key: string, count: number, rtl = false): number | null {
  if (count <= 0) return null;
  const forward = rtl ? "ArrowLeft" : "ArrowRight";
  const back = rtl ? "ArrowRight" : "ArrowLeft";
  if (key === forward || key === "ArrowDown") return Math.min(count - 1, current + 1);
  if (key === back || key === "ArrowUp") return Math.max(0, current - 1);
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  return null;
}
