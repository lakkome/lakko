// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which picture of a person a tile shows, and what this device asks the SFU
 * for (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5).
 *
 * A person whose camera is on sends up to three copies of it: the main one,
 * a middle one (half the size) and the light one (a quarter). The light one
 * always arrives. Of the two heavy ones the SFU forwards the one this device
 * says it shows, per person, in a `video-wants` frame; a person this device
 * shows no heavy picture of costs it the light stream alone.
 *
 * Which one a tile shows is the shared rule
 * (`lakko_ui_rules::call_group::tile_video_quality`, over wasm here, over
 * UniFFI on iOS and Android): the people in the call, the tile's width in
 * physical pixels, and whether it is the pinned person or the stage. This
 * module is the web side of it:
 *
 *   * {@link tileQualityOf} asks the rule;
 *   * {@link askedQuality} and {@link heavyStreamFor} turn its answer into
 *     what is asked of the SFU and which stream the tile's heavy element
 *     plays;
 *   * {@link VideoWantsBook} keeps, per person, the highest quality any
 *     mounted tile of them wants;
 *   * {@link VideoWantsSender} sends the whole set when it changes, at most
 *     once per {@link VIDEO_WANTS_MIN_INTERVAL_MS};
 *   * {@link SettledWidth} lets a tile's width settle before it is asked
 *     about again, so dragging a window edge does not ask the sender for a
 *     key frame on every step.
 *
 * Pure: no DOM, no Solid; timers and the clock are handed in.
 */

import { tileVideoQuality, tileVideoQualityName } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";

/** A picture of a person, by the name it has in the `video-wants` frame. */
export type VideoQuality = "main" | "middle" | "light";

const RANK: Record<VideoQuality, number> = { light: 0, middle: 1, main: 2 };

/** Whether `name` is one of the three names. */
export function isVideoQuality(name: unknown): name is VideoQuality {
  return name === "main" || name === "middle" || name === "light";
}

/** The higher of two qualities: main over middle over light. */
export function higherQuality(a: VideoQuality, b: VideoQuality): VideoQuality {
  return RANK[a] >= RANK[b] ? a : b;
}

/** The lower of two qualities: light under middle under main. */
export function lowerQuality(a: VideoQuality, b: VideoQuality): VideoQuality {
  return RANK[a] <= RANK[b] ? a : b;
}

/** The part of the shared rules a tile asks. Structural, so a test can hand
 *  in the real wasm namespace. */
export type TileQualityRules = Pick<Wasm, "tileVideoQuality" | "tileVideoQualityName">;

/** The shared rules themselves. Asked before the wasm module is ready they
 *  throw, which {@link tileQualityOf} reads as `main`. */
export const sharedTileQualityRules: TileQualityRules = { tileVideoQuality, tileVideoQualityName };

/**
 * A width in CSS pixels as physical pixels: what a picture is actually
 * stretched over. A ratio the browser does not give (or gives as nonsense)
 * is read as 1.
 */
export function physicalTileWidth(cssWidth: number, devicePixelRatio: number): number {
  const ratio = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
  if (!Number.isFinite(cssWidth) || cssWidth <= 0) return 0;
  return Math.round(cssWidth * ratio);
}

/**
 * Which picture a tile shows: the shared rule's answer. `people` counts
 * everyone in the call, this device's own person included; `tileWidthPx` is
 * in physical pixels. While the rule cannot be asked (the wasm module is not
 * ready) the answer is `main`, which is what the SFU sends a device that
 * never asked.
 */
export function tileQualityOf(
  rules: TileQualityRules,
  people: number,
  tileWidthPx: number,
  pinnedOrStage: boolean,
): VideoQuality {
  try {
    const name = rules.tileVideoQualityName(
      rules.tileVideoQuality(Math.max(0, Math.floor(people)), Math.max(0, Math.round(tileWidthPx)), pinnedOrStage),
    );
    return isVideoQuality(name) ? name : "main";
  } catch {
    return "main";
  }
}

/**
 * What a tile asks the SFU for, given the rule's answer. The rule's `light`
 * means the light stream alone, which is nothing at all for a person who
 * sends no light copy (a client from before ADR-0070): such a tile asks for
 * `middle`, which the SFU serves as the main track when there is no middle
 * one either.
 */
export function askedQuality(quality: VideoQuality, hasLight: boolean): VideoQuality {
  return quality === "light" && !hasLight ? "middle" : quality;
}

