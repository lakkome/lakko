// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How a group call's tiles are laid out (`docs/ux/group-call-grid.md`
 * sections 3 to 6).
 *
 * Two layouts. **Grid**: everyone the same size, filling the space between
 * the status line and the control bar, with pages instead of a scroll.
 * **Focus**: one large tile (the stage) and everyone else in a strip above
 * it.
 *
 * The geometry and the choice of who is on the stage are the shared rules of
 * `crates/lakko-ui-rules` (`call_group`), reached over wasm; iOS and Android
 * reach the same ones over UniFFI, so the three clients draw the same grid on
 * the same width. What is here is this client's side of it: the order of the
 * tiles, which page a tile is on, and which layout is showing.
 *
 * Pure: no Solid, no DOM. The call screen measures its area and renders the
 * answer.
 */

import type { CallBarPhase, CallGrid } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";

/** The shared rules this module asks. Structural, so a test can hand in the
 *  real wasm namespace or a spy alike. */
export interface GroupCallRules {
  readonly callGrid: (count: number, width: number, height: number) => CallGrid;
  readonly callStage: (
    flags: Uint8Array,
    lastSpokeAtMs: Float64Array,
    pinned?: number | null,
  ) => number | undefined;
  readonly ringbackPlays: (groupCall: boolean, phase: CallBarPhase) => boolean;
}

const _wasmSatisfiesGroupCallRules: (
  w: Pick<Wasm, "callGrid" | "callStage" | "ringbackPlays">,
) => GroupCallRules = (w) => w;
void _wasmSatisfiesGroupCallRules;

/** The grid, as plain numbers. */
export interface GridLayout {
  readonly cols: number;
  readonly rows: number;
  readonly tileWidth: number;
  readonly tileHeight: number;
  readonly gap: number;
  readonly perPage: number;
  readonly pages: number;
}

/**
 * Lay `count` equal tiles into an area: the shared rule's answer, copied out
 * of the wasm object and the object released, so nothing here holds wasm
 * memory across renders.
 */
export function gridLayout(
  rules: Pick<GroupCallRules, "callGrid">,
  count: number,
  width: number,
  height: number,
): GridLayout {
  const grid = rules.callGrid(
    Math.max(0, Math.trunc(count)),
    Math.max(0, Math.floor(width)),
    Math.max(0, Math.floor(height)),
  );
  const out: GridLayout = {
    cols: grid.cols,
    rows: grid.rows,
    tileWidth: grid.tileWidth,
    tileHeight: grid.tileHeight,
    gap: grid.gap,
    perPage: grid.perPage,
    pages: grid.pages,
  };
  (grid as { free?: () => void }).free?.();
  return out;
}

/** One person, as the layout reads them. The store's `Participant` satisfies
 *  it structurally. */
export interface LayoutPerson {
  readonly id: string;
  readonly isLocal: boolean;
  readonly camOn: boolean;
  /** A shared screen's tile (ADR-0083), not a person's. The shared rule puts
   *  a share ahead of the speaker on the stage, never this device's own. */
  readonly screen?: boolean;
}

/** Which of the two layouts is showing. */
export type GroupLayoutKind = "grid" | "focus";

/**
 * The layout on screen.
 *
 * A pin always means Focus: pinning someone is asking to see them large.
 * Otherwise the person's own choice for this call, and Grid until they make
 * one.
 */
export function groupLayoutKind(
  choice: GroupLayoutKind | null,
  pinnedId: string | null,
  people: readonly LayoutPerson[],
): GroupLayoutKind {
  // Alone, there is nobody to put in a strip.
  if (people.filter((p) => !p.isLocal).length === 0) return "grid";
  if (pinnedId !== null && people.some((p) => p.id === pinnedId && !p.isLocal)) return "focus";
  return choice ?? "grid";
}

/**
 * The order tiles are drawn in: everyone else in the order they joined, then
 * this device's own tile last (section 5.3: a fixed place is how a person
 * finds themself without looking).
 */
export function tileOrder<T extends LayoutPerson>(people: readonly T[]): T[] {
  return [...people.filter((p) => !p.isLocal), ...people.filter((p) => p.isLocal)];
}

/**
 * The tiles of one page of the grid.
 *
 * Page 0 always ends with this device's own tile, so it holds one person
 * fewer than `perPage` when the call spills over; the later pages hold
 * everyone else in order. `page` is clamped, so a page that emptied because
 * someone left shows the last page that still has people.
 */
