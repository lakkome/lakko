// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9 — the order an end runs in, and that a crash at any point
 * resumes it from the durable record.
 */
import { describe, expect, it } from "vitest";

import { endKey, MembershipEndStore, type EndNotice, type EndRecord, type MembershipEndBackend, type MembershipEndRow } from "./membership-end";
import { MembershipEndRuntime, type EndPort, type NewEnd } from "./membership-end-runtime";

/** One device's disk: survives a "crash" (a new store and runtime over it). */
function disk(): MembershipEndBackend {
  let row: MembershipEndRow | null = null;
  return {
    load: async () => row,
    save: async (_a, r) => {
      row = r;
    },
  };
}

type Step = "publish" | "stop" | "wipe" | "clear";

function port(log: Step[], failAt: Partial<Record<Step, boolean>> = {}): EndPort {
  const step = (name: Step) => async (): Promise<void> => {
    if (failAt[name] === true) throw new Error(`crash at ${name}`);
    log.push(name);
  };
  return {
    publishEnd: step("publish"),
    stopReceiving: step("stop"),
    wipe: async () => {
      await step("wipe")();
      return "wiped";
    },
    clearSynced: step("clear"),
  };
}

function end(over: Partial<NewEnd> = {}): NewEnd {
  return {
    refKind: 2,
    idHex: "aa",
    segmentsB64: ["qg=="],
    routeIdsB64: ["qg=="],
    title: "Book club",
    left: false,
    origin: "applied",
    endedEpoch: 7n,
    draft: null,
    ...over,
  };
}

const KEY = endKey(2, "aa");

async function storeOn(d: MembershipEndBackend): Promise<MembershipEndStore> {
  const store = new MembershipEndStore(d);
  await store.open("acct");
  return store;
}

describe("the order", () => {
  it("publishes, stops, wipes, then clears the registers, and closes the record", async () => {
    const log: Step[] = [];
    const store = await storeOn(disk());
    await new MembershipEndRuntime(store, port(log)).begin(end());
    expect(log).toEqual(["publish", "stop", "wipe", "clear"]);
    expect(store.records()).toEqual([]);
  });

  it("tells of the end once, in memory, and not for a person who left", async () => {
    const store = await storeOn(disk());
    const told: EndNotice[] = [];
    const runtime = new MembershipEndRuntime(store, port([]), undefined, (n) => told.push(n));
    await runtime.begin(end({ draft: "half a sentence" }));
    expect(told).toMatchObject([{ key: KEY, title: "Book club", draft: "half a sentence" }]);
    await runtime.begin(end({ idHex: "bb", left: true }));
    expect(told).toHaveLength(1);
  });

  it("a device that heard of it publishes nothing and clears nothing", async () => {
    const log: Step[] = [];
    const store = await storeOn(disk());
    const told: EndNotice[] = [];
    await new MembershipEndRuntime(store, port(log), undefined, (n) => told.push(n)).begin(end({ origin: "heard" }));
    expect(log).toEqual(["stop", "wipe"]);
    expect(told).toHaveLength(1);
  });

  it("records the epoch as a decimal, which is what the delete gets back", async () => {
    let seen: EndRecord | null = null;
    const p = port([]);
    const store = await storeOn(disk());
    await new MembershipEndRuntime(store, {
      ...p,
      wipe: async (r) => {
        seen = r;
        return "wiped";
      },
    }).begin(end({ endedEpoch: 18446744073709551615n }));
    expect((seen as EndRecord | null)?.endedEpoch).toBe("18446744073709551615");
  });
});

