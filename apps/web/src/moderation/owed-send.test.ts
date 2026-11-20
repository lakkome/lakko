// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `reporting-and-moderation.md` 1.4 - the block's send to the account's
 * other devices runs in the background, bounded, and is tried again when it
 * fails; it never holds the caller.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { blockPerson, unblockPerson } from "./block-actions";
import { bounded, sendOwedInBackground } from "./owed-send";

const KEY_B64 = btoa(String.fromCharCode(...new Uint8Array(32).fill(7)));

afterEach(() => {
  vi.useRealTimers();
});

describe("a bounded attempt", () => {
  it("is the attempt's own answer when it settles in time", async () => {
    await expect(bounded(() => Promise.resolve(true), 1_000)).resolves.toBe(true);
    await expect(bounded(() => Promise.resolve(false), 1_000)).resolves.toBe(false);
  });

  it("counts a throw as a failure", async () => {
    await expect(bounded(() => Promise.reject(new Error("offline")), 1_000)).resolves.toBe(false);
  });

  it("counts a send that never settles as a failure once its time is up", async () => {
    vi.useFakeTimers();
    const result = bounded(() => new Promise<boolean>(() => {}), 20_000);
    await vi.advanceTimersByTimeAsync(20_000);
    await expect(result).resolves.toBe(false);
  });
});

describe("the background send", () => {
  const noSleep = (): Promise<void> => Promise.resolve();

  it("stops at the first attempt that goes", async () => {
    const seen: number[] = [];
    const ok = await sendOwedInBackground(
      (i) => {
        seen.push(i);
        return Promise.resolve(i === 1);
      },
      { sleep: noSleep, retryDelaysMs: [1, 2, 3] },
    );
    expect(ok).toBe(true);
    expect(seen).toEqual([0, 1]);
  });

  it("tries again after each delay and stays owed when every attempt fails", async () => {
    const slept: number[] = [];
    const seen: number[] = [];
    const ok = await sendOwedInBackground(
      (i) => {
        seen.push(i);
        return Promise.reject(new Error("offline"));
      },
      {
        sleep: (ms) => {
          slept.push(ms);
          return Promise.resolve();
        },
        retryDelaysMs: [30, 120],
      },
    );
    expect(ok).toBe(false);
    expect(seen).toEqual([0, 1, 2]);
    expect(slept).toEqual([30, 120]);
  });

  it("stops once the work no longer belongs to the signed-in account", async () => {
    let current = true;
    const seen: number[] = [];
    const ok = await sendOwedInBackground(
      (i) => {
        seen.push(i);
        current = false;
        return Promise.resolve(false);
      },
      { sleep: noSleep, retryDelaysMs: [1, 2], isCurrent: () => current },
    );
    expect(ok).toBe(false);
    expect(seen).toEqual([0]);
  });
});

describe("blocking does not wait for the other devices", () => {
  it("returns as soon as the register is written, while the send still hangs", async () => {
    // The writer as the provider now runs it: fold, then a send nobody awaits.
    let sendStarted = false;
    const hangingSend = new Promise<boolean>(() => {});
    const write = (): Promise<boolean> => {
      sendStarted = true;
      void sendOwedInBackground(() => hangingSend, { sleep: () => new Promise(() => {}) });
      return Promise.resolve(true);
    };
    const removed: string[] = [];
    const done = await Promise.race([
      blockPerson(KEY_B64, { write, removeContact: (k) => removed.push(k) }).then(() => "done"),
      new Promise((resolve) => setTimeout(() => resolve("held"), 200)),
    ]);
    expect(done).toBe("done");
    expect(sendStarted).toBe(true);
    // UX 1.2: they are taken off the contact list.
    expect(removed).toEqual([KEY_B64]);
  });

  it("unblocking returns at once too and keeps them off the contact list", async () => {
    const removed: string[] = [];
    const calls: boolean[] = [];
    await unblockPerson(KEY_B64, {
      write: (_k, blocked) => {
        calls.push(blocked);
        return Promise.resolve(true);
      },
      removeContact: (k) => removed.push(k),
    });
    expect(calls).toEqual([false]);
    expect(removed).toEqual([]);
  });
});
