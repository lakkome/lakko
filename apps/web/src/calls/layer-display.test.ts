// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The rule is the shared one in `crates/lakko-ui-rules` (`layer_display`),
 * reached through the real wasm: these are the cases its Rust tests carry,
 * asked across the boundary this client uses.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  FIRST_FRAME_GRACE_MS,
  FULL_STALL_MS,
  FULL_STEADY_MS,
  LIGHT_FRESH_MS,
  LayerDisplay,
  LayerDisplayLedger,
  NO_LAYER_DISPLAY_COUNTERS,
  addLayerDisplayCounters,
} from "./layer-display";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

/** Frames of one track every `step` ms over `[from, to]`. */
function play(
  d: LayerDisplay,
  which: "full" | "light" | "both",
  from: number,
  to: number,
  step = 33,
): void {
  for (let t = from; t <= to; t += step) {
    if (which !== "light") d.fullFrame(t);
    if (which !== "full") d.lightFrame(t);
  }
}

/** A tile whose two tracks have both played up to `t = 1000`. */
function playing(): LayerDisplay {
  const d = new LayerDisplay(0, true);
  play(d, "both", 0, 1000, 20);
  return d;
}

describe("the constants", () => {
  it("are the chosen values", () => {
    expect(FULL_STALL_MS).toBe(150);
    expect(LIGHT_FRESH_MS).toBe(250);
    expect(FULL_STEADY_MS).toBe(1000);
    expect(FIRST_FRAME_GRACE_MS).toBe(1500);
  });

  it("are the shared rule's values", () => {
    expect(wasm.layerFullStallMs()).toBe(FULL_STALL_MS);
    expect(wasm.layerLightFreshMs()).toBe(LIGHT_FRESH_MS);
    expect(wasm.layerFullSteadyMs()).toBe(FULL_STEADY_MS);
    expect(wasm.layerFirstFrameGraceMs()).toBe(FIRST_FRAME_GRACE_MS);
    expect(wasm.layerTickMs()).toBe(50);
  });
});

