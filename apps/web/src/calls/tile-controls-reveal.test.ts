// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { TILE_CONTROLS_REVEAL_MS, TileControlsReveal, type RevealTimers } from "./tile-controls-reveal";

/** A clock a test moves by hand. */
function clock(): RevealTimers & { advance(ms: number): void; pending(): number } {
  let now = 0;
  let next = 1;
  const timers = new Map<number, { at: number; run: () => void }>();
  return {
    setTimeout: (run, ms) => {
      const id = next;
      next += 1;
      timers.set(id, { at: now + ms, run });
      return id;
    },
    clearTimeout: (handle) => {
      timers.delete(handle as number);
    },
    advance: (ms) => {
      now += ms;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          timer.run();
        }
      }
    },
    pending: () => timers.size,
  };
}

function rig(): { reveal: TileControlsReveal; seen: (string | null)[]; time: ReturnType<typeof clock> } {
  const time = clock();
  const seen: (string | null)[] = [];
  return { reveal: new TileControlsReveal((id) => seen.push(id), time), seen, time };
}

describe("the controls a tap brings up on a call tile", () => {
  it("come up on a tap and go when the time runs out", () => {
    const { reveal, seen, time } = rig();
    reveal.tap("b");
    expect(reveal.tileId).toBe("b");
    time.advance(TILE_CONTROLS_REVEAL_MS - 1);
    expect(reveal.tileId).toBe("b");
    time.advance(1);
    expect(reveal.tileId).toBeNull();
    expect(seen).toEqual(["b", null]);
    expect(time.pending()).toBe(0);
  });

  it("start the time again on another tap on the same tile, reporting nothing new", () => {
    const { reveal, seen, time } = rig();
    reveal.tap("b");
    time.advance(2_000);
    reveal.tap("b");
    time.advance(2_000);
    expect(reveal.tileId).toBe("b");
    time.advance(1_000);
    expect(reveal.tileId).toBeNull();
    expect(seen).toEqual(["b", null]);
  });

  it("go when a press begins anywhere else, and stay for a press on the same tile", () => {
    const { reveal, seen, time } = rig();
    reveal.tap("b");
    reveal.pressBegan("b");
    expect(reveal.tileId).toBe("b");
    reveal.pressBegan("c");
    expect(reveal.tileId).toBeNull();
    reveal.tap("c");
    reveal.pressBegan(null);
    expect(reveal.tileId).toBeNull();
    expect(seen).toEqual(["b", null, "c", null]);
    expect(time.pending()).toBe(0);
  });

  it("are on one tile at a time", () => {
    const { reveal, seen } = rig();
    reveal.tap("b");
    reveal.tap("c");
    expect(reveal.tileId).toBe("c");
    expect(seen).toEqual(["b", "c"]);
  });

  it("report nothing after dispose and leave no timer", () => {
    const { reveal, seen, time } = rig();
    reveal.tap("b");
    reveal.dispose();
    time.advance(TILE_CONTROLS_REVEAL_MS);
    expect(seen).toEqual(["b"]);
    expect(time.pending()).toBe(0);
  });
});
