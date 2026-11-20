// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one rule for a moderator list the device seals to or proves against
 * (`acceptModeratorList`): it verifies under the pinned root key, and its
 * version is not below the device's floor (a newer one raises it). The
 * suspended screen's appeal goes through it, so an appeal is never sealed to
 * a list older than one this device has seen.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { sendAppeal } from "../auth/suspension";
import { acceptModeratorList, wasmListVersion } from "./report-api";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ROOT = new Uint8Array(32).fill(8);

/** A list whose first byte is its version; `0xff` does not verify. */
function listOf(version: number): Uint8Array {
  return new Uint8Array([version]);
}
const version = (list: Uint8Array): bigint => {
  if (list[0] === 0xff) throw new Error("bad-signature: no");
  return BigInt(list[0] ?? 0);
};

function floorStore(initial: bigint | null): { value: bigint | null; read: () => bigint | null; write: (v: bigint) => void } {
  const store = {
    value: initial,
    read: () => store.value,
    write: (v: bigint) => {
      store.value = v;
    },
  };
  return store;
}

describe("acceptModeratorList", () => {
  it("accepts a first list and records its version as the floor", () => {
    const floor = floorStore(null);
    expect(acceptModeratorList(listOf(3), ROOT, version, floor.read, floor.write)).toEqual(listOf(3));
    expect(floor.value).toBe(3n);
  });

  it("raises the floor on a newer list and keeps it on the same one", () => {
    const floor = floorStore(3n);
    expect(acceptModeratorList(listOf(5), ROOT, version, floor.read, floor.write)).toEqual(listOf(5));
    expect(floor.value).toBe(5n);
    expect(acceptModeratorList(listOf(5), ROOT, version, floor.read, floor.write)).toEqual(listOf(5));
    expect(floor.value).toBe(5n);
  });

  it("refuses an older list and leaves the floor", () => {
    const floor = floorStore(5n);
    expect(acceptModeratorList(listOf(4), ROOT, version, floor.read, floor.write)).toBeNull();
    expect(floor.value).toBe(5n);
  });

  it("refuses a list that does not verify, leaving the floor", () => {
    const floor = floorStore(2n);
    expect(acceptModeratorList(listOf(0xff), ROOT, version, floor.read, floor.write)).toBeNull();
    expect(floor.value).toBe(2n);
  });

  it("refuses through the REAL core a list that is not signed under the root", () => {
    const floor = floorStore(null);
    expect(acceptModeratorList(new Uint8Array([1, 2, 3]), ROOT, wasmListVersion(wasm), floor.read, floor.write)).toBeNull();
    expect(floor.value).toBeNull();
  });
});

describe("the appeal sheet's list", () => {
  function ports(list: Uint8Array, floor: ReturnType<typeof floorStore>) {
    const sealedTo: Uint8Array[] = [];
    return {
      sealedTo,
      ports: {
        rootKey: () => ROOT,
        fetchList: async () => acceptModeratorList(list, ROOT, version, floor.read, floor.write),
        seal: (_text: string, l: Uint8Array) => {
          sealedTo.push(l);
          return new Uint8Array([1]);
        },
        post: async () => ({ status: 204, errorCode: null }),
      },
    };
  }

  it("is never sealed to a list older than the device's floor", async () => {
    const floor = floorStore(7n);
    const p = ports(listOf(6), floor);
    expect(await sendAppeal("I did not break the rules.", p.ports)).toBe("unavailable");
    expect(p.sealedTo).toEqual([]);
    expect(floor.value).toBe(7n);
  });

  it("is sealed to a current list, and a newer one raises the floor", async () => {
    const floor = floorStore(7n);
    const p = ports(listOf(8), floor);
    expect(await sendAppeal("I did not break the rules.", p.ports)).toBe("sent");
    expect(p.sealedTo).toEqual([listOf(8)]);
    expect(floor.value).toBe(8n);
  });

  it("is not sealed to a list that does not verify", async () => {
    const floor = floorStore(null);
    const p = ports(listOf(0xff), floor);
    expect(await sendAppeal("I did not break the rules.", p.ports)).toBe("unavailable");
    expect(p.sealedTo).toEqual([]);
  });
});