describe("LayerDisplay: what is shown", () => {
  it("shows full by default", () => {
    const d = new LayerDisplay(0, true);
    expect(d.shown()).toBe("full");
    expect(d.tick(50)).toBe("full");
  });

  it("stays on full while both tracks play", () => {
    const d = playing();
    expect(d.shown()).toBe("full");
    expect(d.totals()).toEqual(NO_LAYER_DISPLAY_COUNTERS);
  });

  it("switches to light when full stalls while light is fresh", () => {
    const d = playing();
    // Full stops at 1000; light keeps playing.
    play(d, "light", 1020, 1140, 20);
    expect(d.tick(1149)).toBe("full");
    expect(d.tick(1150)).toBe("light");
    expect(d.totals().lightSwitches).toBe(1);
  });

  it("switches on a light frame as well as on a tick", () => {
    const d = playing();
    play(d, "light", 1020, 1140, 20);
    expect(d.lightFrame(1160)).toBe("light");
  });

  it("does not switch while the full track's gap is under the threshold", () => {
    const d = playing();
    play(d, "light", 1020, 1100, 20);
    expect(d.tick(1100)).toBe("full");
    // The full track resumes inside the threshold: nothing happened.
    expect(d.fullFrame(1140)).toBe("full");
    expect(d.tick(1200)).toBe("full");
    expect(d.totals().lightSwitches).toBe(0);
  });

  it("does not switch to a light track that is not fresh", () => {
    const d = playing();
    // Light's last frame was at 1000 as well; at 1251 it is 251 ms old.
    expect(d.tick(1250)).toBe("light");
    const e = playing();
    expect(e.tick(1251)).toBe("full");
    expect(e.tick(2000)).toBe("full");
    expect(e.totals().lightSwitches).toBe(0);
  });

  it("stays on what is shown when both stall, and counts a displayed stall", () => {
    const d = playing();
    // Both stop at 1000; first look is past the light track's freshness.
    for (let t = 1300; t <= 2000; t += 50) expect(d.tick(t)).toBe("full");
    expect(d.totals().lightSwitches).toBe(0);
    expect(d.totals().stallMaxMs).toBe(1000);
    expect(d.totals().stallTotalMs).toBe(1000);

    // The same while light is the shown one.
    const e = playing();
    play(e, "light", 1020, 1200, 20);
    expect(e.shown()).toBe("light");
    for (let t = 1500; t <= 2200; t += 50) expect(e.tick(t)).toBe("light");
    expect(e.totals().lightSwitches).toBe(1);
    // Light's last frame was at 1200.
    expect(e.totals().stallMaxMs).toBe(1000);
  });

  it("returns to full only after the steady window", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    expect(d.shown()).toBe("light");
    // Full comes back at 2000 and plays without a gap; light keeps playing.
    for (let t = 2000; t < 3000; t += 20) {
      d.lightFrame(t);
      expect(d.fullFrame(t)).toBe("light");
    }
    expect(d.tick(2999)).toBe("light");
    expect(d.fullFrame(3000)).toBe("full");
    expect(d.totals().lightSwitches).toBe(1);
  });

  it("returns on a tick too, once the window has passed", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    play(d, "both", 2000, 2980, 20);
    expect(d.shown()).toBe("light");
    expect(d.tick(3000)).toBe("full");
  });

  it("starts the steady window again after a gap inside it", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    // Full plays 2000..2500, then a gap of 200 ms, then plays on from 2700.
    for (let t = 2000; t <= 4000; t += 20) {
      d.lightFrame(t);
      if (t <= 2500 || t >= 2700) d.fullFrame(t);
      // 1000 ms after the first return (3000) is not enough: the window
      // restarted at 2700 and ends at 3700.
      expect(d.shown()).toBe(t < 3700 ? "light" : "full");
    }
    expect(d.totals().lightSwitches).toBe(1);
  });

  it("starts the steady window again when a tick sees the gap", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    play(d, "full", 2000, 2500, 20);
    expect(d.tick(2700)).toBe("light");
    play(d, "full", 2700, 3680, 20);
    expect(d.shown()).toBe("light");
    expect(d.fullFrame(3700)).toBe("full");
  });

  it("a gap of exactly the threshold does not restart the window", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    d.fullFrame(2000);
    d.fullFrame(2150);
    play(d, "full", 2200, 2980, 20);
    expect(d.fullFrame(3000)).toBe("full");
  });

  it("always shows full without a light track", () => {
    const d = new LayerDisplay(0);
    play(d, "full", 0, 1000, 20);
    for (let t = 1050; t <= 5000; t += 50) expect(d.tick(t)).toBe("full");
    expect(d.totals().lightSwitches).toBe(0);
    expect(d.totals().lightMs).toBe(0);
    // Light frames reported for a track that is not there change nothing.
    expect(d.lightFrame(5010)).toBe("full");
  });

  it("goes back to full at once when the light track goes away", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    expect(d.shown()).toBe("light");
    expect(d.setLightAvailable(false, 1210)).toBe("full");
    expect(d.totals().lightSwitches).toBe(1);
    // A light track that comes back is not fresh until it presents a frame.
    expect(d.setLightAvailable(true, 1220)).toBe("full");
    expect(d.lightFrame(1230)).toBe("light");
    expect(d.totals().lightSwitches).toBe(2);
  });
});

