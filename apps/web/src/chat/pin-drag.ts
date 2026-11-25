// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Dragging a pinned conversation to another place among the pins
 * (`docs/ux/navigation.md` section 2.2.1, web and the desktop app).
 *
 * - Mouse or trackpad: press a pinned row and move the pointer more than
 *   {@link DRAG_START_PX}; a press without movement stays the click that opens
 *   the conversation.
 * - Touch or pen: press and hold (the row's own long press), then move; a
 *   hold lifted without moving opens the row's menu, as before.
 * - The row lifts and follows the pointer, the other pins move aside, the list
 *   scrolls near its top and bottom edges. Release drops; Escape, a release
 *   outside the list, or a cancelled touch puts everything back.
 * - A drop lands among the pins only: above the first is first, anywhere below
 *   the last is last. A drag never unpins.
 *
 * The geometry ({@link dropIndex}, {@link shiftOf}, {@link autoscrollStep}) is
 * pure and tested; {@link createPinDrag} wires it to the DOM. Rows are found
 * by their `data-row-key` attribute, so the list's own components carry no
 * drag code.
 */

/** Pointer movement that turns a mouse press into a drag (chosen: past the
 *  jitter of a click, short of a deliberate move). */
export const DRAG_START_PX = 5;
/** Movement after a touch hold that starts the drag; less is a still hold. */
export const HOLD_MOVE_PX = 8;
/** Distance from the list's top or bottom edge where it starts to scroll
 *  (chosen, not measured). */
export const AUTOSCROLL_EDGE_PX = 48;
/** The fastest autoscroll, per animation frame (chosen, not measured). */
export const AUTOSCROLL_MAX_PX = 14;

/** One pin row's place at the start of the drag, in the list's content
 *  coordinates (independent of its scroll position). */
export interface Slot {
  readonly top: number;
  readonly height: number;
}

/**
 * The index among the pins where the dragged one would land if dropped now:
 * the number of OTHER pins whose middle is above the dragged row's middle.
 * Always within `[0, slots.length - 1]`, so a drop never leaves the pins.
 */
export function dropIndex(slots: readonly Slot[], from: number, draggedMiddle: number): number {
  if (slots.length === 0) return 0;
  let index = 0;
  slots.forEach((s, i) => {
    if (i === from) return;
    if (s.top + s.height / 2 < draggedMiddle) index += 1;
  });
  return Math.max(0, Math.min(slots.length - 1, index));
}

/** The distance one row makes room for the dragged one: its height plus the
 *  gap between rows. */
export function pitchOf(slots: readonly Slot[], from: number): number {
  const s = slots[from];
  if (s === undefined) return 0;
  const a = slots[0];
  const b = slots[1];
  const gap = a !== undefined && b !== undefined ? Math.max(0, b.top - (a.top + a.height)) : 0;
  return s.height + gap;
}

/**
 * How far pin `i` moves aside while the dragged pin `from` would land at
 * `to`: up by one pitch for the pins it passes going down, down for those it
 * passes going up, nothing for the rest (the dragged one follows the pointer).
 */
export function shiftOf(i: number, from: number, to: number, pitch: number): number {
  if (i === from) return 0;
  if (from < to && i > from && i <= to) return -pitch;
  if (to < from && i >= to && i < from) return pitch;
  return 0;
}

/**
 * The scroll for one frame when the pointer is at `clientY` in a list whose
 * visible area spans `[top, bottom]`: negative near the top, positive near the
 * bottom, faster nearer the edge, zero elsewhere.
 */
export function autoscrollStep(clientY: number, top: number, bottom: number): number {
  const edge = Math.min(AUTOSCROLL_EDGE_PX, Math.max(0, (bottom - top) / 3));
  if (edge <= 0) return 0;
  if (clientY < top + edge) {
    const depth = Math.min(1, (top + edge - clientY) / edge);
    return -Math.ceil(depth * AUTOSCROLL_MAX_PX);
  }
  if (clientY > bottom - edge) {
    const depth = Math.min(1, (clientY - (bottom - edge)) / edge);
    return Math.ceil(depth * AUTOSCROLL_MAX_PX);
  }
  return 0;
}

/** Whether a pointer moved past `px` from where it went down. */
export function movedPast(dx: number, dy: number, px: number): boolean {
  return dx * dx + dy * dy > px * px;
}

/** One draggable pin, in the order the list draws them. */
export interface DragPin {
  /** The row element's `data-row-key`. */
  readonly key: string;
  /** What the drop reports. */
  readonly id: string;
}

export interface PinDragHost {
  /** The scrolling list the rows are in. */
  container(): HTMLElement | undefined;
  /** The pins that may be dragged right now, in drawn order; empty when no
   *  drag may start (fewer than two pins, a search, the archive view). */
  pins(): readonly DragPin[];
  /** A drop that moved the pin: `toIndex` among the pins. */
  onDrop(id: string, toIndex: number): void;
  /** A touch hold lifted without moving: the row's menu, as before. */
  onHoldMenu(key: string, clientX: number, clientY: number): void;
}

export interface PinDrag {
  /** Listen on the container; returns the detach. */
  attach(container: HTMLElement): () => void;
  /**
   * A row's long press fired (touch or pen). Returns `true` when the row is a
   * draggable pin and the hold now belongs to the drag: moving starts it,
   * lifting opens the menu through {@link PinDragHost.onHoldMenu}.
   */
  takeHold(key: string, clientX: number, clientY: number): boolean;
  /** Whether a drag (or a touch hold that may become one) is under way. */
  active(): boolean;
}

type Phase =
  | { readonly t: "idle" }
  | { readonly t: "pressed"; readonly key: string; readonly x: number; readonly y: number; readonly pointerId: number }
  | { readonly t: "held"; readonly key: string; readonly x: number; readonly y: number }
  | {
      readonly t: "dragging";
      readonly pins: readonly DragPin[];
      readonly elements: readonly HTMLElement[];
      readonly slots: readonly Slot[];
      readonly from: number;
      readonly pitch: number;
      readonly startContentY: number;
      to: number;
      clientY: number;
    };

function rowKeyOf(target: EventTarget | null): { key: string; el: HTMLElement } | null {
  if (!(target instanceof Element)) return null;
  const el = target.closest<HTMLElement>("[data-row-key]");
  if (el === null) return null;
  const key = el.getAttribute("data-row-key");
  return key === null ? null : { key, el };
}

function elementOf(container: HTMLElement, key: string): HTMLElement | null {
  for (const el of Array.from(container.querySelectorAll<HTMLElement>("[data-row-key]"))) {
    if (el.getAttribute("data-row-key") === key) return el;
  }
  return null;
}

export function createPinDrag(host: PinDragHost): PinDrag {
  let phase: Phase = { t: "idle" };
  let frame: number | null = null;
  let suppressClick = false;

  const isPin = (key: string): boolean => host.pins().some((p) => p.key === key);

  const contentY = (container: HTMLElement, clientY: number): number =>
    clientY - container.getBoundingClientRect().top + container.scrollTop;

  const clearStyles = (elements: readonly HTMLElement[]): void => {
    for (const el of elements) {
      el.style.transform = "";
      el.classList.remove("lk-row-lifted", "lk-row-shifting");
    }
  };

  const stopFrame = (): void => {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
  };

  const removeWindowListeners = (): void => {
    window.removeEventListener("pointermove", onWindowMove, true);
    window.removeEventListener("pointerup", onWindowUp, true);
    window.removeEventListener("pointercancel", onWindowCancel, true);
    window.removeEventListener("keydown", onWindowKey, true);
  };

  const addWindowListeners = (): void => {
    window.addEventListener("pointermove", onWindowMove, true);
    window.addEventListener("pointerup", onWindowUp, true);
    window.addEventListener("pointercancel", onWindowCancel, true);
    window.addEventListener("keydown", onWindowKey, true);
  };

  /** Back to rest; `drop` reports the move first when it moved. */
  const finish = (drop: boolean): void => {
    const p = phase;
    phase = { t: "idle" };
    stopFrame();
    removeWindowListeners();
    if (p.t !== "dragging") return;
    host.container()?.classList.remove("lk-pin-dragging");
    // The click that follows this release (if it lands in the list) is
    // swallowed; one that lands elsewhere must not leave the flag set for
    // the next real click. The click is dispatched in the same task as the
    // release, before this timer.
    setTimeout(() => {
      suppressClick = false;
    }, 0);
    const moved = p.to !== p.from;
    const pin = p.pins[p.from];
    // The new order is applied first (the list reorders its elements at
    // once), then the transforms go, so the row does not flash back to its
    // old place in between.
    if (drop && moved && pin !== undefined) host.onDrop(pin.id, p.to);
    clearStyles(p.elements);
  };

  const layout = (p: Extract<Phase, { t: "dragging" }>): void => {
    const container = host.container();
    if (container === undefined) return;
    const dy = contentY(container, p.clientY) - p.startContentY;
    const dragged = p.slots[p.from];
    if (dragged === undefined) return;
    p.to = dropIndex(p.slots, p.from, dragged.top + dragged.height / 2 + dy);
    p.elements.forEach((el, i) => {
      if (i === p.from) {
        el.style.transform = `translateY(${dy}px)`;
      } else {
        const shift = shiftOf(i, p.from, p.to, p.pitch);
        el.style.transform = shift === 0 ? "" : `translateY(${shift}px)`;
      }
    });
  };

  const tick = (): void => {
    frame = null;
    const p = phase;
    const container = host.container();
    if (p.t !== "dragging" || container === undefined) return;
    const rect = container.getBoundingClientRect();
    const step = autoscrollStep(p.clientY, rect.top, rect.bottom);
    if (step !== 0) {
      const before = container.scrollTop;
      container.scrollTop = before + step;
      if (container.scrollTop !== before) layout(p);
    }
    frame = requestAnimationFrame(tick);
  };

  const begin = (key: string, clientY: number, startClientY: number): boolean => {
    const container = host.container();
    if (container === undefined) return false;
    const pins = host.pins();
    const from = pins.findIndex((p) => p.key === key);
    if (from < 0 || pins.length < 2) return false;
    const elements: HTMLElement[] = [];
    for (const pin of pins) {
      const el = elementOf(container, pin.key);
      if (el === null) return false;
      elements.push(el);
    }
    const crect = container.getBoundingClientRect();
    const slots = elements.map((el) => {
      const r = el.getBoundingClientRect();
      return { top: r.top - crect.top + container.scrollTop, height: r.height };
    });
    const next: Extract<Phase, { t: "dragging" }> = {
      t: "dragging",
      pins,
      elements,
      slots,
      from,
      pitch: pitchOf(slots, from),
      startContentY: contentY(container, startClientY),
      to: from,
      clientY,
    };
    phase = next;
    container.classList.add("lk-pin-dragging");
    elements.forEach((el, i) => el.classList.add(i === from ? "lk-row-lifted" : "lk-row-shifting"));
    window.getSelection()?.removeAllRanges();
    layout(next);
    frame = requestAnimationFrame(tick);
    return true;
  };

  function onWindowMove(e: PointerEvent): void {
    const p = phase;
    if (p.t === "pressed") {
      if (e.pointerId !== p.pointerId) return;
      if (!movedPast(e.clientX - p.x, e.clientY - p.y, DRAG_START_PX)) return;
      if (begin(p.key, e.clientY, p.y)) {
        suppressClick = true;
      } else {
        finish(false);
      }
      return;
    }
    if (p.t === "held") {
      if (!movedPast(e.clientX - p.x, e.clientY - p.y, HOLD_MOVE_PX)) return;
      if (!begin(p.key, e.clientY, p.y)) finish(false);
      return;
    }
    if (p.t === "dragging") {
      e.preventDefault();
      p.clientY = e.clientY;
      layout(p);
    }
  }

  function onWindowUp(e: PointerEvent): void {
    const p = phase;
    if (p.t === "held") {
      phase = { t: "idle" };
      removeWindowListeners();
      host.onHoldMenu(p.key, p.x, p.y);
      return;
    }
    if (p.t !== "dragging") {
      finish(false);
      return;
    }
    const container = host.container();
    const r = container?.getBoundingClientRect();
    const inside =
      r !== undefined &&
      e.clientX >= r.left &&
      e.clientX <= r.right &&
      e.clientY >= r.top &&
      e.clientY <= r.bottom;
    finish(inside);
  }

  function onWindowCancel(): void {
    finish(false);
  }

  function onWindowKey(e: KeyboardEvent): void {
    if (e.key !== "Escape") return;
    if (phase.t !== "dragging" && phase.t !== "held") return;
    e.preventDefault();
    e.stopPropagation();
    finish(false);
  }

  return {
    attach(container) {
      const onDown = (e: PointerEvent): void => {
        if (phase.t !== "idle") return;
        if (e.pointerType !== "mouse" || e.button !== 0 || !e.isPrimary) return;
        if (e.target instanceof Element && e.target.closest("button, input, textarea, select") !== null) {
          return;
        }
        const hit = rowKeyOf(e.target);
        if (hit === null || !isPin(hit.key)) return;
        phase = { t: "pressed", key: hit.key, x: e.clientX, y: e.clientY, pointerId: e.pointerId };
        addWindowListeners();
      };
      // A link is natively draggable; the browser's own drag would take the
      // pointer away from a pin being reordered.
      const onDragStart = (e: DragEvent): void => {
        const hit = rowKeyOf(e.target);
        if (hit !== null && isPin(hit.key)) e.preventDefault();
      };
      // The click that ends a mouse drag is not a tap on the row.
      const onClick = (e: MouseEvent): void => {
        if (!suppressClick) return;
        suppressClick = false;
        e.preventDefault();
        e.stopPropagation();
      };
      // After a touch hold the finger moves the row, not the list.
      const onTouchMove = (e: TouchEvent): void => {
        if (phase.t === "held" || phase.t === "dragging") e.preventDefault();
      };
      container.addEventListener("pointerdown", onDown);
      container.addEventListener("dragstart", onDragStart, true);
      container.addEventListener("click", onClick, true);
      container.addEventListener("touchmove", onTouchMove, { passive: false });
      return () => {
        finish(false);
        container.removeEventListener("pointerdown", onDown);
        container.removeEventListener("dragstart", onDragStart, true);
        container.removeEventListener("click", onClick, true);
        container.removeEventListener("touchmove", onTouchMove);
      };
    },
    takeHold(key, clientX, clientY) {
      if (phase.t !== "idle" || !isPin(key)) return false;
      phase = { t: "held", key, x: clientX, y: clientY };
      addWindowListeners();
      return true;
    },
    active() {
      return phase.t === "held" || phase.t === "dragging";
    },
  };
}
