// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The touch path to a row's context menu (docs/ux/client-redesign.md section
 * 5.1): a pointer held still for about half a second opens the same menu the
 * right click does. Mouse presses are left alone (they select text and drag
 * links); a touch or pen press that moves past the tolerance is a scroll, not
 * a press, and is cancelled.
 *
 * The timing state is per call; attach the returned handlers to the element.
 * The click that the browser synthesises after a long press is swallowed once
 * so the row does not also navigate.
 */

export const LONG_PRESS_MS = 500;
export const LONG_PRESS_MOVE_TOLERANCE_PX = 8;

/** Whether a pointer that moved by (dx, dy) has left the press. Pure. */
export function movedPastTolerance(
  dx: number,
  dy: number,
  tolerancePx: number = LONG_PRESS_MOVE_TOLERANCE_PX,
): boolean {
  return dx * dx + dy * dy > tolerancePx * tolerancePx;
}

export interface LongPressHandlers {
  onPointerDown: (e: PointerEvent) => void;
  onPointerMove: (e: PointerEvent) => void;
  onPointerUp: (e: PointerEvent) => void;
  onPointerCancel: (e: PointerEvent) => void;
  onPointerLeave: (e: PointerEvent) => void;
  /** Attach as the element's click handler (or call first from it): returns
   *  `true` when the click was the tail of a long press and was swallowed. */
  swallowClick: (e: MouseEvent) => boolean;
}

export function createLongPress(
  onLongPress: (clientX: number, clientY: number) => void,
  delayMs: number = LONG_PRESS_MS,
): LongPressHandlers {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let start: { x: number; y: number } | null = null;
  let fired = false;

  const cancel = (): void => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    start = null;
  };

  return {
    onPointerDown(e) {
      if (e.pointerType === "mouse" || !e.isPrimary) return;
      cancel();
      fired = false;
      start = { x: e.clientX, y: e.clientY };
      const at = { x: e.clientX, y: e.clientY };
      timer = setTimeout(() => {
        timer = undefined;
        start = null;
        fired = true;
        onLongPress(at.x, at.y);
      }, delayMs);
    },
    onPointerMove(e) {
      if (start === null) return;
      if (movedPastTolerance(e.clientX - start.x, e.clientY - start.y)) cancel();
    },
    onPointerUp() {
      cancel();
    },
    onPointerCancel() {
      cancel();
    },
    onPointerLeave() {
      cancel();
    },
    swallowClick(e) {
      if (!fired) return false;
      fired = false;
      e.preventDefault();
      e.stopPropagation();
      return true;
    },
  };
}
