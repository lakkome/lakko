// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `Noise suppression` switch of the call settings
 * (`docs/ux/calls.md` section 4.1.3, ADR-0088).
 *
 * On by default. Stored on this device only, in `localStorage`; never synced
 * to the account's other devices and never sent to anyone in a call. Every
 * read and write is guarded: a storage that cannot be read gives the default,
 * one that cannot be written keeps the choice for this page's life.
 *
 * A change applies at once: the live call sessions subscribe with
 * {@link onNoiseSuppressionChange} and swap their microphone track.
 */

/** The `localStorage` key. Holds `on` or `off`. */
export const NOISE_SUPPRESSION_STORAGE_KEY = "lakko.call.noise-suppression";

/** The default when nothing is stored. */
export const NOISE_SUPPRESSION_DEFAULT_ON = false;

/** The part of `Storage` the switch uses. */
export interface SettingStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function pageStorage(): SettingStorage | null {
  try {
    const storage = (globalThis as { localStorage?: unknown }).localStorage;
    if (typeof storage !== "object" || storage === null) return null;
    const s = storage as { getItem?: unknown; setItem?: unknown };
    return typeof s.getItem === "function" && typeof s.setItem === "function"
      ? (storage as SettingStorage)
      : null;
  } catch {
    return null;
  }
}

/** Reads the stored choice; anything but `on` / `off` gives the default. */
export function readNoiseSuppression(storage: SettingStorage | null = pageStorage()): boolean {
  let raw: string | null = null;
  try {
    raw = storage?.getItem(NOISE_SUPPRESSION_STORAGE_KEY) ?? null;
  } catch {
    raw = null;
  }
  if (raw === "off") return false;
  if (raw === "on") return true;
  return NOISE_SUPPRESSION_DEFAULT_ON;
}

type Listener = (on: boolean) => void;

const listeners = new Set<Listener>();
let current: boolean | null = null;

/** The switch as it stands now (read from storage once per page). */
export function noiseSuppressionOn(): boolean {
  if (current === null) current = readNoiseSuppression();
  return current;
}

/**
 * Sets the switch: remembered for this page at once, stored when the
 * storage allows, and every subscriber told. Setting the same value again
 * tells nobody.
 */
export function setNoiseSuppression(
  on: boolean,
  storage: SettingStorage | null = pageStorage(),
): void {
  const before = noiseSuppressionOn();
  current = on;
  try {
    storage?.setItem(NOISE_SUPPRESSION_STORAGE_KEY, on ? "on" : "off");
  } catch {
    // Private window, sealed store (ADR-0077) or full quota: the choice still
    // holds for this page.
  }
  if (before === on) return;
  for (const listener of [...listeners]) {
    try {
      listener(on);
    } catch {
      // One subscriber's failure must not stop the others from switching.
    }
  }
}

/** Subscribes to changes; returns the unsubscribe function. */
export function onNoiseSuppressionChange(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Test seam: forget the page's remembered value and every subscriber. */
export function resetNoiseSuppressionForTest(): void {
  current = null;
  listeners.clear();
}
