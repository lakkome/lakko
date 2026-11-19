// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Do this once the element is about to be on screen."
 *
 * Every media bubble needs its bytes before the person acts on them, and the
 * cheapest moment to fetch them is while the bubble is scrolling into view.
 * A photo has always done this; a voice message did not, which is why its
 * first press paid a whole storage round trip plus an AEAD open before a
 * single sample was decoded (`MediaBubbles.tsx`, `VoiceBubble`).
 *
 * Shared by `MediaMosaic.tsx` and `MediaBubbles.tsx` so both ask the same
 * question with the same margin, rather than one of them growing a second
 * answer.
 *
 * Without an `IntersectionObserver` (an older engine, a test environment) it
 * runs immediately: eager is the safe direction here, since the alternative is
 * a bubble that never loads at all.
 */

import { onCleanup, onMount } from "solid-js";

/** How far ahead of the viewport a bubble starts loading. Wide enough that a
 *  normal scroll lands on bytes that are already there. */
export const NEAR_VIEWPORT_MARGIN = "200px";

export function whenVisible(el: () => HTMLElement | undefined, start: () => void): void {
  onMount(() => {
    const node = el();
    if (typeof IntersectionObserver === "undefined" || node === undefined) {
      start();
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          observer.disconnect();
          start();
        }
      },
      { rootMargin: NEAR_VIEWPORT_MARGIN },
    );
    observer.observe(node);
    onCleanup(() => observer.disconnect());
  });
}
