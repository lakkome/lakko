// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The written order shows at once (`docs/ux/navigation.md` 2.2.1: "The new
 * order applies on this device at once, on drop"), and the register takes
 * over when the write returns.
 */
import { describe, expect, it } from "vitest";

import { effectivePinOrderKeys, writePinOrder } from "./pin-order-state";

const k = (b: number): Uint8Array => new Uint8Array(32).fill(b);

function deferred(): { promise: Promise<void>; resolve: () => void; reject: (e: unknown) => void } {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("writePinOrder", () => {
  it("shows the written order before the publish returns, then the register", async () => {
    const stored = { groupIds: [k(1), k(2)] };
    const d = deferred();
    const written: Uint8Array[][] = [];
    const sync = {
      setOrder: (ids: readonly Uint8Array[]) => {
        written.push([...ids]);
        return d.promise;
      },
    };
    const done = writePinOrder(sync, [k(2), k(1)]);
    expect(effectivePinOrderKeys(stored)).toEqual([k(2), k(1)]);
    expect(written).toEqual([[k(2), k(1)]]);
    d.resolve();
    await done;
    expect(effectivePinOrderKeys(stored)).toEqual([k(1), k(2)]);
  });

  it("an earlier write finishing does not drop a later one's order", async () => {
    const first = deferred();
    const second = deferred();
    const queue = [first, second];
    const sync = { setOrder: () => (queue.shift() as ReturnType<typeof deferred>).promise };
    const a = writePinOrder(sync, [k(1)]);
    const b = writePinOrder(sync, [k(2)]);
    first.resolve();
    await a;
    expect(effectivePinOrderKeys(null)).toEqual([k(2)]);
    second.resolve();
    await b;
    expect(effectivePinOrderKeys(null)).toEqual([]);
  });

  it("a failed write gives the register back", async () => {
    const d = deferred();
    const done = writePinOrder({ setOrder: () => d.promise }, [k(3)]);
    expect(effectivePinOrderKeys({ groupIds: [k(1)] })).toEqual([k(3)]);
    d.reject(new Error("offline"));
    await expect(done).rejects.toThrow("offline");
    expect(effectivePinOrderKeys({ groupIds: [k(1)] })).toEqual([k(1)]);
  });
});
