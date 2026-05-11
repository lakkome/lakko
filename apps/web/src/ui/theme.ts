// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Theme switching (Phase 1.18f).
 *
 * Three states:
 *
 *   * **`"light"`** — explicit user override, beats system.
 *   * **`"dark"`** — explicit user override, beats system.
 *   * **`"system"`** — no `data-theme` on `<html>`, the
 *     `prefers-color-scheme: dark` media query in `styles.css`
 *     decides at paint time.
 *
 * The user-facing toggle in `AppShell.tsx` cycles
 * `system → dark → light → system`. The explicit choice is
 * persisted to `localStorage[STORAGE_KEY]`; absence ⇒ system
 * mode. The signal is initialised once at app boot via
 * [`bootstrapTheme`]; subsequent flips go through [`setTheme`].
 *
 * Why no Solid context: the choice is page-global, applied via
 * a `data-theme` attribute on `<html>` that every component's
 * CSS reads through the cascade. There's no Solid sub-tree
 * scope to give the value, so a module-level signal +
 * imperative apply-to-DOM is the minimum surface that works.
 */
import { createSignal, type Accessor } from "solid-js";

const STORAGE_KEY = "lakko.theme";

/** All values the toggle cycles through. */
export type Theme = "light" | "dark" | "system";

const VALID_THEMES: readonly Theme[] = ["light", "dark", "system"];

const [theme, setThemeSignal] = createSignal<Theme>("system");

/** Reactive accessor — components subscribe via `theme()` to
 *  re-render the toggle label / icon when the state changes. */
export const useTheme: Accessor<Theme> = theme;

/**
 * Initialise the theme on app boot. Reads
 * `localStorage[STORAGE_KEY]`, validates it, applies the
 * `data-theme` attribute on `<html>`. Safe to call multiple
 * times — second call overwrites the first.
 *
 * Must run **before** the Solid app paints so the very first
 * frame doesn't flash the wrong theme. Called from `main.tsx`
 * right after the SW register.
 */
export function bootstrapTheme(): void {
  if (typeof localStorage === "undefined") return;
  const raw = localStorage.getItem(STORAGE_KEY);
  const t: Theme = isValidTheme(raw) ? raw : "system";
  setThemeSignal(t);
  applyToDom(t);
}

/**
 * Switch theme. Updates the signal (re-renders the toggle in
 * `AppShell`), writes to `localStorage`, and applies the
 * `data-theme` attribute. `"system"` deletes both the
 * storage row and the attribute — the CSS media query takes
 * over.
 */
export function setTheme(next: Theme): void {
  setThemeSignal(next);
  if (typeof localStorage === "undefined") return;
  if (next === "system") {
    localStorage.removeItem(STORAGE_KEY);
  } else {
    localStorage.setItem(STORAGE_KEY, next);
  }
  applyToDom(next);
}

/** Cycle `system → dark → light → system`. The toggle button
 *  uses this as its single click handler — three calls walk
 *  through every state. */
export function cycleTheme(): void {
  const idx = VALID_THEMES.indexOf(theme());
  const next = VALID_THEMES[(idx + 1) % VALID_THEMES.length] ?? "system";
  setTheme(next);
}

function applyToDom(t: Theme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (t === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", t);
  }
}

function isValidTheme(raw: string | null): raw is Theme {
  return raw !== null && (VALID_THEMES as readonly string[]).includes(raw);
}