describe("LayerDisplay: before the full track's first frame", () => {
  it("does not show light during the grace", () => {
    const d = new LayerDisplay(0, true);
    for (let t = 0; t < FIRST_FRAME_GRACE_MS; t += 20) expect(d.lightFrame(t)).toBe("full");
    expect(d.tick(FIRST_FRAME_GRACE_MS - 1)).toBe("full");
  });

  it("shows a playing light track once the grace has passed", () => {
    const d = new LayerDisplay(0, true);
    play(d, "light", 0, 1480, 20);
    expect(d.shown()).toBe("full");
    expect(d.lightFrame(1500)).toBe("light");
    expect(d.totals().lightSwitches).toBe(1);
    // The full track's first frame starts the steady window.
    play(d, "both", 2000, 2980, 20);
    expect(d.shown()).toBe("light");
    expect(d.fullFrame(3000)).toBe("full");
  });

  it("does not show a light track that is not playing", () => {
    const d = new LayerDisplay(0, true);
    d.lightFrame(100);
    expect(d.tick(1500)).toBe("full");
    expect(d.tick(3000)).toBe("full");
  });

  it("counts no stall while nothing has been presented", () => {
    const d = new LayerDisplay(0, true);
    for (let t = 50; t <= 3000; t += 50) d.tick(t);
    expect(d.totals()).toEqual(NO_LAYER_DISPLAY_COUNTERS);
  });

  it("the full track's first frame inside the grace keeps full", () => {
    const d = new LayerDisplay(0, true);
    play(d, "light", 0, 1000, 20);
    play(d, "both", 1020, 3000, 20);
    expect(d.shown()).toBe("full");
    expect(d.totals().lightSwitches).toBe(0);
  });
});

describe("LayerDisplay: counters", () => {
  it("counts the time light was shown", () => {
    const d = playing();
    play(d, "light", 1020, 1140, 20);
    expect(d.tick(1150)).toBe("light");
    play(d, "light", 1160, 1980, 20);
    // Full back from 2000, shown again at 3000.
    play(d, "both", 2000, 3000, 20);
    expect(d.shown()).toBe("full");
    d.tick(3500);
    expect(d.totals().lightMs).toBe(3000 - 1150);
    expect(d.totals().lightSwitches).toBe(1);
  });

  it("counts a stall the switch cut short at the length it had", () => {
    // First look after the full track stopped is at a gap of 180 ms.
    const d = playing();
    play(d, "light", 1020, 1140, 20);
    expect(d.tick(1180)).toBe("light");
    expect(d.totals().stallMaxMs).toBe(180);
    expect(d.totals().stallTotalMs).toBe(180);
    // A look at exactly the threshold switches before any stall is counted.
    const e = playing();
    play(e, "light", 1020, 1140, 20);
    expect(e.tick(1150)).toBe("light");
    expect(e.totals().stallTotalMs).toBe(0);
  });

  it("counts a displayed stall of the full track once, at its whole length", () => {
    const d = new LayerDisplay(0);
    play(d, "full", 0, 1000, 20);
    for (let t = 1050; t <= 1950; t += 50) d.tick(t);
    d.fullFrame(1987);
    play(d, "full", 2000, 3000, 20);
    expect(d.totals().stallMaxMs).toBe(987);
    expect(d.totals().stallTotalMs).toBe(987);
  });

  it("adds up several stalls and keeps the longest", () => {
    const d = new LayerDisplay(0);
    play(d, "full", 0, 1000, 20);
    d.fullFrame(1400);
    play(d, "full", 1420, 2000, 20);
    d.fullFrame(2900);
    d.fullFrame(3000);
    expect(d.totals().stallMaxMs).toBe(900);
    expect(d.totals().stallTotalMs).toBe(400 + 900);
  });

  it("does not count a gap of exactly the threshold", () => {
    const d = new LayerDisplay(0);
    d.fullFrame(0);
    d.fullFrame(150);
    expect(d.totals().stallTotalMs).toBe(0);
    d.fullFrame(301);
    expect(d.totals().stallTotalMs).toBe(151);
  });

  it("measures a stall of the light track from when light became the shown one", () => {
    const d = playing();
    // Light's last frame is at 1100; the switch happens at 1300 (gap 200,
    // still fresh), and light then stands still as well.
    play(d, "light", 1020, 1100, 20);
    expect(d.tick(1300)).toBe("light");
    const afterSwitch = d.totals().stallTotalMs;
    expect(afterSwitch).toBe(300); // the full track's stall, 1000..1300
    d.tick(1800);
    // Light stood on screen from 1300, not from its last frame at 1100.
    expect(d.totals().stallTotalMs).toBe(afterSwitch + 500);
    expect(d.totals().stallMaxMs).toBe(500);
  });

  it("drains an interval and keeps the totals", () => {
    const d = new LayerDisplay(0);
    play(d, "full", 0, 1000, 20);
    d.tick(1500);
    expect(d.drain()).toEqual({ lightSwitches: 0, lightMs: 0, stallMaxMs: 500, stallTotalMs: 500 });
    // The stall runs on into the next interval: its time is split, and the
    // interval's longest stall is the length it has reached.
    d.tick(1800);
    expect(d.drain()).toEqual({ lightSwitches: 0, lightMs: 0, stallMaxMs: 800, stallTotalMs: 300 });
    expect(d.drain()).toEqual(NO_LAYER_DISPLAY_COUNTERS);
    expect(d.totals()).toEqual({ lightSwitches: 0, lightMs: 0, stallMaxMs: 800, stallTotalMs: 800 });
  });

  it("never lets time run backwards", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    expect(d.shown()).toBe("light");
    const before = d.totals();
    d.tick(900);
    d.fullFrame(800);
    expect(d.totals().lightMs).toBe(before.lightMs);
    expect(d.totals().stallTotalMs).toBe(before.stallTotalMs);
  });
});

