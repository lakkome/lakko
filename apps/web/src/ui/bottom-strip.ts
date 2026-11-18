// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The strip a bottom-anchored app banner (the "new version" banner,
 * `sw/SwUpdateBanner.tsx`) holds at the bottom of the viewport, in CSS
 * pixels; `0` when none shows. The page's own layout ends above it through
 * CSS (`html[data-update-banner]`); what places itself from script, the
 * collapsed call window (`calls/CallWindow.tsx`), reads it here, so a window
 * already in a bottom corner moves up when the banner appears.
 */
import { createSignal } from "solid-js";

const [stripPx, setStripPx] = createSignal(0);

/** The strip's height now (a Solid signal: reading it tracks it). */
export function bottomStripPx(): number {
  return stripPx();
}

/** Set by the banner as it shows, measures and goes. */
export function setBottomStripPx(px: number): void {
  setStripPx(Number.isFinite(px) && px > 0 ? Math.ceil(px) : 0);
}
