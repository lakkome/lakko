// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keeping the newest message in view when the space for the conversation
 * changes under it (the same rule as Android's `StickToBottom.kt`).
 *
 * The timeline is a top-anchored scroller: when it gets shorter (the pinned
 * bar appears above it, the composer grows with a reply or edit strip or a
 * longer draft, the on-screen keyboard takes room) or its content grows (a new
 * message, an edited bubble growing, a picture loading in the last message),
 * it keeps its scroll offset and the newest rows end up under the composer.
 *
 * The rule: while the person was at the bottom and is not scrolling, any such
 * change scrolls back to the end. Someone reading older messages (or taken to
 * a pinned message) is left where they are; scrolling back to the end arms it
 * again. One observer on the scroller and its content applies it, not one
 * scroll per trigger, so a trigger nobody listed is covered too.
 */

/** Within this many pixels of the end counts as "at the bottom" (fractional
 *  layout; chosen, not measured). */
export const STICK_TOLERANCE_PX = 2;

/** After the last wheel or touch move, the person counts as scrolling for
 *  this long (a momentum scroll; chosen, not measured). */
export const USER_SCROLL_SETTLE_MS = 150;

/** The scroller's geometry, as the DOM reports it. */
export interface StickGeometry {
  readonly scrollTop: number;
  readonly scrollHeight: number;
  readonly clientHeight: number;
}

/** The end of the content is in view. */
export function atBottom(g: StickGeometry): boolean {
  return g.scrollHeight - g.scrollTop - g.clientHeight <= STICK_TOLERANCE_PX;
}

/** What to do after a change of size, and whether the list stays held. */
export interface StickDecision {
  /** Scroll to this offset, or leave the scroller alone. */
  readonly scrollTo: number | null;
  readonly stuck: boolean;
}

/**
 * `stuck`: the list was held at the bottom before this change. `scrolling`:
 * the person is scrolling; the list then follows them and is held only if
 * their scroll ends at the bottom.
 */
export function decideStick(stuck: boolean, scrolling: boolean, g: StickGeometry): StickDecision {
  if (scrolling || !stuck || atBottom(g)) return { scrollTo: null, stuck: atBottom(g) };
  return { scrollTo: Math.max(0, g.scrollHeight - g.clientHeight), stuck: true };
}

/** The live controller a conversation view holds. */
export interface StickToBottom {
  /** The person is reading something older on purpose (a jump to a quoted or
   *  pinned message): stop holding the bottom until they scroll back. */
  release(): void;
  /** Whether the list is held at the bottom now. */
  stuck(): boolean;
  /** Stop observing. */
  dispose(): void;
}

/**
 * Apply the rule to `scroller`: observe its own size, the size of each of its
 * children (new rows, a growing bubble, a loaded picture) and the visual
 * viewport, and scroll back to the end when the rule says so.
 */
export function attachStickToBottom(scroller: HTMLElement): StickToBottom {
  let stuck = true;
  let userScrollUntil = 0;
  let touching = false;
  const now = (): number => (typeof performance !== "undefined" ? performance.now() : Date.now());
  const scrolling = (): boolean => touching || now() < userScrollUntil;
  const geometry = (): StickGeometry => ({
    scrollTop: scroller.scrollTop,
    scrollHeight: scroller.scrollHeight,
    clientHeight: scroller.clientHeight,
  });

  const apply = (): void => {
    const d = decideStick(stuck, scrolling(), geometry());
    stuck = d.stuck;
    if (d.scrollTo !== null) scroller.scrollTop = d.scrollTo;
  };

  // A scroll (the person's, a clamp, or ours) says where the list now is.
  const onScroll = (): void => {
    stuck = atBottom(geometry());
  };
  const markUserScroll = (): void => {
    userScrollUntil = now() + USER_SCROLL_SETTLE_MS;
  };
  const onTouchStart = (): void => {
    touching = true;
  };
  const onTouchEnd = (): void => {
    touching = false;
    markUserScroll();
  };
  scroller.addEventListener("scroll", onScroll, { passive: true });
  scroller.addEventListener("wheel", markUserScroll, { passive: true });
  scroller.addEventListener("touchstart", onTouchStart, { passive: true });
  scroller.addEventListener("touchend", onTouchEnd, { passive: true });
  scroller.addEventListener("touchcancel", onTouchEnd, { passive: true });

  const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => apply());
  // The scroller's own children: a row or bubble growing anywhere inside one
  // changes that child's size.
  const observeChild = (n: Node): void => {
    if (n instanceof Element && n.parentNode === scroller) ro?.observe(n);
  };
  ro?.observe(scroller);
  for (const child of Array.from(scroller.children)) observeChild(child);
  const mo =
    typeof MutationObserver === "undefined"
      ? null
      : new MutationObserver((records) => {
          for (const r of records) {
            for (const n of Array.from(r.addedNodes)) observeChild(n);
            for (const n of Array.from(r.removedNodes)) if (n instanceof Element) ro?.unobserve(n);
          }
          // A row added or removed changes the content even before its size
          // is reported.
          apply();
        });
  // Rows are added a level or two down (`<Show>` / `<For>` output), so the
  // subtree is watched for additions too.
  mo?.observe(scroller, { childList: true, subtree: true });

  const vv = typeof globalThis.visualViewport === "undefined" ? null : globalThis.visualViewport;
  vv?.addEventListener("resize", apply);

  return {
    release: () => {
      stuck = false;
    },
    stuck: () => stuck,
    dispose: () => {
      ro?.disconnect();
      mo?.disconnect();
      vv?.removeEventListener("resize", apply);
      scroller.removeEventListener("scroll", onScroll);
      scroller.removeEventListener("wheel", markUserScroll);
      scroller.removeEventListener("touchstart", onTouchStart);
      scroller.removeEventListener("touchend", onTouchEnd);
      scroller.removeEventListener("touchcancel", onTouchEnd);
    },
  };
}
