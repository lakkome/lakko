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
 * tile shows the light one once the full one has really stopped.
 *
 * "Really stopped" is why there are two thresholds. A full track that
 * delivers its frames at a steady average rate but in clumps (a publisher on
 * a congested uplink: 28 frames a second with three gaps of about 200 ms
 * every two seconds, seen on a live call) is sharp and moving and is the one
 * to show. Such gaps are counted as displayed stalls ({@link FULL_STALL_MS})
 * but neither send the tile to the light copy nor keep it there
 * ({@link FULL_STOPPED_MS}). While one threshold did both jobs, that tile
 * showed the light copy for the rest of the call.
 *
 * The rule and its counters are written once, in Rust
 * (`crates/lakko-ui-rules`, `layer_display`), and reached here over wasm; iOS
 * and Android reach the same rule over UniFFI, so the three clients switch at
 * the same moments. This module is the web side of it: {@link LayerDisplay}
 * wraps the shared object under the names this client's tiles use, and
 * {@link LayerDisplayLedger} adds up what the tiles of a call counted.
 *
 * No DOM, no timers, and the caller supplies every timestamp (one clock,
 * milliseconds). One {@link LayerDisplay} per remote tile; the tile reports
 * each presented frame of each track and a low-frequency tick, and reads back
 * what to show.
 *
 * The rules, as the shared module states them:
 *
 *   * `full` is shown by default, and always while there is no light track.
 *   * `full` -> `light` when the full track has presented no frame for
 *     {@link FULL_STOPPED_MS} and the light track presented one within
 *     {@link LIGHT_FRESH_MS}.
 *   * `light` -> `full` only once the full track has presented frames with no
 *     gap longer than {@link FULL_STOPPED_MS} for {@link FULL_STEADY_MS}. A
 *     longer gap inside that window starts it again. The window also hides the fast
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
 * between {@link FULL_STOPPED_MS} and that plus one tick, and it is what the
 * viewer saw.
 */

import { LayerDisplay as WasmLayerDisplay, ShownLayer } from "../../wasm/lakko_ffi_wasm";

/** Which track a tile shows. */
export type LayerShown = "full" | "light";

/**
 * The shown track is counted as standing still (a displayed stall) once it
 * has presented no frame for more than this, milliseconds. Chosen, not
 * measured: about four frame intervals at 30 frames a second and two at 15,
 * so ordinary jitter is not counted. It only counts; what is shown is decided
 * by {@link FULL_STOPPED_MS}.
 *
 * This and the constants below restate the shared rule's values
 * (`layerFullStallMs()` and its neighbours over wasm) so they can be read
 * before the wasm module is loaded; `layer-display.test.ts` pins that they
 * are equal. The rule itself never reads these copies.
 */
export const FULL_STALL_MS = 150;

/**
 * The full track has really stopped once it has presented no frame for this
 * long, milliseconds: the tile then shows a playing light track, and a full
 * track counts as playing again only while its gaps stay within this. Chosen,
 * not measured: twice {@link FULL_STALL_MS}, so a track whose frames arrive
 * in clumps with gaps of about 200 ms stays the shown one.
 *
 * The shared rule has no wasm getter for this one; `layer-display.test.ts`
 * pins it against the rule's behaviour (the tick at which a stopped full
 * track hands over) instead.
 */
export const FULL_STOPPED_MS = 300;

/**
 * The light track is playing if it presented a frame within this,
 * milliseconds. Chosen, not measured: longer than {@link FULL_STALL_MS} so a
 * light track at a low frame rate still counts as playing when the full one
 * is found standing.
 */
export const LIGHT_FRESH_MS = 250;

/**
 * How long the full track must present frames without a gap longer than
 * {@link FULL_STOPPED_MS} before the tile returns to it, milliseconds. Chosen,
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

/** `ShownLayer` of the shared rule, as this module's names. */
function layerName(layer: ShownLayer): LayerShown {
  return layer === ShownLayer.Light ? "light" : "full";
}

/** The shared rule's four numbers (`[lightSwitches, lightMs, stallMaxMs,
 *  stallTotalMs]`) as counters. */
function countersOf(numbers: Float64Array): LayerDisplayCounters {
  return {
    lightSwitches: numbers[0] ?? 0,
    lightMs: numbers[1] ?? 0,
    stallMaxMs: numbers[2] ?? 0,
    stallTotalMs: numbers[3] ?? 0,
  };
}

/**
 * One remote tile's choice between its two tracks.
 *
 * A thin wrapper: every decision and every count is the shared rule's
 * (`LayerDisplay` of `lakko-ui-rules`, over wasm). What is here is the string
 * names this client's tiles use and the counters as an object.
 *
 * The shared rule keeps whole milliseconds, so a fractional timestamp
 * (`performance.now()`) is read as the millisecond it falls in.
 *
 * The wasm module is loaded by the time a call is on screen. Should a tile be
 * built before it is, the tile shows `full` and counts nothing, exactly as a
 * tile that cannot observe frames does, and starts following the rule on the
 * first report after the module is ready.
 */
