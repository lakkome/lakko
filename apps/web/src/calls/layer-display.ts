// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which of a remote camera's two pictures a tile shows.
 *
 * A publisher's uplink that loses most of its packets for about a second
 * cannot be repaired inside a 720p stream in that second: the picture stands
 * for about a second and then plays the late second fast. The light layer
 * (180p, 150 kbit/s) is repaired in a fraction of that. So a viewer receives
 * both as two tracks (`light-stream.ts`), decodes both all the time, and the
 * tile shows the light one the moment the full one stands still.
 *
 * This module is the rule and its counters. Pure: no DOM, no timers, and the
 * caller supplies every timestamp (one clock, milliseconds). One
 * {@link LayerDisplay} per remote tile; the tile reports each presented frame
 * of each track and a low-frequency tick, and reads back what to show.
 *
 * The rules:
 *
 *   * `full` is shown by default, and always while there is no light track.
 *   * `full` -> `light` when the full track has presented no frame for
 *     {@link FULL_STALL_MS} and the light track presented one within
 *     {@link LIGHT_FRESH_MS}.
 *   * `light` -> `full` only once the full track has presented frames with no
 *     gap longer than {@link FULL_STALL_MS} for {@link FULL_STEADY_MS}. A gap
 *     inside that window starts it again. The window also hides the fast
 *     catch-up playback that follows a stall.
 *   * Before the full track's first frame, `light` is shown only when the
 *     full track has presented nothing for {@link FIRST_FRAME_GRACE_MS} while
 *     the light one plays, so a slow first key frame on the full layer does
 *     not leave the tile empty.
 *   * When both stand still, whatever is shown stays shown.
 *
 * A **displayed stall** is the shown track presenting no frame for more than
 * {@link FULL_STALL_MS}. Its length is the whole time the picture stood: from
 * the shown track's last presented frame (or from the moment that track
 * became the shown one, when that is later) until its next frame or until it
 * stops being shown. A stall the switch to `light` cuts short is counted at
 * the length it had when the switch happened: with the tile's tick that is
 * between {@link FULL_STALL_MS} and that plus one tick, and it is what the
 * viewer saw.
 */

/** Which track a tile shows. */
export type LayerShown = "full" | "light";

/**
 * The full track is standing still once it has presented no frame for this
 * long, milliseconds. Chosen, not measured: about four frame intervals at
 * 30 frames a second and two at 15, so ordinary jitter does not trip it.
 */
export const FULL_STALL_MS = 150;

/**
 * The light track is playing if it presented a frame within this,
 * milliseconds. Chosen, not measured: longer than {@link FULL_STALL_MS} so a
 * light track at a low frame rate still counts as playing when the full one
 * is found standing.
 */
export const LIGHT_FRESH_MS = 250;

/**
 * How long the full track must present frames without a gap longer than
 * {@link FULL_STALL_MS} before the tile returns to it, milliseconds. Chosen,
 * not measured: the late second a recovered stream plays fast was about a
 * second long on the calls that prompted this.
 */
export const FULL_STEADY_MS = 1000;

/**
 * How long a full track that has never presented a frame is waited for before
 * a playing light track is shown instead, milliseconds. Chosen, not measured.
 */
export const FIRST_FRAME_GRACE_MS = 1500;

/** What a {@link LayerDisplay} counted, milliseconds where it says so. */
export interface LayerDisplayCounters {
  /** Times the tile switched to the light track. */
  readonly lightSwitches: number;
  /** Total time the light track was the shown one. */
  readonly lightMs: number;
  /** The longest displayed stall. */
  readonly stallMaxMs: number;
  /** Total displayed stall time. */
  readonly stallTotalMs: number;
}

/** Counters with nothing counted. */
export const NO_LAYER_DISPLAY_COUNTERS: LayerDisplayCounters = {
  lightSwitches: 0,
  lightMs: 0,
  stallMaxMs: 0,
  stallTotalMs: 0,
};

