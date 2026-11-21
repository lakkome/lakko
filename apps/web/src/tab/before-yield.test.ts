// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** docs/ux/one-tab.md section 4 — a takeover saves the open composer's draft,
 *  then the synced state that holds it, within a bound, before the reload. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createDraftBroadcaster } from "../chat/draft-broadcaster";
import {
  PHASE_DRAFTS,
  PHASE_SYNCED_STATE,
  registerBeforeYield,
  resetBeforeYieldForTests,
  runBeforeYield,
} from "./before-yield";

beforeEach(() => {
  resetBeforeYieldForTests();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  resetBeforeYieldForTests();
  vi.restoreAllMocks();
});

/** Timers that never fire on their own (the debounce stays pending). */
function heldTimers() {
  const timers = new Map<number, () => void>();
  let next = 1;
  return {
    setTimer: (fn: () => void): unknown => {
      const id = next++;
      timers.set(id, fn);
      return id;
    },
    clearTimer: (id: unknown): void => {
      timers.delete(id as number);
    },
    pending: (): number => timers.size,
  };
}

describe("a takeover saves the draft typed within the debounce", () => {
  it("the draft is sent, then the synced state is written, before the run resolves", async () => {
    const order: string[] = [];
    const held: string[] = []; // what the synced state holds
    const disk: string[][] = []; // what reached disk
    const t = heldTimers();
    const broadcaster = createDraftBroadcaster({
      send: async (text) => {
        order.push(`send:${text}`);
        held.push(text);
      },
      debounceMs: 500,
      setTimer: t.setTimer,
      clearTimer: t.clearTimer,
    });
    registerBeforeYield(PHASE_DRAFTS, () => broadcaster.flush());
    registerBeforeYield(PHASE_SYNCED_STATE, async () => {
      order.push("persist");
      disk.push([...held]);
    });

    broadcaster.update("half a sente");
    expect(t.pending()).toBe(1); // still inside the debounce: nothing sent yet

    expect(await runBeforeYield(1000, () => undefined)).toBe(true);
    expect(order).toEqual(["send:half a sente", "persist"]);
    expect(disk).toEqual([["half a sente"]]);
    expect(t.pending()).toBe(0);
  });

  it("nothing pending: no send; a draft already sent is not sent again", async () => {
    const sent: string[] = [];
    const broadcaster = createDraftBroadcaster({
      send: async (text) => {
        sent.push(text);
      },
      debounceMs: 500,
      ...heldTimers(),
    });
    await broadcaster.flush();
    expect(sent).toEqual([]);
    broadcaster.update("hello");
    await broadcaster.flush();
    broadcaster.update("hello");
    await broadcaster.flush();
    expect(sent).toEqual(["hello"]);
  });

  it("a failed draft send does not stop the synced-state write", async () => {
    const order: string[] = [];
    registerBeforeYield(PHASE_DRAFTS, async () => {
      order.push("send");
      throw new Error("offline");
    });
    registerBeforeYield(PHASE_SYNCED_STATE, async () => {
      order.push("persist");
    });
    expect(await runBeforeYield(1000, () => undefined)).toBe(true);
    expect(order).toEqual(["send", "persist"]);
  });

  it("a save that hangs is cut off at the deadline, so the tab still reloads", async () => {
    registerBeforeYield(PHASE_DRAFTS, () => new Promise<void>(() => {}));
    let fire: () => void = () => {};
    const run = runBeforeYield(1000, (fn) => {
      fire = fn;
    });
    fire();
    expect(await run).toBe(false);
  });

  it("an unregistered saver is not run", async () => {
    const ran: string[] = [];
    const off = registerBeforeYield(PHASE_DRAFTS, async () => {
      ran.push("x");
    });
    off();
    await runBeforeYield(1000, () => undefined);
    expect(ran).toEqual([]);
  });
});

describe("the composer debounce (unchanged behaviour)", () => {
  it("sends the last text once typing pauses, and never re-sends the same text", async () => {
    const sent: string[] = [];
    const timers: Array<() => void> = [];
    const broadcaster = createDraftBroadcaster({
      send: async (text) => {
        sent.push(text);
      },
      debounceMs: 500,
      setTimer: (fn) => timers.push(fn),
      clearTimer: () => {},
    });
    broadcaster.update("h");
    broadcaster.update("hi");
    timers[timers.length - 1]!();
    await Promise.resolve();
    expect(sent).toEqual(["hi"]);
    broadcaster.update("hi");
    expect(timers).toHaveLength(2);
  });
});