export class LayerDisplay {
  private inner: WasmLayerDisplay | null = null;
  private lightAvailable: boolean;
  private disposed = false;

  /**
   * @param now when the tile started showing this peer.
   * @param lightAvailable whether the peer has a light track yet.
   */
  constructor(now: number, lightAvailable = false) {
    this.lightAvailable = lightAvailable;
    this.rule(now);
  }

  /** What the tile shows. */
  shown(): LayerShown {
    return this.inner === null ? "full" : layerName(this.inner.shown());
  }

  /** The full track presented a frame at `t`. */
  fullFrame(t: number): LayerShown {
    const rule = this.rule(t);
    return rule === null ? "full" : layerName(rule.fullFrame(t));
  }

  /** The light track presented a frame at `t`. */
  lightFrame(t: number): LayerShown {
    const rule = this.rule(t);
    return rule === null ? "full" : layerName(rule.lightFrame(t));
  }

  /** Time has passed with no frame: `now`. Notices a stall without a frame
   *  arriving. */
  tick(now: number): LayerShown {
    const rule = this.rule(now);
    return rule === null ? "full" : layerName(rule.tick(now));
  }

  /** The peer's light track appeared or went away. Without one the tile
   *  shows `full`, whatever the full track is doing. */
  setLightAvailable(available: boolean, t: number): LayerShown {
    this.lightAvailable = available;
    const rule = this.rule(t);
    return rule === null ? "full" : layerName(rule.setLightAvailable(available, t));
  }

  /**
   * The tile could not observe frames between the last report and `t` (a
   * hidden page presents none). Nothing is counted for that time, what was
   * known about either track is forgotten, and the tile starts again on
   * `full` as a new tile would. Not counted as a switch.
   */
  resync(t: number): LayerShown {
    const rule = this.rule(t);
    return rule === null ? "full" : layerName(rule.resync(t));
  }

  /** Everything counted since the tile started. */
  totals(): LayerDisplayCounters {
    return this.inner === null ? NO_LAYER_DISPLAY_COUNTERS : countersOf(this.inner.counters());
  }

  /**
   * Everything counted since the last drain, and start a new interval. A
   * displayed stall still in progress is reported in each interval it runs
   * through: its time is split between them, and each interval's longest
   * stall is the length it had reached by then.
   */
  drain(): LayerDisplayCounters {
    return this.inner === null ? NO_LAYER_DISPLAY_COUNTERS : countersOf(this.inner.drain());
  }

  /** Release the shared rule's memory now rather than when the tile is
   *  collected. The tile shows `full` and counts nothing afterwards. */
  dispose(): void {
    this.inner?.free();
    this.inner = null;
    this.disposed = true;
  }

  /** The shared rule, built on first use; `null` while the wasm module is
   *  not ready (or after {@link dispose}). */
  private rule(now: number): WasmLayerDisplay | null {
    if (this.inner !== null || this.disposed) return this.inner;
    try {
      this.inner = new WasmLayerDisplay(now, this.lightAvailable);
    } catch {
      // The wasm module is not instantiated yet.
      this.inner = null;
    }
    return this.inner;
  }
}

/** Whether a tile's heavy picture has shown a frame: `none` when the tile has
 *  no heavy picture (the light one is shown alone), `never` when it has one
 *  that has shown no frame since it was put there, `yes` otherwise. */
export type HeavySeen = "none" | "never" | "yes";

/**
 * The frames a tile observed on its two elements, beside what the rule
 * counted. The rule's counters cannot tell three different things apart that
 * all read `light_switches=0` with `light_ms` the whole interval:
 *
 *   * the heavy picture has never produced a frame (a slow first key frame:
 *     the tile is right to show the light copy): `heavy=never`;
 *   * the heavy element's frames do not reach the rule (a binding fault):
 *     `heavy=never`, or `heavy=yes` with `heavy_frames=0`, while the
 *     receiver of that track reports decoded frames;
 *   * the heavy picture produces frames and the rule keeps the light copy
 *     (its thresholds): `heavy=yes` with `heavy_frames` near the receiver's
 *     decoded count, and `heavy_gap_max_ms` saying how irregular they were.
 */
export interface TileFrameCounts {
  readonly heavy: HeavySeen;
  /** Frames observed on the heavy element. */
  readonly heavyFrames: number;
  /** The longest time without a heavy frame, milliseconds: between two
   *  frames, or from the last one to the end of the interval. Zero before
   *  the heavy picture's first frame. */
  readonly heavyGapMaxMs: number;
  /** Frames observed on the light element. */
  readonly lightFrames: number;
  /** What the tile showed when it last reported. */
  readonly shownNow: LayerShown;
}

/** Frame counts with nothing observed. */
export const NO_TILE_FRAME_COUNTS: TileFrameCounts = {
  heavy: "none",
  heavyFrames: 0,
  heavyGapMaxMs: 0,
  lightFrames: 0,
  shownNow: "full",
};

