// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a share goes (`docs/ux/screen-share.md` section 5,
 * `docs/ux/group-call-grid.md` section 6.3): on the stage ahead of the
 * speaker through the shared rule, never over a pin, never this device's own;
 * the layout it switches to and gives back; the zoom; the announcements.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { t } from "../l10n";
import { stagePerson, type LayoutPerson } from "./group-call-layout";
import {
  panBy,
  SCREEN_TILE_ID,
  screenTilePerson,
  SHARE_ZOOM_WHOLE,
  shareAnnouncement,
  shareLayoutStep,
  zoomBy,
  zoomScale,
} from "./screen-share-layout";
import type { ShareOnScreen } from "./screen-share-store";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const anna: ShareOnScreen = { key: "1:aa", peerId: "pa", name: "Anna", isOwn: false, withSound: false };
const mine: ShareOnScreen = { key: "2:bb", peerId: "me", name: "", isOwn: true, withSound: true };

const person = (id: string, extra: Partial<LayoutPerson> = {}): LayoutPerson => ({
  id,
  isLocal: false,
  camOn: true,
  ...extra,
});

describe("the stage with a share (the shared rule)", () => {
  const share = screenTilePerson(anna) as LayoutPerson;
  const people = [person("peer-a"), person("peer-b"), share, person("local", { isLocal: true })];
  const speech = { activeSpeakerId: "peer-b", lastSpokeAtMs: { "peer-b": 5 } };

  it("puts the share ahead of the speaker", () => {
    expect(stagePerson(wasmNs, people, null, speech)?.id).toBe(SCREEN_TILE_ID);
  });

  it("leaves a pin where it is", () => {
    expect(stagePerson(wasmNs, people, "peer-a", speech)?.id).toBe("peer-a");
  });

  it("never stages this device's own share", () => {
    const own = screenTilePerson(mine) as LayoutPerson;
    const withOwn = [person("peer-a"), person("peer-b"), person("local", { isLocal: true }), own];
    expect(stagePerson(wasmNs, withOwn, null, speech)?.id).toBe("peer-b");
  });
});

describe("shareLayoutStep", () => {
  it("switches to Focus on a new share and gives the layout back when it stops", () => {
    const on = shareLayoutStep(false, true, "grid", undefined);
    expect(on).toEqual({ choice: "focus", saved: "grid" });
    // A share replacing another changes nothing.
    expect(shareLayoutStep(true, true, on.choice, on.saved)).toEqual(on);
    expect(shareLayoutStep(true, false, on.choice, on.saved)).toEqual({ choice: "grid", saved: undefined });
  });

  it("gives back no choice when there was none", () => {
    const on = shareLayoutStep(false, true, null, undefined);
    expect(shareLayoutStep(true, false, on.choice, on.saved).choice).toBeNull();
  });
});

describe("screenTilePerson", () => {
  it("is one tile, local for this device's own share", () => {
    expect(screenTilePerson(null)).toBeNull();
    expect(screenTilePerson(anna)).toMatchObject({ id: SCREEN_TILE_ID, isLocal: false, screen: true });
    expect(screenTilePerson(mine)?.isLocal).toBe(true);
  });
});

describe("shareAnnouncement", () => {
  it("says someone else's start and stop once, and nothing of this device's own", () => {
    expect(shareAnnouncement(null, anna)).toBe(t("call.screen_share.started", { name: "Anna" }));
    expect(shareAnnouncement(anna, anna)).toBeNull();
    expect(shareAnnouncement(anna, null)).toBe(t("call.screen_share.stopped", { name: "Anna" }));
    expect(shareAnnouncement(null, mine)).toBeNull();
    expect(shareAnnouncement(mine, null)).toBeNull();
  });
});

describe("zoom and pan", () => {
  it("steps in and out, and back at whole the pan is gone", () => {
    const z1 = zoomBy(SHARE_ZOOM_WHOLE, 1, 400, 300);
    expect(zoomScale(z1)).toBeGreaterThan(1);
    const panned = panBy(z1, 50, 20, 400, 300);
    expect(panned.panX).toBe(50);
    expect(zoomBy(panned, -1, 400, 300)).toEqual(SHARE_ZOOM_WHOLE);
    expect(zoomBy(SHARE_ZOOM_WHOLE, -1)).toEqual(SHARE_ZOOM_WHOLE);
  });

  it("never pans the picture out of its box, and not at all when whole", () => {
    const z1 = zoomBy(SHARE_ZOOM_WHOLE, 1, 400, 300);
    const s = zoomScale(z1);
    const far = panBy(z1, 10_000, -10_000, 400, 300);
    expect(far.panX).toBe((400 * (s - 1)) / 2);
    expect(far.panY).toBe(-(300 * (s - 1)) / 2);
    expect(panBy(SHARE_ZOOM_WHOLE, 30, 30, 400, 300)).toEqual(SHARE_ZOOM_WHOLE);
  });
});
