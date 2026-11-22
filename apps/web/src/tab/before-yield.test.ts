// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** docs/ux/one-tab.md section 4 — a takeover saves the open composer's draft,
 *  then the synced state that holds it, within a bound, before the reload. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createDraftBroadcaster } from "../chat/draft-broadcaster";
import { foldThenPublish } from "../devices/sync/draft-write";
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

/**
 * The draft against a network that is slow or refuses: the sync context's
 * `saveDraft` order (fold, then publish) under the real broadcaster. Before
 * the fix the fold waited for the publish, so a takeover inside the debounce
 * wrote a synced state without the draft (live on test.lakko.me 2026-11-21).
 */
describe("the draft reaches disk whatever the publish does", () => {
  /** A synced state, its on-disk row, and a `saveDraft` over them. */
  function device(publish: (text: string) => Promise<void>) {
    let held: string | null = null; // the fold
    const disk: Array<string | null> = []; // each synced-state write
    const published: string[] = [];
    const saveDraft = (text: string): Promise<void> =>
      foldThenPublish(
        () => {
          held = text;
        },
        async () => {
          await publish(text);
          published.push(text);
        },
      );
    return {
      saveDraft,
      persistNow: async (): Promise<void> => {
        disk.push(held);
      },
      held: () => held,
      disk,
      published,
    };
  }

  it("a publish that never answers: the run still writes the draft, inside the bound", async () => {
    const d = device(() => new Promise<void>(() => {}));
    const t = heldTimers();
    const broadcaster = createDraftBroadcaster({
      send: d.saveDraft,
      debounceMs: 500,
      setTimer: t.setTimer,
      clearTimer: t.clearTimer,
    });
    registerBeforeYield(PHASE_DRAFTS, () => broadcaster.flush());
    registerBeforeYield(PHASE_SYNCED_STATE, d.persistNow);

    broadcaster.update("takeover draft 2056");
    // The deadline timer never fires: the run must finish on its own.
    expect(await runBeforeYield(1000, () => undefined)).toBe(true);
    expect(d.disk).toEqual(["takeover draft 2056"]);
    expect(d.published).toEqual([]);
  });

  it("a refused publish: the draft is written and the failure reported", async () => {
    const errors: unknown[] = [];
    const d = device(async () => {
      throw new Error("401");
    });
    const broadcaster = createDraftBroadcaster({
      send: d.saveDraft,
      debounceMs: 500,
      ...heldTimers(),
      onError: (err) => errors.push(err),
    });
    registerBeforeYield(PHASE_DRAFTS, () => broadcaster.flush());
    registerBeforeYield(PHASE_SYNCED_STATE, d.persistNow);

    broadcaster.update("takeover draft 2057");
    expect(await runBeforeYield(1000, () => undefined)).toBe(true);
    expect(d.disk).toEqual(["takeover draft 2057"]);
    await Promise.resolve();
    await Promise.resolve();
    expect(errors).toHaveLength(1);
  });

  it("the debounced path keeps the draft when the publish is refused", async () => {
    const errors: unknown[] = [];
    const timers: Array<() => void> = [];
    const d = device(async () => {
      throw new Error("401");
    });
    const broadcaster = createDraftBroadcaster({
      send: d.saveDraft,
      debounceMs: 500,
      setTimer: (fn) => timers.push(fn),
      clearTimer: () => {},
      onError: (err) => errors.push(err),
    });
    broadcaster.update("typed and paused");
    timers[timers.length - 1]!();
    // Kept on this device at once; the debounced persister writes it.
    expect(d.held()).toBe("typed and paused");
    await d.persistNow();
    expect(d.disk).toEqual(["typed and paused"]);
    await Promise.resolve();
    await Promise.resolve();
    expect(errors).toHaveLength(1);
  });

  it("foldThenPublish folds before the publish starts and returns the publish", async () => {
    const order: string[] = [];
    let release: () => void = () => {};
    const done = foldThenPublish(
      () => order.push("fold"),
      () => {
        order.push("publish");
        return new Promise<void>((r) => {
          release = r;
        });
      },
    );
    expect(order).toEqual(["fold", "publish"]);
    let settled = false;
    void done.then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);
    release();
    await done;
    expect(settled).toBe(true);
  });
});