/** Two sets of counters as one: sums, and the longer of the longest stalls. */
export function addLayerDisplayCounters(
  a: LayerDisplayCounters,
  b: LayerDisplayCounters,
): LayerDisplayCounters {
  return {
    lightSwitches: a.lightSwitches + b.lightSwitches,
    lightMs: a.lightMs + b.lightMs,
    stallMaxMs: Math.max(a.stallMaxMs, b.stallMaxMs),
    stallTotalMs: a.stallTotalMs + b.stallTotalMs,
  };
}

/** One remote tile's choice between its two tracks. */
export class LayerDisplay {
  private current: LayerShown = "full";
  /** When the shown track became the shown one. */
  private shownSince: number;
  /** The grace for the full track's first frame runs from here. */
  private startedAt: number;
  /** The last time the clock was advanced; time never runs backwards. */
  private lastEval: number;
  private lightAvailable: boolean;
  private lastFull: number | null = null;
  private lastLight: number | null = null;
  /** Start of the full track's current run of frames with no gap longer than
   *  {@link FULL_STALL_MS}; `null` while it has none. */
  private steadySince: number | null = null;
  /** How much of the displayed stall in progress is already counted. */
  private stallCounted = 0;

  private total: LayerDisplayCounters = NO_LAYER_DISPLAY_COUNTERS;
  private interval: LayerDisplayCounters = NO_LAYER_DISPLAY_COUNTERS;

  /**
   * @param now when the tile started showing this peer.
   * @param lightAvailable whether the peer has a light track yet.
   */
  constructor(now: number, lightAvailable = false) {
    this.shownSince = now;
    this.startedAt = now;
    this.lastEval = now;
    this.lightAvailable = lightAvailable;
  }

  /** What the tile shows. */
  shown(): LayerShown {
    return this.current;
  }

  /** The full track presented a frame at `t`. */
  fullFrame(t: number): LayerShown {
    const now = this.advance(t);
    if (this.current === "full") {
      // Close the stall this frame ends, at its full length.
      this.countStall(now);
      this.stallCounted = 0;
    }
    if (
      this.steadySince === null ||
      this.lastFull === null ||
      now - this.lastFull > FULL_STALL_MS
    ) {
      this.steadySince = now;
    }
    this.lastFull = now;
    return this.settle(now);
  }

  /** The light track presented a frame at `t`. */
  lightFrame(t: number): LayerShown {
    const now = this.advance(t);
    if (this.current === "light") {
      this.countStall(now);
      this.stallCounted = 0;
    }
    this.lastLight = now;
    return this.settle(now);
  }

  /** Time has passed with no frame: `now`. Notices a stall without a frame
   *  arriving. */
  tick(now: number): LayerShown {
    return this.settle(this.advance(now));
  }

  /** The peer's light track appeared or went away. Without one the tile
   *  shows `full`, whatever the full track is doing. */
  setLightAvailable(available: boolean, t: number): LayerShown {
    const now = this.advance(t);
    this.lightAvailable = available;
    if (!available) this.lastLight = null;
    return this.settle(now);
  }

  /**
   * The tile could not observe frames between the last report and `t` (a
   * hidden page presents none). Nothing is counted for that time, what was
   * known about either track is forgotten, and the tile starts again on
   * `full` as a new tile would. Not counted as a switch.
   */
  resync(t: number): LayerShown {
    const now = Math.max(t, this.lastEval);
    this.lastEval = now;
    this.current = "full";
    this.shownSince = now;
    this.startedAt = now;
    this.lastFull = null;
    this.lastLight = null;
    this.steadySince = null;
    this.stallCounted = 0;
    return this.current;
  }

  /** Everything counted since the tile started. */
  totals(): LayerDisplayCounters {
    return this.total;
  }

  /**
   * Everything counted since the last drain, and start a new interval. A
   * displayed stall still in progress is reported in each interval it runs
   * through: its time is split between them, and each interval's longest
   * stall is the length it had reached by then.
   */
  drain(): LayerDisplayCounters {
    const out = this.interval;
    this.interval = NO_LAYER_DISPLAY_COUNTERS;
    return out;
  }

  /** Move the clock to `t` (never backwards) and count the time shown light. */
  private advance(t: number): number {
    const now = Math.max(t, this.lastEval);
    const dt = now - this.lastEval;
    if (dt > 0 && this.current === "light") this.count({ lightMs: dt });
    this.lastEval = now;
    return now;
  }