const HEAVY_SEEN_RANK: Record<HeavySeen, number> = { none: 0, never: 1, yes: 2 };

/** Two reports as one: `b` is the later. Sums, the longer gap, the most the
 *  heavy picture was seen, and what was shown last. */
export function addTileFrameCounts(a: TileFrameCounts, b: TileFrameCounts): TileFrameCounts {
  return {
    heavy: HEAVY_SEEN_RANK[b.heavy] >= HEAVY_SEEN_RANK[a.heavy] ? b.heavy : a.heavy,
    heavyFrames: a.heavyFrames + b.heavyFrames,
    heavyGapMaxMs: Math.max(a.heavyGapMaxMs, b.heavyGapMaxMs),
    lightFrames: a.lightFrames + b.lightFrames,
    shownNow: b.shownNow,
  };
}

/**
 * Counts the frames one tile observes on its two elements. No part of the
 * rule: nothing here decides what is shown. Timestamps are the caller's, one
 * clock, milliseconds.
 */
export class TileFrameTally {
  private hasHeavy = false;
  /** Whether the heavy picture now in place has shown a frame. */
  private seen = false;
  private lastHeavy: number | null = null;
  private heavyFrames = 0;
  private heavyGapMax = 0;
  private lightFrames = 0;

  /** The tile has a heavy picture, or has none (the light one alone). A new
   *  heavy picture (`replaced`) has shown no frame yet. */
  setHeavy(present: boolean, replaced = false): void {
    if (!present || replaced || !this.hasHeavy) {
      this.lastHeavy = null;
      this.seen = false;
    }
    this.hasHeavy = present;
  }

  /** The heavy element showed a frame at `t`. */
  heavyFrame(t: number): void {
    this.hasHeavy = true;
    this.seen = true;
    if (this.lastHeavy !== null) this.heavyGapMax = Math.max(this.heavyGapMax, t - this.lastHeavy);
    this.lastHeavy = t;
    this.heavyFrames += 1;
  }

  /** The light element showed a frame. */
  lightFrame(): void {
    this.lightFrames += 1;
  }

  /** Frames could not be observed for a while (a hidden page): the time
   *  since the last heavy frame is not a gap. */
  unobserved(): void {
    this.lastHeavy = null;
  }

  /** Everything observed since the last drain, as of `now`, and start a new
   *  interval. */
  drain(now: number, shownNow: LayerShown): TileFrameCounts {
    const open = this.lastHeavy === null ? 0 : Math.max(0, now - this.lastHeavy);
    const out: TileFrameCounts = {
      heavy: !this.hasHeavy ? "none" : this.seen ? "yes" : "never",
      heavyFrames: this.heavyFrames,
      heavyGapMaxMs: Math.max(this.heavyGapMax, open),
      lightFrames: this.lightFrames,
      shownNow,
    };
    this.heavyFrames = 0;
    this.heavyGapMax = 0;
    this.lightFrames = 0;
    return out;
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
  private readonly framesByPeer = new Map<string, TileFrameCounts>();
  private readonly shownNowByPeer = new Map<string, LayerShown>();

  /** A tile of `peerId` reports what it counted since its last report, and,
   *  when it has them, the frames it observed in that time. */
  add(peerId: string, counters: LayerDisplayCounters, frames?: TileFrameCounts): void {
    if (frames !== undefined) {
      this.shownNowByPeer.set(peerId, frames.shownNow);
      const before = this.framesByPeer.get(peerId);
      this.framesByPeer.set(
        peerId,
        before === undefined ? frames : addTileFrameCounts(before, frames),
      );
    }
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

  /** The frames the tiles of each peer observed since the last call, and
   *  start a new interval. A peer no tile reported frames of is absent. */
  takeFrames(): ReadonlyMap<string, TileFrameCounts> {
    const out = new Map(this.framesByPeer);
    this.framesByPeer.clear();
    return out;
  }

  /**
   * What the tile of `peerId` that reported last said it showed; `undefined`
   * when none ever reported frames. Kept across {@link takeFrames}: it is
   * read between the media counters' intervals (`av-align.ts`). It is only
   * as fresh as the tile's last report, and says nothing of whether a tile
   * is still mounted.
   */
  shownNowOf(peerId: string): LayerShown | undefined {
    return this.shownNowByPeer.get(peerId);
  }

  /** Every peer a tile ever reported on, counted from the start. */
  totals(): ReadonlyMap<string, LayerDisplayCounters> {
    return new Map(this.totalByPeer);
  }

  /** Forget everything: a new call starts. */
  reset(): void {
    this.totalByPeer.clear();
    this.intervalByPeer.clear();
    this.framesByPeer.clear();
    this.shownNowByPeer.clear();
  }
}

/** The ledger of the call this page is in: the tiles write to it, the group
 *  call's media counters read it. */
export const shownLayerLedger = new LayerDisplayLedger();