export function gridPage<T extends LayoutPerson>(
  people: readonly T[],
  perPage: number,
  page: number,
): { readonly tiles: T[]; readonly page: number; readonly pages: number } {
  const ordered = tileOrder(people);
  if (perPage <= 0 || ordered.length === 0) return { tiles: [], page: 0, pages: 0 };
  if (ordered.length <= perPage) return { tiles: ordered, page: 0, pages: 1 };
  const own = ordered.filter((p) => p.isLocal);
  const others = ordered.filter((p) => !p.isLocal);
  const firstCount = Math.max(0, perPage - own.length);
  const rest = others.slice(firstCount);
  const pages = 1 + Math.ceil(rest.length / perPage);
  const at = Math.min(Math.max(0, Math.trunc(page)), pages - 1);
  if (at === 0) return { tiles: [...others.slice(0, firstCount), ...own], page: 0, pages };
  const from = (at - 1) * perPage;
  return { tiles: rest.slice(from, from + perPage), page: at, pages };
}

/** Bits of the `flags` byte `callStage` reads; they mirror
 *  `STAGE_FLAG_*` in `crates/lakko-ffi-wasm/src/ui_rules.rs`. */
const STAGE_FLAG_LOCAL = 1;
const STAGE_FLAG_CAMERA = 2;
const STAGE_FLAG_SPEAKING = 4;
const STAGE_FLAG_SCREEN = 8;

/** What the stage rule needs beyond the roster. */
export interface StageSpeech {
  /** The one person holding the floor now, or `null`. */
  readonly activeSpeakerId: string | null;
  /** When each person last held the floor, by id. */
  readonly lastSpokeAtMs: Readonly<Record<string, number>>;
}

/**
 * Who is on the stage in Focus, or `null` for an empty call.
 *
 * The shared rule decides: pinned, else a shared screen (never this
 * device's own), else speaking now, else the most recent speaker, else the
 * first person with a picture, else the first other person; this device's
 * own tile only when alone.
 */
export function stagePerson<T extends LayoutPerson>(
  rules: Pick<GroupCallRules, "callStage">,
  people: readonly T[],
  pinnedId: string | null,
  speech: StageSpeech,
): T | null {
  if (people.length === 0) return null;
  const flags = new Uint8Array(people.length);
  const spoke = new Float64Array(people.length);
  people.forEach((p, i) => {
    flags[i] =
      (p.isLocal ? STAGE_FLAG_LOCAL : 0) |
      (p.camOn ? STAGE_FLAG_CAMERA : 0) |
      (speech.activeSpeakerId === p.id ? STAGE_FLAG_SPEAKING : 0) |
      (p.screen === true ? STAGE_FLAG_SCREEN : 0);
    spoke[i] = speech.lastSpokeAtMs[p.id] ?? 0;
  });
  const pinnedIndex = pinnedId === null ? -1 : people.findIndex((p) => p.id === pinnedId);
  const index = rules.callStage(flags, spoke, pinnedIndex < 0 ? undefined : pinnedIndex);
  return index === undefined ? null : (people[index] ?? null);
}

/** The strip in Focus: everyone but the stage, in tile order. */
export function stripPeople<T extends LayoutPerson>(people: readonly T[], stage: T | null): T[] {
  return tileOrder(people).filter((p) => stage === null || p.id !== stage.id);
}

/** The strip's tile size and the space it takes, by the width of the call
 *  area. Chosen, not measured. */
export function stripMetrics(areaWidth: number): {
  readonly tileWidth: number;
  readonly tileHeight: number;
  readonly gap: number;
} {
  if (areaWidth < 600) return { tileWidth: 104, tileHeight: 78, gap: 8 };
  return { tileWidth: 160, tileHeight: 90, gap: 10 };
}

/** The stage tile's size: the largest tile of an allowed shape that fits
 *  under the strip. Asked of the same shared rule as the grid, for one tile. */
export function stageSize(
  rules: Pick<GroupCallRules, "callGrid">,
  areaWidth: number,
  areaHeight: number,
): { readonly width: number; readonly height: number } {
  const strip = stripMetrics(areaWidth);
  const height = Math.max(0, Math.floor(areaHeight) - strip.tileHeight - strip.gap);
  const grid = gridLayout(rules, 1, areaWidth, height);
  return { width: grid.tileWidth, height: grid.tileHeight };
}