  /** Count the shown track's stall up to `now`, decide what is shown from
   *  `now` on, and count again in case the newly shown track is the one
   *  standing still. */
  private settle(now: number): LayerShown {
    this.countStall(now);
    this.decide(now);
    this.countStall(now);
    return this.current;
  }

  private decide(now: number): void {
    if (!this.lightAvailable) {
      if (this.current === "light") this.switchTo("full", now);
      return;
    }
    const fullGap = this.lastFull === null ? null : now - this.lastFull;
    if (this.current === "full") {
      const lightFresh = this.lastLight !== null && now - this.lastLight <= LIGHT_FRESH_MS;
      if (!lightFresh) return;
      const standing =
        fullGap === null ? now - this.startedAt >= FIRST_FRAME_GRACE_MS : fullGap >= FULL_STALL_MS;
      if (standing) {
        // The run of steady full frames is over; the next full frame starts
        // a new one.
        this.steadySince = null;
        this.switchTo("light", now);
      }
      return;
    }
    // Shown: light.
    if (fullGap === null || fullGap > FULL_STALL_MS) {
      // A gap inside the steady window starts it again.
      this.steadySince = null;
      return;
    }
    if (this.steadySince !== null && now - this.steadySince >= FULL_STEADY_MS) {
      this.switchTo("full", now);
    }
  }

  private switchTo(next: LayerShown, now: number): void {
    this.current = next;
    this.shownSince = now;
    this.stallCounted = 0;
    if (next === "light") this.count({ lightSwitches: 1 });
  }

  /** Count the displayed stall of the shown track up to `now`. */
  private countStall(now: number): void {
    const last = this.current === "full" ? this.lastFull : this.lastLight;
    if (last === null || now - last <= FULL_STALL_MS) return;
    const length = now - Math.max(last, this.shownSince);
    this.count({
      stallTotalMs: Math.max(0, length - this.stallCounted),
      stallMaxMs: length,
    });
    this.stallCounted = Math.max(this.stallCounted, length);
  }

  private count(delta: Partial<LayerDisplayCounters>): void {
    const add: LayerDisplayCounters = { ...NO_LAYER_DISPLAY_COUNTERS, ...delta };
    this.total = addLayerDisplayCounters(this.total, add);
    this.interval = addLayerDisplayCounters(this.interval, add);
  }
}

/**
 * What every remote tile of a call counted, by peer.
 *
 * A peer can be drawn by several tiles over a call (the grid, the stage, a
 * tile rebuilt when the layout changes), so each tile hands over what it
 * drained and the ledger adds it up. The media counters read an interval at a
 * time and the total at the end of the call.
 */
export class LayerDisplayLedger {
  private readonly totalByPeer = new Map<string, LayerDisplayCounters>();
  private readonly intervalByPeer = new Map<string, LayerDisplayCounters>();

  /** A tile of `peerId` reports what it counted since its last report. */
  add(peerId: string, counters: LayerDisplayCounters): void {
    this.totalByPeer.set(
      peerId,
      addLayerDisplayCounters(this.totalByPeer.get(peerId) ?? NO_LAYER_DISPLAY_COUNTERS, counters),
    );
    this.intervalByPeer.set(
      peerId,
      addLayerDisplayCounters(
        this.intervalByPeer.get(peerId) ?? NO_LAYER_DISPLAY_COUNTERS,
        counters,
      ),
    );
  }

  /** Every peer a tile reported on since the last call, and start a new
   *  interval. A peer no tile reported on is absent. */
  takeInterval(): ReadonlyMap<string, LayerDisplayCounters> {
    const out = new Map(this.intervalByPeer);
    this.intervalByPeer.clear();
    return out;
  }

  /** Every peer a tile ever reported on, counted from the start. */
  totals(): ReadonlyMap<string, LayerDisplayCounters> {
    return new Map(this.totalByPeer);
  }

  /** Forget everything: a new call starts. */
  reset(): void {
    this.totalByPeer.clear();
    this.intervalByPeer.clear();
  }
}

/** The ledger of the call this page is in: the tiles write to it, the group
 *  call's media counters read it. */
export const shownLayerLedger = new LayerDisplayLedger();
