// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { AccountSlot } from "./account-slot";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

describe("AccountSlot (the own username certificate)", () => {
  it("fetches once per account and keeps the value", async () => {
    const published: Array<string | null> = [];
    const slot = new AccountSlot<string>((v) => published.push(v));
    let fetches = 0;
    const fetch = async (): Promise<string> => {
      fetches += 1;
      return "cert-A";
    };
    await slot.ensure("A", fetch);
    await slot.ensure("A", fetch);
    expect(fetches).toBe(1);
    expect(slot.get()).toBe("cert-A");
    expect(published).toEqual(["cert-A"]);
  });

  it("does not hand account A's certificate to account B", async () => {
    const published: Array<string | null> = [];
    const slot = new AccountSlot<string>((v) => published.push(v));
    await slot.ensure("A", async () => "cert-A");
    // Sign out, sign in as B. B must fetch its own; A's must be gone at once.
    slot.rebind("B");
    expect(slot.get()).toBeNull();
    await slot.ensure("B", async () => "cert-B");
    expect(slot.get()).toBe("cert-B");
    expect(published).toEqual(["cert-A", null, "cert-B"]);
  });

  it("drops A's fetch when it returns after B signed in", async () => {
    const slot = new AccountSlot<string>(() => undefined);
    const forA = deferred<string>();
    const forB = deferred<string>();
    const pendingA = slot.ensure("A", () => forA.promise);
    slot.rebind(null);
    const pendingB = slot.ensure("B", () => forB.promise);
    // A's answer arrives while B's is still out.
    forA.resolve("cert-A");
    await pendingA;
    expect(slot.get()).toBeNull();
    forB.resolve("cert-B");
    await pendingB;
    expect(slot.get()).toBe("cert-B");
  });

  it("does not let A's settled fetch clear B's in-flight marker", async () => {
    const slot = new AccountSlot<string>(() => undefined);
    const forA = deferred<string>();
    const forB = deferred<string>();
    let fetchesB = 0;
    const fetchB = (): Promise<string> => {
      fetchesB += 1;
      return forB.promise;
    };
    const pendingA = slot.ensure("A", () => forA.promise);
    const firstB = slot.ensure("B", fetchB);
    forA.resolve("cert-A");
    await pendingA;
    // Still one fetch for B: the second call joins the one in flight.
    const secondB = slot.ensure("B", fetchB);
    forB.resolve("cert-B");
    await Promise.all([firstB, secondB]);
    expect(fetchesB).toBe(1);
    expect(slot.get()).toBe("cert-B");
  });

  it("retries after a failed or empty fetch", async () => {
    const slot = new AccountSlot<string>(() => undefined);
    await expect(slot.ensure("A", async () => Promise.reject(new Error("down")))).rejects.toThrow("down");
    await slot.ensure("A", async () => null);
    expect(slot.get()).toBeNull();
    await slot.ensure("A", async () => "cert-A");
    expect(slot.get()).toBe("cert-A");
  });

  it("holds nothing while signed out", async () => {
    const slot = new AccountSlot<string>(() => undefined);
    let fetches = 0;
    await slot.ensure(null, async () => {
      fetches += 1;
      return "x";
    });
    expect(fetches).toBe(0);
    expect(slot.get()).toBeNull();
  });
});