describe("a crash resumes from the record", () => {
  const steps: readonly Step[] = ["publish", "stop", "wipe", "clear"];

  for (const crashAt of steps) {
    it(`at ${crashAt}: nothing done before it runs twice, and the end finishes`, async () => {
      const d = disk();
      const before: Step[] = [];
      const first = await storeOn(d);
      await new MembershipEndRuntime(first, port(before, { [crashAt]: true })).begin(end());
      // The steps before the crash ran, and the record is still owed.
      expect(before).toEqual(steps.slice(0, steps.indexOf(crashAt)));

      const after: Step[] = [];
      const second = await storeOn(d);
      expect(second.records().map((r) => r.key)).toEqual([KEY]);
      await new MembershipEndRuntime(second, port(after)).resume();
      expect([...before, ...after]).toEqual(steps);
      expect(second.records()).toEqual([]);
    });
  }

  it("the wipe is not undone by a registers step that keeps failing", async () => {
    const log: Step[] = [];
    const store = await storeOn(disk());
    await new MembershipEndRuntime(store, port(log, { clear: true })).begin(end());
    expect(log).toEqual(["publish", "stop", "wipe"]);
    expect(store.record(KEY)?.stage).toBe("wiped");
  });
});

describe("a sign-out", () => {
  it("stops the run: the next step is never taken in another account's stores", async () => {
    const log: Step[] = [];
    const p = port(log);
    const store = await storeOn(disk());
    await new MembershipEndRuntime(store, {
      ...p,
      stopReceiving: async () => {
        log.push("stop");
        store.close();
      },
    }).begin(end());
    expect(log).toEqual(["publish", "stop"]);
  });
});

describe("the end meets a Welcome", () => {
  it("stops when the group is back: this device is in it again", async () => {
    const log: Step[] = [];
    const p = port(log);
    const store = await storeOn(disk());
    await new MembershipEndRuntime(store, { ...p, wipe: async () => "replaced" }).begin(end());
    expect(log).toEqual(["publish", "stop"]);
    expect(store.records()).toEqual([]);
  });
});

describe("running", () => {
  it("shares one run between callers", async () => {
    const log: Step[] = [];
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const p = port(log);
    const store = await storeOn(disk());
    const runtime = new MembershipEndRuntime(store, {
      ...p,
      stopReceiving: async () => {
        log.push("stop");
        await gate;
      },
    });
    const begun = runtime.begin(end());
    await new Promise((r) => setTimeout(r, 0));
    const resumed = runtime.resume();
    release();
    await Promise.all([begun, resumed]);
    expect(log.filter((s) => s === "stop")).toHaveLength(1);
  });

  it("does not record an end twice", async () => {
    const log: Step[] = [];
    const store = await storeOn(disk());
    const runtime = new MembershipEndRuntime(store, port(log, { wipe: true }));
    await runtime.begin(end());
    await runtime.begin(end({ title: "Another name" }));
    expect(store.records()).toHaveLength(1);
    expect(store.record(KEY)?.title).toBe("Book club");
  });
});

describe("each removal tells the person once (UX section 9, 2026-11-06)", () => {
  it("every removal is told, in order; nothing is written for it", async () => {
    const d = disk();
    const store = await storeOn(d);
    const told: EndNotice[] = [];
    const runtime = new MembershipEndRuntime(store, port([]), undefined, (n) => told.push(n));
    await runtime.begin(end({ idHex: "aa", title: "QA Phase A Walk", routeIdsB64: ["qg=="], segmentsB64: ["qg=="] }));
    await runtime.begin(end({ idHex: "bb", title: "QA Base Alone", routeIdsB64: ["uw=="], segmentsB64: ["uw=="] }));
    expect(told.map((n) => n.title)).toEqual(["QA Phase A Walk", "QA Base Alone"]);
    // A store opened again (the next start) holds no notice to show.
    const again = await storeOn(d);
    expect("notices" in again).toBe(false);
  });

  it("a removal heard from the account's other device tells this device too; leaving tells nobody", async () => {
    const store = await storeOn(disk());
    const told: EndNotice[] = [];
    const runtime = new MembershipEndRuntime(store, port([]), undefined, (n) => told.push(n));
    await runtime.begin(end({ idHex: "cc", title: "Heard", origin: "heard" }));
    await runtime.begin(end({ idHex: "dd", title: "Left", left: true }));
    expect(told.map((n) => n.title)).toEqual(["Heard"]);
  });
});