/**
 * The stream a tile's heavy `<video>` plays for the quality it asked for:
 * the main stream for `main`; the middle stream for `middle`, or the main
 * one while the person has no middle track (the SFU forwards the main track
 * then); none for `light`, where the light stream alone is shown.
 */
export function heavyStreamFor<S>(
  quality: VideoQuality,
  main: S | undefined,
  middle: S | undefined,
): S | undefined {
  if (quality === "light") return undefined;
  if (quality === "middle") return middle ?? main;
  return main;
}

/**
 * After the heavy picture of a tile was replaced by another (a different
 * quality, or the person's middle track arriving), the element that plays it
 * is blank until the new stream's first frame. `true` while the light copy
 * should cover that: the heavy source changed, no frame of the new one has
 * been presented yet, and there is a light copy to show.
 */
export function coversHeavySwap(awaitingFirstHeavyFrame: boolean, hasLight: boolean): boolean {
  return awaitingFirstHeavyFrame && hasLight;
}

// ---- The set this device asks for -----------------------------------------

/** The most entries a `video-wants` frame carries
 *  (`lakko_sfu_proto::MAX_VIDEO_WANTS`). */
export const VIDEO_WANTS_MAX = 64;

/** The fewest milliseconds between two `video-wants` frames. Chosen. */
export const VIDEO_WANTS_MIN_INTERVAL_MS = 200;

/**
 * What every mounted tile wants, by person.
 *
 * A person can be drawn by several tiles at once (the stage and the strip
 * while the layout changes), so each tile reports under its own key and the
 * person's want is the highest of them. A person of the call no tile reports
 * for (another page of the grid, a camera that is off, a collapsed call)
 * wants `light`: the SFU then forwards neither heavy track of theirs.
 */
export class VideoWantsBook {
  private readonly peers = new Set<string>();
  private readonly tiles = new Map<object, { readonly peerId: string; readonly quality: VideoQuality }>();

  /** The people of the call, other than this device: replaces the set. */
  setPeers(peerIds: Iterable<string>): void {
    this.peers.clear();
    for (const id of peerIds) this.peers.add(id);
  }

  /** A person joined. */
  addPeer(peerId: string): void {
    this.peers.add(peerId);
  }

  /** A person left: they are in no later set. */
  removePeer(peerId: string): void {
    this.peers.delete(peerId);
  }

  /** How many people are in the call, this device's own person included. */
  people(): number {
    return this.peers.size + 1;
  }

  /**
   * The tile `tile` (any object that is the same for the tile's lifetime)
   * shows `peerId` at `quality`; `null` when it stops showing a picture of
   * them. Returns whether anything changed.
   */
  setTile(tile: object, peerId: string, quality: VideoQuality | null): boolean {
    const before = this.tiles.get(tile);
    if (quality === null) return this.tiles.delete(tile);
    if (before !== undefined && before.peerId === peerId && before.quality === quality) return false;
    this.tiles.set(tile, { peerId, quality });
    return true;
  }

  /** What this device shows of `peerId`: the highest of their mounted tiles,
   *  `light` with none. */
  qualityOf(peerId: string): VideoQuality {
    let best: VideoQuality = "light";
    for (const t of this.tiles.values()) {
      if (t.peerId === peerId) best = higherQuality(best, t.quality);
    }
    return best;
  }

  /** Whether any mounted tile draws a picture of `peerId` now. */
  hasTile(peerId: string): boolean {
    for (const t of this.tiles.values()) {
      if (t.peerId === peerId) return true;
    }
    return false;
  }

  /** The whole set: every person of the call, in the order they joined. */
  wants(): Map<string, VideoQuality> {
    const out = new Map<string, VideoQuality>();
    for (const peerId of this.peers) out.set(peerId, this.qualityOf(peerId));
    return out;
  }

  /** Forget everything: the call is over. */
  clear(): void {
    this.peers.clear();
    this.tiles.clear();
  }
}

/** A set of wants as the two parallel lists `composeSfuVideoWants` takes. */
export interface VideoWantsFrame {
  readonly peers: string[];
  readonly qualities: VideoQuality[];
}

/**
 * `wants` as a frame within the protocol's bound. A frame replaces the whole
 * set and a person it leaves out is at `main`, so past {@link VIDEO_WANTS_MAX}
 * people the `main` entries are the ones left out; if the rest still does not
 * fit, the last to join are left out (and are sent in full, as before step 5).
 */
