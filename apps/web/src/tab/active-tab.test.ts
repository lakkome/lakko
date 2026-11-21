// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** docs/ux/one-tab.md — one Lakko tab per browser, through the claim logic
 *  with a fake Web Lock manager and a fake channel bus. */
import { describe, expect, it } from "vitest";

import {
  ACTIVE_TAB_LOCK,
  claimActiveTab,
  PROBE_WAIT_MS,
  TAKEOVER_STEAL_AFTER_MS,
  type ActiveTabDeps,
  type ChannelLike,
  type LockManagerLike,
} from "./active-tab";

/** A lock manager shared by several "pages": FIFO queue, ifAvailable, steal,
 *  signal, and release of everything a page held (its unload). */
class FakeLocks {
  private holder: { page: number; reject: (e: unknown) => void } | null = null;
  private queue: Array<{ page: number; grant: () => void }> = [];

  forPage(page: number): LockManagerLike {
    return {
      request: (name, options, callback) =>
        new Promise((resolve, reject) => {
          expect(name).toBe(ACTIVE_TAB_LOCK);
          const grant = (): void => {
            this.holder = { page, reject };
            const held = callback({ name });
            void Promise.resolve(held).then(resolve, reject);
          };
          if (options.steal === true) {
            const previous = this.holder;
            this.holder = null;
            previous?.reject(Object.assign(new Error("stolen"), { name: "AbortError" }));
            grant();
            return;
          }
          if (this.holder === null) {
            grant();
            return;
          }
          if (options.ifAvailable === true) {
            void Promise.resolve(callback(null)).then(resolve, reject);
            return;
          }
          const entry = { page, grant };
          this.queue.push(entry);
          options.signal?.addEventListener("abort", () => {
            this.queue = this.queue.filter((e) => e !== entry);
            reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
          });
        }),
    };
  }

  /** The page unloads: its lock goes to the next in the queue. */
  unload(page: number): void {
    if (this.holder?.page === page) {
      this.holder = null;
      const next = this.queue.shift();
      next?.grant();
    }
    this.queue = this.queue.filter((e) => e.page !== page);
  }

  holderPage(): number | null {
    return this.holder?.page ?? null;
  }
}

/** Channels between pages; a page never hears its own message. */
class Bus {
  private readonly members: Array<{ page: number; fns: Array<(ev: { data: unknown }) => void> }> = [];
  open(page: number): ChannelLike {
    const me = { page, fns: [] as Array<(ev: { data: unknown }) => void> };
    this.members.push(me);
    return {
      postMessage: (message) => {
        for (const m of this.members) {
          if (m === me) continue;
          for (const fn of m.fns) queueMicrotask(() => fn({ data: message }));
        }
      },
      addEventListener: (_type, fn) => {
        me.fns.push(fn);
      },
    };
  }
}

/** Timers a test advances by hand. */
class Clock {
  now = 0;
  private timers: Array<{ at: number; fn: () => void; id: number }> = [];
  private next = 1;
  set = (fn: () => void, ms: number): unknown => {
    const id = this.next++;
    this.timers.push({ at: this.now + ms, fn, id });
    return id;
  };
  clear = (id: unknown): void => {
    this.timers = this.timers.filter((t) => t.id !== id);
  };
  async advance(ms: number): Promise<void> {
    this.now += ms;
    const due = this.timers.filter((t) => t.at <= this.now);
    this.timers = this.timers.filter((t) => t.at > this.now);
    for (const t of due) t.fn();
    await flush();
  }
}

async function flush(): Promise<void> {
  for (let i = 0; i < 20; i++) await Promise.resolve();
}

function page(
  id: number,
  env: { locks: FakeLocks | null; bus: Bus | null; clock: Clock },
  onLost: (reason: "takeover" | "stolen") => void = () => {},
): ActiveTabDeps {
  return {
    locks: env.locks?.forPage(id) ?? null,
    openChannel: env.bus === null ? null : () => env.bus!.open(id),
    setTimer: env.clock.set,
    clearTimer: env.clock.clear,
    onLost,
  };
}

