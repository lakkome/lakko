// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { HandleWriteError } from "../discovery/handle-lookup";
import { MemoryStorage } from "../test/fake-storage";
import {
  KEPT_GROUP_ATTEMPTS,
  claimOrTakeBackGroupHandle,
  clearClaimedGroupHandles,
  forgetGroupAttempts,
  isClaimedGroupHandle,
  keepGroupAttempt,
  keptGroupAttempts,
} from "./claimed-group-handles";

describe("the group ids kept for a claimed @name", () => {
  it("keeps each attempt newest first and returns the earlier ones", () => {
    const s = new MemoryStorage();
    expect(keepGroupAttempt("a", "team", "g1", s)).toEqual([]);
    expect(keepGroupAttempt("a", "@Team ", "g2", s)).toEqual(["g1"]);
    expect(keptGroupAttempts("a", "team", s)).toEqual(["g2", "g1"]);
    expect(isClaimedGroupHandle("a", "team", s)).toBe(true);
    expect(isClaimedGroupHandle("b", "team", s)).toBe(false);
    expect(isClaimedGroupHandle("a", "other", s)).toBe(false);
  });

  it("keeps a bounded number of attempts", () => {
    const s = new MemoryStorage();
    for (let i = 0; i < KEPT_GROUP_ATTEMPTS + 3; i++) keepGroupAttempt("a", "team", `g${i}`, s);
    const kept = keptGroupAttempts("a", "team", s);
    expect(kept.length).toBe(KEPT_GROUP_ATTEMPTS);
    expect(kept[0]).toBe(`g${KEPT_GROUP_ATTEMPTS + 2}`);
  });

  it("goes when the group exists, the name is someone else's, or with the account", () => {
    const s = new MemoryStorage();
    keepGroupAttempt("a", "team", "g1", s);
    forgetGroupAttempts("a", "team", s);
    expect(isClaimedGroupHandle("a", "team", s)).toBe(false);
    keepGroupAttempt("a", "team", "g1", s);
    clearClaimedGroupHandles("a", s);
    expect(keptGroupAttempts("a", "team", s)).toEqual([]);
  });

  it("an unreadable row reads as none", () => {
    const s = new MemoryStorage();
    s.setItem("lakko.groups.claimed-handles.v1.a", "not json");
    expect(keptGroupAttempts("a", "team", s)).toEqual([]);
  });
});

describe("a New group retry takes its @name back", () => {
  function ops(rekey: (id: string) => Promise<void>) {
    const calls: string[] = [];
    return {
      calls,
      claim: async () => {
        calls.push("claim");
        return "claimed";
      },
      rekeyFrom: async (id: string) => {
        calls.push(`rekey:${id}`);
        await rekey(id);
      },
      update: async () => {
        calls.push("update");
        return "updated";
      },
    };
  }

  it("a first attempt is a plain claim", async () => {
    const o = ops(async () => undefined);
    expect(await claimOrTakeBackGroupHandle([], o)).toBe("claimed");
    expect(o.calls).toEqual(["claim"]);
  });

  it("a retry rekeys from the earlier attempt and updates under the new key", async () => {
    const o = ops(async () => undefined);
    expect(await claimOrTakeBackGroupHandle(["g1"], o)).toBe("updated");
    expect(o.calls).toEqual(["rekey:g1", "update"]);
  });

  it("a first claim that never landed (404) is a plain claim", async () => {
    const o = ops(async () => {
      throw new HandleWriteError(404, "no-such-handle");
    });
    expect(await claimOrTakeBackGroupHandle(["g1"], o)).toBe("claimed");
    expect(o.calls).toEqual(["rekey:g1", "claim"]);
  });

  it("a lost rekey answer: the newest earlier attempt holds the name", async () => {
    // g2's rekey landed but its answer was lost; g1 no longer holds the row.
    const o = ops(async (id) => {
      if (id === "g1") throw new HandleWriteError(401, "statement-refused");
    });
    expect(await claimOrTakeBackGroupHandle(["g2", "g1"], o)).toBe("updated");
    expect(o.calls).toEqual(["rekey:g2", "update"]);
  });

  it("another key's row: the name is held", async () => {
    const o = ops(async () => {
      throw new HandleWriteError(401, "statement-refused");
    });
    const err = await claimOrTakeBackGroupHandle(["g2", "g1"], o).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(HandleWriteError);
    expect((err as HandleWriteError).code).toBe("name-taken");
    expect(o.calls).toEqual(["rekey:g2", "rekey:g1"]);
  });

  it("any other refusal is passed on", async () => {
    const o = ops(async () => {
      throw new HandleWriteError(429, null);
    });
    const err = await claimOrTakeBackGroupHandle(["g1"], o).catch((e: unknown) => e);
    expect((err as HandleWriteError).status).toBe(429);
    expect(o.calls).toEqual(["rekey:g1"]);
  });
});
