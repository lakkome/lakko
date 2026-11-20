// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The supporters list (`docs/ux/supporters.md` section 2): verified under
 * the pinned key, highest version kept, fetched at most daily unless About
 * asks, and the row only for a list with names.
 */
import { describe, expect, it, vi } from "vitest";

import {
  keepNewer,
  readHeldSupporters,
  refreshSupporters,
  showsSupportersRow,
  SUPPORTERS_PATH,
  SUPPORTERS_REFRESH_MS,
  SUPPORTERS_STORE_KEY,
  supportersFetchDue,
  type SupportersDeps,
  type SupportersStorage,
} from "./supporters-list";

function memoryStorage(): SupportersStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return { map, getItem: (k) => map.get(k) ?? null, setItem: (k, v) => void map.set(k, v) };
}

const KEY = new Uint8Array(32).fill(7);

function deps(
  body: Uint8Array | null,
  verify: (bytes: Uint8Array) => { version: bigint; names: string[] },
  storage = memoryStorage(),
  now = 1_000_000,
): SupportersDeps & { storage: ReturnType<typeof memoryStorage>; fetchMock: ReturnType<typeof vi.fn> } {
  const fetchMock = vi.fn(async () =>
    body === null ? new Response("nope", { status: 404 }) : new Response(body as Uint8Array<ArrayBuffer>, { status: 200 }),
  );
  return {
    storage,
    fetchMock,
    fetch: fetchMock as unknown as typeof fetch,
    key: KEY,
    nowMs: () => now,
    wasm: async () => ({
      verifySupportersList: (bytes, key) => {
        expect(key).toBe(KEY);
        const v = verify(bytes);
        return { listVersion: v.version, names: v.names, free: () => {} };
      },
    }),
  };
}

describe("supporters list", () => {
  it("no pinned key: dormant, nothing fetched", async () => {
    const d = { ...deps(new Uint8Array([1]), () => ({ version: 1n, names: ["A"] })), key: null };
    expect(await refreshSupporters(d, true)).toEqual({ kind: "dormant" });
    expect(d.fetchMock).not.toHaveBeenCalled();
  });

  it("fetches the same-origin path, verifies and keeps it", async () => {
    const d = deps(new Uint8Array([1, 2]), () => ({ version: 3n, names: ["Ann", "Bo"] }));
    const r = await refreshSupporters(d, true);
    expect(d.fetchMock).toHaveBeenCalledWith(SUPPORTERS_PATH, expect.objectContaining({ method: "GET" }));
    expect(r).toEqual({ kind: "ok", held: { version: "3", names: ["Ann", "Bo"], fetchedAtMs: 1_000_000 } });
    expect(readHeldSupporters(d.storage)?.names).toEqual(["Ann", "Bo"]);
  });

  it("a list that fails its check is ignored", async () => {
    const d = deps(new Uint8Array([1]), () => {
      throw new Error("supporters-list: bad signature");
    });
    expect(await refreshSupporters(d, true)).toEqual({ kind: "failed", held: null });
    expect(d.storage.map.has(SUPPORTERS_STORE_KEY)).toBe(false);
  });

  it("a network failure or non-2xx keeps the held list", async () => {
    const storage = memoryStorage();
    storage.map.set(SUPPORTERS_STORE_KEY, JSON.stringify({ version: "2", names: ["A"], fetchedAtMs: 0 }));
    const d = deps(null, () => ({ version: 9n, names: [] }), storage);
    expect(await refreshSupporters(d, true)).toEqual({
      kind: "failed",
      held: { version: "2", names: ["A"], fetchedAtMs: 0 },
    });
  });

  it("an older or equal version does not replace the held list", () => {
    const held = { version: "5", names: ["New"], fetchedAtMs: 0 };
    expect(keepNewer(held, { version: 4n, names: ["Old"] }, 10)).toEqual({ ...held, fetchedAtMs: 10 });
    expect(keepNewer(held, { version: 5n, names: ["Other"] }, 10).names).toEqual(["New"]);
    expect(keepNewer(held, { version: 6n, names: ["Newer"] }, 10)).toEqual({
      version: "6",
      names: ["Newer"],
      fetchedAtMs: 10,
    });
  });

  it("fetches at most daily unless About asks", async () => {
    const held = { version: "1", names: ["A"], fetchedAtMs: 1000 };
    expect(supportersFetchDue(held, 1000 + SUPPORTERS_REFRESH_MS - 1, false)).toBe(false);
    expect(supportersFetchDue(held, 1000 + SUPPORTERS_REFRESH_MS, false)).toBe(true);
    expect(supportersFetchDue(held, 1001, true)).toBe(true);
    expect(supportersFetchDue(null, 0, false)).toBe(true);
    expect(supportersFetchDue(held, 500, false)).toBe(true);
    const storage = memoryStorage();
    storage.map.set(SUPPORTERS_STORE_KEY, JSON.stringify(held));
    const d = deps(new Uint8Array([1]), () => ({ version: 2n, names: ["B"] }), storage, 2000);
    expect(await refreshSupporters(d, false)).toEqual({ kind: "ok", held });
    expect(d.fetchMock).not.toHaveBeenCalled();
  });

  it("the row shows only for a list with names", () => {
    expect(showsSupportersRow(null)).toBe(false);
    expect(showsSupportersRow({ version: "1", names: [], fetchedAtMs: 0 })).toBe(false);
    expect(showsSupportersRow({ version: "1", names: ["A"], fetchedAtMs: 0 })).toBe(true);
  });

  it("an unreadable held value reads as none", () => {
    const s = memoryStorage();
    s.map.set(SUPPORTERS_STORE_KEY, "{");
    expect(readHeldSupporters(s)).toBeNull();
    s.map.set(SUPPORTERS_STORE_KEY, JSON.stringify({ version: "x", names: [], fetchedAtMs: 0 }));
    expect(readHeldSupporters(s)).toBeNull();
  });
});