describe("with Web Locks", () => {
  it("the first tab runs, a second one waits", async () => {
    const env = { locks: new FakeLocks(), bus: new Bus(), clock: new Clock() };
    expect((await claimActiveTab(page(1, env))).kind).toBe("active");
    expect((await claimActiveTab(page(2, env))).kind).toBe("inactive");
  });

  it("Use here: the running tab stops and reloads, then this tab runs", async () => {
    const env = { locks: new FakeLocks(), bus: new Bus(), clock: new Clock() };
    let lost = 0;
    // The running tab's onLost freezes and reloads: the reload is its unload.
    const reasons: string[] = [];
    await claimActiveTab(page(1, env, (reason) => {
      lost += 1;
      reasons.push(reason);
      env.locks.unload(1);
    }));
    const second = await claimActiveTab(page(2, env));
    if (second.kind !== "inactive") throw new Error("expected inactive");
    let ran = false;
    void second.takeOver().then(() => {
      ran = true;
    });
    await flush();
    expect(lost).toBe(1);
    // Asked, not stolen: the page saves the draft before it goes.
    expect(reasons).toEqual(["takeover"]);
    expect(ran).toBe(true);
    expect(env.locks.holderPage()).toBe(2);
    // The reloaded first tab now waits.
    expect((await claimActiveTab(page(3, env))).kind).toBe("inactive");
  });

  it("a running tab that does not answer is taken from after the wait, and learns it", async () => {
    const env = { locks: new FakeLocks(), bus: new Bus(), clock: new Clock() };
    let lost = 0;
    // A hung tab: no channel (it never hears the takeover), but its lock
    // request rejects when stolen.
    const reasons: string[] = [];
    await claimActiveTab({
      ...page(1, { ...env, bus: null }),
      onLost: (reason) => {
        lost += 1;
        reasons.push(reason);
      },
    });
    const second = await claimActiveTab(page(2, env));
    if (second.kind !== "inactive") throw new Error("expected inactive");
    let ran = false;
    void second.takeOver().then(() => {
      ran = true;
    });
    await flush();
    expect(ran).toBe(false);
    await env.clock.advance(TAKEOVER_STEAL_AFTER_MS);
    await flush();
    expect(ran).toBe(true);
    expect(env.locks.holderPage()).toBe(2);
    expect(lost).toBe(1);
    // Already taken: nothing more may be written, no draft save.
    expect(reasons).toEqual(["stolen"]);
  });

  it("a waiting tab does not react to another waiting tab's Use here", async () => {
    const env = { locks: new FakeLocks(), bus: new Bus(), clock: new Clock() };
    let lostWaiting = 0;
    await claimActiveTab(page(1, env, () => env.locks.unload(1)));
    await claimActiveTab(page(2, env, () => (lostWaiting += 1)));
    const third = await claimActiveTab(page(3, env));
    if (third.kind !== "inactive") throw new Error("expected inactive");
    await third.takeOver();
    expect(lostWaiting).toBe(0);
    expect(env.locks.holderPage()).toBe(3);
  });
});

describe("without Web Locks", () => {
  it("a tab that hears no answer runs; the next one is answered and waits", async () => {
    const env = { locks: null, bus: new Bus(), clock: new Clock() };
    const first = claimActiveTab(page(1, env));
    await flush();
    await env.clock.advance(PROBE_WAIT_MS);
    expect((await first).kind).toBe("active");
    const second = claimActiveTab(page(2, env));
    await flush();
    expect((await second).kind).toBe("inactive");
  });

  it("Use here stops the running tab", async () => {
    const env = { locks: null, bus: new Bus(), clock: new Clock() };
    let lost = 0;
    const first = claimActiveTab(page(1, env, () => (lost += 1)));
    await flush();
    await env.clock.advance(PROBE_WAIT_MS);
    await first;
    const second = claimActiveTab(page(2, env));
    await flush();
    const claim = await second;
    if (claim.kind !== "inactive") throw new Error("expected inactive");
    const done = claim.takeOver();
    await flush();
    expect(lost).toBe(1);
    await env.clock.advance(PROBE_WAIT_MS);
    await done;
    // The running tab is now the second: a third is answered by it.
    const third = claimActiveTab(page(3, env));
    await flush();
    expect((await third).kind).toBe("inactive");
  });

  it("with neither API every tab runs", async () => {
    const env = { locks: null, bus: null, clock: new Clock() };
    expect((await claimActiveTab(page(1, env))).kind).toBe("active");
    expect((await claimActiveTab(page(2, env))).kind).toBe("active");
  });
});
