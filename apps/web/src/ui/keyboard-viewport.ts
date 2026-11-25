// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The app takes the height left above an on-screen keyboard.
 *
 * iOS Safari keeps the page at full height when the keyboard opens and
 * scrolls the whole page up to show the focused field: in a conversation the
 * header and the messages went off the top of the screen (seen 2026-11-25 in
 * Safari on an iPhone). On a touch screen the root height follows the visual
 * viewport and the page stays at the top, so the conversation shrinks the way
 * the native apps do. A pinch zoom (scale not 1) leaves the height alone.
 */

/** The root height for a visual viewport, or `null` to use the full page. */
export function appHeightFor(viewport: { readonly height: number; readonly scale: number }): number | null {
  if (!Number.isFinite(viewport.height) || viewport.height <= 0) return null;
  if (Math.abs(viewport.scale - 1) > 0.01) return null;
  return Math.round(viewport.height);
}

export function installKeyboardViewport(): void {
  if (typeof window === "undefined") return;
  const vv = window.visualViewport;
  if (vv === null || vv === undefined) return;
  if (!window.matchMedia("(pointer: coarse)").matches) return;
  const root = document.documentElement;
  const update = (): void => {
    const h = appHeightFor(vv);
    if (h === null) root.style.removeProperty("--app-height");
    else root.style.setProperty("--app-height", `${h}px`);
    if (h !== null && window.scrollY !== 0) window.scrollTo(0, 0);
  };
  vv.addEventListener("resize", update);
  vv.addEventListener("scroll", update);
  update();
}
