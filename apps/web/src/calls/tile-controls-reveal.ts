// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which call tile shows its controls after a tap
 * (`docs/ux/group-call-grid.md` section 6.2).
 *
 * A touch screen has no hover, so the pin button of a tile is brought up by
 * a tap on the tile. That tap does nothing else: it never pins. The controls
 * of one tile at a time stay up for a few seconds, and go when the time runs
 * out or the person taps anywhere else.
 *
 * Pure: the timer is handed in, so the whole rule runs in a test.
 */

/** How long a tile's controls stay up after the tap. Chosen, not measured. */
export const TILE_CONTROLS_REVEAL_MS = 3_000;

/** The timer this needs; the defaults are the platform's. */
export interface RevealTimers {
  readonly setTimeout: (run: () => void, ms: number) => unknown;
  readonly clearTimeout: (handle: unknown) => void;
}

const platformTimers: RevealTimers = {
  setTimeout: (run, ms) => setTimeout(run, ms),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** Holds the one tile whose controls are up. */
export class TileControlsReveal {
  private shown: string | null = null;
  private timer: unknown = null;

  public constructor(
    private readonly onChange: (tileId: string | null) => void,
    private readonly timers: RevealTimers = platformTimers,
  ) {}

  /** The tile whose controls are up, or `null`. */
  public get tileId(): string | null {
    return this.shown;
  }

  /** A tap on a tile: its controls come up, and the time starts again. */
  public tap(tileId: string): void {
    this.stopTimer();
    this.timer = this.timers.setTimeout(() => {
      this.timer = null;
      this.set(null);
    }, TILE_CONTROLS_REVEAL_MS);
    this.set(tileId);
  }

  /**
   * A press began somewhere: on the tile with this id, or on no tile
   * (`null`). Anywhere but the tile whose controls are up hides them.
   */
  public pressBegan(tileId: string | null): void {
    if (this.shown !== null && tileId !== this.shown) this.hide();
  }

  /** Hide the controls now. */
  public hide(): void {
    this.stopTimer();
    this.set(null);
  }

  /** Stop the timer; nothing is reported any more. */
  public dispose(): void {
    this.stopTimer();
    this.shown = null;
  }

  private stopTimer(): void {
    if (this.timer !== null) this.timers.clearTimeout(this.timer);
    this.timer = null;
  }

  private set(tileId: string | null): void {
    if (this.shown === tileId) return;
    this.shown = tileId;
    this.onChange(tileId);
  }
}