describe("LayerDisplay: a page that was hidden", () => {
  it("counts nothing for the hidden time and starts again on full", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    expect(d.shown()).toBe("light");
    const before = d.totals();
    expect(d.resync(60_000)).toBe("full");
    expect(d.totals()).toEqual(before);
    // A new grace runs from the return.
    play(d, "light", 60_000, 61_480, 20);
    expect(d.shown()).toBe("full");
    expect(d.lightFrame(61_500)).toBe("light");
  });
});

describe("LayerDisplay: the shared rule's clock", () => {
  it("reads a fractional timestamp as the millisecond it falls in", () => {
    const d = playing();
    play(d, "light", 1020, 1140, 20);
    expect(d.tick(1149.9)).toBe("full");
    expect(d.tick(1180.7)).toBe("light");
    expect(d.totals().stallMaxMs).toBe(180);
  });

  it("shows full and counts nothing once disposed", () => {
    const d = playing();
    play(d, "light", 1020, 1200, 20);
    expect(d.shown()).toBe("light");
    d.dispose();
    expect(d.shown()).toBe("full");
    expect(d.tick(5000)).toBe("full");
    expect(d.drain()).toEqual(NO_LAYER_DISPLAY_COUNTERS);
    d.dispose();
  });
});

describe("LayerDisplayLedger", () => {
  const c = (lightSwitches: number, lightMs: number, stallMaxMs: number, stallTotalMs: number) => ({
    lightSwitches,
    lightMs,
    stallMaxMs,
    stallTotalMs,
  });

  it("adds counters: sums, and the longer of the longest stalls", () => {
    expect(addLayerDisplayCounters(c(1, 100, 300, 400), c(2, 50, 200, 250))).toEqual(
      c(3, 150, 300, 650),
    );
  });

  it("adds up what several tiles of one peer report, by interval and in total", () => {
    const ledger = new LayerDisplayLedger();
    ledger.add("peerX", c(1, 100, 200, 200));
    ledger.add("peerX", c(0, 50, 0, 0));
    ledger.add("peerY", c(0, 0, 900, 900));
    expect([...ledger.takeInterval()]).toEqual([
      ["peerX", c(1, 150, 200, 200)],
      ["peerY", c(0, 0, 900, 900)],
    ]);
    // A new interval: a peer nobody reported on is absent.
    ledger.add("peerX", c(1, 10, 400, 400));
    expect([...ledger.takeInterval()]).toEqual([["peerX", c(1, 10, 400, 400)]]);
    expect([...ledger.takeInterval()]).toEqual([]);
    expect([...ledger.totals()]).toEqual([
      ["peerX", c(2, 160, 400, 600)],
      ["peerY", c(0, 0, 900, 900)],
    ]);
    ledger.reset();
    expect(ledger.totals().size).toBe(0);
  });
});
