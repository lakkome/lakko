// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `onGroupMlsIdle` — the "this group's MLS state has gone quiet" signal the call
 * host rekeys a live group call on (G-18 / ADR-0029).
 *
 * The property that matters is WHEN it fires: once per drain of a group's
 * queue, after the last queued region, and never between two queued regions —
 * where the next one may already have eagerly merged a commit it has not
 * published (ADR-0040), i.e. an epoch that is still in flight.
 */
import { afterEach, describe, expect, it } from "vitest";

import { onGroupMlsIdle, withGroupMlsLock } from "./group-lock";

/** The queue is module-level, so each test uses its own group ids. */
let seq = 0;
function groupId(): string {
  seq += 1;
  return `idle-test-group-${seq}`;
}

/** Let every pending microtask (and the lock's `.catch().finally()` chain)
 *  settle. */
async function flush(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

function deferred(): { promise: Promise<void>; resolve: () => void; reject: (e: unknown) => void } {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const unsubscribers: (() => void)[] = [];
afterEach(() => {
  for (const u of unsubscribers.splice(0)) u();
});

function listen(): string[] {
  const seen: string[] = [];
  unsubscribers.push(onGroupMlsIdle((g) => seen.push(g)));
  return seen;
}

describe("onGroupMlsIdle", () => {
  it("fires once when a single region settles", async () => {
    const seen = listen();
    const g = groupId();
    await withGroupMlsLock(g, async () => {});
    await flush();
    expect(seen).toEqual([g]);
  });

  it("does not fire between two queued regions, only after the last one", async () => {
    const seen = listen();
    const g = groupId();
    const first = deferred();
    const second = deferred();
    const a = withGroupMlsLock(g, () => first.promise);
    const b = withGroupMlsLock(g, () => second.promise);

    first.resolve();
    await a;
    await flush();
    // The second region may be mid-way through an eager merge here.
    expect(seen).toEqual([]);

    second.resolve();
    await b;
    await flush();
    expect(seen).toEqual([g]);
  });

  it("fires on a drain that ended in a rejection too", async () => {
    const seen = listen();
    const g = groupId();
    await expect(
      withGroupMlsLock(g, async () => {
        throw new Error("boom");
      }),
    ).rejects.toThrow("boom");
    await flush();
    expect(seen).toEqual([g]);
  });

  it("reports each group separately", async () => {
    const seen = listen();
    const g1 = groupId();
    const g2 = groupId();
    await Promise.all([withGroupMlsLock(g1, async () => {}), withGroupMlsLock(g2, async () => {})]);
    await flush();
    expect([...seen].sort()).toEqual([g1, g2].sort());
  });

  it("a throwing listener affects neither the lock nor the other listeners", async () => {
    unsubscribers.push(
      onGroupMlsIdle(() => {
        throw new Error("listener failure");
      }),
    );
    const seen = listen();
    const g = groupId();
    await withGroupMlsLock(g, async () => {});
    await flush();
    expect(seen).toEqual([g]);
    // The queue was released: a new region runs at once.
    await expect(withGroupMlsLock(g, async () => 42)).resolves.toBe(42);
  });

  it("a listener that queues a new region starts a fresh queue instead of deadlocking", async () => {
    const g = groupId();
    const ran: string[] = [];
    let queued = false;
    unsubscribers.push(
      onGroupMlsIdle((settled) => {
        if (settled !== g || queued) return;
        queued = true;
        void withGroupMlsLock(g, async () => {
          ran.push("from-listener");
        });
      }),
    );
    await withGroupMlsLock(g, async () => {});
    await flush();
    expect(ran).toEqual(["from-listener"]);
  });

  it("stops after unsubscribe", async () => {
    const seen: string[] = [];
    const off = onGroupMlsIdle((g) => seen.push(g));
    off();
    await withGroupMlsLock(groupId(), async () => {});
    await flush();
    expect(seen).toEqual([]);
  });
});
