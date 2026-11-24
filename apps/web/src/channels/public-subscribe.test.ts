// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

const calls: unknown[][] = [];
vi.mock("./channel-access", () => ({
  subscribeToChannel: async (...args: unknown[]): Promise<"reading"> => {
    calls.push(args);
    return "reading";
  },
}));

import { subscribeToPublicChannel } from "./public-subscribe";

describe("the one public-channel Subscribe (search result and Discover card)", () => {
  it("subscribes with the record's read key, so the channel reads at once under its name", async () => {
    const readKey = new Uint8Array(32).fill(7);
    const outcome = await subscribeToPublicChannel({
      idHex: "aa".repeat(16),
      readKey,
      handle: "walknews",
      name: "Walk news",
      description: "Posts from the base walk.",
      avatar: null,
    });
    expect(outcome).toBe("reading");
    expect(calls).toEqual([
      [
        "aa".repeat(16),
        { kind: "public", readKey },
        { name: "Walk news", description: "Posts from the base walk.", handle: "walknews", avatar: null },
      ],
    ]);
  });
});
