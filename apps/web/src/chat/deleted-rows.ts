// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A deleted message leaves the conversation (`docs/ux/message-actions.md`
 * section 9). Pure apart from the timers of {@link DeletedHold}, which take an
 * injectable clock so a test can drive them.
 *
 * The rule, one sentence: a deleted message is not drawn, except during its
 * hold - when its deletion was applied while the person was looking at the
 * conversation (this conversation open, the tab visible, and the row already
 * drawn before the deletion was applied), the bubble reads "Message deleted"
 * for {@link DELETED_HOLD_MS}, then collapses over {@link DELETED_COLLAPSE_MS}
 * and is gone. With reduced motion the row goes at the end of the hold. The
 * hold is in memory only: it is never persisted, and leaving the conversation
 * or the tab going hidden ends it.
 *
 * Every other deletion shows nothing: a reload, the account's other devices,
 * restored history, a message and its deletion in one batch (the message was
 * never drawn), a deletion that arrived while another screen was open.
 */

/** How long a deleted row reads "Message deleted" before it collapses
 *  (chosen, not measured). */
export const DELETED_HOLD_MS = 2000;
/** How long the collapse takes (chosen, not measured). */
export const DELETED_COLLAPSE_MS = 250;

/** Where a held row is: reading "Message deleted", or collapsing. */
export type DeletedPhase = "hold" | "collapsing";

/** The overlay shape this module reads: only whether a message is deleted. */
export interface DeletedFlag {
  readonly deleted?: boolean | undefined;
}

/**
 * The ids an op batch deleted: deleted in `next` and not in `prev`. Order is
 * `next`'s iteration order.
 */
export function newlyDeletedIds(
  prev: ReadonlyMap<number, DeletedFlag>,
  next: ReadonlyMap<number, DeletedFlag>,
): number[] {
  const out: number[] = [];
  for (const [id, state] of next) {
    if (state.deleted === true && prev.get(id)?.deleted !== true) out.push(id);
  }
  return out;
}

/**
 * Which of the ids an op batch deleted enter the hold: only while the
 * conversation is in front of the person, and only the rows that were drawn
 * before the batch was applied (a message and its deletion in one batch were
 * never drawn, so they show nothing).
 */
export function idsEnteringHold(input: {
  readonly drawnBefore: ReadonlySet<number>;
  readonly newlyDeleted: Iterable<number>;
  readonly inFront: boolean;
}): number[] {
  if (!input.inFront) return [];
  const out: number[] = [];
  for (const id of input.newlyDeleted) {
    if (input.drawnBefore.has(id) && !out.includes(id)) out.push(id);
  }
  return out;
}

/**
 * Whether a row is drawn: a row with no id (an un-acked send), a row that is
 * not deleted, or a deleted row in its hold.
 */
export function isRowDrawn(
  messageId: number | undefined,
  deleted: (id: number) => boolean,
  held: ReadonlyMap<number, DeletedPhase>,
): boolean {
  if (messageId === undefined) return true;
  if (!deleted(messageId)) return true;
  return held.has(messageId);
}

/** `items` without the deleted ones, except those in their hold. Keeps the
 *  input order. */
export function withoutDeleted<T>(
  items: readonly T[],
  idOf: (item: T) => number | undefined,
  deleted: (id: number) => boolean,
  held: ReadonlyMap<number, DeletedPhase>,
): T[] {
  return items.filter((item) => isRowDrawn(idOf(item), deleted, held));
}

/**
 * The newest item that is not deleted, scanning from the end, among those
 * `eligible` accepts; `undefined` when none is left. What a conversation's
 * row in the chat list previews (section 9, "Chat list and notifications").
 */
export function newestNotDeleted<T extends DeletedFlag>(
  items: readonly T[],
  eligible: (item: T) => boolean = () => true,
): T | undefined {
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i] as T;
    if (item.deleted === true) continue;
    if (eligible(item)) return item;
  }
  return undefined;
}

/** The timers {@link DeletedHold} runs on; the browser's by default. */
export interface HoldClock {
  readonly setTimeout: (fn: () => void, ms: number) => unknown;
  readonly clearTimeout: (handle: unknown) => void;
}

const browserClock: HoldClock = {
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (h) => globalThis.clearTimeout(h as ReturnType<typeof setTimeout>),
};

/**
 * The rows in their hold, for one open conversation. Each row has its own
 * timer: `hold` for {@link DELETED_HOLD_MS}, then `collapsing` for
 * {@link DELETED_COLLAPSE_MS} (skipped with reduced motion), then gone.
 * `onChange` receives a fresh map on every change (replace-to-rerender).
 */
export class DeletedHold {
  private phases = new Map<number, DeletedPhase>();
  private readonly timers = new Map<number, unknown>();

  constructor(
    private readonly onChange: (phases: ReadonlyMap<number, DeletedPhase>) => void,
    private readonly reducedMotion: () => boolean,
    private readonly clock: HoldClock = browserClock,
  ) {}

  /** The current phases (a snapshot). */
  current(): ReadonlyMap<number, DeletedPhase> {
    return this.phases;
  }

  /** Start the hold of each id that is not already held. */
  start(ids: readonly number[]): void {
    let changed = false;
    for (const id of ids) {
      if (this.phases.has(id)) continue;
      this.phases.set(id, "hold");
      changed = true;
      this.timers.set(
        id,
        this.clock.setTimeout(() => this.endHold(id), DELETED_HOLD_MS),
      );
    }
    if (changed) this.emit();
  }

  /** End every hold at once: the conversation left the screen. */
  clear(): void {
    for (const h of this.timers.values()) this.clock.clearTimeout(h);
    this.timers.clear();
    if (this.phases.size === 0) return;
    this.phases = new Map();
    this.emit();
  }

  private endHold(id: number): void {
    this.timers.delete(id);
    if (!this.phases.has(id)) return;
    if (this.reducedMotion()) {
      this.remove(id);
      return;
    }
    this.phases = new Map(this.phases).set(id, "collapsing");
    this.emit();
    this.timers.set(
      id,
      this.clock.setTimeout(() => {
        this.timers.delete(id);
        this.remove(id);
      }, DELETED_COLLAPSE_MS),
    );
  }

  private remove(id: number): void {
    const next = new Map(this.phases);
    next.delete(id);
    this.phases = next;
    this.emit();
  }

  private emit(): void {
    this.phases = new Map(this.phases);
    this.onChange(this.phases);
  }
}

/**
 * Start the collapse of a held row: fix its height to what it is now, then
 * set it to zero, so the CSS transition (`.deleted-collapsing` in
 * styles.css, {@link DELETED_COLLAPSE_MS}) animates it. The caller removes
 * the row when the time is up.
 */
export function collapseRowHeight(el: HTMLElement): void {
  const height = el.getBoundingClientRect().height;
  el.style.height = `${String(height)}px`;
  void el.offsetHeight;
  el.style.height = "0px";
}

/** Whether the person turned reduced motion on; `false` where it cannot be
 *  read. */
export function prefersReducedMotion(): boolean {
  try {
    return globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
  } catch {
    return false;
  }
}
