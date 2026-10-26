// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a screen share goes on the call screen (`docs/ux/screen-share.md`
 * section 5, `docs/ux/group-call-grid.md` section 6.3), as plain rules.
 *
 * Which tile is on the stage is the shared rule's (`call_stage`, reached
 * through `group-call-layout.ts`): a pin, then a share, then the speaker.
 * What is here is this client's side: the share as one more tile, the layout
 * a new share switches to and the one it gives back, the zoom of the large
 * picture, and the polite announcements.
 *
 * Pure: no Solid, no DOM.
 */

import { t } from "../l10n";
import type { GroupLayoutKind } from "./group-call-layout";
import type { ShareOnScreen } from "./screen-share-store";

/** The id of the share's tile. One share at a time, so one id: a share that
 *  replaces another keeps the tile and its place. */
export const SCREEN_TILE_ID = "screen";

/** The share as a tile of the group layout. */
export interface ScreenTilePerson {
  readonly id: typeof SCREEN_TILE_ID;
  /** This device's own share: a tile with no picture, never on the stage. */
  readonly isLocal: boolean;
  readonly camOn: true;
  readonly screen: true;
  readonly displayName: string;
}

/** The share's tile, or `null` when nothing is shared. */
export function screenTilePerson(share: ShareOnScreen | null): ScreenTilePerson | null {
  if (share === null) return null;
  return {
    id: SCREEN_TILE_ID,
    isLocal: share.isOwn,
    camOn: true,
    screen: true,
    displayName: share.name,
  };
}

/** `Anna's screen`. */
export function screenTileLabel(name: string): string {
  return t("call.screen_share.tile", { name: nameOrUnnamed(name) });
}

/** A sharer's name, or the call screen's unnamed word while it is not known. */
export function nameOrUnnamed(name: string): string {
  return name.length > 0 ? name : t("call.participant.unnamed");
}

/**
 * The layout around a share (section 5.2): a new share from someone else
 * switches to Focus and remembers what the layout was; a share replacing
 * another changes nothing; when the share stops, the layout goes back to
 * what it was before the first share began. A pin is left alone throughout
 * (it already means Focus, and the share is offered by a chip). This
 * device's own share moves nothing.
 *
 * `saved` is `undefined` while no share has moved the layout.
 */
export function shareLayoutStep(
  wasShowing: boolean,
  nowShowing: boolean,
  choice: GroupLayoutKind | null,
  saved: GroupLayoutKind | null | undefined,
): { readonly choice: GroupLayoutKind | null; readonly saved: GroupLayoutKind | null | undefined } {
  if (!wasShowing && nowShowing) return { choice: "focus", saved: saved === undefined ? choice : saved };
  if (wasShowing && !nowShowing) return { choice: saved === undefined ? choice : saved, saved: undefined };
  return { choice, saved };
}

/** Whether `share` is someone else's: the share that moves the layout. */
export function isRemoteShare(share: ShareOnScreen | null): boolean {
  return share !== null && !share.isOwn;
}

/**
 * The polite announcement for a change of the share on screen (section 5.3),
 * or `null`. Someone else's share starting or stopping is said once; this
 * device's own is not (its strip says it).
 */
export function shareAnnouncement(before: ShareOnScreen | null, after: ShareOnScreen | null): string | null {
  if (before?.key === after?.key) return null;
  if (after !== null && !after.isOwn) {
    return t("call.screen_share.started", { name: nameOrUnnamed(after.name) });
  }
  if (before !== null && !before.isOwn) {
    return t("call.screen_share.stopped", { name: nameOrUnnamed(before.name) });
  }
  return null;
}

/** The zoom steps of the large share picture (section 5.3). Chosen, not
 *  measured. */
export const SHARE_ZOOM_STEPS: readonly number[] = [1, 1.5, 2, 3, 4];

/** The large picture's zoom: a step and a pan offset in pixels. */
export interface ShareZoom {
  readonly step: number;
  readonly panX: number;
  readonly panY: number;
}

export const SHARE_ZOOM_WHOLE: ShareZoom = { step: 0, panX: 0, panY: 0 };

/** One step in (`+1`) or out (`-1`). Back at whole, the pan is reset. */
export function zoomBy(zoom: ShareZoom, delta: 1 | -1, width = 0, height = 0): ShareZoom {
  const step = Math.min(SHARE_ZOOM_STEPS.length - 1, Math.max(0, zoom.step + delta));
  if (step === 0) return SHARE_ZOOM_WHOLE;
  return clampPan({ ...zoom, step }, width, height);
}

/** The scale a zoom step draws at. */
export function zoomScale(zoom: ShareZoom): number {
  return SHARE_ZOOM_STEPS[zoom.step] ?? 1;
}

/**
 * Pan by `dx`, `dy` pixels, kept so the picture never leaves its box: at
 * scale `s` a box `width` wide may move by at most `width * (s - 1) / 2`
 * each way. `width` / `height` of 0 (unmeasured) leave the pan at 0.
 */
export function panBy(zoom: ShareZoom, dx: number, dy: number, width = 0, height = 0): ShareZoom {
  if (zoom.step === 0) return SHARE_ZOOM_WHOLE;
  return clampPan({ ...zoom, panX: zoom.panX + dx, panY: zoom.panY + dy }, width, height);
}

function clampPan(zoom: ShareZoom, width: number, height: number): ShareZoom {
  const s = zoomScale(zoom);
  const maxX = Math.max(0, (width * (s - 1)) / 2);
  const maxY = Math.max(0, (height * (s - 1)) / 2);
  return {
    step: zoom.step,
    panX: Math.min(maxX, Math.max(-maxX, zoom.panX)),
    panY: Math.min(maxY, Math.max(-maxY, zoom.panY)),
  };
}
