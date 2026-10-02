// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-call-grid.md` sections 3 to 6, asked through the real shared
 * rules over wasm: the grid fits its area without scrolling, the order keeps
 * this device's own tile last, and the stage follows the pin and the speaker.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  gridLayout,
  gridPage,
  groupLayoutKind,
  stagePerson,
  stageSize,
  stripPeople,
  tileOrder,
  type LayoutPerson,
} from "./group-call-layout";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const person = (id: string, over: Partial<LayoutPerson> = {}): LayoutPerson => ({
  id,
  isLocal: false,
  camOn: false,
  ...over,
});
const me = person("me", { isLocal: true });
const NO_SPEECH = { activeSpeakerId: null, lastSpokeAtMs: {} };

describe("the grid", () => {
  it("fits one page into its area on a phone and on a laptop, whatever the count", () => {
    for (const [width, height] of [
      [366, 560],
      [760, 250],
      [1200, 600],
    ] as const) {
      for (let count = 1; count <= 24; count += 1) {
        const g = gridLayout(wasm, count, width, height);
        const shown = Math.min(count, g.perPage);
        expect(g.cols * g.rows).toBeGreaterThanOrEqual(shown);
        expect(g.cols * g.tileWidth + (g.cols - 1) * g.gap).toBeLessThanOrEqual(width);
        expect(g.rows * g.tileHeight + (g.rows - 1) * g.gap).toBeLessThanOrEqual(height);
      }
    }
  });

  it("draws four people two by two on a phone and three in a row on a laptop", () => {
    const phone = gridLayout(wasm, 4, 366, 560);
    expect([phone.cols, phone.rows]).toEqual([2, 2]);
    const laptop = gridLayout(wasm, 3, 1200, 600);
    expect([laptop.cols, laptop.rows]).toEqual([3, 1]);
  });

  it("pages instead of scrolling once a phone holds more than six", () => {
    const g = gridLayout(wasm, 8, 366, 560);
    expect(g.perPage).toBe(6);
    expect(g.pages).toBe(2);
  });
});

describe("the order of the tiles", () => {
  it("keeps everyone else in the order they joined and this device's own tile last", () => {
    const people = [me, person("a"), person("b")];
    expect(tileOrder(people).map((p) => p.id)).toEqual(["a", "b", "me"]);
  });

  it("keeps the own tile on the first page when the call spills over", () => {
    const people = [me, ...["a", "b", "c", "d", "e", "f", "g"].map((id) => person(id))];
    const first = gridPage(people, 6, 0);
    expect(first.pages).toBe(2);
    expect(first.tiles.map((p) => p.id)).toEqual(["a", "b", "c", "d", "e", "me"]);
    expect(gridPage(people, 6, 1).tiles.map((p) => p.id)).toEqual(["f", "g"]);
  });

  it("falls back to the last page that still has people", () => {
    const people = [me, person("a"), person("b")];
    expect(gridPage(people, 6, 3).page).toBe(0);
  });
});

describe("which layout shows", () => {
  const people = [me, person("a"), person("b")];
  it("is the grid until the person chooses otherwise", () => {
    expect(groupLayoutKind(null, null, people)).toBe("grid");
    expect(groupLayoutKind("focus", null, people)).toBe("focus");
  });
  it("is focus while someone is pinned, whatever was chosen", () => {
    expect(groupLayoutKind("grid", "a", people)).toBe("focus");
  });
  it("is the grid when alone, since there is nobody for a strip", () => {
    expect(groupLayoutKind("focus", null, [me])).toBe("grid");
  });
  it("ignores a pin on someone who has left", () => {
    expect(groupLayoutKind(null, "gone", people)).toBe("grid");
  });
});

describe("the stage", () => {
  const people = [me, person("a"), person("b", { camOn: true }), person("c")];
  it("holds the pinned person", () => {
    expect(stagePerson(wasm, people, "c", NO_SPEECH)?.id).toBe("c");
  });
  it("holds the person speaking, and keeps them after they stop", () => {
    expect(
      stagePerson(wasm, people, null, { activeSpeakerId: "a", lastSpokeAtMs: { a: 10 } })?.id,
    ).toBe("a");
    expect(
      stagePerson(wasm, people, null, { activeSpeakerId: null, lastSpokeAtMs: { a: 10, c: 5 } })?.id,
    ).toBe("a");
  });
  it("falls back to the first person with a picture before anyone has spoken", () => {
    expect(stagePerson(wasm, people, null, NO_SPEECH)?.id).toBe("b");
  });
  it("never gives the stage to this device's own voice while others are present", () => {
    expect(
      stagePerson(wasm, people, null, { activeSpeakerId: "me", lastSpokeAtMs: { me: 10 } })?.id,
    ).toBe("b");
  });
  it("puts everyone but the stage in the strip, own tile last", () => {
    const stage = stagePerson(wasm, people, "a", NO_SPEECH);
    expect(stripPeople(people, stage).map((p) => p.id)).toEqual(["b", "c", "me"]);
  });
  it("fits under the strip", () => {
    const size = stageSize(wasm, 366, 560);
    expect(size.width).toBeLessThanOrEqual(366);
    expect(size.height).toBeLessThanOrEqual(560 - 78 - 8);
    expect(size.width).toBeGreaterThan(0);
  });
});

describe("the outgoing tone", () => {
  it("never plays in a group call, and plays in a one-to-one only until it is answered", () => {
    const p = wasm.CallBarPhase;
    for (const phase of [p.Preparing, p.Calling, p.Connecting, p.Connected]) {
      expect(wasm.ringbackPlays(true, phase)).toBe(false);
    }
    expect(wasm.ringbackPlays(false, p.Calling)).toBe(true);
    expect(wasm.ringbackPlays(false, p.Connecting)).toBe(false);
    expect(wasm.ringbackPlays(false, p.Connected)).toBe(false);
  });
});
