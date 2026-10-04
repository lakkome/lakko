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

import { LayerDisplay as WasmLayerDisplay, ShownLayer } from "../../wasm/lakko_ffi_wasm";

/** Which track a tile shows. */
export type LayerShown = "full" | "light";

/**
 * The full track is standing still once it has presented no frame for this
 * long, milliseconds. Chosen, not measured: about four frame intervals at
 * 30 frames a second and two at 15, so ordinary jitter does not trip it.
 *
 * This and the three constants below restate the shared rule's values
 * (`layerFullStallMs()` and its neighbours over wasm) so they can be read
 * before the wasm module is loaded; `layer-display.test.ts` pins that they
 * are equal. The rule itself never reads these copies.
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
