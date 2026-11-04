// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which parts of Settings exist only in the desktop app
 * (`docs/ux/desktop.md` sections 3.1 and 5).
 */

/** Settings sections only the desktop app has. Notifications is in both
 *  builds since the reactions switch (`docs/ux/reactions-and-emoji.md`
 *  section 12); the browser's has that switch alone. */
export const DESKTOP_ONLY_SECTIONS: ReadonlySet<string> = new Set([]);

/** The sections this build shows: the desktop-only ones dropped on the web. */
export function sectionsForBuild<T extends { readonly id: string }>(
  all: readonly T[],
  desktop: boolean,
): readonly T[] {
  return desktop ? all : all.filter((s) => !DESKTOP_ONLY_SECTIONS.has(s.id));
}

/** Whether About lists the keyboard shortcuts (desktop only). */
export function showsShortcutList(desktop: boolean): boolean {
  return desktop;
}
