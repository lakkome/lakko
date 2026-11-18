// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` 7.1a — drag a row by its handle to reorder. Pointer
 * events (mouse, pen and touch alike); the keyboard and screen readers use
 * `Alt+Up` / `Alt+Down` in the row instead. The geometry is pure and tested.
 */

/** The index a dragged row lands at, from the pointer's y and the rows'
 *  vertical extents (in order), clamped to `0..=maxIndex`. */
export function dropIndex(pointerY: number, rows: readonly { top: number; bottom: number }[], maxIndex: number): number {
  let at = 0;
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i]!;
    if (pointerY >= (r.top + r.bottom) / 2) at = i;
  }
  return Math.max(0, Math.min(at, maxIndex));
}

/** Where index `i` ends up when the row at `from` moves to `to`. */
export function remapIndex(i: number, from: number, to: number): number {
  if (i === from) return to;
  if (from < to && i > from && i <= to) return i - 1;
  if (to < from && i >= to && i < from) return i + 1;
  return i;
}

/**
 * Start a drag from a handle: rows are the container's direct children with
 * `data-reorder-row`. Calls `onDrop(from, to)` once on release when the row
 * moved; marks the dragged row with `dragging` while it moves.
 */
export function startDragReorder(
  e: PointerEvent,
  container: HTMLElement | undefined,
  from: number,
  maxIndex: number,
  onDrop: (from: number, to: number) => void,
): void {
  if (container === undefined || from > maxIndex) return;
  e.preventDefault();
  const handle = e.currentTarget as HTMLElement | null;
  handle?.setPointerCapture?.(e.pointerId);
  const rowEls = (): HTMLElement[] => Array.from(container.querySelectorAll<HTMLElement>("[data-reorder-row]"));
  const dragged = rowEls()[from];
  dragged?.classList.add("dragging");
  let to = from;
  const move = (ev: PointerEvent): void => {
    const rects = rowEls().map((el) => el.getBoundingClientRect());
    to = dropIndex(ev.clientY, rects, maxIndex);
    for (const [k, el] of rowEls().entries()) el.classList.toggle("drop-target", k === to && to !== from);
  };
  const end = (): void => {
    handle?.removeEventListener("pointermove", move);
    handle?.removeEventListener("pointerup", end);
    handle?.removeEventListener("pointercancel", end);
    for (const el of rowEls()) el.classList.remove("dragging", "drop-target");
    if (to !== from) onDrop(from, to);
  };
  handle?.addEventListener("pointermove", move);
  handle?.addEventListener("pointerup", end);
  handle?.addEventListener("pointercancel", end);
}
