// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The large tile of the Focus layout over time
 * (`docs/ux/group-call-grid.md` sections 5.2, 6.1 and 6.2): the sound levels
 * go through `SpeakingTracker`, its answer through the real shared rule
 * (`lakko_ui_rules::call_stage` over wasm), exactly as the call screen does.
 *
 * Written against a live five-person call in which one person sat on the
 * stage and stayed there while others spoke.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import { stagePerson, type LayoutPerson } from "./group-call-layout";
import { FLOOR_ONSET_MS, RING_OFF_MS, SPEAKING_LEVEL, SpeakingTracker } from "./speaking";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const LOUD = SPEAKING_LEVEL * 5;

/** A call of this device and four others, the first of them with a camera. */
class Call {
  public people: LayoutPerson[] = [
    { id: "me", isLocal: true, camOn: true },
    { id: "a", isLocal: false, camOn: true },
    { id: "b", isLocal: false, camOn: false },
    { id: "c", isLocal: false, camOn: false },
    { id: "d", isLocal: false, camOn: false },
  ];
  public pinned: string | null = null;
  private readonly tracker = new SpeakingTracker();
  private now = 0;

  /** Let `ms` pass with these people making sound; who is on the stage after. */
  public run(speaking: readonly string[], ms: number): string | null {
    const until = this.now + ms;
    let stage: string | null = null;
    for (; this.now <= until; this.now += 100) {
      for (const p of this.people) {
        this.tracker.sample(p.id, speaking.includes(p.id) ? LOUD : 0, this.now);
      }
      const snap = this.tracker.snapshot(
        this.people.filter((p) => !p.isLocal).map((p) => p.id),
        this.now,
      );
      stage = stagePerson(wasm, this.people, this.pinned, snap)?.id ?? null;
    }
    return stage;
  }

  public leave(id: string): void {
    this.people = this.people.filter((p) => p.id !== id);
    this.tracker.keepOnly(new Set(this.people.map((p) => p.id)));
    // What the call store does (`callActions.participantLeft`).
    if (this.pinned === id) this.pinned = null;
  }
}

describe("the stage with nobody pinned", () => {
  it("starts on the first person with a picture and moves to whoever speaks", () => {
    const call = new Call();
    expect(call.run([], 1_000)).toBe("a");
    // A cough does not move it.
    expect(call.run(["c"], FLOOR_ONSET_MS - 400)).toBe("a");
    expect(call.run([], RING_OFF_MS + 200)).toBe("a");
    expect(call.run(["c"], FLOOR_ONSET_MS + 200)).toBe("c");
  });

  it("stays on the speaker through a pause and after they stop", () => {
    const call = new Call();
    call.run(["c"], FLOOR_ONSET_MS + 500);
    expect(call.run([], 500)).toBe("c");
    expect(call.run(["c"], 500)).toBe("c");
    // Long silence: nobody holds the floor, the most recent speaker stays.
    expect(call.run([], 20_000)).toBe("c");
  });

  it("moves on to the next speaker, and back", () => {
    const call = new Call();
    call.run(["c"], FLOOR_ONSET_MS + 500);
    call.run([], RING_OFF_MS + 200);
    expect(call.run(["d"], FLOOR_ONSET_MS + 200)).toBe("d");
    call.run([], RING_OFF_MS + 200);
    expect(call.run(["c"], FLOOR_ONSET_MS + 200)).toBe("c");
  });

  it("follows the speaker while this device's own microphone hears sound the whole time", () => {
    const call = new Call();
    expect(call.run(["me"], 5_000)).toBe("a");
    expect(call.run(["me", "d"], FLOOR_ONSET_MS + 200)).toBe("d");
    expect(call.run(["me"], 5_000)).toBe("d");
    expect(call.run(["me", "b"], FLOOR_ONSET_MS + 200)).toBe("b");
  });

  it("goes back to the earlier speaker when the one on the stage leaves", () => {
    const call = new Call();
    call.run(["b"], FLOOR_ONSET_MS + 500);
    call.run([], RING_OFF_MS + 200);
    call.run(["d"], FLOOR_ONSET_MS + 500);
    call.leave("d");
    expect(call.run([], 500)).toBe("b");
  });
});

describe("the stage with a pin", () => {
  it("keeps the pinned person whoever speaks, and returns to the speaker on unpin", () => {
    const call = new Call();
    call.pinned = "b";
    expect(call.run([], 500)).toBe("b");
    expect(call.run(["c"], FLOOR_ONSET_MS + 2_000)).toBe("b");
    call.pinned = null;
    expect(call.run(["c"], 200)).toBe("c");
  });

  it("returns to the speaker when the pinned person leaves", () => {
    const call = new Call();
    call.run(["c"], FLOOR_ONSET_MS + 500);
    call.pinned = "b";
    expect(call.run(["c"], 500)).toBe("b");
    call.leave("b");
    expect(call.pinned).toBeNull();
    expect(call.run(["c"], 200)).toBe("c");
  });

  it("never puts this device's own tile on the stage, pinned or speaking", () => {
    const call = new Call();
    call.pinned = "me";
    expect(call.run(["me"], 5_000)).toBe("a");
  });
});
