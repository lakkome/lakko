// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The camera background the person chose (ADR-0089, UX
 * `docs/ux/call-backgrounds.md` section 2.1, section 8).
 *
 * Stored on this device only, in `localStorage`, as the shared stored text:
 * `none`, `blur`, `strong-blur`, `builtin:<id>` or `photo:<id>`. Never synced
 * to the account's other devices and never sent to anyone: the others see the
 * result, not the setting (the rule of the noise suppressor).
 *
 * What a stored value means is decided in Rust (`lakko_segment::Choice`,
 * through `backgroundChoiceNormalize`): empty or `none` is none, anything
 * unreadable is `blur`, so a damaged setting still hides the room. A change
 * applies at once: live call sessions subscribe with
 * {@link onBackgroundChoiceChange}.
 */

import { wasmSync } from "../wasm";

/** The `localStorage` key. Holds the stored text of the choice. */
export const BACKGROUND_STORAGE_KEY = "lakko.call.background";

/** A choice, decoded from its stored text. */
export type BackgroundChoice =
  | { readonly kind: "none" }
  | { readonly kind: "blur"; readonly strong: boolean }
  | { readonly kind: "builtin"; readonly id: string }
  | { readonly kind: "photo"; readonly id: string };

/** The part of `Storage` the setting uses. */
export interface ChoiceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The Rust rule that turns a stored value into canonical stored text. */
export type ChoiceNormalizer = (stored: string) => string;

/**
 * The shared rule, or a stand-in that only knows `none` when the core is not
 * loaded (the app loads it at start, so this is a corner): anything else is
 * read as `blur`, toward hiding the room, never toward the raw camera.
 */
function rustNormalizer(stored: string): string {
  try {
    return wasmSync().backgroundChoiceNormalize(stored);
  } catch {
    const trimmed = stored.trim();
    return trimmed.length === 0 || trimmed === "none" ? "none" : "blur";
  }
}

/** Decodes canonical stored text (the normalizer's output). */
export function decodeChoice(canonical: string): BackgroundChoice {
  if (canonical === "none") return { kind: "none" };
  if (canonical === "blur") return { kind: "blur", strong: false };
  if (canonical === "strong-blur") return { kind: "blur", strong: true };
  if (canonical.startsWith("builtin:")) return { kind: "builtin", id: canonical.slice(8) };
  if (canonical.startsWith("photo:")) return { kind: "photo", id: canonical.slice(6) };
  // The normalizer never gives anything else; read it as blur all the same.
  return { kind: "blur", strong: false };
}

/** The stored text of a choice. */
export function encodeChoice(choice: BackgroundChoice): string {
  switch (choice.kind) {
    case "none":
      return "none";
    case "blur":
      return choice.strong ? "strong-blur" : "blur";
    case "builtin":
      return `builtin:${choice.id}`;
    case "photo":
      return `photo:${choice.id}`;
  }
}

/** Whether `choice` replaces anything (every choice but none). */
export function isBackground(choice: BackgroundChoice): boolean {
  return choice.kind !== "none";
}

/** Whether two choices are the same. */
export function sameChoice(a: BackgroundChoice, b: BackgroundChoice): boolean {
  return encodeChoice(a) === encodeChoice(b);
}

function pageStorage(): ChoiceStorage | null {
  try {
    const storage = (globalThis as { localStorage?: unknown }).localStorage;
    if (typeof storage !== "object" || storage === null) return null;
    const s = storage as { getItem?: unknown; setItem?: unknown };
    return typeof s.getItem === "function" && typeof s.setItem === "function"
      ? (storage as ChoiceStorage)
      : null;
  } catch {
    return null;
  }
}

/**
 * Reads the stored choice. Nothing stored, or a storage that cannot be read,
 * is none (the default); a stored value nobody can read is blur.
 */
export function readBackgroundChoice(
  storage: ChoiceStorage | null = pageStorage(),
  normalize: ChoiceNormalizer = rustNormalizer,
): BackgroundChoice {
  let raw: string | null = null;
  try {
    raw = storage?.getItem(BACKGROUND_STORAGE_KEY) ?? null;
  } catch {
    raw = null;
  }
  if (raw === null) return { kind: "none" };
  return decodeChoice(normalize(raw));
}

/**
 * The choice to apply in a call, given whether its own photo can be read:
 * a selected photo that is missing or unreadable is blur, never the camera
 * (UX section 2.3, contract item 1).
 */
export function choiceForCall(choice: BackgroundChoice, photoReadable: boolean): BackgroundChoice {
  if (choice.kind === "photo" && !photoReadable) return { kind: "blur", strong: false };
  return choice;
}

type Listener = (choice: BackgroundChoice) => void;

const listeners = new Set<Listener>();
let current: BackgroundChoice | null = null;

/** The choice as it stands now (read from storage once per page). */
export function backgroundChoice(): BackgroundChoice {
  if (current === null) current = readBackgroundChoice();
  return current;
}

/**
 * Sets the choice: remembered for this page at once, stored when the storage
 * allows, and every subscriber told, also when the same choice is picked
 * again (picking a background again restarts one that failed, UX section 3).
 */
export function setBackgroundChoice(
  choice: BackgroundChoice,
  storage: ChoiceStorage | null = pageStorage(),
): void {
  current = choice;
  try {
    storage?.setItem(BACKGROUND_STORAGE_KEY, encodeChoice(choice));
  } catch {
    // Private window or full quota: the choice still holds for this page.
  }
  for (const listener of [...listeners]) {
    try {
      listener(choice);
    } catch {
      // One subscriber's failure must not stop the others.
    }
  }
}

/** Subscribes to changes; returns the unsubscribe function. */
export function onBackgroundChoiceChange(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Test seam: forget the page's remembered value and every subscriber. */
export function resetBackgroundChoiceForTest(): void {
  current = null;
  listeners.clear();
}
