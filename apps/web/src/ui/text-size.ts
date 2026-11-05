// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Text size (`docs/ux/text-size.md`, decided 2026-11-04): one per-device
 * setting, Small / Medium / Large, that scales the text of messages and the
 * reactions under them and nothing else.
 *
 * The step is stored in `localStorage` on this device only (never synced,
 * never sent anywhere) and read before the first render (`main.tsx`), like
 * the theme. It is applied as one CSS custom property, `--lk-text-scale`, on
 * `<html>`; only the chat rules in `styles.css` read it, so the rest of the
 * interface keeps following the browser's own text size. A stored value
 * this build does not recognise reads as Medium. Choosing a step applies at
 * once.
 */
import { createSignal, type Accessor } from "solid-js";

export type TextSize = "small" | "medium" | "large";

/** The steps in the order the setting lists them. */
export const TEXT_SIZES: readonly TextSize[] = ["small", "medium", "large"];

/** The factor of each step (chosen, not measured; Medium is the design
 *  system's own sizes). */
export const TEXT_SIZE_FACTOR: Readonly<Record<TextSize, number>> = {
  small: 0.87,
  medium: 1,
  large: 1.2,
};

/** The catalog key of each step's name. */
export const TEXT_SIZE_LABEL_KEY: Readonly<Record<TextSize, string>> = {
  small: "settings.text_size.small",
  medium: "settings.text_size.medium",
  large: "settings.text_size.large",
};

export const TEXT_SIZE_STORAGE_KEY = "lakko.textSize";
export const TEXT_SIZE_CSS_PROPERTY = "--lk-text-scale";

const [current, setCurrent] = createSignal<TextSize>("medium");

/** The step in force. Reactive. */
export const useTextSize: Accessor<TextSize> = current;

/** A stored value read as a step: anything unknown (or nothing) is Medium. */
export function textSizeOf(raw: string | null | undefined): TextSize {
  return raw === "small" || raw === "medium" || raw === "large" ? raw : "medium";
}

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** Read the stored step and apply it. Run before the app paints. */
export function bootstrapTextSize(): void {
  let raw: string | null = null;
  try {
    raw = storage()?.getItem(TEXT_SIZE_STORAGE_KEY) ?? null;
  } catch {
    raw = null;
  }
  const size = textSizeOf(raw);
  setCurrent(size);
  applyToDom(size);
}

/** Choose a step: applied at once, kept on this device. Medium, the default,
 *  is stored as no value. */
export function setTextSize(next: TextSize): void {
  setCurrent(next);
  applyToDom(next);
  try {
    const s = storage();
    if (next === "medium") s?.removeItem(TEXT_SIZE_STORAGE_KEY);
    else s?.setItem(TEXT_SIZE_STORAGE_KEY, next);
  } catch {
    // The choice still holds for this session.
  }
}

function applyToDom(size: TextSize): void {
  if (typeof document === "undefined") return;
  document.documentElement.style.setProperty(TEXT_SIZE_CSS_PROPERTY, String(TEXT_SIZE_FACTOR[size]));
}