export function videoWantsFrame(wants: ReadonlyMap<string, VideoQuality>, max = VIDEO_WANTS_MAX): VideoWantsFrame {
  let entries = [...wants];
  if (entries.length > max) entries = entries.filter(([, q]) => q !== "main");
  if (entries.length > max) entries = entries.slice(0, max);
  return { peers: entries.map(([p]) => p), qualities: entries.map(([, q]) => q) };
}

/** The timer and the clock {@link VideoWantsSender} and {@link SettledWidth}
 *  use; replaced in tests. */
export interface TimerDeps {
  now(): number;
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export const browserTimerDeps: TimerDeps = {
  now: () => Date.now(),
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/**
 * Sends the book's whole set when it changed, coalesced.
 *
 * Every send goes through a timer, so the reports of one turn (every tile of
 * a grid measuring itself in the same frame) become one frame, and two frames
 * are never closer than `minIntervalMs`. {@link changed} sends only a set that
 * differs from the last one sent; {@link resend} sends the set whatever it is
 * (after the connection is up, and after a person joins). `send` returns
 * whether the frame went out; one that did not (no connection yet) is tried
 * again at the next call. An empty set is never sent: with nobody else in the
 * call there is nothing to ask for.
 */
export class VideoWantsSender {
  private lastKey: string | null = null;
  private lastSentAt: number | null = null;
  private timer: unknown = null;
  private force = false;

  constructor(
    private readonly book: VideoWantsBook,
    private readonly send: (frame: VideoWantsFrame) => boolean,
    private readonly deps: TimerDeps = browserTimerDeps,
    private readonly minIntervalMs = VIDEO_WANTS_MIN_INTERVAL_MS,
  ) {}

  /** The set may have changed. */
  changed(): void {
    this.schedule();
  }

  /** Send the set at the next opportunity even if it is the one last sent. */
  resend(): void {
    this.force = true;
    this.schedule();
  }

  /** Stop: nothing is sent after this, and a later use starts afresh. */
  reset(): void {
    if (this.timer !== null) this.deps.clearTimeout(this.timer);
    this.timer = null;
    this.lastKey = null;
    this.lastSentAt = null;
    this.force = false;
  }

  private schedule(): void {
    if (this.timer !== null) return;
    const wait =
      this.lastSentAt === null ? 0 : Math.max(0, this.lastSentAt + this.minIntervalMs - this.deps.now());
    this.timer = this.deps.setTimeout(() => {
      this.timer = null;
      this.flush();
    }, wait);
  }

  private flush(): void {
    const frame = videoWantsFrame(this.book.wants());
    const key = frame.peers.map((p, i) => `${p}=${frame.qualities[i] ?? ""}`).join(",");
    if (frame.peers.length === 0) {
      this.lastKey = key;
      this.force = false;
      return;
    }
    if (!this.force && key === this.lastKey) return;
    if (!this.send(frame)) return;
    this.lastKey = key;
    this.force = false;
    this.lastSentAt = this.deps.now();
  }
}

// ---- A tile's width --------------------------------------------------------

/** How long a tile's width must stand still before it is used, milliseconds.
 *  Chosen. */
export const TILE_WIDTH_SETTLE_MS = 300;

/**
 * A tile's width, settled. The first width a tile measures is used at once (a
 * new tile has nothing to keep showing); every later one only after it has
 * stood for {@link TILE_WIDTH_SETTLE_MS}.
 */
export class SettledWidth {
  private measured = false;
  private applied = 0;
  private timer: unknown = null;

  constructor(
    private readonly apply: (width: number) => void,
    private readonly deps: Pick<TimerDeps, "setTimeout" | "clearTimeout"> = browserTimerDeps,
    private readonly settleMs = TILE_WIDTH_SETTLE_MS,
  ) {}

  /** The tile was measured at `width`. */
  observe(width: number): void {
    if (this.timer !== null) {
      this.deps.clearTimeout(this.timer);
      this.timer = null;
    }
    if (!this.measured) {
      this.measured = true;
      this.applied = width;
      this.apply(width);
      return;
    }
    if (width === this.applied) return;
    this.timer = this.deps.setTimeout(() => {
      this.timer = null;
      this.applied = width;
      this.apply(width);
    }, this.settleMs);
  }

  /** The tile is gone: nothing is applied after this. */
  dispose(): void {
    if (this.timer !== null) this.deps.clearTimeout(this.timer);
    this.timer = null;
  }
}
