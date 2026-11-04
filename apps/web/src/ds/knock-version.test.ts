// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  INITIAL_KNOCK_VERSION,
  learnedOwnerState,
  ownerKnockStep,
  rootKeyForVersion,
  rotatedKnockState,
} from "./knock-seeds";

describe("the knock version (ADR-0087 section 5)", () => {
  it("records the admin set first, without a bump", () => {
    const step = ownerKnockStep(undefined, new Set(["a", "b"]));
    expect(step).toEqual({ next: { version: INITIAL_KNOCK_VERSION, admins: ["a", "b"] }, bumped: false });
  });

  it("a new admin joins the current version", () => {
    const step = ownerKnockStep({ version: 2, admins: ["a"] }, new Set(["a", "b"]));
    expect(step.bumped).toBe(false);
    expect(step.next.version).toBe(2);
  });

  it("an admin who left bumps the version once", () => {
    const step = ownerKnockStep({ version: 2, admins: ["a", "b"] }, new Set(["a"]));
    expect(step).toEqual({ next: { version: 3, admins: ["a"] }, bumped: true });
    expect(ownerKnockStep(step.next, new Set(["a"])).bumped).toBe(false);
  });

  it("a version learned from another owner device records the set afresh", () => {
    expect(ownerKnockStep({ version: 3, admins: null }, new Set(["a"])).bumped).toBe(false);
  });

  it("a key rotation moves later versions to the new root and keeps the earlier ones", () => {
    const after = rotatedKnockState({ version: 2, admins: ["a"] }, "old", "new");
    expect(after).toEqual({ version: 3, admins: ["a"], roots: [{ from: 0, key: "old" }, { from: 3, key: "new" }] });
    expect(rootKeyForVersion(after?.roots, 0)).toBe("old");
    expect(rootKeyForVersion(after?.roots, 2)).toBe("old");
    expect(rootKeyForVersion(after?.roots, 3)).toBe("new");
    // Done once: running the step again changes nothing.
    expect(rotatedKnockState(after ?? undefined, "old", "new")).toBeNull();
    // A second rotation adds a boundary.
    const twice = rotatedKnockState(after ?? undefined, "new", "newer");
    expect(twice?.roots).toEqual([
      { from: 0, key: "old" },
      { from: 3, key: "new" },
      { from: 4, key: "newer" },
    ]);
  });

  it("another owner device learns the version and the root from the header", () => {
    // Rotated elsewhere: version 3 under the new key, this device knew 2.
    const learned = learnedOwnerState({ version: 2, admins: ["a"] }, 3, "new", "new");
    expect(learned).toEqual({ version: 3, admins: null });
    // Before this device adopted the new key: the boundary is recorded.
    const early = learnedOwnerState({ version: 2, admins: ["a"] }, 3, "new", "old");
    expect(early?.roots).toEqual([{ from: 3, key: "new" }]);
    // The same envelope again records nothing.
    expect(learnedOwnerState(early ?? undefined, 3, "new", "old")).toBeNull();
    // Every version arrives, in any order: an older one does not move the
    // newer ones, and the result does not depend on the order.
    const v3 = learnedOwnerState({ version: 2, admins: null }, 3, "new", "new");
    const thenV0 = learnedOwnerState(v3 ?? undefined, 0, "old", "new");
    expect(thenV0?.roots).toEqual([{ from: 0, key: "old" }, { from: 1, key: "new" }]);
    const thenV1 = learnedOwnerState(thenV0 ?? undefined, 1, "old", "new");
    const thenV2 = learnedOwnerState(thenV1 ?? undefined, 2, "old", "new");
    expect(thenV2?.roots).toEqual([{ from: 0, key: "old" }, { from: 3, key: "new" }]);
    expect(rootKeyForVersion(thenV2?.roots, 3) ?? "new").toBe("new");
    // An older version's envelope does not move the version back.
    expect(learnedOwnerState({ version: 5, admins: ["a"] }, 3, null, "new")).toBeNull();
  });

  it("no boundary means the current account identity", () => {
    expect(rootKeyForVersion(undefined, 5)).toBeNull();
  });

  it("an admin reconcile keeps the boundaries", () => {
    const step = ownerKnockStep({ version: 3, admins: ["a", "b"], roots: [{ from: 3, key: "k" }] }, new Set(["a"]));
    expect(step.next.roots).toEqual([{ from: 3, key: "k" }]);
  });
});

describe("the owed republish lands before the bump", () => {
  it("runs before the bumped version is written, and a throw keeps the old version", async () => {
    const writes: string[] = [];
    vi.resetModules();
    vi.doMock("../storage/account-kv", () => ({
      loadAccountJson: async () => null,
      saveAccountJson: async (_a: string, _r: string, body: { o?: Record<string, { v: number }> }) => {
        writes.push(`save v=${body.o?.["g"]?.v ?? "-"}`);
      },
    }));
    const { KnockSeedStore } = await import("./knock-seeds");
    const store = new KnockSeedStore();
    await store.reconcileOwnerAdmins("acct", "g", new Set(["a", "b"]));
    writes.length = 0;
    await expect(
      store.reconcileOwnerAdmins("acct", "g", new Set(["a"]), async () => {
        throw new Error("offline store");
      }),
    ).rejects.toThrow();
    expect(await store.ownerVersion("g")).toBe(0);
    expect(writes).toEqual([]);
    const result = await store.reconcileOwnerAdmins("acct", "g", new Set(["a"]), async () => {
      writes.push("owed");
    });
    expect(result).toEqual({ version: 1, bumped: true });
    expect(writes).toEqual(["owed", "save v=1"]);
    vi.doUnmock("../storage/account-kv");
  });
});

describe("a seed given for a version", () => {
  it("is kept the first time and never replaced", async () => {
    vi.resetModules();
    vi.doMock("../storage/account-kv", () => ({
      loadAccountJson: async () => null,
      saveAccountJson: async () => undefined,
    }));
    const { KnockSeedStore } = await import("./knock-seeds");
    const store = new KnockSeedStore();
    await store.keep("acct", "g", 0, new Uint8Array(32).fill(1));
    await store.keep("acct", "g", 0, new Uint8Array(32).fill(2));
    expect((await store.seedFor("g", 0))?.[0]).toBe(1);
    vi.doUnmock("../storage/account-kv");
  });
});
