// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { prepareDeletionTokens, pushUnsubscribeCount } from "./account-deletion-cleanups";

const SUB = "0123456789abcdef0123456789abcdef";

describe("prepareDeletionTokens", () => {
  it("asks the pool for one push-register token per subscription", async () => {
    const asked: Array<[string, number]> = [];
    await prepareDeletionTokens("acct", {
      listRoutes: async () => [{ subIdHex: SUB }, {}, { subIdHex: SUB }, { subIdHex: "bad" }],
      pool: {
        ensureAtLeast: async (context, count) => {
          asked.push([context, count]);
          return count;
        },
      },
    });
    expect(asked).toEqual([["push-register", 2]]);
  });

  it("asks for nothing when the device holds no subscription", async () => {
    let called = false;
    await prepareDeletionTokens("acct", {
      listRoutes: async () => [{}],
      pool: {
        ensureAtLeast: async () => {
          called = true;
          return 0;
        },
      },
    });
    expect(called).toBe(false);
  });

  it("does not throw when the pool gives fewer", async () => {
    await expect(
      prepareDeletionTokens("acct", {
        listRoutes: async () => [{ subIdHex: SUB }, { subIdHex: SUB }],
        pool: { ensureAtLeast: async () => 1 },
      }),
    ).resolves.toBeUndefined();
  });

  it("never asks for more than the daily quota", () => {
    expect(pushUnsubscribeCount(Array.from({ length: 500 }, () => ({ subIdHex: SUB })))).toBe(400);
  });
});
